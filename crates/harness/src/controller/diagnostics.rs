//! Session notices use the existing journal. Host warnings are bounded, process-local status.
use super::HostError;
use crate::{
    Store,
    session::{SessionCommand, SessionId},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostWarning {
    MonitorUnavailable,
    MonitorStatusUnavailable,
    EventIntakeStopped,
    CompletionHookRecoveryFailed,
    SessionNoticeUnavailable,
}

impl HostWarning {
    pub fn message(self) -> &'static str {
        match self {
            Self::MonitorUnavailable => {
                "Trace monitoring failed. User tasks can continue; monitoring will retry."
            }
            Self::MonitorStatusUnavailable => {
                "The monitor failure status could not be saved. This warning is available only in the current host process."
            }
            Self::EventIntakeStopped => {
                "Background event intake stopped. Inspect pending events before restarting the host; do not replay uncertain effects."
            }
            Self::CompletionHookRecoveryFailed => {
                "Completion-hook recovery failed. Inspect durable delivery records before restarting the host; do not replay uncertain effects."
            }
            Self::SessionNoticeUnavailable => {
                "A session warning could not be saved. Some nonfatal diagnostics are missing from session history."
            }
        }
    }
}

pub(super) enum SessionWarning {
    MonitorBehavior,
    MonitorOrigin,
    MemoryProposal,
    CompletionHookRecord,
}

impl SessionWarning {
    fn message(self) -> &'static str {
        match self {
            Self::MonitorBehavior => {
                "Warning: monitor behavior is unavailable; this session uses the compiled baseline."
            }
            Self::MonitorOrigin => {
                "Warning: the session monitor origin could not be recorded; monitoring attribution may be incomplete."
            }
            Self::MemoryProposal => {
                "Warning: the post-run memory proposal failed. The task outcome is unchanged."
            }
            Self::CompletionHookRecord => {
                "Warning: the completion-hook delivery record could not be saved. Inspect durable delivery records; do not retry an uncertain hook. The task outcome is unchanged."
            }
        }
    }
}

pub(super) struct Diagnostics {
    store: Arc<Mutex<Store>>,
    pub(super) warnings: watch::Sender<BTreeSet<HostWarning>>,
}

impl Diagnostics {
    pub(super) fn new(store: Arc<Mutex<Store>>) -> Self {
        Self {
            store,
            warnings: watch::channel(BTreeSet::new()).0,
        }
    }

    pub(super) fn set(&self, warning: HostWarning, present: bool) {
        self.warnings.send_if_modified(|warnings| {
            if present {
                warnings.insert(warning)
            } else {
                warnings.remove(&warning)
            }
        });
    }

    pub(super) fn snapshot(&self) -> Vec<HostWarning> {
        self.warnings.borrow().iter().copied().collect()
    }

    pub(super) async fn session_warning(&self, session: SessionId, warning: SessionWarning) {
        if self.feedback(session, warning.message()).await.is_err() {
            self.set(HostWarning::SessionNoticeUnavailable, true);
        }
    }

    pub(super) async fn feedback(
        &self,
        session: SessionId,
        message: &str,
    ) -> Result<(), HostError> {
        let mut store = self.store.lock().await;
        let state = store.load_session(session)?;
        store.session_command(
            session,
            state.revision,
            Uuid::new_v4(),
            SessionCommand::Feedback {
                message: message.into(),
            },
        )?;
        Ok(())
    }
}

impl super::Host {
    pub(crate) fn warn(&self, warning: HostWarning) {
        self.diagnostics.set(warning, true);
    }

    pub(crate) fn subscribe_warnings(&self) -> watch::Receiver<BTreeSet<HostWarning>> {
        self.diagnostics.warnings.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Channel, Digest,
        controller::{Host, notification::DeliveryEvent},
        inference::{
            Limits, ModelSettings, ResponsesClient, Route, Transport,
            auth::{Auth, SecretString},
        },
        ipc::{self, Command, Request, Response},
        session::SessionAdmissionRequest,
        state::Outcome,
    };
    use tokio::time::{Duration, timeout};
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn diagnostic_background_failures_reach_status_without_replaying_effects() {
        let root = tempfile::tempdir().unwrap();
        let state_root = root.path().join("state");
        let provider = ResponsesClient::new(
            Auth::api_key(SecretString::new("fixture".into())).unwrap(),
            Route::new(Transport::Http, "http://127.0.0.1:1/v1/responses").unwrap(),
            Limits {
                max_attempts: 1,
                ..Limits::default()
            },
        )
        .unwrap();
        let host =
            Arc::new(Host::open_native(&state_root, provider, Digest::of(b"fixture")).unwrap());
        let session = host
            .create_session(SessionAdmissionRequest::new(
                root.path().to_owned(),
                ModelSettings::default(),
                10000,
                Channel::Stable,
            ))
            .await
            .unwrap();
        let request = Uuid::new_v4();
        let task = {
            let mut store = host.store.lock().await;
            store
                .session_command(
                    session.id,
                    session.revision,
                    Uuid::new_v4(),
                    SessionCommand::CompletionHook(DeliveryEvent::Armed {
                        request,
                        command: "printf x > should-not-run".into(),
                    }),
                )
                .unwrap();
            let intake = store.artifacts().put(br#"{"version":1,"delivery":"source","profile":{"version":1,"name":"fixture","checks":{}}}"#).unwrap();
            let (_, task, _) = store
                .start_request(
                    session.id,
                    request,
                    "fixture".into(),
                    Default::default(),
                    intake,
                )
                .unwrap();
            store
                .stop(
                    task.id,
                    task.revision,
                    Outcome::BudgetExhausted,
                    "fixture allowance".into(),
                )
                .unwrap();
            let state = store.load_session(session.id).unwrap();
            store
                .session_command(
                    session.id,
                    state.revision,
                    Uuid::new_v4(),
                    SessionCommand::TurnSettled {
                        request,
                        outcome: Some(Outcome::BudgetExhausted),
                        error: None,
                    },
                )
                .unwrap();
            task.id
        };
        let db = rusqlite::Connection::open(state_root.join("v1.sqlite3")).unwrap();
        db.execute("DROP TABLE event_intake", []).unwrap();
        db.execute_batch("CREATE TRIGGER reject_hook_claim BEFORE INSERT ON events WHEN json_extract(NEW.event,'$.data.command.type')='completion_hook' AND json_extract(NEW.event,'$.data.command.data.type')='claimed' BEGIN SELECT RAISE(FAIL, 'credential-bearing hook failure must stay private'); END;").unwrap();
        let stop = CancellationToken::new();
        let serving = tokio::spawn(ipc::serve(host.clone(), stop.clone()));
        timeout(Duration::from_secs(3), async {
            let mut warnings = host.subscribe_warnings();
            loop {
                let current = warnings.borrow_and_update().clone();
                if current.contains(&HostWarning::EventIntakeStopped)
                    && current.contains(&HostWarning::CompletionHookRecoveryFailed)
                {
                    break;
                }
                warnings.changed().await.unwrap();
            }
        })
        .await
        .expect("both background failures must be observable");
        let response = ipc::call(
            &state_root.join("host.sock"),
            &Request::new(Command::Info),
            Duration::from_secs(3),
        )
        .await
        .unwrap();
        let Response::Info(info) = response else {
            panic!("expected host status")
        };
        assert!(info.warnings.contains(&HostWarning::EventIntakeStopped));
        assert!(
            info.warnings
                .contains(&HostWarning::CompletionHookRecoveryFailed)
        );
        assert!(
            !serde_json::to_string(&info)
                .unwrap()
                .contains("credential-bearing")
        );
        assert!(info.active_sessions.is_empty());
        assert_eq!(host.sessions(0, 64).await.unwrap().0.len(), 1);
        assert_eq!(
            host.task(task).await.unwrap().outcome,
            Some(Outcome::BudgetExhausted)
        );
        assert!(!root.path().join("should-not-run").exists());
        stop.cancel();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn diagnostic_interpreter_reply_failure_is_receiver_dropped_cancellation() {
        use crate::interpreter::{Interpreter, VmLimits};
        let vm = Interpreter::start(None, VmLimits::default()).await.unwrap();
        let cancellation = CancellationToken::new();
        let mut run = vm
            .start_cell(
                "return await Promise.all([host.call('read_file', {path: 'one'}), host.call('read_file', {path: 'two'})]);".into(),
                cancellation.clone(),
            )
            .unwrap();
        let call = timeout(Duration::from_secs(3), run.calls.recv())
            .await
            .unwrap()
            .unwrap();
        let abandoned = timeout(Duration::from_secs(3), run.calls.recv())
            .await
            .unwrap()
            .unwrap();
        cancellation.cancel();
        call.reply
            .send(Ok(serde_json::json!({"settled": true})))
            .unwrap();
        let output = timeout(Duration::from_secs(3), run.done)
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.is_err(),
            "cancellation is reported by the cell's own terminal result"
        );
        // Wait for actor shutdown, not just its terminal result, before inspecting the receiver.
        if let Ok(probe) = vm.start_cell("return true;".into(), CancellationToken::new()) {
            assert!(
                timeout(Duration::from_secs(3), probe.done)
                    .await
                    .unwrap()
                    .is_err()
            );
        }
        assert!(
            abandoned
                .reply
                .send(Ok(serde_json::json!({"settled": true})))
                .is_err(),
            "the VM has already dropped the pending receiver"
        );
    }
}
