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

#[test]
fn inline_skill_content_is_self_contained_only_when_its_digest_matches() {
    use orvek_harness::Digest;
    let original = "exact skill body";
    let digest = Digest::of(original.as_bytes());
    for body in [original, "changed skill body"] {
        let (root, mut store, id) = fixture();
        let output = json!({"path":"/source/SKILL.md","digest":digest,"content":body});
        let receipt = store
            .public_artifacts()
            .write(
                &serde_json::to_vec(&json!({"kind":"diagnostic","output":output.to_string()}))
                    .unwrap(),
            )
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
                    record: receipt,
                },
            )
            .unwrap();
        let mut bundle = export(root.path());
        assert_eq!(bundle.replay().unwrap().exact, body == original);
        if body == original {
            assert!(matches!(bundle.artifacts[&digest], Payload::Identity));
        } else {
            assert!(matches!(bundle.artifacts[&digest], Payload::Missing));
            bundle.exact = true;
            assert!(bundle.replay().is_err());
        }
    }
}

fn start_task(store: &mut Store, session: SessionId) -> orvek_harness::state::TaskId {
    let policy = store
        .public_artifacts()
        .write(br#"{"version":1,"delivery":"source","profile":{"version":1,"name":"fixture","checks":{}}}"#)
        .unwrap()
        .digest();
    store
        .start_request(
            session,
            Uuid::new_v4(),
            "inspect".into(),
            Default::default(),
            policy,
        )
        .unwrap()
        .1
        .id
}

fn assert_shared_receipt_is_bounded(
    root: &std::path::Path,
    receipt: orvek_harness::Digest,
    body_pointer: &str,
) {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let mut bundle = export(root);
    let report = bundle.replay().unwrap();
    let bodies = report
        .spans
        .iter()
        .filter_map(|span| span.pointer(body_pointer))
        .collect::<Vec<_>>();
    assert_eq!(bodies.len(), 24);
    assert!(
        bodies
            .iter()
            .all(|body| body.as_str().unwrap().len() == 128 * 1024)
    );
    let Payload::Present(encoded) = &bundle.artifacts[&receipt] else {
        panic!("missing fixture receipt")
    };
    let materialized_bytes = serde_json::to_vec(&report.spans).unwrap().len()
        + bodies.len() * STANDARD.decode(encoded).unwrap().len()
        + bundle
            .records
            .iter()
            .filter(|record| record.kind == "session")
            .map(|record| STANDARD.decode(&record.event_base64).unwrap().len())
            .sum::<usize>()
        + serde_json::to_vec(&report.causality).unwrap().len();
    bundle.limits.bytes = materialized_bytes as u64;
    assert!(
        bundle.replay().is_ok(),
        "the exact materialization budget must fit"
    );
    bundle.limits.bytes -= 1;
    let error = bundle
        .replay()
        .map(|_| ())
        .expect_err("one byte below the materialization budget must fail");
    assert!(error.to_string().contains("materialization"), "{error}");

    let stored_bytes: usize = bundle
        .records
        .iter()
        .map(|record| STANDARD.decode(&record.event_base64).unwrap().len())
        .chain(
            bundle
                .artifacts
                .values()
                .filter_map(|payload| match payload {
                    Payload::Present(encoded) => Some(STANDARD.decode(encoded).unwrap().len()),
                    _ => None,
                }),
        )
        .sum();
    bundle.limits.bytes = 512 * 1024;
    assert!(stored_bytes < bundle.limits.bytes as usize);
    let error = bundle
        .replay()
        .map(|_| ())
        .expect_err("shared bodies exceed the replay budget");
    assert!(error.to_string().contains("materialization"), "{error}");
    let error = TraceBundle::export(root, None, bundle.limits, &BTreeSet::new(), None)
        .map(|_| ())
        .expect_err("export must also bound its final replay");
    assert!(error.to_string().contains("materialization"), "{error}");
    let error = bundle
        .review()
        .map(|_| ())
        .expect_err("review must not amplify receipts");
    assert!(error.to_string().contains("materialization"), "{error}");
    let file = root.join("bounded-trace.json");
    let envelope =
        json!({"digest":orvek_harness::Digest::of_value(&bundle).unwrap(),"bundle":bundle});
    std::fs::write(&file, serde_json::to_vec(&envelope).unwrap()).unwrap();
    let error = TraceBundle::read(&file)
        .map(|_| ())
        .expect_err("import must bound shared receipts");
    assert!(error.to_string().contains("materialization"), "{error}");
}

#[test]
fn shared_trace_receipts_cannot_amplify_replay_memory() {
    let (root, mut store, session) = fixture();
    let body = json!({"kind":"diagnostic","text":"x".repeat(128 * 1024)});
    let receipt = store
        .public_artifacts()
        .write(&serde_json::to_vec(&body).unwrap())
        .unwrap()
        .digest();
    let mut state = store.load_session(session).unwrap();
    for _ in 0..24 {
        state = store
            .session_command(
                session,
                state.revision,
                Uuid::new_v4(),
                SessionCommand::TraceRecorded {
                    request: Uuid::new_v4(),
                    record: receipt,
                },
            )
            .unwrap();
    }
    assert_shared_receipt_is_bounded(root.path(), receipt, "/span/text");
}

#[test]
fn shared_model_reports_cannot_amplify_replay_memory() {
    use orvek_harness::state::{ModelCallReceipt, ModelCallStatus};

    let (root, mut store, session) = fixture();
    let task = start_task(&mut store, session);
    let body = json!({"partial_text":"x".repeat(128 * 1024)});
    let report = store
        .public_artifacts()
        .write(&serde_json::to_vec(&body).unwrap())
        .unwrap()
        .digest();
    for _ in 0..24 {
        let call = Uuid::new_v4();
        store.reserve_model_call(task, call).unwrap();
        store
            .record_model_call(
                task,
                call,
                ModelCallReceipt {
                    status: ModelCallStatus::Completed,
                    tokens: Some(10),
                    report,
                },
            )
            .unwrap();
    }
    assert_shared_receipt_is_bounded(root.path(), report, "/span/report/partial_text");
}

#[test]
fn stored_byte_bounds_are_checked_before_receipt_expansion() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let (root, mut store, session) = fixture();
    let digest = receipt(&mut store, session);
    let mut bundle = export(root.path());
    bundle.limits.bytes = 1024;
    bundle
        .artifacts
        .insert(digest, Payload::Present(STANDARD.encode(vec![b'x'; 2048])));
    let error = bundle.replay().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("decoded bundle exceeds byte bound"),
        "{error}"
    );
}

fn known_call(store: &mut Store, session: SessionId, task: orvek_harness::state::TaskId) {
    use orvek_harness::state::{ModelCallReceipt, ModelCallStatus};

    let call = Uuid::new_v4();
    let report = store.public_artifacts().write(b"{}").unwrap().digest();
    store.reserve_model_call(task, call).unwrap();
    store
        .record_model_call(
            task,
            call,
            ModelCallReceipt {
                status: ModelCallStatus::Completed,
                tokens: Some(10),
                report,
            },
        )
        .unwrap();
    let state = store.load_session(session).unwrap();
    store
        .session_command(
            session,
            state.revision,
            Uuid::new_v4(),
            SessionCommand::ProviderCost {
                request: Uuid::new_v4(),
                call,
                cost_usd: Some("0.125".parse().unwrap()),
            },
        )
        .unwrap();
}

#[test]
fn linked_receipts_keep_known_token_and_cost_totals() {
    let (root, mut store, session) = fixture();
    let task = start_task(&mut store, session);
    known_call(&mut store, session, task);
    let cost = export(root.path()).replay().unwrap().cost;
    assert_eq!(cost.total_tokens, Some(10));
    assert_eq!(cost.recorded_usd, "0.125".parse().unwrap());
    assert!(cost.complete);
    assert_eq!(cost.calls, 1);
    assert_eq!(cost.unknown_calls, 0);
}

fn assert_unlinked_provider_usage_keeps_totals_unknown(mixed: bool) {
    use orvek_harness::inference::Usage;

    let (root, mut store, session) = fixture();
    if mixed {
        let task = start_task(&mut store, session);
        known_call(&mut store, session, task);
    }
    let mut state = store.load_session(session).unwrap();
    if state.active_request.is_none() {
        state = store
            .session_command(
                session,
                state.revision,
                Uuid::new_v4(),
                SessionCommand::Input {
                    kind: orvek_harness::state::RequestKind::Conversation,
                    content: vec![json!({"role":"user","content":"inspect"})],
                },
            )
            .unwrap();
    }
    let request = state.active_request.unwrap();
    store
        .session_command(
            session,
            state.revision,
            Uuid::new_v4(),
            SessionCommand::ProviderUsage {
                request,
                call: None,
                usage: Usage {
                    input_tokens: Some(5),
                    output_tokens: Some(5),
                    total_tokens: Some(10),
                    cost_usd: Some("0.25".parse().unwrap()),
                    ..Default::default()
                },
                representation: None,
            },
        )
        .unwrap();
    let cost = export(root.path()).replay().unwrap().cost;
    assert_eq!(cost.total_tokens, None, "mixed={mixed}");
    assert!(!cost.complete, "mixed={mixed}");
    assert_eq!(cost.calls, usize::from(mixed), "do not invent call IDs");
    assert_eq!(cost.unknown_calls, 0, "no linked call has unknown cost");
    assert_eq!(
        cost.recorded_usd,
        if mixed { "0.125" } else { "0" }.parse().unwrap()
    );
}

fn assert_legacy_usage_charged_keeps_totals_unknown(mixed: bool) {
    use orvek_harness::state::Usage;

    let (root, mut store, session) = fixture();
    let task = start_task(&mut store, session);
    if mixed {
        known_call(&mut store, session, task);
    }
    store
        .charge_usage(
            task,
            Uuid::new_v4(),
            Usage {
                model_calls: 2,
                tokens: 7,
            },
        )
        .unwrap();
    let report = export(root.path()).replay().unwrap();
    assert_eq!(report.tasks[&task].usage.tokens, if mixed { 17 } else { 7 });
    assert_eq!(report.cost.total_tokens, None, "mixed={mixed}");
    assert!(!report.cost.complete, "mixed={mixed}");
    assert_eq!(
        report.cost.calls,
        usize::from(mixed),
        "do not invent call IDs"
    );
    assert_eq!(
        report.cost.unknown_calls, 0,
        "no linked call has unknown cost"
    );
    assert_eq!(
        report.cost.recorded_usd,
        if mixed { "0.125" } else { "0" }.parse().unwrap()
    );
}

#[test]
fn unlinked_provider_usage_keeps_totals_unknown() {
    assert_unlinked_provider_usage_keeps_totals_unknown(false);
}

#[test]
fn mixed_linked_and_unlinked_provider_usage_keeps_totals_unknown() {
    assert_unlinked_provider_usage_keeps_totals_unknown(true);
}

#[test]
fn legacy_usage_charged_keeps_totals_unknown() {
    assert_legacy_usage_charged_keeps_totals_unknown(false);
}

#[test]
fn mixed_linked_and_legacy_usage_charged_keeps_totals_unknown() {
    assert_legacy_usage_charged_keeps_totals_unknown(true);
}

fn append_span(store: &mut Store, session: SessionId, request: Uuid, span: serde_json::Value) {
    let record = store
        .public_artifacts()
        .write(&serde_json::to_vec(&span).unwrap())
        .unwrap()
        .digest();
    let state = store.load_session(session).unwrap();
    store
        .session_command(
            session,
            state.revision,
            Uuid::new_v4(),
            SessionCommand::TraceRecorded { request, record },
        )
        .unwrap();
}

#[test]
fn hash_correct_dispatch_cannot_contradict_journal_identity() {
    let (root, mut store, session) = fixture();
    let task = start_task(&mut store, session);
    let request = store.load_session(session).unwrap().active_request.unwrap();
    let blob = store.public_artifacts().write(b"[]").unwrap().digest();
    append_span(
        &mut store,
        session,
        request,
        json!({
            "version":1, "kind":"model_dispatch", "session":SessionId::new(),
            "request":request,"task":task,"child":null,"call":Uuid::new_v4(),
            "model":ModelSettings::default(),"input":blob,"tools":blob,
            "instructions":blob,"payload":blob,"cache":{}
        }),
    );
    let error = TraceBundle::export(
        root.path(),
        None,
        Default::default(),
        &BTreeSet::new(),
        None,
    )
    .expect_err("a correctly hashed dispatch with the wrong session must fail");
    assert!(error.to_string().contains("session"), "{error}");
}

#[test]
fn hash_correct_child_response_cannot_contradict_journal_request() {
    let (root, mut store, session) = fixture();
    let task = start_task(&mut store, session);
    let request = store.load_session(session).unwrap().active_request.unwrap();
    append_span(
        &mut store,
        session,
        request,
        json!({
            "version":1,"kind":"model_response","session":session,"request":Uuid::new_v4(),
            "task":task,"child":Uuid::new_v4(),"call":Uuid::new_v4(),
            "outcome":orvek_harness::inference::CallOutcome::default()
        }),
    );
    let error = TraceBundle::export(
        root.path(),
        None,
        Default::default(),
        &BTreeSet::new(),
        None,
    )
    .expect_err("a correctly hashed response with the wrong request must fail");
    assert!(error.to_string().contains("request"), "{error}");
}
