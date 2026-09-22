use super::*;

enum EmptyResponse {
    Continue,
    Finish(Box<TaskRun>),
}

struct EmptyResponseContext<'a> {
    session_id: SessionId,
    request: Uuid,
    task: &'a TaskState,
    workspace: &'a TaskWorkspace,
    native: bool,
    discovery: bool,
    cancellation: &'a CancellationToken,
    emit: &'a EventSink,
}

impl Host {
    pub(super) async fn run_contract(
        &self,
        session_id: SessionId,
        request: Uuid,
        admission: TaskRequest,
        cancellation: CancellationToken,
        emit: EventSink,
    ) -> Result<TaskRun, HostError> {
        let native = self.native_tools.is_some();
        let (mut session, mut task, created) = {
            let mut store = self.store.lock().await;
            task_phases::admit(&mut store, session_id, request, admission)?
        };
        if !created {
            task = self.store.lock().await.audit_evidence(task.id)?;
            return Ok(TaskRun {
                session: session_id,
                task,
                message: "Request was already admitted; inspect its recorded outcome".into(),
            });
        }
        let deadline = TaskDeadline::start(&task, cancellation.clone());
        let scope_revision = task.scope_revision;
        // A native task never materializes or removes anything: the session
        // workspace is the user's live directory and the only copy of the work.
        let workspace = if native {
            TaskWorkspace::Native {
                cwd: session.workspace().clone(),
            }
        } else {
            let (updated, workspace) = self.prepare_isolated_workspace(&session, task).await?;
            task = updated;
            workspace
        };
        emit(HostUpdate::TaskChanged {
            session: session_id,
            task: Arc::new(task.clone()),
        });
        let mut provider_retries = 0_u32;
        let mut context_session = self.open_context(session.workspace())?;
        if let Some(context) = &mut context_session {
            context.bind_run(crate::services::ContextRun {
                session: session_id,
                request,
                task: task.id,
            });
        }
        let mut force_native_context = false;
        loop {
            self.install_finished_context_render(session_id).await?;
            task = self.store.lock().await.load(task.id)?;
            let unknown_provider_tokens = task
                .model_reservations
                .iter()
                .filter(|call| {
                    task.model_receipts
                        .get(call)
                        .is_some_and(|receipt| receipt.tokens.is_none())
                })
                .count() as u64
                * session.context_window_tokens();
            if task.usage.tokens.saturating_add(unknown_provider_tokens) >= task.limits().tokens {
                return self
                    .end_task(
                        session_id,
                        request,
                        task.id,
                        Outcome::BudgetExhausted,
                        "Unknown provider usage reserves the full context window; the configured token allowance is exhausted".into(),
                        emit,
                    )
                    .await;
            }
            if task.scope_revision != scope_revision {
                return self.end_task(session_id, request, task.id, Outcome::Blocked, "Turn superseded by a recorded user follow-up; its requirements await admission".into(), emit).await;
            }
            if cancellation.is_cancelled() {
                let outcome = deadline.cancellation_outcome();
                return self
                    .end_task(
                        session_id,
                        request,
                        task.id,
                        outcome,
                        if outcome == Outcome::BudgetExhausted {
                            "Task elapsed-time allowance exhausted"
                        } else {
                            "Cancelled by user"
                        }
                        .into(),
                        emit,
                    )
                    .await;
            }
            let call = Uuid::new_v4();
            let (projection, prompt_cache_lineage, representation_measurement) = {
                let mut store = self.store.lock().await;
                session = store.load_session(session_id)?;
                self.validate_session_admission(&session)?;
                let prompt_cache_lineage = store.prompt_cache_lineage(session_id)?;
                let byte_limit =
                    crate::context::projection_byte_limit(session.context_window_tokens())?;
                let mut projection = crate::context::project(&session, byte_limit)?;
                let native_context_fallback = std::mem::take(&mut force_native_context);
                let representation_measurement = if !native_context_fallback
                    && let Some(cached) = &session.context_view
                {
                    crate::context::reuse_representations(&mut projection, cached, &session);
                    let profile = representation_profile(&task, store.artifacts());
                    select_context_representations(&mut projection, session.model().model, &profile)
                } else {
                    None
                };
                if projection.manifest.omitted_items > 0
                    || !projection.manifest.interrupted_calls.is_empty()
                {
                    // The projection is already in hand for this turn, so the
                    // journaled copy is only a cache for later representation
                    // reuse. A rejected cache write must not fail the turn.
                    if let Err(error) = store.session_command(
                        session_id,
                        session.revision,
                        Uuid::new_v5(&call, b"context-projection"),
                        SessionCommand::ContextProjected {
                            source_revision: session.revision,
                            view: Some(projection.clone()),
                            projection: Vec::new(),
                        },
                    ) {
                        let _ = error;
                    } else {
                        session = store.load_session(session_id)?;
                    }
                }
                (projection, prompt_cache_lineage, representation_measurement)
            };
            let discovery = task.contract.is_none() || task.amendment_pending;
            let TurnTools {
                instructions,
                definitions,
                allowed,
                context,
            } = self
                .prepare_turn_tools(
                    &session,
                    &task,
                    native,
                    discovery,
                    &mut context_session,
                    session_id,
                    request,
                    call,
                    &cancellation,
                )
                .await?;
            let artifacts = self.store.lock().await.artifacts().clone();
            let mut paired_input_tokens = BTreeMap::new();
            let (projection, mut materialized_input) =
                if let Some(segment) = representation_measurement {
                    let mut bitmap_projection = projection;
                    let bitmap_input =
                        materialize_context_projection(&mut bitmap_projection, &artifacts)?;
                    let mut native_projection = bitmap_projection.clone();
                    select_native_representation(&mut native_projection, segment);
                    let native_input =
                        materialize_context_projection(&mut native_projection, &artifacts)?;
                    let (native_count, bitmap_count) = tokio::join!(
                        self.provider
                            .count_input_tokens(session.model(), &native_input),
                        self.provider
                            .count_input_tokens(session.model(), &bitmap_input),
                    );
                    match (native_count, bitmap_count) {
                        (Ok(native), Ok(bitmap)) => {
                            paired_input_tokens.insert(
                                segment,
                                crate::context_cost::PairedInputTokens { native, bitmap },
                            );
                            if bitmap < native {
                                (bitmap_projection, bitmap_input)
                            } else {
                                (native_projection, native_input)
                            }
                        }
                        _ => (native_projection, native_input),
                    }
                } else {
                    let mut projection = projection;
                    let input = materialize_context_projection(&mut projection, &artifacts)?;
                    (projection, input)
                };
            let using_bitmap_context = projection.manifest.segments.iter().any(|segment| {
                matches!(
                    segment.representation,
                    crate::context::ContextRepresentation::Bitmap(_)
                )
            });
            let sent_input = crate::Digest::of_value(&materialized_input)?;
            let stable_segments = projection
                .manifest
                .segments
                .iter()
                .filter(|segment| {
                    matches!(
                        segment.role,
                        crate::context::ContextSegmentRole::StableHistory
                            | crate::context::ContextSegmentRole::DerivedSummary
                    )
                })
                .map(|segment| {
                    let start = usize::try_from(segment.input_range.start)
                        .map_err(|_| HostError::Invalid("context segment range is invalid"))?;
                    let end = usize::try_from(segment.input_range.end)
                        .map_err(|_| HostError::Invalid("context segment range is invalid"))?;
                    let input = materialized_input
                        .get(start..end)
                        .ok_or(HostError::Invalid("context segment range is invalid"))?;
                    crate::Digest::of_value(input).map_err(HostError::from)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let live_input = materialized_input.split_off(projection.manifest.stable_input_items);
            let prompt_input =
                PromptInput::segmented(materialized_input, live_input, stable_segments)
                    .map_err(|_| HostError::Invalid("inference context segmentation is invalid"))?;
            let inference = InferenceRequest::new_segmented(
                session.model(),
                prompt_input,
                definitions,
                instructions,
                session_id.to_string(),
                crate::context::output_token_limit(session.context_window_tokens()),
            )
            .and_then(|request| request.with_prompt_cache_key(prompt_cache_lineage.to_string()))
            .map_err(|_| {
                HostError::Invalid("inference context could not be represented without loss")
            })?;
            {
                let mut store = self.store.lock().await;
                if store.load(task.id)?.scope_revision != scope_revision {
                    drop(store);
                    return self
                        .end_task(
                            session_id,
                            request,
                            task.id,
                            Outcome::Blocked,
                            "User input superseded this prepared provider request before dispatch"
                                .into(),
                            emit,
                        )
                        .await;
                }
                let reservation = store.reserve_model_call(task.id, call);
                match reservation {
                    Ok(state) => task = state,
                    Err(StoreError::Budget) => {
                        drop(store);
                        return self
                            .end_task(
                                session_id,
                                request,
                                task.id,
                                Outcome::BudgetExhausted,
                                "Task execution budget exhausted".into(),
                                emit,
                            )
                            .await;
                    }
                    Err(StoreError::Cancelled) => {
                        drop(store);
                        return self
                            .end_task(
                                session_id,
                                request,
                                task.id,
                                Outcome::Cancelled,
                                "Cancelled by user".into(),
                                emit,
                            )
                            .await;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            {
                let mut store = self.store.lock().await;
                crate::trace::record_dispatch(
                    &mut store, session_id, request, task.id, None, call, &inference,
                )?;
            }
            let streaming = emit.clone();
            let response = self
                .provider
                .respond(&inference, &cancellation, move |delta| {
                    streaming(HostUpdate::Provisional {
                        session: session_id,
                        request,
                        delta,
                    })
                })
                .await;
            self.schedule_context_render(
                session_id,
                projection.clone(),
                session.clone(),
                cancellation.clone(),
            )
            .await;
            let representation = representation_observation(
                session.model().model,
                &projection,
                inference.cache_identity(),
                &response,
                paired_input_tokens,
            )?;
            record_provider_cost(&self.store, session_id, request, call, &response).await?;
            let retryable_rejection = response.retryable_pre_generation_rejection();
            let tokens = response.accounted_tokens();
            {
                let mut store = self.store.lock().await;
                let report = store
                    .artifacts()
                    .put(&serde_json::to_vec(
                        &json!({"version":3,"model":session.model(),"harness_binding":session.admission().map(SessionAdmissionProfile::binding),"host_config":self.config_identity,"adapter_version":env!("CARGO_PKG_VERSION"),"context":projection.manifest,"cache":inference.cache_identity(),"sent_input":sent_input,"representation":representation,"outcome":response}),
                    )?)
                    .map_err(StoreError::from)?;
                let status = if cancellation.is_cancelled() {
                    ModelCallStatus::Cancelled
                } else if response
                    .response
                    .as_ref()
                    .is_some_and(|output| output.status == ResponseStatus::Completed)
                    && response.failure.is_none()
                {
                    ModelCallStatus::Completed
                } else {
                    ModelCallStatus::Failed
                };
                store.record_model_call(
                    task.id,
                    call,
                    ModelCallReceipt {
                        status,
                        tokens,
                        report,
                    },
                )?;
            }
            let retry_native_context = using_bitmap_context
                && response.response.is_none()
                && response.partial_text.is_empty()
                && response.partial_items.is_empty()
                && response.attempts.iter().all(|attempt| !attempt.dispatched)
                && response.failure.as_ref().is_some_and(|failure| {
                    matches!(
                        failure.kind,
                        crate::inference::FailureKind::InvalidRequest
                            | crate::inference::FailureKind::SizeLimit
                    )
                });
            if retry_native_context {
                force_native_context = true;
                continue;
            }
            if self.store.lock().await.load(task.id)?.scope_revision != scope_revision {
                return self.end_task(session_id, request, task.id, Outcome::Blocked, "Provider response retained as an attempt receipt; its authority was superseded by user input".into(), emit).await;
            }
            if retryable_rejection
                && provider_retries < MAX_RECOVERABLE_PROVIDER_RETRIES
                && !cancellation.is_cancelled()
            {
                // Re-enter admission so each retry keeps its own receipt and budget charge.
                let retry = provider_retries;
                provider_retries += 1;
                if self.wait_for_provider_retry(retry, &cancellation).await {
                    continue;
                }
            }
            provider_retries = 0;
            let Some(output) = response.response.filter(|output| {
                output.status == ResponseStatus::Completed && response.failure.is_none()
            }) else {
                let outcome = if cancellation.is_cancelled() {
                    deadline.cancellation_outcome()
                } else {
                    Outcome::Failed
                };
                let mut reason = "Provider request did not complete".to_owned();
                if let Some(failure) = &response.failure {
                    reason.push_str(&format!(": {}", failure.kind));
                    if let Some(status) = failure.http_status {
                        reason.push_str(&format!(" (HTTP {status})"));
                    }
                }
                reason.push_str("; partial output is not acceptance evidence");
                return self
                    .end_task(session_id, request, task.id, outcome, reason, emit)
                    .await;
            };
            if tokens.is_none() {
                return self.end_task(session_id, request, task.id, Outcome::BudgetExhausted, "Provider token usage is unknown; the configured token allowance cannot be established".into(), emit).await;
            }
            {
                let mut store = self.store.lock().await;
                task = store.load(task.id)?;
                let mut state = store.load_session(session_id)?;
                state = store.session_command(
                    session_id,
                    state.revision,
                    Uuid::new_v5(&call, b"provider-usage"),
                    SessionCommand::ProviderUsage {
                        request,
                        call: Some(call),
                        usage: output.usage.clone(),
                        representation: Some(representation.clone()),
                    },
                )?;
                store.session_command(
                    session_id,
                    state.revision,
                    Uuid::new_v5(&call, b"response"),
                    SessionCommand::Response {
                        request,
                        items: output.history_items,
                    },
                )?;
            }
            if task.usage.model_calls > task.limits().model_calls
                || task.usage.tokens > task.limits().tokens
            {
                return self
                    .end_task(
                        session_id,
                        request,
                        task.id,
                        Outcome::BudgetExhausted,
                        "Provider usage exceeded the configured task budget".into(),
                        emit,
                    )
                    .await;
            }
            let proposals = output
                .output
                .into_iter()
                .filter_map(|item| match item {
                    OutputItem::ToolProposal(proposal) => Some(proposal),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if proposals.is_empty() {
                match self
                    .handle_empty_response(EmptyResponseContext {
                        session_id,
                        request,
                        task: &task,
                        workspace: &workspace,
                        native,
                        discovery,
                        cancellation: &cancellation,
                        emit: &emit,
                    })
                    .await?
                {
                    EmptyResponse::Continue => continue,
                    EmptyResponse::Finish(run) => return Ok(*run),
                }
            }
            if let ProposalOutcome::Finish(run) = self
                .execute_proposals(
                    session_id,
                    request,
                    &task,
                    scope_revision,
                    &workspace,
                    native,
                    proposals,
                    &allowed,
                    &context,
                    &mut context_session,
                    &cancellation,
                    &emit,
                )
                .await?
            {
                return Ok(*run);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_proposals(
        &self,
        session_id: SessionId,
        request: Uuid,
        task: &TaskState,
        scope_revision: u64,
        workspace: &TaskWorkspace,
        native: bool,
        proposals: Vec<ToolProposal>,
        allowed: &std::collections::BTreeSet<String>,
        context: &std::collections::BTreeSet<String>,
        context_session: &mut Option<Box<dyn ContextSession>>,
        cancellation: &CancellationToken,
        emit: &EventSink,
    ) -> Result<ProposalOutcome, HostError> {
        let exclusive_control = proposals.len() == 1;
        for proposal in proposals {
            if cancellation.is_cancelled() {
                break;
            }
            let args = if proposal.validity == ArgumentValidity::JsonObject {
                serde_json::from_str::<Value>(&proposal.arguments).ok()
            } else {
                None
            };
            emit(HostUpdate::ToolStarted {
                session: session_id,
                call_id: proposal.call_id.clone(),
                name: proposal.name.clone(),
                arguments: args.clone().unwrap_or(Value::Null),
            });
            let result = match args {
                None => {
                    json!({"error":"tool arguments must be a JSON object; no semantic repair was attempted"})
                }
                Some(_) if !allowed.contains(&proposal.name) => {
                    json!({"error":"this tool is not admitted in the current task phase"})
                }
                Some(args) => {
                    if matches!(
                        proposal.name.as_str(),
                        "propose_completion"
                            | "report_blocker"
                            | "propose_contract"
                            | "transition_context"
                    ) && !exclusive_control
                    {
                        json!({"error":"completion, contract, blocker and context transition proposals must be the only tool call in their response; settle other work first"})
                    } else if proposal.name == "propose_contract" {
                        if native {
                            json!({"error":"native host mode does not admit contracts; continue the work directly and finish with a summary or propose_completion"})
                        } else {
                            let (_, baseline, _) = workspace.isolated()?;
                            self.admit_proposal(task.id, scope_revision, args, baseline)
                                .await?
                        }
                    } else if context.contains(&proposal.name) {
                        if self.store.lock().await.load(task.id)?.scope_revision != scope_revision {
                            json!({"error":"tool proposal predates a user follow-up"})
                        } else {
                            Self::execute_context_tool(
                                context_session
                                    .as_deref_mut()
                                    .expect("admitted context service"),
                                &proposal.name,
                                args,
                                ContextAccess::ReadWrite,
                                cancellation,
                            )
                            .await
                        }
                    } else if proposal.name == "read_review_feedback" {
                        self.read_review_feedback(session_id, args).await?
                    } else if proposal.name == "transition_context" {
                        self.transition_context(session_id, request, &proposal.call_id, args)
                            .await?
                    } else if proposal.name == "read_context" {
                        self.read_context(session_id, args).await?
                    } else if proposal.name == "interpreter_eval" {
                        self.evaluate_cell(
                            session_id,
                            request,
                            task.id,
                            scope_revision,
                            &proposal.call_id,
                            args,
                            allowed,
                            workspace,
                            cancellation.clone(),
                            emit.clone(),
                        )
                        .await?
                    } else if proposal.name == "read_legacy" {
                        #[derive(Deserialize)]
                        #[serde(deny_unknown_fields)]
                        struct Query {
                            #[serde(default)]
                            cursor: Option<crate::import::ImportCursor>,
                        }
                        match serde_json::from_value::<Query>(args) {
                            Ok(query) => match self
                                .legacy_page(session_id, query.cursor, 8, 16 * 1024)
                                .await
                            {
                                Ok(page) => {
                                    json!({"source":"historical data; not current authority or execution evidence","page":page})
                                }
                                Err(error) => json!({"error":error.to_string()}),
                            },
                            Err(error) => json!({"error":error.to_string()}),
                        }
                    } else if proposal.name == "propose_completion" {
                        if !args.as_object().is_some_and(|args| args.is_empty()) {
                            json!({"error":"propose_completion takes no arguments"})
                        } else if native {
                            self.record_tool_result(
                                session_id,
                                request,
                                &proposal,
                                &json!({"accepted":true,"finished_unverified":true}),
                                emit.clone(),
                            )
                            .await?;
                            let run = self
                                .end_task(
                                    session_id,
                                    request,
                                    task.id,
                                    Outcome::FinishedUnverified,
                                    "Finished on the native host without verification evidence"
                                        .into(),
                                    emit.clone(),
                                )
                                .await?;
                            return Ok(ProposalOutcome::Finish(Box::new(run)));
                        } else {
                            let (working, baseline, baseline_path) = workspace.isolated()?;
                            if let Some(completed) = self
                                .try_complete(
                                    session_id,
                                    request,
                                    task.id,
                                    working,
                                    baseline,
                                    baseline_path,
                                    cancellation.clone(),
                                    emit.clone(),
                                )
                                .await?
                            {
                                let result = json!({"accepted":true,"certificate":completed.task.certificates.last()});
                                self.record_tool_result(
                                    session_id,
                                    request,
                                    &proposal,
                                    &result,
                                    emit.clone(),
                                )
                                .await?;
                                return Ok(ProposalOutcome::Finish(Box::new(
                                    self.finish_run(request, completed, emit.clone()).await?,
                                )));
                            }
                            json!({"accepted":false,"reason":"required evidence did not satisfy the completion contract"})
                        }
                    } else if proposal.name == "report_blocker" {
                        #[derive(Deserialize)]
                        #[serde(deny_unknown_fields)]
                        struct Blocker {
                            reason: String,
                        }
                        match serde_json::from_value::<Blocker>(args) {
                            Ok(blocker) if !blocker.reason.trim().is_empty() => {
                                self.record_tool_result(
                                    session_id,
                                    request,
                                    &proposal,
                                    &json!({"reported":true,"reason":blocker.reason}),
                                    emit.clone(),
                                )
                                .await?;
                                let run = self
                                    .end_task(
                                        session_id,
                                        request,
                                        task.id,
                                        Outcome::Blocked,
                                        blocker.reason,
                                        emit.clone(),
                                    )
                                    .await?;
                                return Ok(ProposalOutcome::Finish(Box::new(run)));
                            }
                            _ => json!({"error":"a nonempty blocker reason is required"}),
                        }
                    } else {
                        self.dispatch(
                            session_id,
                            request,
                            task.id,
                            scope_revision,
                            &proposal.name,
                            &proposal.call_id,
                            args,
                            workspace,
                            cancellation.clone(),
                        )
                        .await?
                    }
                }
            };
            self.record_tool_result(session_id, request, &proposal, &result, emit.clone())
                .await?;
        }
        Ok(ProposalOutcome::Continue)
    }

    #[allow(clippy::too_many_arguments)]
    async fn prepare_turn_tools(
        &self,
        session: &SessionState,
        task: &TaskState,
        native: bool,
        discovery: bool,
        context_session: &mut Option<Box<dyn ContextSession>>,
        session_id: SessionId,
        request: Uuid,
        call: Uuid,
        cancellation: &CancellationToken,
    ) -> Result<TurnTools, HostError> {
        let policy = if let Some(intake) = task.intake {
            self.store
                .lock()
                .await
                .artifacts()
                .read(intake)
                .map_err(StoreError::from)?
        } else {
            Vec::new()
        };
        let mut sections = Vec::with_capacity(6);
        if native {
            sections.push(NATIVE_INSTRUCTIONS.to_owned());
            sections.push("Use direct native tools for normal work. interpreter_eval is optional: use it for data-heavy filtering or multi-step composition that benefits from retained working values. Do not wrap ordinary reads, searches, edits, or commands in interpreter cells. If a cell fails, continue with direct tools when possible. Keep completed inner-call receipts and never automatically repeat an unknown effect.".to_owned());
        } else {
            if task
                .contract
                .as_ref()
                .is_some_and(|contract| !contract.open_questions.is_empty())
            {
                sections.push("The contract has unresolved product questions. Continue workspace research to ground them; report a precise blocker if user input is needed. Do not claim completion while these questions remain unresolved.".to_owned());
            }
            if discovery {
                sections.push(ADMISSION_INSTRUCTIONS.to_owned());
            }
        }
        sections.push(format!(
            "Pinned harness behavior:\n{}",
            session
                .behavior_instructions()
                .map_err(HostError::Invalid)?
        ));
        sections.push(format!("Original user request:\n{}", task.request));
        sections.push(format!(
            "Protected intake policy:\n{}",
            String::from_utf8_lossy(&policy)
        ));
        if !native {
            sections.push(format!(
                "Authoritative task contract:\n{}",
                serde_json::to_string(&task.contract)?
            ));
        }
        let mut instructions = sections.join("\n\n");
        if !native && task.amendment_pending {
            let artifacts = self.store.lock().await.artifacts().clone();
            let directives = task
                .directives
                .iter()
                .map(|(id, digest)| {
                    Ok(json!({"request":id,"input":crate::input::load(*digest, &artifacts)?.messages}))
                })
                .collect::<Result<Vec<Value>, StoreError>>()?;
            instructions.push_str(&format!("\n\nRecorded user follow-ups (data from the authenticated operator):\n{}\nPrefer admitting these follow-ups with propose_contract before implementation; workspace tools remain available. Propose additions with new requirement/check IDs. Existing requirements, checks, limits, protected behavior, original outcome and scope are retained by the host. Reusing an existing ID with changed meaning is rejected. A follow-up cannot silently weaken the previous contract.", serde_json::to_string(&directives)?));
        }
        self.prepare_context(
            context_session,
            session_id,
            request,
            call,
            &mut instructions,
            cancellation,
        )
        .await?;

        let mut definitions = if native {
            native_tool_definitions()
        } else {
            sandbox_tool_definitions(discovery, self.subagents.enabled())
        };
        if self.experimental_context_transitions {
            definitions.push(crate::context::transitions::tool_definition());
            instructions.push_str(&format!("\n\nExperimental context transitions are enabled. Settled source history: [0, {}). Current request history is a protected native live tail. Use read_context for exact indexed source. Summaries are derived, may omit obligations, and never alter task truth or completion checks.", session.settled_history_items));
        }
        let context_definitions = context_session
            .as_ref()
            .map(|context| context.definitions(ContextAccess::ReadWrite))
            .unwrap_or_default();
        let context = context_definitions
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect();
        definitions.extend(context_definitions);
        let allowed = definitions
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect();
        Ok(TurnTools {
            instructions,
            definitions,
            allowed,
            context,
        })
    }

    async fn handle_empty_response(
        &self,
        context: EmptyResponseContext<'_>,
    ) -> Result<EmptyResponse, HostError> {
        let EmptyResponseContext {
            session_id,
            request,
            task,
            workspace,
            native,
            discovery,
            cancellation,
            emit,
        } = context;
        if native {
            return Ok(EmptyResponse::Finish(Box::new(
                self.end_task(
                    session_id,
                    request,
                    task.id,
                    Outcome::FinishedUnverified,
                    "Finished on the native host without verification evidence".into(),
                    emit.clone(),
                )
                .await?,
            )));
        }
        if discovery {
            self.feedback(session_id, "The task still has no accepted executable contract. Inspect the source and submit propose_contract; final prose does not establish a verifiable contract or satisfy the request.").await?;
            return Ok(EmptyResponse::Continue);
        }
        let (working, baseline, baseline_path) = workspace.isolated()?;
        if let Some(completed) = self
            .try_complete(
                session_id,
                request,
                task.id,
                working,
                baseline,
                baseline_path,
                cancellation.clone(),
                emit.clone(),
            )
            .await?
        {
            return Ok(EmptyResponse::Finish(Box::new(
                self.finish_run(request, completed, emit.clone()).await?,
            )));
        }
        self.feedback(session_id, "The host rejected completion. Use task_status and verify_task to inspect the unmet obligations, change the implementation, then propose completion again. Repeating a final answer cannot satisfy the contract.").await?;
        Ok(EmptyResponse::Continue)
    }
}

enum ProposalOutcome {
    Continue,
    Finish(Box<TaskRun>),
}

struct TurnTools {
    instructions: String,
    definitions: Vec<Value>,
    allowed: std::collections::BTreeSet<String>,
    context: std::collections::BTreeSet<String>,
}

pub(super) fn admit(
    store: &mut Store,
    session: SessionId,
    request: Uuid,
    admission: TaskRequest,
) -> Result<(SessionState, TaskState, bool), HostError> {
    let admitted = match admission {
        TaskRequest::Continue { request } => {
            let classification = store.ordinary_classification(session, request)?;
            let mut run = store.continue_submission(session, request)?;
            if let Some((kind, call, receipt, started_ms)) = classification {
                run.1 = store.account_ordinary_classification(
                    run.1.id, request, kind, call, receipt, started_ms,
                )?;
            }
            run
        }
        TaskRequest::DiscoverInput {
            input,
            limits,
            intake,
        } => {
            let classification = store.ordinary_classification(session, request)?;
            let mut run = store.start_prepared_request(session, request, input, limits, intake)?;
            if let Some((kind, call, receipt, started_ms)) = classification {
                run.1 = store.account_ordinary_classification(
                    run.1.id, request, kind, call, receipt, started_ms,
                )?;
            }
            run
        }
        TaskRequest::Discover {
            input,
            limits,
            intake,
        } => store.start_request(session, request, input, limits, intake)?,
        TaskRequest::Start(contract) => store.start_task(session, request, *contract)?,
        TaskRequest::Resume {
            task,
            revision,
            reason,
        } => store.resume_task(session, request, task, revision, reason)?,
    };
    Ok(admitted)
}

pub(super) fn next_phase(task: &TaskState) -> Phase {
    if task
        .contract
        .as_ref()
        .is_some_and(|contract| contract.open_questions.is_empty())
        && !task.amendment_pending
    {
        Phase::Implement
    } else {
        Phase::Understand
    }
}
