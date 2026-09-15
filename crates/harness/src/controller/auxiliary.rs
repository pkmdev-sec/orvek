use super::*;
use crate::{
    auxiliary::{AuxiliaryContext, AuxiliaryRecord, AuxiliarySpec, AuxiliaryStatus},
    submission::{OrdinaryKind, SubmissionStatus, parse_ordinary_kind},
};

impl Host {
    pub(super) async fn execute_auxiliary(
        &self,
        session: SessionId,
        request: Uuid,
        spec: AuxiliarySpec,
    ) -> Result<SubmissionStatus, HostError> {
        let permit = self
            .runs
            .clone()
            .try_acquire_owned()
            .map_err(|_| HostError::Busy)?;
        let cancellation = self.queue_stop.child_token();
        {
            let mut active = self.active.lock().await;
            if !self.accepting.load(Ordering::Acquire) {
                return Err(HostError::ShuttingDown);
            }
            if active.contains_key(&session) {
                return Err(HostError::Busy);
            }
            active.insert(session, cancellation.clone());
        }
        let result = async {
            let state = self.store.lock().await.begin_auxiliary(session, request)?;
            let run = tokio::time::timeout(Duration::from_millis(spec.limits.elapsed_ms), self.run_auxiliary(state, request, &spec, cancellation.clone())).await;
            let (status, text, error) = match run {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => (if cancellation.is_cancelled() { AuxiliaryStatus::Cancelled } else if matches!(error, HostError::Store(StoreError::Budget)) { AuxiliaryStatus::BudgetExhausted } else { AuxiliaryStatus::Failed }, String::new(), Some(error.to_string())),
                Err(_) => { cancellation.cancel(); (AuxiliaryStatus::BudgetExhausted, String::new(), Some("Auxiliary elapsed-time allowance exhausted; dispatched calls may have unknown billing".into())) },
            };
            self.store.lock().await.publish_auxiliary(session, request, status, text, error.clone())?;
            Ok(SubmissionStatus::Finished { task: None, outcome: None, error })
        }.await;
        self.active.lock().await.remove(&session);
        drop(permit);
        self.queue_wake.notify_waiters();
        result
    }

    pub(super) async fn classify_ordinary(
        &self,
        session: SessionId,
        request: Uuid,
        submission: &crate::submission::Submission,
        limits: crate::contract::Limits,
    ) -> Result<OrdinaryKind, HostError> {
        // Register the token before dispatching. `cancel_request` can only
        // reach an in-flight call through `self.active`, so without this entry
        // cancelling during classification is a no-op against the provider
        // call. Mirrors `execute_auxiliary` below.
        let cancellation = self.queue_stop.child_token();
        {
            let mut active = self.active.lock().await;
            if active.contains_key(&session) {
                return Err(HostError::Busy);
            }
            active.insert(session, cancellation.clone());
        }
        let result = self
            .classify_ordinary_call(session, request, submission, limits, &cancellation)
            .await;
        self.active.lock().await.remove(&session);
        result
    }

    async fn classify_ordinary_call(
        &self,
        session: SessionId,
        request: Uuid,
        submission: &crate::submission::Submission,
        _limits: crate::contract::Limits,
        cancellation: &CancellationToken,
    ) -> Result<OrdinaryKind, HostError> {
        let (artifacts, model, input) = {
            let store = self.store.lock().await;
            let session_state = store.load_session(session)?;
            self.validate_session_admission(&store, &session_state)?;
            (
                store.artifacts().clone(),
                session_state.model(),
                crate::input::load(submission.input, store.artifacts())?,
            )
        };
        let request_body = ordinary_classification_request(model, session, input.messages)?;
        let invocation = artifacts
            .put(&serde_json::to_vec(
                &request_body.wire(crate::inference::Transport::Http),
            )?)
            .map_err(StoreError::from)?;
        let call = Uuid::new_v4();
        {
            let mut store = self.store.lock().await;
            store.begin_classification(session, request)?;
            store.record_auxiliary(
                session,
                request,
                Uuid::new_v5(&call, b"classification-intended"),
                AuxiliaryRecord::ClassificationIntended {
                    at_ms: crate::store::now_ms(),
                    input: invocation,
                    call,
                },
            )?;
        }
        let response = self
            .provider
            .respond(&request_body, cancellation, |_| {})
            .await;
        let tokens = if response.billing_uncertain() {
            None
        } else {
            response
                .response
                .as_ref()
                .and_then(|output| output.usage.total_tokens)
                .or_else(|| {
                    response
                        .attempts
                        .iter()
                        .all(|attempt| !attempt.dispatched)
                        .then_some(0)
                })
        };
        let successful = response.failure.is_none()
            && response
                .response
                .as_ref()
                .is_some_and(|output| output.status == ResponseStatus::Completed);
        let report = artifacts
            .put(&serde_json::to_vec(&response)?)
            .map_err(StoreError::from)?;
        let receipt = ModelCallReceipt {
            status: if successful {
                ModelCallStatus::Completed
            } else {
                ModelCallStatus::Unknown
            },
            tokens,
            report,
        };
        let kind = successful
            .then(|| {
                response
                    .response
                    .as_ref()
                    .and_then(|output| classification_text(&output.output))
                    .and_then(|text| parse_ordinary_kind(text).ok())
            })
            .flatten();
        {
            let mut store = self.store.lock().await;
            store.record_auxiliary(
                session,
                request,
                Uuid::new_v5(&call, b"classification-observed"),
                AuxiliaryRecord::ClassificationObserved {
                    call,
                    receipt,
                    kind: kind
                        .map(|kind| kind.as_tag())
                        .unwrap_or_default()
                        .to_owned(),
                },
            )?;
        }
        match (tokens, kind) {
            (Some(_), Some(kind)) => Ok(kind),
            _ => Err(HostError::Invalid("ordinary classification failed closed")),
        }
    }

    pub(super) async fn run_auxiliary(
        &self,
        session: SessionState,
        request: Uuid,
        spec: &AuxiliarySpec,
        cancellation: CancellationToken,
    ) -> Result<(AuxiliaryStatus, String, Option<String>), HostError> {
        let (artifacts, input) = {
            let store = self.store.lock().await;
            self.validate_session_admission(&store, &session)?;
            (
                store.artifacts().clone(),
                crate::input::load(
                    store.submission(session.id, request)?.input,
                    store.artifacts(),
                )?,
            )
        };
        let mut history = if spec.context == AuxiliaryContext::CurrentConversation {
            session.history.clone()
        } else {
            Vec::new()
        };
        if spec.context != AuxiliaryContext::CurrentConversation || !spec.visible() {
            history.extend(input.messages);
        } else {
            history.extend(input.messages.clone());
        }
        let pending = if let Some(task) = session.current_task {
            self.store.lock().await.load(task)?.workspace_override
        } else {
            None
        };
        let source = if let Some(review) = spec.review {
            crate::review::manifest(&artifacts, review)?;
            None
        } else {
            let working = session
                .current_task
                .map(|id| {
                    self.root
                        .join("workspaces")
                        .join(id.to_string())
                        .join("working")
                })
                .filter(|path| path.is_dir());
            let snapshot = if let Some(source) = pending {
                Snapshot::load(source, &artifacts)?
            } else if let Some(working) = working {
                Snapshot::capture(&working, SnapshotPolicy::default(), &artifacts)?
            } else {
                self.prepare_workspace(&session, &artifacts)?.1
            };
            Some((snapshot.publish(&artifacts)?, snapshot))
        };
        let temporary = tempfile::tempdir_in(&self.root)?;
        let working = temporary.path().join("source");
        if let Some((_, snapshot)) = &source {
            snapshot.materialize(&working, &artifacts, false)?;
        }
        self.store.lock().await.record_auxiliary(
            session.id,
            request,
            Uuid::new_v5(&request, b"auxiliary-started"),
            AuxiliaryRecord::Started {
                at_ms: crate::store::now_ms(),
                source: source.as_ref().map(|source| source.0),
                review: spec.review,
            },
        )?;
        let definitions = auxiliary_tools(spec);
        let allowed = definitions
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let mut seen = std::collections::BTreeSet::new();
        loop {
            if cancellation.is_cancelled() {
                return Ok((
                    AuxiliaryStatus::Cancelled,
                    String::new(),
                    Some("Auxiliary request cancelled".into()),
                ));
            }
            let mut view = session.clone();
            view.history = history;
            // Downstream-only fix: the cloned projection view must not look like it
            // still has a request in flight.
            view.active_request = None;
            let byte_limit =
                crate::context::projection_byte_limit(session.context_window_tokens())?;
            let projection = crate::context::project(&view, byte_limit)?;
            history = projection.input;
            let materialized = crate::input::materialize(history.clone(), &artifacts)?;
            let instructions = format!(
                "Provide {:?} assistance. {AUXILIARY_INSTRUCTIONS}\nPinned harness behavior:\n{}\nOriginal auxiliary request:\n{}",
                spec.kind,
                session
                    .behavior_instructions()
                    .map_err(HostError::Invalid)?,
                input.text
            );
            let inference = InferenceRequest::new(
                session.model(),
                materialized,
                definitions.clone(),
                instructions,
                session.id.to_string(),
                8192,
            )
            .map_err(|_| HostError::Invalid("invalid auxiliary inference context"))?;
            let invocation = artifacts
                .put(&serde_json::to_vec(
                    &inference.wire(crate::inference::Transport::Http),
                )?)
                .map_err(StoreError::from)?;
            let call = Uuid::new_v4();
            self.store.lock().await.record_auxiliary(
                session.id,
                request,
                Uuid::new_v5(&call, b"intent"),
                AuxiliaryRecord::ModelIntended {
                    call,
                    input: invocation,
                },
            )?;
            let previews = self.previews.clone();
            let response = self
                .provider
                .respond(&inference, &cancellation, move |delta| {
                    let update = HostUpdate::Provisional {
                        session: session.id,
                        request,
                        delta,
                    };
                    if serde_json::to_vec(&update).is_ok_and(|bytes| bytes.len() <= 64 * 1024) {
                        let _ = previews.send(update);
                    }
                })
                .await;
            let tokens = if response.billing_uncertain() {
                None
            } else {
                response
                    .response
                    .as_ref()
                    .and_then(|output| output.usage.total_tokens)
                    .or_else(|| {
                        response
                            .attempts
                            .iter()
                            .all(|attempt| !attempt.dispatched)
                            .then_some(0)
                    })
            };
            let successful = response.failure.is_none()
                && response
                    .response
                    .as_ref()
                    .is_some_and(|output| output.status == ResponseStatus::Completed);
            let report = artifacts
                .put(&serde_json::to_vec(&response)?)
                .map_err(StoreError::from)?;
            self.store.lock().await.record_auxiliary(
                session.id,
                request,
                Uuid::new_v5(&call, b"receipt"),
                AuxiliaryRecord::ModelObserved {
                    call,
                    receipt: ModelCallReceipt {
                        status: if cancellation.is_cancelled() {
                            ModelCallStatus::Cancelled
                        } else if successful {
                            ModelCallStatus::Completed
                        } else {
                            ModelCallStatus::Failed
                        },
                        tokens,
                        report,
                    },
                },
            )?;
            if cancellation.is_cancelled() {
                return Ok((
                    AuxiliaryStatus::Cancelled,
                    String::new(),
                    Some("Auxiliary request cancelled".into()),
                ));
            }
            if tokens.is_none() {
                return Ok((
                    AuxiliaryStatus::UnknownBilling,
                    String::new(),
                    Some(
                        "Provider usage is unknown; no further auxiliary calls were admitted"
                            .into(),
                    ),
                ));
            }
            if !successful {
                return Ok((
                    AuxiliaryStatus::Failed,
                    String::new(),
                    Some("Provider did not complete the auxiliary request".into()),
                ));
            }
            let output = response.response.expect("successful response has output");
            history.extend(output.history_items);
            let mut text = String::new();
            let mut proposals = Vec::new();
            for item in output.output {
                match item {
                    OutputItem::Message { text: part, .. } => text.push_str(&part),
                    OutputItem::ToolProposal(proposal) => proposals.push(proposal),
                    _ => {}
                }
            }
            if proposals.is_empty() {
                if text.trim().is_empty() {
                    return Ok((
                        AuxiliaryStatus::Failed,
                        text,
                        Some("Provider returned no answer".into()),
                    ));
                }
                return Ok((AuxiliaryStatus::Completed, text, None));
            }
            for proposal in proposals {
                if !seen.insert(proposal.call_id.clone()) {
                    return Err(HostError::Invalid(
                        "provider reused an auxiliary tool call ID",
                    ));
                }
                let args = (proposal.validity == ArgumentValidity::JsonObject)
                    .then(|| serde_json::from_str::<Value>(&proposal.arguments))
                    .transpose()?;
                let result = if !allowed.contains(proposal.name.as_str()) {
                    json!({"error":"tool is not admitted for this read-only request"})
                } else if let Some(args) = args {
                    self.auxiliary_tool(
                        &session,
                        request,
                        spec,
                        &working,
                        &proposal,
                        args,
                        cancellation.clone(),
                    )
                    .await?
                } else {
                    json!({"error":"tool arguments must be a JSON object"})
                };
                let input = artifacts
                    .put(&serde_json::to_vec(&proposal)?)
                    .map_err(StoreError::from)?;
                let encoded = serde_json::to_string(&result)?;
                let output = artifacts
                    .put(encoded.as_bytes())
                    .map_err(StoreError::from)?;
                self.store.lock().await.record_auxiliary(
                    session.id,
                    request,
                    Uuid::new_v5(&request, proposal.call_id.as_bytes()),
                    AuxiliaryRecord::ToolObserved {
                        call_id: proposal.call_id.clone(),
                        name: proposal.name,
                        input,
                        output,
                    },
                )?;
                history.push(json!({"type":"function_call_output","call_id":proposal.call_id,"output":encoded}));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn auxiliary_tool(
        &self,
        session: &SessionState,
        request: Uuid,
        spec: &AuxiliarySpec,
        working: &Path,
        proposal: &ToolProposal,
        args: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, HostError> {
        if proposal.name == "task_status" {
            if !args.as_object().is_some_and(|args| args.is_empty()) {
                return Ok(json!({"error":"task_status takes no arguments"}));
            }
            let Some(task) = session.current_task else {
                return Ok(json!({"error":"no current task"}));
            };
            let state = self.store.lock().await.load(task)?;
            return Ok(
                json!({"task":state.id,"request":state.request,"phase":state.phase,"outcome":state.outcome,"scope_revision":state.scope_revision,"requirements":state.contract.as_ref().map(|contract|contract.requirements.len()),"evidence":state.evidence,"findings":state.findings}),
            );
        }
        if proposal.name == "read_review_feedback" {
            return self.read_review_feedback(session.id, args).await;
        }
        if proposal.name == "read_context" {
            return self.read_context(session.id, args).await;
        }
        if proposal.name == "config_show" {
            if !args.as_object().is_some_and(|args| args.is_empty()) {
                return Ok(json!({"error":"config_show takes no arguments"}));
            }
            return Ok(
                json!({"model":session.model(),"workspace":session.workspace(),"host_config":self.config_identity,"executor":self.executor.environment(),"admission":session.admission().map(|profile| json!({"request":profile.request_digest(),"binding":profile.binding(),"provenance":profile.provenance(),"authority":profile.authority()}))}),
            );
        }
        if proposal.name == "read_review" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Query {
                #[serde(default)]
                offset: usize,
                limit: usize,
            }
            let query = match serde_json::from_value::<Query>(args) {
                Ok(query) => query,
                Err(error) => return Ok(json!({"error":error.to_string()})),
            };
            let Some(digest) = spec.review else {
                return Ok(json!({"error":"no review was selected"}));
            };
            let artifacts = self.store.lock().await.artifacts().clone();
            let manifest = crate::review::manifest(&artifacts, digest)?;
            return Ok(
                json!({"manifest":manifest,"patch":self.read_artifact(manifest.patch, query.offset, query.limit).await?}),
            );
        }
        if proposal.name == "list_review_files" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Query {
                side: crate::review::ReviewSide,
                #[serde(default)]
                offset: usize,
                limit: usize,
            }
            let query = match serde_json::from_value::<Query>(args) {
                Ok(query) => query,
                Err(error) => return Ok(json!({"error":error.to_string()})),
            };
            let Some(manifest) = spec.review else {
                return Ok(json!({"error":"no review was selected"}));
            };
            return Ok(serde_json::to_value(
                self.review_files(manifest, query.side, query.offset, query.limit)
                    .await?,
            )?);
        }
        if proposal.name == "read_review_file" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Query {
                side: crate::review::ReviewSide,
                path: String,
                #[serde(default)]
                offset: usize,
                limit: usize,
            }
            let query = match serde_json::from_value::<Query>(args) {
                Ok(query) => query,
                Err(error) => return Ok(json!({"error":error.to_string()})),
            };
            let Some(manifest) = spec.review else {
                return Ok(json!({"error":"no review was selected"}));
            };
            let Some(file) = self.review_file(manifest, query.side, query.path).await? else {
                return Ok(json!({"missing":true}));
            };
            return Ok(
                json!({"file":file,"chunk":self.read_artifact(file.content, query.offset, query.limit).await?}),
            );
        }
        let context = ToolContext {
            workspace: working.to_owned(),
            task_id: request,
            generation: 0,
            job_id: Uuid::new_v4(),
            readonly: true,
            can_write: false,
            max_output_bytes: 32 * 1024,
            timeout_ms: 30_000,
        };
        let result = self
            .tools
            .execute(&proposal.name, args, context, cancellation)
            .await;
        Ok(match result {
            Ok(value) => value,
            Err(error) => json!({"error":error.to_string()}),
        })
    }
}

fn classification_text(output: &[OutputItem]) -> Option<&str> {
    let mut message = None;
    for item in output {
        match item {
            OutputItem::Message { text, .. } if message.is_none() => message = Some(text.as_str()),
            OutputItem::Opaque { item }
                if item.get("type").and_then(Value::as_str) == Some("reasoning") => {}
            _ => return None,
        }
    }
    message
}

fn ordinary_classification_request(
    model: crate::inference::ModelSettings,
    session: SessionId,
    mut latest_input: Vec<Value>,
) -> Result<InferenceRequest, HostError> {
    let [message] = latest_input.as_mut_slice() else {
        return Err(HostError::Invalid("invalid classification input"));
    };
    let parts = message["content"]
        .as_array_mut()
        .ok_or(HostError::Invalid("invalid classification input"))?;
    for part in parts {
        let marker = match part["type"].as_str() {
            Some("input_text") => continue,
            Some("tact_image") => "[Image attachment present; content omitted for routing.]",
            Some("tact_review") => "[Saved review feedback attached; content omitted for routing.]",
            _ => return Err(HostError::Invalid("invalid classification input")),
        };
        *part = json!({"type":"input_text","text":marker});
    }
    // The output budget must cover reasoning tokens too: GLM-class models
    // think by default and their reasoning counts against
    // `max_output_tokens`, so a tiny cap truncates the JSON verdict.
    InferenceRequest::new(
        model,
        latest_input,
        Vec::new(),
        CLASSIFICATION_INSTRUCTIONS.into(),
        session.to_string(),
        2048,
    )
    .map_err(|_| HostError::Invalid("invalid classification request"))
}

fn auxiliary_tools(spec: &AuxiliarySpec) -> Vec<Value> {
    let mut tools = WorkspaceTools::definitions()
        .into_iter()
        .filter(|tool| {
            spec.review.is_none() && matches!(tool["name"].as_str(), Some("read_file" | "search"))
        })
        .collect::<Vec<_>>();
    tools.push(json!({"type":"function","name":"config_show","description":"Read the effective non-secret model and execution configuration.","parameters":{"type":"object","properties":{},"additionalProperties":false}}));
    tools.push(json!({"type":"function","name":"task_status","description":"Read authoritative facts about the session's current coding task.","parameters":{"type":"object","properties":{},"additionalProperties":false}}));
    tools.push(json!({"type":"function","name":"read_review_feedback","description":"Read exact human review feedback already attached to this session.","parameters":{"type":"object","properties":{"digest":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":16384}},"required":["digest","limit"],"additionalProperties":false}}));
    if spec.context == AuxiliaryContext::CurrentConversation {
        tools.extend(
            tool_definitions(true)
                .into_iter()
                .filter(|tool| tool["name"] == "read_context"),
        );
    }
    if spec.review.is_some() {
        tools.push(json!({"type":"function","name":"read_review","description":"Read selected review metadata and bounded full-context patch bytes (base64), without choosing another source.","parameters":{"type":"object","properties":{"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":65536}},"required":["limit"],"additionalProperties":false}}));
        tools.push(json!({"type":"function","name":"list_review_files","description":"List frozen paths from the selected review side.","parameters":{"type":"object","properties":{"side":{"type":"string","enum":["before","after"]},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":128}},"required":["side","limit"],"additionalProperties":false}}));
        tools.push(json!({"type":"function","name":"read_review_file","description":"Read exact frozen bytes from the selected review range. Bytes are base64 encoded.","parameters":{"type":"object","properties":{"side":{"type":"string","enum":["before","after"]},"path":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":65536}},"required":["side","path","limit"],"additionalProperties":false}}));
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifier_accepts_reasoning_plus_exactly_one_message() {
        let reasoning = OutputItem::Opaque {
            item: json!({"type":"reasoning","encrypted_content":"ciphertext"}),
        };
        let message = OutputItem::Message {
            id: "message-1".into(),
            text: r#"{"kind":"information"}"#.into(),
            refusals: Vec::new(),
        };

        assert_eq!(
            classification_text(&[reasoning.clone(), message.clone()]),
            Some(r#"{"kind":"information"}"#)
        );
        assert_eq!(classification_text(&[message.clone(), message]), None);
        assert_eq!(
            classification_text(&[
                reasoning,
                OutputItem::Opaque {
                    item: json!({"type":"future_output"}),
                },
            ]),
            None
        );
    }

    #[test]
    fn ordinary_classification_uses_only_current_input_and_attachment_markers() {
        let request = ordinary_classification_request(
            crate::inference::ModelSettings::default(),
            SessionId::new(),
            vec![json!({
                "role":"user",
                "content":[
                    {"type":"input_text","text":"Explain this"},
                    {"type":"tact_image","digest":"image-digest-sentinel","mime":"image/png","detail":"high"},
                    {"type":"tact_review","digest":"review-digest-sentinel"}
                ]
            })],
        )
        .unwrap();

        let wire = request.wire(crate::inference::Transport::Http);
        assert_eq!(
            wire["input"],
            json!([{
                "role":"user",
                "content":[
                    {"type":"input_text","text":"Explain this"},
                    {"type":"input_text","text":"[Image attachment present; content omitted for routing.]"},
                    {"type":"input_text","text":"[Saved review feedback attached; content omitted for routing.]"}
                ]
            }])
        );
        assert_eq!(wire["tools"], json!([]));
        assert_eq!(wire["max_output_tokens"], 2048);
        let encoded = serde_json::to_string(&wire).unwrap();
        assert!(!encoded.contains("image-digest-sentinel"));
        assert!(!encoded.contains("review-digest-sentinel"));
    }
}
