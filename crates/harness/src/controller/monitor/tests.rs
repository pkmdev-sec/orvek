use super::*;
use crate::{
    Channel,
    inference::{
        ModelSettings, ResponsesClient, Route, Transport,
        auth::{Auth, SecretString},
    },
    session::SessionAdmissionRequest,
};

fn open(root: &std::path::Path) -> Host {
    let client = ResponsesClient::new(
        Auth::api_key(SecretString::new("fixture-key".into())).unwrap(),
        Route::new(Transport::Http, "http://127.0.0.1:1/v1/responses").unwrap(),
        crate::inference::Limits {
            max_attempts: 1,
            ..Default::default()
        },
    )
    .unwrap();
    Host::open_native(root, client, Digest::of(b"fixture")).unwrap()
}
async fn diagnosed(host: &Host, workspace: &std::path::Path) -> Episode {
    let initial = host.monitor_report(0, 100).await.unwrap().status.active;
    host.install_read_behavior(initial, 4096, "injected regression".into())
        .await
        .unwrap();
    let session = host
        .create_session(SessionAdmissionRequest::new(
            workspace.to_owned(),
            ModelSettings::default(),
            10000,
            Channel::Stable,
        ))
        .await
        .unwrap();
    let mut store = host.store.lock().await;
    let mut cohort = store.monitor_cohort(session.id).unwrap().unwrap();
    cohort.build = host.monitor_build;
    let receipt = store.artifacts().put(b"controlled receipt").unwrap();
    let episode = diagnose(&store, 1, cohort, receipt, 4096).unwrap().unwrap();
    let status = store.monitor_status().unwrap();
    store
        .monitor_commit_page(&status, &[], std::slice::from_ref(&episode))
        .unwrap();
    episode
}

#[tokio::test]
async fn evaluator_rejects_heldout_tampering_and_user_admission_still_works() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    let host = open(&root);
    let episode = diagnosed(&host, dir.path()).await;
    fs::write(
        root.join("artifacts").join(episode.heldout.to_string()),
        b"[]",
    )
    .unwrap();
    host.monitor_tick().await.unwrap();
    let report = host.monitor_report(0, 100).await.unwrap();
    assert_eq!(report.status.active, episode.regressed);
    assert!(matches!(
        report.episodes[0].state,
        EpisodeState::Uncertain { .. }
    ));
    assert!(
        report
            .status
            .last_error
            .unwrap()
            .contains("digest mismatch")
    );
    assert!(
        report
            .measures
            .iter()
            .any(|m| m.signature == Signature::EvaluatorFailure && m.failures == 1)
    );
    host.create_session(SessionAdmissionRequest::new(
        dir.path().to_owned(),
        ModelSettings::default(),
        10000,
        Channel::Stable,
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn frozen_candidate_cannot_replace_checks_or_change_config() {
    let dir = tempfile::tempdir().unwrap();
    let host = open(&dir.path().join("state"));
    let mut episode = diagnosed(&host, dir.path()).await;
    let artifacts = host.store.lock().await.artifacts().clone();
    let candidate_dir = tempfile::tempdir().unwrap();
    let candidate = candidate_dir.path().join("candidate.json");
    let valid = serde_json::to_vec(&evaluation::CandidateConfig {
        native_read_output_bytes: 32768,
    })
    .unwrap();
    episode.candidate = Some(Digest::of(&valid));
    fs::write(
        &candidate,
        b"{\"native_read_output_bytes\":32768,\"checks\":[]}",
    )
    .unwrap();
    assert!(
        evaluation::evaluate(&artifacts, &episode, &candidate, 4096, 32768)
            .await
            .is_err()
    );
    fs::write(&candidate, b"{\"native_read_output_bytes\":4096}").unwrap();
    assert!(
        evaluation::evaluate(&artifacts, &episode, &candidate, 4096, 32768)
            .await
            .is_err()
    );
    fs::write(&candidate, &valid).unwrap();
    assert!(
        evaluation::evaluate(&artifacts, &episode, &candidate, 4096, 32768)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn interrupted_evaluation_never_reexecutes_and_cursor_commit_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    let host = open(&root);
    let mut episode = diagnosed(&host, dir.path()).await;
    episode.state = EpisodeState::Evaluating;
    host.store
        .lock()
        .await
        .monitor_save_episode(&episode)
        .unwrap();
    let mut status = host.store.lock().await.monitor_status().unwrap();
    status.cursor = host.info().await.unwrap().journal_sequence;
    let measure = Measure::new(episode.cohort.clone(), Signature::MissingUsage, 3, 1, 2);
    host.store
        .lock()
        .await
        .monitor_commit_page(
            &status,
            &[crate::monitor::Measurement::new(episode.id, measure)],
            &[],
        )
        .unwrap();
    drop(host);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "controller::monitor::tests::fresh_process_monitor_recovery_probe",
            "--nocapture",
        ])
        .env("ORVEK_MONITOR_RECOVERY_ROOT", &root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let host = open(&root);
    host.monitor_tick().await.unwrap();
    let report = host.monitor_report(0, 100).await.unwrap();
    assert!(matches!(
        report.episodes[0].state,
        EpisodeState::Uncertain { .. }
    ));
    assert!(report.episodes[0].candidate.is_none());
    assert_eq!(report.status.cursor, status.cursor);
    assert_eq!(report.status.active, episode.regressed);
    assert_eq!(
        report
            .measures
            .iter()
            .find(|m| m.signature == Signature::MissingUsage)
            .unwrap()
            .opportunities,
        3
    );
    host.monitor_tick().await.unwrap();
    assert_eq!(
        host.monitor_report(0, 100)
            .await
            .unwrap()
            .measures
            .iter()
            .find(|m| m.signature == Signature::MissingUsage)
            .unwrap()
            .opportunities,
        3
    );
}

#[tokio::test]
async fn missing_monitor_state_cannot_block_new_user_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    let host = open(&root);
    let db = rusqlite::Connection::open(root.join("v1.sqlite3")).unwrap();
    db.execute("DELETE FROM monitor_state", []).unwrap();
    assert!(host.monitor_tick().await.is_err());
    let session = host
        .create_session(SessionAdmissionRequest::new(
            dir.path().to_owned(),
            ModelSettings::default(),
            10000,
            Channel::Stable,
        ))
        .await
        .unwrap();
    assert!(session.admission().unwrap().native_read().is_none());
}

#[tokio::test]
async fn reconciled_usage_costs_and_unknown_calls_preserve_denominators() {
    let dir = tempfile::tempdir().unwrap();
    let host = open(&dir.path().join("state"));
    let session = host
        .create_session(SessionAdmissionRequest::new(
            dir.path().to_owned(),
            ModelSettings::default(),
            10000,
            Channel::Stable,
        ))
        .await
        .unwrap();
    let mut store = host.store.lock().await;
    let intake = store.artifacts().put(br#"{"version":1,"delivery":"source","profile":{"version":1,"name":"fixture","checks":{}}}"#).unwrap();
    let (_, task, _) = store
        .start_request(
            session.id,
            uuid::Uuid::new_v4(),
            "fixture".into(),
            Default::default(),
            intake,
        )
        .unwrap();
    let unknown = uuid::Uuid::new_v4();
    let lost = uuid::Uuid::new_v4();
    store.reserve_model_call(task.id, unknown).unwrap();
    store.reserve_model_call(task.id, lost).unwrap();
    let receipt = store
        .artifacts()
        .put(b"controlled unknown receipt")
        .unwrap();
    store
        .record_model_call(
            task.id,
            unknown,
            crate::state::ModelCallReceipt {
                status: ModelCallStatus::Unknown,
                tokens: None,
                report: receipt,
            },
        )
        .unwrap();
    drop(store);
    host.monitor_tick().await.unwrap();
    let report = host.monitor_report(0, 100).await.unwrap();
    let usage = report
        .measures
        .iter()
        .find(|m| m.signature == Signature::MissingUsage)
        .unwrap();
    assert_eq!((usage.opportunities, usage.measured), (2, 0));
    let cost = report
        .measures
        .iter()
        .find(|m| m.signature == Signature::Cost)
        .unwrap();
    assert_eq!((cost.opportunities, cost.measured), (2, 0));
    let mut store = host.store.lock().await;
    store
        .record_model_call(
            task.id,
            unknown,
            crate::state::ModelCallReceipt {
                status: ModelCallStatus::Failed,
                tokens: Some(11),
                report: receipt,
            },
        )
        .unwrap();
    let state = store.load_session(session.id).unwrap();
    store
        .session_command(
            session.id,
            state.revision,
            uuid::Uuid::new_v4(),
            SessionCommand::ProviderCost {
                request: state.active_request.unwrap(),
                call: unknown,
                cost_usd: Some("0.03".parse().unwrap()),
            },
        )
        .unwrap();
    drop(store);
    host.monitor_tick().await.unwrap();
    let report = host.monitor_report(0, 100).await.unwrap();
    let usage = report
        .measures
        .iter()
        .find(|m| m.signature == Signature::MissingUsage)
        .unwrap();
    assert_eq!(
        (usage.opportunities, usage.measured, usage.failures),
        (2, 1, 0)
    );
    let cost = report
        .measures
        .iter()
        .find(|m| m.signature == Signature::Cost)
        .unwrap();
    assert_eq!((cost.opportunities, cost.measured), (2, 1));
    assert_eq!(cost.recorded_usd, "0.03".parse().unwrap());
    host.monitor_tick().await.unwrap();
    assert_eq!(
        serde_json::to_value(&report.measures).unwrap(),
        serde_json::to_value(host.monitor_report(0, 100).await.unwrap().measures).unwrap()
    );
}

#[tokio::test]
async fn evaluation_origin_is_excluded_from_user_metrics() {
    let dir = tempfile::tempdir().unwrap();
    let host = open(&dir.path().join("state"));
    let session = host
        .create_session(SessionAdmissionRequest::new(
            dir.path().to_owned(),
            ModelSettings::default(),
            10000,
            Channel::Stable,
        ))
        .await
        .unwrap();
    let mut store = host.store.lock().await;
    let evaluation = SessionId::new();
    store
        .create_bound_session(evaluation, session.admission().unwrap().clone(), None)
        .unwrap();
    store
        .monitor_set_origin(evaluation, Origin::Evaluation)
        .unwrap();
    let intake = store.artifacts().put(br#"{"version":1,"delivery":"source","profile":{"version":1,"name":"fixture","checks":{}}}"#).unwrap();
    let (_, task, _) = store
        .start_request(
            evaluation,
            uuid::Uuid::new_v4(),
            "evaluation".into(),
            Default::default(),
            intake,
        )
        .unwrap();
    store
        .reserve_model_call(task.id, uuid::Uuid::new_v4())
        .unwrap();
    drop(store);
    host.monitor_tick().await.unwrap();
    let report = host.monitor_report(0, 100).await.unwrap();
    assert!(report.measures.is_empty());
    assert!(report.status.sampling.skipped_origin > 0);
    assert!(report.episodes.is_empty());
}

#[tokio::test]
async fn fresh_process_monitor_recovery_probe() {
    let Ok(root) = std::env::var("ORVEK_MONITOR_RECOVERY_ROOT") else {
        return;
    };
    let host = open(std::path::Path::new(&root));
    host.monitor_tick().await.unwrap();
    fs::write(
        std::path::Path::new(&root).join("recovered.json"),
        serde_json::to_vec(&host.monitor_report(0, 100).await.unwrap()).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn only_authenticated_changes_requested_reviews_count_as_user_corrections() {
    let dir = tempfile::tempdir().unwrap();
    let host = open(&dir.path().join("state"));
    let session = host
        .create_session(SessionAdmissionRequest::new(
            dir.path().to_owned(),
            ModelSettings::default(),
            10000,
            Channel::Stable,
        ))
        .await
        .unwrap();
    let (manifest, source_identity) = {
        let store = host.store.lock().await;
        let tree = store
            .artifacts()
            .put(br#"{"version":1,"files":{}}"#)
            .unwrap();
        let patch = store.artifacts().put(b"").unwrap();
        let range = crate::review::ReviewRange::Snapshots {
            before: tree,
            after: tree,
        };
        let source_identity = Digest::of_value(&(
            1u32,
            &range,
            None::<String>,
            None::<String>,
            tree,
            tree,
            patch,
        ))
        .unwrap();
        let manifest = crate::review::ReviewManifest {
            version: 1,
            source_identity,
            repository: "fixture".into(),
            range,
            base_revision: None,
            head_revision: None,
            before: tree,
            after: tree,
            patch,
            git_executable: Digest::of(b"fixture"),
            metadata_changes: vec![],
        };
        (
            store
                .artifacts()
                .put(&serde_json::to_vec(&manifest).unwrap())
                .unwrap(),
            source_identity,
        )
    };
    for disposition in [
        crate::feedback::Disposition::Approved,
        crate::feedback::Disposition::Comment,
        crate::feedback::Disposition::ChangesRequested,
    ] {
        host.record_review(
            session.id,
            uuid::Uuid::new_v4(),
            manifest,
            source_identity,
            disposition,
            "fixture human note".into(),
        )
        .await
        .unwrap();
    }
    let mut store = host.store.lock().await;
    let revision = store.load_session(session.id).unwrap().revision;
    store
        .session_command(
            session.id,
            revision,
            uuid::Uuid::new_v4(),
            SessionCommand::Feedback {
                message: "internal host diagnostic, not a human correction".into(),
            },
        )
        .unwrap();
    drop(store);
    host.monitor_tick().await.unwrap();
    let report = host.monitor_report(0, 100).await.unwrap();
    let metric = report
        .measures
        .iter()
        .find(|m| m.signature == Signature::UserCorrection)
        .unwrap();
    assert_eq!(
        (metric.opportunities, metric.failures, metric.measured),
        (3, 1, 3)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn monitor_large_result_intake_keeps_native_host_status_responsive() {
    use crate::{session::SessionCommand, state::RequestKind};
    use std::{
        future::Future,
        task::{Context, Waker},
    };
    use uuid::Uuid;

    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(open(&directory.path().join("state")));
    let mut session = host
        .create_session(SessionAdmissionRequest::new(
            directory.path().to_owned(),
            ModelSettings::default(),
            10000,
            Channel::Stable,
        ))
        .await
        .unwrap();
    let request = Uuid::new_v4();
    {
        let mut store = host.store.lock().await;
        session = store
            .session_command(
                session.id,
                session.revision,
                request,
                SessionCommand::Input {
                    kind: RequestKind::Conversation,
                    content: vec![json!({"role":"user","content":"large result"})],
                },
            )
            .unwrap();
        session = store.session_command(session.id, session.revision, Uuid::new_v4(),
            SessionCommand::Response { request, items: vec![json!({"type":"function_call","call_id":"large","name":"interpreter_eval","arguments":"{}"})] }).unwrap();
        store
            .session_command(
                session.id,
                session.revision,
                Uuid::new_v4(),
                SessionCommand::ToolResult {
                    request,
                    call_id: "large".into(),
                    output: json!({"output":{"value":"\\".repeat(2 * 1024 * 1024 - 1)}})
                        .to_string(),
                },
            )
            .unwrap();
    }

    let guard = host.store.lock().await;
    let monitoring_host = host.clone();
    let mut monitor = Box::pin(async move { monitoring_host.monitor_tick().await });
    let mut status = Box::pin(host.info());
    // Register the monitor before the status request in the mutex's FIFO queue.
    {
        let mut context = Context::from_waker(Waker::noop());
        assert!(monitor.as_mut().poll(&mut context).is_pending());
        assert!(status.as_mut().poll(&mut context).is_pending());
    }
    let monitor = tokio::spawn(monitor);
    drop(guard);
    let result = tokio::time::timeout(Duration::from_secs(5), status).await;
    monitor.await.unwrap().unwrap();
    assert!(
        result.is_ok(),
        "optional monitoring held the native store beyond the IPC window"
    );
    result.unwrap().unwrap();
}
