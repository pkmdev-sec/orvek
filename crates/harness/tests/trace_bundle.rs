use orvek_harness::{
    Store,
    inference::ModelSettings,
    session::{SessionCommand, SessionConfig, SessionId},
    trace::{Payload, TraceBundle, TraceLimits},
};
use serde_json::json;
use std::collections::BTreeSet;
use uuid::Uuid;

fn fixture() -> (tempfile::TempDir, Store, SessionId) {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let id = SessionId::new();
    store
        .create_session(
            id,
            SessionConfig {
                workspace: root.path().to_owned(),
                model: ModelSettings::default(),
                instructions: String::new(),
                context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            },
            None,
        )
        .unwrap();
    (root, store, id)
}
fn export(root: &std::path::Path) -> TraceBundle {
    TraceBundle::export(root, None, TraceLimits::default(), &BTreeSet::new(), None).unwrap()
}
fn receipt(store: &mut Store, id: SessionId) -> orvek_harness::Digest {
    let digest = store
        .public_artifacts()
        .write(br#"{"kind":"diagnostic","text":"durable diagnostic payload"}"#)
        .unwrap()
        .digest();
    let state = store.load_session(id).unwrap();
    store
        .session_command(
            id,
            state.revision,
            Uuid::new_v4(),
            SessionCommand::TraceRecorded {
                request: Uuid::new_v4(),
                record: digest,
            },
        )
        .unwrap();
    digest
}
#[test]
fn exact_prefix_replays_state_context_and_is_read_only() {
    let (root, mut store, id) = fixture();
    let state = store.load_session(id).unwrap();
    let state = store
        .session_command(
            id,
            state.revision,
            Uuid::new_v4(),
            SessionCommand::Input {
                kind: orvek_harness::state::RequestKind::Task,
                content: vec![json!({"role":"user","content":"inspect, not execute"})],
            },
        )
        .unwrap();
    let view = orvek_harness::context::project(&state, 1024 * 1024).unwrap();
    let state = store
        .session_command(
            id,
            state.revision,
            Uuid::new_v4(),
            SessionCommand::ContextProjected {
                source_revision: state.revision,
                view: Some(view),
                projection: vec![],
            },
        )
        .unwrap();
    let before = store.journal_head().unwrap();
    let bundle = export(root.path());
    let report = bundle.replay().unwrap();
    assert!(report.exact, "{:?}", report.unresolved);
    assert_eq!(report.sessions[&id], state);
    assert_eq!(store.journal_head().unwrap(), before);
    let file = root.path().join("trace.json");
    bundle.write(&file).unwrap();
    assert_eq!(
        TraceBundle::read(&file).unwrap().replay().unwrap().identity,
        report.identity
    );
    let prefix = TraceBundle::export(
        root.path(),
        Some(1),
        Default::default(),
        &BTreeSet::new(),
        None,
    )
    .unwrap();
    assert_eq!(prefix.replay().unwrap().sessions[&id].revision, 1);
    assert!(
        TraceBundle::export(
            root.path(),
            Some(before + 1),
            Default::default(),
            &BTreeSet::new(),
            None
        )
        .is_err()
    );
}
#[test]
fn missing_omitted_and_bounded_payloads_never_claim_exactness() {
    let (root, mut store, id) = fixture();
    let digest = receipt(&mut store, id);
    let bundle = export(root.path());
    let redacted = TraceBundle::export(
        root.path(),
        None,
        Default::default(),
        &BTreeSet::from([digest]),
        None,
    )
    .unwrap();
    assert!(!redacted.exact);
    assert!(!redacted.replay().unwrap().exact);
    assert!(matches!(redacted.artifacts[&digest], Payload::Omitted));
    let mut forged = redacted.clone();
    forged.exact = true;
    assert!(forged.replay().is_err());
    std::fs::remove_file(root.path().join("artifacts").join(digest.to_string())).unwrap();
    let missing = export(root.path());
    assert!(!missing.exact);
    assert!(matches!(missing.artifacts[&digest], Payload::Missing));
    assert!(!missing.replay().unwrap().unresolved.is_empty());
    assert!(
        bundle.replay().is_ok(),
        "portable copy does not read original store"
    );
    let bounded = TraceBundle::export(
        root.path(),
        None,
        TraceLimits {
            artifacts: 1,
            ..Default::default()
        },
        &BTreeSet::new(),
        None,
    )
    .unwrap();
    assert!(!bounded.exact);
}
#[test]
fn tampered_blob_event_expectation_and_cursor_gap_fail_closed() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let (root, mut store, id) = fixture();
    let digest = receipt(&mut store, id);
    let bundle = export(root.path());
    let mut tampered = bundle.clone();
    tampered
        .artifacts
        .insert(digest, Payload::Present(STANDARD.encode(b"forged")));
    assert!(tampered.replay().is_err());
    let mut gap = bundle.clone();
    gap.records.remove(0);
    assert!(gap.replay().is_err());
    let mut revision = bundle.clone();
    revision.records[1].revision += 1;
    assert!(revision.replay().is_err());
    let mut state = bundle.clone();
    state.expected.states.clear();
    assert!(state.replay().is_err());
    let mut event = bundle.clone();
    event.records[0].event_base64 = STANDARD.encode(b"{}");
    assert!(event.replay().is_err());
    let file = root.path().join("trace.json");
    bundle.write(&file).unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    value["bundle"]["through"] = json!(1234);
    std::fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(TraceBundle::read(&file).is_err());
    std::fs::write(
        root.path().join("artifacts").join(digest.to_string()),
        b"forged",
    )
    .unwrap();
    assert!(
        TraceBundle::export(
            root.path(),
            None,
            Default::default(),
            &BTreeSet::new(),
            None
        )
        .is_err()
    );
}
#[test]
fn journal_gap_is_rejected_at_read_only_export() {
    let (root, mut store, id) = fixture();
    receipt(&mut store, id);
    let connection = rusqlite::Connection::open(root.path().join("v1.sqlite3")).unwrap();
    connection
        .execute("DELETE FROM events WHERE sequence=1", [])
        .unwrap();
    assert!(
        TraceBundle::export(
            root.path(),
            None,
            Default::default(),
            &BTreeSet::new(),
            None
        )
        .is_err()
    );
}

#[test]
fn uncertain_jobs_block_fresh_experiments_and_unknown_cost_stays_unknown() {
    use orvek_harness::state::{JobInvocation, ModelCallReceipt, ModelCallStatus};
    let (root, mut store, session) = fixture();
    let policy=store.public_artifacts().write(br#"{"version":1,"delivery":"source","profile":{"version":1,"name":"fixture","checks":{}}}"#).unwrap().digest();
    let request = Uuid::new_v4();
    let (_, task, _) = store
        .start_request(
            session,
            request,
            "inspect".into(),
            Default::default(),
            policy,
        )
        .unwrap();
    let artifact = store.public_artifacts().write(b"{}").unwrap().digest();
    store
        .start_execution_job(
            task.id,
            task.revision,
            false,
            60_000,
            JobInvocation {
                session,
                request,
                call_id: None,
                capability: "read_file".into(),
                input: artifact,
                environment: artifact,
            },
        )
        .unwrap();
    let call = Uuid::new_v4();
    store.reserve_model_call(task.id, call).unwrap();
    store
        .record_model_call(
            task.id,
            call,
            ModelCallReceipt {
                status: ModelCallStatus::Unknown,
                tokens: None,
                report: artifact,
            },
        )
        .unwrap();
    let bundle = export(root.path());
    assert!(
        bundle
            .reexecution_intent(task.id)
            .unwrap_err()
            .to_string()
            .contains("uncertain")
    );
    let report = bundle.replay().unwrap();
    assert!(!report.cost.complete);
    assert_eq!(report.cost.total_tokens, None);
    assert_eq!(
        bundle.prefixes().unwrap().count(),
        0,
        "no historical dispatches are invented"
    );
    assert_eq!(report.cost.unknown_calls, 1);
    assert_eq!(
        report.cost.recorded_usd,
        orvek_harness::inference::UsdCost::ZERO
    );
    assert_eq!(report.tasks[&task.id].model_receipts[&call].tokens, None);
}

#[test]
fn removed_and_mislabeled_closure_cannot_claim_exact_replay() {
    let (root, mut store, session) = fixture();
    let digest = receipt(&mut store, session);
    let bundle = export(root.path());
    let mut removed = bundle.clone();
    removed.artifacts.remove(&digest);
    assert!(removed.replay().is_err());
    removed.exact = false;
    assert!(!removed.replay().unwrap().exact);
    let mut mislabeled = bundle;
    mislabeled.artifacts.insert(digest, Payload::Identity);
    assert!(mislabeled.replay().is_err());
}

#[test]
fn nested_closure_enforces_hop_bounds_on_export_and_replay() {
    let (root, mut store, session) = fixture();
    let leaf = store
        .public_artifacts()
        .write(b"nested payload")
        .unwrap()
        .digest();
    let middle = store
        .public_artifacts()
        .write(&serde_json::to_vec(&json!({"payload":leaf})).unwrap())
        .unwrap()
        .digest();
    let state = store.load_session(session).unwrap();
    store
        .session_command(
            session,
            state.revision,
            Uuid::new_v4(),
            SessionCommand::TraceRecorded {
                request: Uuid::new_v4(),
                record: middle,
            },
        )
        .unwrap();
    let bundle = export(root.path());
    assert!(bundle.exact);
    let mut forged_bound = bundle.clone();
    forged_bound.limits.depth = 0;
    assert!(
        forged_bound.replay().is_err(),
        "replay must enforce the manifest's traversal depth"
    );
    let bounded = TraceBundle::export(
        root.path(),
        None,
        TraceLimits {
            depth: 0,
            ..Default::default()
        },
        &BTreeSet::new(),
        None,
    )
    .unwrap();
    assert!(!bounded.exact);
    assert!(matches!(bounded.artifacts[&leaf], Payload::Bounded));
    std::fs::remove_file(root.path().join("artifacts").join(leaf.to_string())).unwrap();
    let missing = export(root.path());
    assert!(!missing.replay().unwrap().exact);
    drop(store);
    drop(root);
    assert!(
        bundle.replay().unwrap().exact,
        "offline replay must not consult the original workspace or store"
    );
}
