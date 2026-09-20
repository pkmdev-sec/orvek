//! Intake owns identities and cursors, never task execution or effect replay.
use super::{Host, HostError};
use crate::{
    StoreError,
    event_intake::{EventRecord, SourceConfig, SourceRecord},
    submission::SubmitIntent,
};
use serde_json::json;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use uuid::Uuid;

impl Host {
    pub async fn register_event_source(
        &self,
        config: SourceConfig,
    ) -> Result<SourceRecord, HostError> {
        let _intake = self.event_intake.lock().await;
        if !self.accepting.load(Ordering::Acquire) {
            return Err(HostError::ShuttingDown);
        }
        let mut store = self.store.lock().await;
        let session = store.load_session(config.session)?;
        self.validate_session_admission(&session)?;
        let authority = session
            .admission()
            .expect("validated admission")
            .authority();
        Ok(store.register_event_source(config, authority)?)
    }

    pub async fn event_source(&self, source: Uuid) -> Result<SourceRecord, HostError> {
        Ok(self.store.lock().await.event_source(source)?)
    }

    pub async fn event(&self, source: Uuid, key: &str) -> Result<EventRecord, HostError> {
        self.store
            .lock()
            .await
            .event(source, key)?
            .ok_or(HostError::Invalid("unknown event"))
    }

    pub async fn deliver_event(
        self: &Arc<Self>,
        source: Uuid,
        key: &str,
        payload: &str,
    ) -> Result<EventRecord, HostError> {
        let _intake = self.event_intake.lock().await;
        if !self.accepting.load(Ordering::Acquire) {
            return Err(HostError::ShuttingDown);
        }
        let event = {
            let mut store = self.store.lock().await;
            if store.event(source, key)?.is_none() {
                let configured = store.event_source(source)?;
                self.validate_event_source(&store, &configured)?;
            }
            store.receive_event(source, key, payload, crate::store::now_ms())?
        };
        self.reconcile_event(event).await
    }

    pub async fn cancel_event(
        self: &Arc<Self>,
        source: Uuid,
        key: &str,
    ) -> Result<EventRecord, HostError> {
        let _intake = self.event_intake.lock().await;
        let mut event = self.event(source, key).await?;
        event.cancel_requested = true;
        self.store.lock().await.save_event(&event)?;
        self.reconcile_event(event).await
    }

    pub async fn disable_event_source(
        self: &Arc<Self>,
        source: Uuid,
    ) -> Result<SourceRecord, HostError> {
        let _intake = self.event_intake.lock().await;
        // Durable disable also implies cancellation of every outstanding event.
        let record = self.store.lock().await.disable_event_source(source)?;
        let pending = self.store.lock().await.pending_events()?;
        for event in pending.into_iter().filter(|event| event.source == source) {
            self.reconcile_event(event).await?;
        }
        Ok(record)
    }

    fn validate_event_source(
        &self,
        store: &crate::Store,
        source: &SourceRecord,
    ) -> Result<(), HostError> {
        let session = store.load_session(source.config.session)?;
        self.validate_session_admission(&session)?;
        if session
            .admission()
            .expect("validated admission")
            .authority()
            != source.authority
        {
            return Err(HostError::Invalid(
                "event source admission authority changed",
            ));
        }
        Ok(())
    }

    async fn reconcile_event(
        self: &Arc<Self>,
        mut event: EventRecord,
    ) -> Result<EventRecord, HostError> {
        if event.settled {
            return Ok(event);
        }
        let (source, existing) = {
            let store = self.store.lock().await;
            let source = store.event_source(event.source)?;
            let existing = store
                .load_session(event.session)?
                .submissions
                .get(&event.request)
                .cloned();
            (source, existing)
        };
        if let Some(submission) = &existing {
            let expected = crate::submission::WorkIntent::NewTask {
                limits: source.config.limits,
                policy: crate::Digest::of_value(&source.config.policy)?,
            };
            if submission.initial_input != event.input || submission.intent != expected {
                event.error =
                    Some("event request ID conflicts with another queue submission".into());
                event.settled = true;
                self.store.lock().await.save_event(&event)?;
                return Ok(event);
            }
        }
        event.cancel_requested |= source.disabled;
        // Adopt even Interrupted/Finished receipts after an admission acknowledgement loss.
        // Never turn their old work into a new request or replay arbitrary effects.
        event.submission = existing;
        event.error = None;
        if event.cancel_requested {
            self.store.lock().await.save_event(&event)?;
            if event.submission.is_some() {
                event.submission =
                    Some(self.cancel_submission(event.session, event.request).await?);
            }
            event.settled = event
                .submission
                .as_ref()
                .is_none_or(|submission| !submission.status.pending());
        } else if event.submission.is_none() {
            let admission = async {
                let input = {
                    let store = self.store.lock().await;
                    self.validate_event_source(&store, &source)?;
                    crate::input::load(event.input, store.artifacts())?
                };
                self.submit(
                    event.session,
                    event.request,
                    vec![json!({"type":"input_text","text":input.text})],
                    SubmitIntent::NewTask {
                        limits: source.config.limits,
                        policy: source.config.policy.clone(),
                    },
                )
                .await
            }
            .await;
            match admission {
                Ok(submission) => event.submission = Some(submission),
                Err(error) => event.error = Some(error.to_string()),
            }
        }
        if event
            .submission
            .as_ref()
            .is_some_and(|submission| !submission.status.pending())
        {
            event.settled = true;
        }
        self.store.lock().await.save_event(&event)?;
        Ok(event)
    }

    pub(crate) async fn run_event_intake(self: &Arc<Self>) -> Result<(), HostError> {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let _intake = self.event_intake.lock().await;
            if !self.accepting.load(Ordering::Acquire) {
                return Ok(());
            }
            let pending = self.store.lock().await.pending_events()?;
            for event in pending {
                self.reconcile_event(event).await?;
            }
            let sources = self.store.lock().await.event_sources()?;
            for source in sources {
                let event = {
                    let mut store = self.store.lock().await;
                    let source_id = source.config.id;
                    let error = self
                        .validate_event_source(&store, &source)
                        .err()
                        .map(|error| error.to_string());
                    let rejected = error.is_some();
                    store.record_event_source_admission(source, error)?;
                    if rejected {
                        continue;
                    }
                    match store.tick_event_source(source_id, crate::store::now_ms()) {
                        Ok(event) => event,
                        Err(StoreError::Invalid("host event intake capacity reached")) => continue,
                        Err(error) => return Err(error.into()),
                    }
                };
                if let Some(event) = event {
                    self.reconcile_event(event).await?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Digest,
        admission::{RepositoryProfile, RequestPolicy},
        contract::{DeliveryKind, Limits},
        event_intake::TriggerKind,
        inference::{
            ModelSettings, ResponsesClient, Route, Transport,
            auth::{Auth, SecretString},
        },
        session::{SessionAdmissionRequest, SessionId},
        submission::{SubmissionStatus, WorkIntent},
    };
    use std::path::Path;

    fn open(root: &Path) -> Arc<Host> {
        let client = ResponsesClient::new(
            Auth::api_key(SecretString::new("fixture-key".into())).unwrap(),
            Route::new(Transport::Http, "http://127.0.0.1:1/v1/responses").unwrap(),
            crate::inference::Limits {
                max_attempts: 1,
                ..Default::default()
            },
        )
        .unwrap();
        Arc::new(Host::open_native(root, client, Digest::of(b"fixture-host")).unwrap())
    }

    async fn setup(host: &Arc<Host>, workspace: &Path) -> SourceRecord {
        let session = host
            .create_session(SessionAdmissionRequest::new(
                workspace.to_owned(),
                ModelSettings::default(),
                crate::context::DEFAULT_WINDOW_TOKENS,
                crate::Channel::Stable,
            ))
            .await
            .unwrap();
        host.register_event_source(SourceConfig {
            id: Uuid::new_v4(),
            session: session.id,
            objective: "Summarize the note".into(),
            trigger: TriggerKind::Webhook,
            limits: Limits::default(),
            policy: RequestPolicy {
                version: 1,
                delivery: DeliveryKind::Source,
                profile: RepositoryProfile {
                    version: 1,
                    name: "fixture".into(),
                    checks: Default::default(),
                },
            },
        })
        .await
        .unwrap()
    }

    // Each crash cut reopens the real SQLite state in a fresh executable, not
    // merely another Host value sharing the test process's runtime.
    fn recover_in_fresh_process(root: &Path, source: Uuid, key: &str) -> EventRecord {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "controller::event_intake::tests::fresh_process_recovery_probe",
                "--nocapture",
            ])
            .env("ORVEK_EVENT_RECOVERY_ROOT", root)
            .env("ORVEK_EVENT_RECOVERY_SOURCE", source.to_string())
            .env("ORVEK_EVENT_RECOVERY_KEY", key)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&std::fs::read(root.join("probe.json")).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn fresh_process_recovery_probe() {
        let Ok(root) = std::env::var("ORVEK_EVENT_RECOVERY_ROOT") else {
            return;
        };
        let source = std::env::var("ORVEK_EVENT_RECOVERY_SOURCE")
            .unwrap()
            .parse()
            .unwrap();
        let key = std::env::var("ORVEK_EVENT_RECOVERY_KEY").unwrap();
        let host = open(Path::new(&root));
        let stop = tokio_util::sync::CancellationToken::new();
        let service = tokio::spawn(crate::ipc::serve(host.clone(), stop.clone()));
        let socket = Path::new(&root).join("host.sock");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !socket.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let info = crate::ipc::call(
            &socket,
            &crate::ipc::Request::new(crate::ipc::Command::Info),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert!(matches!(info, crate::ipc::Response::Info(_)));
        // No redelivery drives this recovery: the startup intake worker does.
        let event = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = host.event(source, &key).await.unwrap();
                if event.settled || event.submission.is_some() {
                    break event;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        std::fs::write(
            Path::new(&root).join("probe.json"),
            serde_json::to_vec(&event).unwrap(),
        )
        .unwrap();
        stop.cancel();
        host.queue_stop.cancel();
        tokio::time::timeout(Duration::from_secs(5), service)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn crash_before_admission_recovers_stable_request_and_cancellation_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let root = dir.path().join("host");
        let host = open(&root);
        let source = setup(&host, &workspace).await;
        let event = host
            .store
            .lock()
            .await
            .receive_event(source.config.id, "before", "data", 10)
            .unwrap();
        let cancelled = host
            .store
            .lock()
            .await
            .receive_event(source.config.id, "cancelled", "data", 10)
            .unwrap();
        // Crash after durable cancellation intent but before queue reconciliation.
        let mut cancelled = cancelled;
        cancelled.cancel_requested = true;
        host.store.lock().await.save_event(&cancelled).unwrap();
        drop(host);
        let recovered = recover_in_fresh_process(&root, source.config.id, "before");
        assert_eq!(recovered.request, event.request);
        assert_eq!(recovered.submission.unwrap().id, event.request);
        let cancelled = recover_in_fresh_process(&root, source.config.id, "cancelled");
        let host = open(&root);
        assert!(cancelled.settled && cancelled.submission.is_none());
        assert!(
            host.submission(source.config.session, cancelled.request)
                .await
                .is_err()
        );
        host.queue_stop.cancel();
    }

    #[tokio::test]
    async fn crash_after_queue_admission_adopts_interrupted_work_without_effect_retry() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let root = dir.path().join("host");
        let host = open(&root);
        let source = setup(&host, &workspace).await;
        let event = {
            let mut store = host.store.lock().await;
            let event = store
                .receive_event(source.config.id, "after", "data", 10)
                .unwrap();
            let policy = store
                .artifacts()
                .put(&serde_json::to_vec(&source.config.policy).unwrap())
                .unwrap();
            store
                .submit(
                    event.session,
                    event.request,
                    event.input,
                    WorkIntent::NewTask {
                        limits: Limits::default(),
                        policy,
                    },
                )
                .unwrap();
            store
                .set_submission_status(event.session, event.request, SubmissionStatus::Running)
                .unwrap();
            let input = crate::input::load(event.input, store.artifacts()).unwrap();
            store
                .start_prepared_request(
                    event.session,
                    event.request,
                    input,
                    Limits::default(),
                    policy,
                )
                .unwrap();
            event
        };
        drop(host);
        let recovered = recover_in_fresh_process(&root, source.config.id, "after");
        let host = open(&root);
        assert_eq!(recovered.request, event.request);
        assert_eq!(
            recovered.submission.unwrap().status,
            SubmissionStatus::Interrupted
        );
        assert!(recovered.settled);
        let state = host.session(event.session).await.unwrap();
        assert_eq!(state.submissions.len(), 1);
        assert_eq!(state.tasks_by_request.len(), 1);
    }

    #[tokio::test]
    async fn source_cannot_bind_unknown_session() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let host = open(&dir.path().join("host"));
        let mut source = setup(&host, &workspace).await;
        source.config.id = Uuid::new_v4();
        source.config.session = SessionId::new();
        assert!(host.register_event_source(source.config).await.is_err());
    }
    #[tokio::test]
    async fn changed_host_authority_rejects_delivery_and_reports_schedule_failure() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let root = dir.path().join("host");
        let host = open(&root);
        let source = setup(&host, &workspace).await;
        let mut schedule = source.config.clone();
        schedule.id = Uuid::new_v4();
        schedule.trigger = TriggerKind::Interval {
            first_due_ms: 1,
            interval_ms: 1000,
        };
        let schedule_id = schedule.id;
        host.register_event_source(schedule).await.unwrap();
        drop(host);
        let provider = ResponsesClient::new(
            Auth::api_key(SecretString::new("fixture-key".into())).unwrap(),
            Route::new(Transport::Http, "http://127.0.0.1:1/v1/responses").unwrap(),
            crate::inference::Limits {
                max_attempts: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let host = Arc::new(
            Host::open_native(&root, provider, Digest::of(b"other-host-authority")).unwrap(),
        );
        assert!(
            host.deliver_event(source.config.id, "changed", "data")
                .await
                .is_err()
        );
        assert!(
            host.session(source.config.session)
                .await
                .unwrap()
                .submissions
                .is_empty()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), host.run_event_intake())
                .await
                .is_err()
        );
        let status = serde_json::to_value(host.event_source(schedule_id).await.unwrap()).unwrap();
        assert!(
            status["admission_error"]
                .as_str()
                .is_some_and(|error| !error.is_empty()),
            "a due schedule must expose why its pinned authority prevents admission: {status}"
        );
        drop(host);
        let host = open(&root);
        assert!(
            host.event_source(schedule_id)
                .await
                .unwrap()
                .admission_error
                .is_some()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), host.run_event_intake())
                .await
                .is_err()
        );
        let resumed = host.event_source(schedule_id).await.unwrap();
        assert!(resumed.admission_error.is_none());
        assert!(resumed.next_due_ms.unwrap() > 1);
        host.queue_stop.cancel();
    }

    #[tokio::test]
    async fn disable_before_admission_survives_restart_without_work() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let root = dir.path().join("host");
        let host = open(&root);
        let source = setup(&host, &workspace).await;
        host.store
            .lock()
            .await
            .receive_event(source.config.id, "disabled", "data", 1)
            .unwrap();
        // Crash after the source disable commit, before individual event cancellation.
        host.store
            .lock()
            .await
            .disable_event_source(source.config.id)
            .unwrap();
        drop(host);
        let recovered = recover_in_fresh_process(&root, source.config.id, "disabled");
        assert!(recovered.cancel_requested && recovered.settled && recovered.submission.is_none());
    }

    #[tokio::test]
    async fn conflicting_queue_identity_cannot_be_adopted_or_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let host = open(&dir.path().join("host"));
        let source = setup(&host, &workspace).await;
        let event = {
            let mut store = host.store.lock().await;
            let event = store
                .receive_event(source.config.id, "collision", "data", 1)
                .unwrap();
            let input = crate::input::prepare(
                vec![json!({"type":"input_text","text":"unrelated operator work"})],
                store.artifacts(),
            )
            .unwrap();
            let policy = store
                .artifacts()
                .put(&serde_json::to_vec(&source.config.policy).unwrap())
                .unwrap();
            store
                .submit(
                    event.session,
                    event.request,
                    input.artifact,
                    WorkIntent::NewTask {
                        limits: Limits::default(),
                        policy,
                    },
                )
                .unwrap();
            event
        };
        let cancelled = host
            .cancel_event(source.config.id, "collision")
            .await
            .unwrap();
        assert!(cancelled.settled && cancelled.error.is_some() && cancelled.submission.is_none());
        assert_eq!(
            host.submission(event.session, event.request)
                .await
                .unwrap()
                .status,
            SubmissionStatus::Queued
        );
    }
}
