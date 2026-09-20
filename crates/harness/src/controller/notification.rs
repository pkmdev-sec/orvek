//! Terminal notifications are host effects, not task evidence or client callbacks.
//!
//! Persist a claim before spawning the shell. A lost acknowledgement remains
//! unknown: arbitrary shell effects cannot safely be retried automatically.
use super::{Host, HostError};
use crate::{
    session::{SessionCommand, SessionId},
    state::{Outcome, TaskId},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io, path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};
use uuid::Uuid;

const HOOK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompletionPayload {
    pub version: u32,
    pub delivery_id: Uuid,
    pub session: SessionId,
    pub request: Uuid,
    pub task: TaskId,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum DeliveryResult {
    Succeeded,
    Failed { exit_code: i32 },
    NotStarted { error: String },
    Unknown { reason: UnknownReason },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    Interrupted,
    TimedOut,
    Signaled,
    WaitFailed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum DeliveryState {
    Pending,
    Attempted {
        payload: CompletionPayload,
        result: DeliveryResult,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompletionDelivery {
    pub id: Uuid,
    pub command: String,
    pub state: DeliveryState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum DeliveryEvent {
    Armed {
        request: Uuid,
        command: String,
    },
    Claimed {
        payload: CompletionPayload,
    },
    Finished {
        request: Uuid,
        result: DeliveryResult,
    },
}

impl DeliveryEvent {
    pub(crate) fn apply(
        &self,
        session: SessionId,
        deliveries: &mut BTreeMap<Uuid, CompletionDelivery>,
    ) -> Result<(), serde_json::Error> {
        match self {
            Self::Armed { request, command } if !deliveries.contains_key(request) => {
                deliveries.insert(
                    *request,
                    CompletionDelivery {
                        id: Uuid::new_v5(
                            &session.0,
                            format!("orvek-completion-hook-v1:{request}").as_bytes(),
                        ),
                        command: command.clone(),
                        state: DeliveryState::Pending,
                    },
                );
                return Ok(());
            }
            Self::Claimed { payload } => {
                if let Some(delivery) = deliveries.get_mut(&payload.request)
                    && payload.session == session
                    && payload.delivery_id == delivery.id
                    && matches!(delivery.state, DeliveryState::Pending)
                {
                    delivery.state = DeliveryState::Attempted {
                        payload: payload.clone(),
                        result: DeliveryResult::Unknown {
                            reason: UnknownReason::Interrupted,
                        },
                    };
                    return Ok(());
                }
            }
            Self::Finished { request, result } => {
                if let Some(delivery) = deliveries.get_mut(request)
                    && let DeliveryState::Attempted {
                        result: previous, ..
                    } = &mut delivery.state
                    && *previous
                        == (DeliveryResult::Unknown {
                            reason: UnknownReason::Interrupted,
                        })
                {
                    *previous = result.clone();
                    return Ok(());
                }
            }
            _ => {}
        }
        Err(serde_json::Error::io(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid completion delivery transition",
        )))
    }
}

impl Host {
    /// Pin this host's configured shell command in each task request's journal.
    pub fn with_completion_hook(mut self, command: Option<String>) -> Self {
        self.completion_hook = command;
        self
    }

    pub(super) async fn arm_completion_hook(
        &self,
        session: SessionId,
        request: Uuid,
    ) -> Result<(), HostError> {
        let Some(command) = &self.completion_hook else {
            return Ok(());
        };
        let mut store = self.store.lock().await;
        let state = store.load_session(session)?;
        // Repeated admissions must not add hooks to historical unconfigured turns.
        if state.completion_deliveries.contains_key(&request)
            || state.tasks_by_request.contains_key(&request)
        {
            return Ok(());
        }
        store.session_command(
            session,
            state.revision,
            Uuid::new_v5(&request, b"completion-hook-armed"),
            SessionCommand::CompletionHook(DeliveryEvent::Armed {
                request,
                command: command.clone(),
            }),
        )?;
        Ok(())
    }

    /// Resume only durable, unclaimed intents after the store reconciles tasks.
    pub(crate) async fn recover_completion_hooks(&self) -> Result<(), HostError> {
        let mut offset = 0;
        loop {
            let sessions = self.store.lock().await.sessions(offset, 64)?;
            if sessions.is_empty() {
                return Ok(());
            }
            offset += sessions.len();
            for session in sessions {
                if session
                    .completion_deliveries
                    .values()
                    .any(|delivery| matches!(delivery.state, DeliveryState::Pending))
                {
                    self.deliver_completion_hooks(session.id).await?;
                }
            }
        }
    }

    pub(super) async fn deliver_completion_hooks(
        &self,
        session: SessionId,
    ) -> Result<(), HostError> {
        loop {
            let claimed = {
                let mut store = self.store.lock().await;
                let state = store.load_session(session)?;
                let mut claimed = None;
                for (request, delivery) in &state.completion_deliveries {
                    if !matches!(delivery.state, DeliveryState::Pending)
                        || state.active_request == Some(*request)
                    {
                        continue;
                    }
                    let Some(task_id) = state.tasks_by_request.get(request) else {
                        continue;
                    };
                    let task = store.load(*task_id)?;
                    let Some(outcome) = task.outcome else {
                        continue;
                    };
                    let payload = CompletionPayload {
                        version: 1,
                        delivery_id: delivery.id,
                        session,
                        request: *request,
                        task: task.id,
                        outcome,
                    };
                    store.session_command(
                        session,
                        state.revision,
                        Uuid::new_v5(&delivery.id, b"claimed"),
                        SessionCommand::CompletionHook(DeliveryEvent::Claimed {
                            payload: payload.clone(),
                        }),
                    )?;
                    claimed = Some((delivery.command.clone(), state.workspace().clone(), payload));
                    break;
                }
                claimed
            };
            let Some((command, workspace, payload)) = claimed else {
                return Ok(());
            };
            let result = run_hook(&command, &workspace, &payload).await;
            let mut store = self.store.lock().await;
            let state = store.load_session(session)?;
            store.session_command(
                session,
                state.revision,
                Uuid::new_v5(&payload.delivery_id, b"finished"),
                SessionCommand::CompletionHook(DeliveryEvent::Finished {
                    request: payload.request,
                    result,
                }),
            )?;
        }
    }
}

#[cfg(unix)]
struct HookProcessGroup(Option<rustix::process::Pid>);

#[cfg(unix)]
impl Drop for HookProcessGroup {
    fn drop(&mut self) {
        // Also clean up when host shutdown drops the delivery future. SIGKILL
        // or deliberately detached descendants cannot be covered by this guard.
        if let Some(pid) = self.0 {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}

#[cfg(unix)]
async fn run_hook(command: &str, workspace: &Path, payload: &CompletionPayload) -> DeliveryResult {
    let outcome = serde_json::to_value(payload.outcome).expect("outcome is serializable");
    let mut child = match Command::new("/bin/sh")
        .args(["-c", command])
        .current_dir(workspace)
        .env("ORVEK_COMPLETION_ID", payload.delivery_id.to_string())
        .env("ORVEK_SESSION_ID", payload.session.to_string())
        .env("ORVEK_REQUEST_ID", payload.request.to_string())
        .env("ORVEK_TASK_ID", payload.task.to_string())
        .env(
            "ORVEK_OUTCOME",
            outcome.as_str().expect("outcome is a string"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return DeliveryResult::NotStarted {
                error: error.to_string(),
            };
        }
    };
    let group = HookProcessGroup(
        child
            .id()
            .and_then(|id| rustix::process::Pid::from_raw(id as i32)),
    );
    let execution = async {
        if let Some(mut stdin) = child.stdin.take() {
            let mut input =
                serde_json::to_vec(payload).expect("completion payload is serializable");
            input.push(b'\n');
            // A shell command may use only the environment and close stdin early.
            let _ = stdin.write_all(&input).await;
        }
        child.wait().await
    };
    let result = match timeout(HOOK_TIMEOUT, execution).await {
        Ok(Ok(status)) => match status.code() {
            Some(0) => DeliveryResult::Succeeded,
            Some(exit_code) => DeliveryResult::Failed { exit_code },
            None => DeliveryResult::Unknown {
                reason: UnknownReason::Signaled,
            },
        },
        Ok(Err(_)) => DeliveryResult::Unknown {
            reason: UnknownReason::WaitFailed,
        },
        Err(_) => DeliveryResult::Unknown {
            reason: UnknownReason::TimedOut,
        },
    };
    drop(group);
    let _ = child.kill().await;
    result
}

#[cfg(not(unix))]
async fn run_hook(
    _command: &str,
    _workspace: &Path,
    _payload: &CompletionPayload,
) -> DeliveryResult {
    DeliveryResult::NotStarted {
        error: "completion hooks require a Unix shell".into(),
    }
}
