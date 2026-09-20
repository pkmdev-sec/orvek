use super::*;
use crate::interpreter::{
    CellStatus, Checkpoint, CheckpointRef, Interpreter, InterpreterEvent, VmLimits,
};
use std::collections::BTreeSet;

pub(super) fn definition() -> Value {
    json!({"type":"function","name":"interpreter_eval","description":"Run a JavaScript async function body in this session's persistent, bounded interpreter. Use globalThis for retained working values. Return only selected JSON evidence. await host.call(name, args) uses admitted read_file/search/read_context/read_review_feedback/task_status and Docker child tools; writes and completion controls are unavailable. host.checkpoint(value) saves explicit lossless JSON, restored as globalThis.restored after state loss. Unsaved globals/live handles are lost on restart/error. Inner calls have durable cell/ordinal citations and ordinary receipts, not parent prompt entries. No filesystem/network API. Host waits do not use the VM CPU budget.","parameters":{"type":"object","properties":{"code":{"type":"string","maxLength":65536}},"required":["code"],"additionalProperties":false}})
}

fn composable(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "search"
            | "read_context"
            | "read_review_feedback"
            | "task_status"
            | "spawn_agent"
            | "send_agent_message"
            | "wait_agent"
            | "list_agents"
            | "interrupt_agent"
            | "close_agent"
    )
}

impl Host {
    async fn record_interpreter(
        &self,
        session: SessionId,
        request: Uuid,
        operation: Uuid,
        event: InterpreterEvent,
    ) -> Result<(), HostError> {
        let mut store = self.store.lock().await;
        let state = store.load_session(session)?;
        store.session_command(
            session,
            state.revision,
            operation,
            SessionCommand::Interpreter { request, event },
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn evaluate_cell(
        &self,
        session: SessionId,
        request: Uuid,
        task: TaskId,
        scope_revision: u64,
        outer_call: &str,
        args: Value,
        allowed: &BTreeSet<String>,
        workspace: &TaskWorkspace,
        cancellation: CancellationToken,
        emit: EventSink,
    ) -> Result<Value, HostError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Arguments {
            code: String,
        }
        let args = match serde_json::from_value::<Arguments>(args) {
            Ok(args) if args.code.len() <= 64 * 1024 => args,
            _ => {
                return Ok(
                    json!({"error":"interpreter_eval requires code up to 65536 UTF-8 bytes"}),
                );
            }
        };
        let cell = Uuid::new_v5(&request, format!("interpreter:{outer_call}").as_bytes());
        let (source, generation, environment, saved, prior_cells) = {
            let store = self.store.lock().await;
            let current = store.load_session(session)?;
            if current.active_request != Some(request)
                || current.tasks_by_request.get(&request) != Some(&task)
            {
                return Ok(json!({"error":"cell belongs to a superseded request"}));
            }
            if let Some(result) = current.interpreter.cells.get(&cell) {
                return match &result.status {
                    CellStatus::Settled { result } => Ok(serde_json::from_slice(
                        &store.artifacts().read(*result).map_err(StoreError::from)?,
                    )?),
                    CellStatus::Pending | CellStatus::Interrupted => Ok(
                        json!({"cell":cell,"error":"cell interrupted; never replayed; inspect inner receipts","state_lost":true}),
                    ),
                };
            }
            let task_state = store.load(task)?;
            if task_state.scope_revision != scope_revision {
                return Ok(json!({"error":"cell predates a user follow-up"}));
            }
            let source = store
                .artifacts()
                .put(args.code.as_bytes())
                .map_err(StoreError::from)?;
            let environment = store
                .artifacts()
                .put(&serde_json::to_vec(&match &self.executor {
                    Some(executor) => {
                        serde_json::to_value(executor.environment_for(ExecutionPolicy::Workspace))?
                    }
                    None => native_environment(),
                })?)
                .map_err(StoreError::from)?;
            (
                source,
                task_state.generation,
                environment,
                current.interpreter.checkpoint,
                !current.interpreter.cells.is_empty(),
            )
        };
        let (actor, restored) = {
            let mut actors = self.interpreters.lock().await;
            if let Some(actor) = actors.get(&session) {
                (actor.clone(), false)
            } else {
                let values = if let Some(saved) = saved {
                    let store = self.store.lock().await;
                    let bytes = store
                        .artifacts()
                        .read(saved.artifact)
                        .map_err(StoreError::from)?;
                    let checkpoint: Checkpoint = match serde_json::from_slice(&bytes) {
                        Ok(checkpoint) => checkpoint,
                        Err(error) => {
                            return Ok(
                                json!({"error":format!("checkpoint invalid: {error}"),"state_lost":true}),
                            );
                        }
                    };
                    if let Err(error) = checkpoint.validate(session, saved.source) {
                        return Ok(json!({"error":error,"state_lost":true}));
                    }
                    store
                        .artifacts()
                        .read(saved.source)
                        .map_err(StoreError::from)?;
                    Some(checkpoint.values)
                } else {
                    None
                };
                let actor = match Interpreter::start(values, VmLimits::default()).await {
                    Ok(actor) => Arc::new(Mutex::new(actor)),
                    Err(error) => return Ok(json!({"error":error,"state_lost":true})),
                };
                actors.insert(session, actor.clone());
                (actor, prior_cells)
            }
        };
        // The session lock spans the cell, not host-wide execution or other sessions.
        let actor = actor.lock().await;
        self.record_interpreter(
            session,
            request,
            Uuid::new_v5(&cell, b"start"),
            InterpreterEvent::Started {
                cell,
                task,
                outer_call: outer_call.into(),
                source,
                runtime: crate::interpreter::source_identity(),
                generation,
                environment,
            },
        )
        .await?;
        let cancellation = cancellation.child_token();
        let mut run = match actor.start_cell(args.code, cancellation.clone()) {
            Ok(run) => run,
            Err(_) => {
                self.interpreters.lock().await.remove(&session);
                return Err(HostError::Invalid(
                    "interpreter actor unavailable; live state lost",
                ));
            }
        };
        let mut citations = Vec::new();
        while let Some(call) = run.calls.recv().await {
            let call_id = format!("{cell}/{}", call.ordinal);
            let arguments = {
                let store = self.store.lock().await;
                store
                    .artifacts()
                    .put(&serde_json::to_vec(&call.arguments)?)
                    .map_err(StoreError::from)?
            };
            self.record_interpreter(
                session,
                request,
                Uuid::new_v5(&cell, format!("call:{}:start", call.ordinal).as_bytes()),
                InterpreterEvent::CallStarted {
                    cell,
                    ordinal: call.ordinal,
                    call_id: call_id.clone(),
                    name: call.name.clone(),
                    arguments,
                },
            )
            .await?;
            emit(HostUpdate::ToolStarted {
                session,
                call_id: call_id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            });
            let current = self.store.lock().await.load_session(session)?;
            let result = if cancellation.is_cancelled() {
                json!({"error":"cell cancelled before host admission"})
            } else if current.active_request != Some(request)
                || current.tasks_by_request.get(&request) != Some(&task)
            {
                json!({"error":"host call belongs to a superseded request"})
            } else if !composable(&call.name) || !allowed.contains(&call.name) {
                json!({"error":"tool is not admitted for interpreter composition"})
            } else if !call.arguments.is_object() {
                json!({"error":"host arguments must be a JSON object"})
            } else {
                // This is the same admission/execution/receipt path as a provider tool call.
                self.dispatch(session, request, task, scope_revision, &call.name, &call_id,
                    call.arguments, workspace, cancellation.clone()).await
                    .unwrap_or_else(|error| json!({"error":error.to_string(),"note":"inspect execution receipts; an interpreter error does not settle an unknown effect"}))
            };
            let digest = {
                let store = self.store.lock().await;
                store
                    .artifacts()
                    .put(&serde_json::to_vec(&result)?)
                    .map_err(StoreError::from)?
            };
            self.record_interpreter(
                session,
                request,
                Uuid::new_v5(&cell, format!("call:{}:settle", call.ordinal).as_bytes()),
                InterpreterEvent::CallSettled {
                    cell,
                    ordinal: call.ordinal,
                    result: digest,
                },
            )
            .await?;
            // Durable full values go to artifacts and the VM, not the parent prompt or previews.
            emit(HostUpdate::ToolFinished {
                session,
                call_id: call_id.clone(),
                name: call.name,
                result: json!({"cell":cell,"ordinal":call.ordinal,"artifact":digest}),
            });
            citations.push(json!({"call_id":call_id,"result":digest}));
            let reply = match result.get("error") {
                Some(error) => Err(error.to_string()),
                None => Ok(result),
            };
            let _ = call.reply.send(reply);
        }
        let output = run
            .done
            .await
            .unwrap_or_else(|_| Err("interpreter actor stopped".into()));
        let state_lost = output.is_err();
        let (value, checkpoint) = match output {
            Ok(output) => (json!({"value":output.value}), output.checkpoint),
            Err(error) => (json!({"error":error}), None),
        };
        let checkpoint = if let Some(values) = checkpoint {
            let checkpoint = Checkpoint::new(session, source, values);
            let store = self.store.lock().await;
            let artifact = store
                .artifacts()
                .put(&serde_json::to_vec(&checkpoint)?)
                .map_err(StoreError::from)?;
            Some(CheckpointRef { artifact, source })
        } else {
            None
        };
        let result = json!({"cell":cell,"output":value,"calls":citations,"checkpoint":checkpoint,
            "state_lost":state_lost || restored,"state_note":if state_lost || restored {
                "Only explicitly checkpointed JSON can be restored. Unsaved globals and live handles are lost; no cell was rerun. Inspect inner receipts for effect status."
            } else {"Working globals remain in this host process only."}});
        let digest = {
            let store = self.store.lock().await;
            store
                .artifacts()
                .put(&serde_json::to_vec(&result)?)
                .map_err(StoreError::from)?
        };
        self.record_interpreter(
            session,
            request,
            Uuid::new_v5(&cell, b"settled"),
            InterpreterEvent::Settled {
                cell,
                result: digest,
                checkpoint,
                state_lost,
            },
        )
        .await?;
        if state_lost {
            self.interpreters.lock().await.remove(&session);
        }
        Ok(result)
    }
}
