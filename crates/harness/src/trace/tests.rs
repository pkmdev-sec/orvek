use super::*;
use crate::{
    context::{self, ContextSegmentRole, ContextView},
    inference::{
        ArgumentValidity, CallOutcome, ModelSettings, OutputItem, PromptInput, ProviderResponse,
        RequestProvenance, RequestRoute, ResponseDialect, ResponseStatus, ToolProposal, Usage,
    },
    session::SessionConfig,
    state::{JobInvocation, JobStatus, ModelCallReceipt, ModelCallStatus, RequestKind},
};

struct Fixture {
    root: tempfile::TempDir,
    store: Store,
    session: SessionId,
    request: Uuid,
    task: TaskId,
    call: Uuid,
    projection: ContextView,
    inference: InferenceRequest,
    dispatch: Value,
}

impl Fixture {
    fn new(child: Option<Uuid>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path()).unwrap();
        let session = SessionId::new();
        store
            .create_session(
                session,
                SessionConfig {
                    workspace: root.path().into(),
                    model: ModelSettings::default(),
                    instructions: String::new(),
                    context_window_tokens: context::DEFAULT_WINDOW_TOKENS,
                },
                None,
            )
            .unwrap();
        let request = Uuid::new_v4();
        let policy = store.artifacts().put(br#"{"version":1,"delivery":"source","profile":{"version":1,"name":"test","checks":{}}}"#).unwrap();
        let (_, task, _) = store
            .start_request(
                session,
                request,
                "inspect".into(),
                Default::default(),
                policy,
            )
            .unwrap();
        let projection =
            context::project(&store.load_session(session).unwrap(), 1024 * 1024).unwrap();
        let segments = projection
            .manifest
            .segments
            .iter()
            .filter(|s| s.role == ContextSegmentRole::StableHistory)
            .map(|s| s.input)
            .collect();
        let input = PromptInput::segmented(
            projection.stable_input().to_vec(),
            projection.live_input().to_vec(),
            segments,
        )
        .unwrap();
        let inference = InferenceRequest::new_segmented(
            ModelSettings::default(),
            input,
            vec![json!({"type":"function","name":"read_file","parameters":{"type":"object"}})],
            "recorded instructions".into(),
            session.to_string(),
            100,
        )
        .unwrap();
        let wire = inference.wire(Transport::Http);
        let input = store
            .artifacts()
            .put(&serde_json::to_vec(&wire["input"]).unwrap())
            .unwrap();
        let tools = store
            .artifacts()
            .put(&serde_json::to_vec(&wire["tools"]).unwrap())
            .unwrap();
        let instructions = store
            .artifacts()
            .put(wire["instructions"].as_str().unwrap().as_bytes())
            .unwrap();
        let payload = store
            .artifacts()
            .put(&serde_json::to_vec(&wire).unwrap())
            .unwrap();
        let call = Uuid::new_v4();
        let dispatch = json!({"version":1,"kind":"model_dispatch","session":session,"request":request,"task":task.id,"child":child,"call":call,"model":inference.settings(),"input":input,"tools":tools,"instructions":instructions,"payload":payload,"cache":inference.cache_identity()});
        Self {
            root,
            store,
            session,
            request,
            task: task.id,
            call,
            projection,
            inference,
            dispatch,
        }
    }
    fn dispatch(&mut self) {
        if self.dispatch["child"].is_null() {
            self.store.reserve_model_call(self.task, self.call).unwrap();
        }
        record_span(
            &mut self.store,
            self.session,
            self.request,
            self.dispatch.clone(),
        )
        .unwrap();
    }
    fn outcome(&self, transport: Transport, dialect: ResponseDialect) -> CallOutcome {
        CallOutcome {
            request: RequestProvenance::Prepared {
                transport,
                dialect,
                route: RequestRoute::Default,
                body: serde_json::to_string(&self.inference.effective_wire(transport, dialect))
                    .unwrap(),
            },
            ..Default::default()
        }
    }
    fn report(&self, outcome: &CallOutcome) -> Value {
        json!({"version":3,"model":self.inference.settings(),"context":self.projection.manifest,"cache":self.inference.cache_identity(),"sent_input":Digest::of_value(&self.inference.wire(Transport::Http)["input"]).unwrap(),"outcome":outcome})
    }
    fn record_outcome(&mut self, report: Value, tokens: Option<u64>) -> Digest {
        let digest = self
            .store
            .artifacts()
            .put(&serde_json::to_vec(&report).unwrap())
            .unwrap();
        self.store
            .record_model_call(
                self.task,
                self.call,
                ModelCallReceipt {
                    status: ModelCallStatus::Failed,
                    tokens,
                    report: digest,
                },
            )
            .unwrap();
        digest
    }
    fn export(&self) -> Result<TraceBundle, TraceError> {
        TraceBundle::export(
            self.root.path(),
            None,
            Default::default(),
            &BTreeSet::new(),
            None,
        )
    }
    fn replace_artifact(&mut self, key: &str, value: &Value) {
        self.dispatch[key] = json!(
            self.store
                .artifacts()
                .put(&serde_json::to_vec(value).unwrap())
                .unwrap()
        );
    }
    fn proposal_outcome(&self) -> CallOutcome {
        let proposal = ToolProposal {
            item_id: "item".into(),
            call_id: "tool".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"answer.txt"}"#.into(),
            validity: ArgumentValidity::JsonObject,
        };
        let mut outcome = self.outcome(Transport::Http, ResponseDialect::OpenAi);
        outcome.response = Some(ProviderResponse {
            id: "response".into(),
            status: ResponseStatus::Completed,
            history_items: vec![
                json!({"type":"function_call","id":proposal.item_id,"call_id":proposal.call_id,"name":proposal.name,"arguments":proposal.arguments}),
            ],
            output: vec![OutputItem::ToolProposal(proposal)],
            usage: Usage::default(),
        });
        outcome
    }
}

#[test]
fn recorded_provider_validates_all_effective_body_dialects_offline() {
    for transport in [Transport::Http, Transport::WebSocket] {
        for dialect in [ResponseDialect::OpenAi, ResponseDialect::ChatGpt] {
            let mut fixture = Fixture::new(None);
            fixture.dispatch();
            let outcome = fixture.outcome(transport, dialect);
            fixture.record_outcome(fixture.report(&outcome), Some(0));
            let bundle = fixture.export().unwrap();
            let call = fixture.call;
            drop(fixture);
            let replay = bundle.replay().unwrap();
            assert!(replay.exact);
            assert!(replay.causality.complete, "{:?}", replay.causality);
            assert!(matches!(
                replay.causality.calls[&call].outcome,
                RecordedStatus::Served
            ));
            assert!(replay.causality.calls[&call].prepared_body_checked);
        }
    }
}

#[test]
fn hash_correct_dispatch_components_and_cache_must_agree() {
    for field in ["input", "tools", "instructions", "payload", "cache"] {
        let mut fixture = Fixture::new(None);
        match field {
            "input" => {
                fixture.replace_artifact(field, &json!([{"role":"user","content":"different"}]))
            }
            "tools" => fixture.replace_artifact(field, &json!([])),
            "instructions" => fixture.replace_artifact(field, &json!("different")),
            "payload" => {
                let mut payload = fixture.inference.wire(Transport::Http);
                payload["model"] = json!("wrong");
                fixture.replace_artifact(field, &payload);
            }
            "cache" => fixture.dispatch["cache"]["lineage"] = json!(Digest::of(b"wrong")),
            _ => unreachable!(),
        }
        fixture.dispatch();
        assert!(fixture.export().is_err(), "accepted inconsistent {field}");
    }
}

#[test]
fn hash_correct_reports_must_match_input_cache_context_body_and_tokens() {
    for field in [
        "sent_input",
        "cache",
        "model",
        "context",
        "context_order",
        "body",
        "tokens",
    ] {
        let mut fixture = Fixture::new(None);
        fixture.dispatch();
        let outcome = fixture.outcome(Transport::WebSocket, ResponseDialect::ChatGpt);
        let mut report = fixture.report(&outcome);
        match field {
            "sent_input" => report[field] = json!(Digest::of(b"wrong")),
            "cache" => report[field]["lineage"] = json!(Digest::of(b"wrong")),
            "model" => report[field]["fast_mode"] = json!(true),
            "context" => report[field]["original_history"] = json!(Digest::of(b"wrong")),
            "context_order" => {
                report["context"] = json!(
                    context::project(
                        &fixture.store.load_session(fixture.session).unwrap(),
                        1024 * 1024
                    )
                    .unwrap()
                    .manifest
                )
            }
            "body" => report["outcome"]["request"][field] = json!("{}"),
            "tokens" => {}
            _ => unreachable!(),
        }
        fixture.record_outcome(report, Some(if field == "tokens" { 1 } else { 0 }));
        assert!(fixture.export().is_err(), "accepted inconsistent {field}");
    }
}

#[test]
fn prefixes_redaction_and_legacy_keep_per_call_unknowns_separate_from_exactness() {
    let mut fixture = Fixture::new(None);
    fixture.dispatch();
    let prefix = fixture.export().unwrap();
    let before = prefix.replay().unwrap();
    assert!(before.exact);
    assert!(!before.causality.complete);
    assert!(
        before.causality.calls[&fixture.call]
            .gaps
            .contains(&CausalGap::OutcomeMissing)
    );
    let outcome = fixture.outcome(Transport::Http, ResponseDialect::OpenAi);
    let report = fixture.record_outcome(fixture.report(&outcome), Some(0));
    let redacted = TraceBundle::export(
        fixture.root.path(),
        None,
        Default::default(),
        &BTreeSet::from([report]),
        None,
    )
    .unwrap()
    .replay()
    .unwrap();
    assert!(!redacted.exact);
    assert!(
        redacted.causality.calls[&fixture.call]
            .gaps
            .contains(&CausalGap::OutcomeUnavailable)
    );
    assert!(matches!(
        redacted.causality.calls[&fixture.call].outcome,
        RecordedStatus::Unavailable
    ));
    let input: Digest = serde_json::from_value(fixture.dispatch["input"].clone()).unwrap();
    let redacted = TraceBundle::export(
        fixture.root.path(),
        None,
        Default::default(),
        &BTreeSet::from([input]),
        None,
    )
    .unwrap()
    .replay()
    .unwrap();
    assert!(
        redacted.causality.calls[&fixture.call]
            .gaps
            .contains(&CausalGap::DispatchPayloadUnavailable)
    );
    let legacy = Uuid::new_v4();
    fixture
        .store
        .reserve_model_call(fixture.task, legacy)
        .unwrap();
    let replay = fixture.export().unwrap().replay().unwrap();
    assert!(replay.exact);
    assert_eq!(replay.cost.total_tokens, None);
    assert!(
        replay.causality.calls[&legacy]
            .gaps
            .contains(&CausalGap::DispatchMissing)
    );
    assert!(
        replay.causality.calls[&legacy]
            .gaps
            .contains(&CausalGap::OutcomeMissing)
    );
}

#[test]
fn child_tool_stub_checks_proposals_and_job_receipts_without_running_tools() {
    for corrupt in ["none", "arguments", "job", "call", "child", "status"] {
        let child = Uuid::new_v4();
        let mut fixture = Fixture::new(Some(child));
        fixture.dispatch();
        let outcome = fixture.proposal_outcome();
        record_span(&mut fixture.store,fixture.session,fixture.request,json!({"version":1,"kind":"model_response","session":fixture.session,"request":fixture.request,"task":fixture.task,"child":child,"call":fixture.call,"outcome":outcome})).unwrap();
        let args = json!({"path":if corrupt=="arguments" {"wrong.txt"} else {"answer.txt"}});
        let input = fixture
            .store
            .artifacts()
            .put(&serde_json::to_vec(&json!({"name":"read_file","arguments":args})).unwrap())
            .unwrap();
        let environment = fixture.store.artifacts().put(b"{}").unwrap();
        let state = fixture.store.load(fixture.task).unwrap();
        let (state, job) = fixture
            .store
            .start_execution_job(
                fixture.task,
                state.revision,
                false,
                60000,
                JobInvocation {
                    session: fixture.session,
                    request: fixture.request,
                    call_id: None,
                    capability: "read_file".into(),
                    input,
                    environment,
                },
            )
            .unwrap();
        record_span(&mut fixture.store,fixture.session,fixture.request,json!({"version":1,"kind":"tool_dispatch","session":fixture.session,"request":fixture.request,"task":fixture.task,"child":child,"call":fixture.call,"tool_call":"tool","job":job,"generation":state.generation,"name":"read_file"})).unwrap();
        let mut receipt = json!({"version":1,"task":fixture.task,"job":job,"generation":state.generation,"session":fixture.session,"request":fixture.request,"subagent":child,"call":fixture.call,"tool_call":"tool","environment":environment,"status":"succeeded","tool_result":{"text":"recorded only"}});
        match corrupt {
            "job" | "call" => receipt[corrupt] = json!(Uuid::new_v4()),
            "child" => receipt["subagent"] = json!(Uuid::new_v4()),
            "status" => receipt["status"] = json!("failed"),
            _ => {}
        }
        let receipt = fixture
            .store
            .artifacts()
            .put(&serde_json::to_vec(&receipt).unwrap())
            .unwrap();
        fixture
            .store
            .settle_execution_job(fixture.task, job, JobStatus::Succeeded, receipt)
            .unwrap();
        if corrupt == "none" {
            let bundle = fixture.export().unwrap();
            let call = fixture.call;
            drop(fixture);
            let replay = bundle.replay().unwrap();
            assert!(matches!(
                replay.causality.tools[&job].result,
                RecordedStatus::Served
            ));
            assert_eq!(replay.causality.tools[&job].call, Some(call));
            assert!(
                replay.causality.calls[&call]
                    .gaps
                    .contains(&CausalGap::ChildOriginUnavailable)
            );
        } else {
            assert!(fixture.export().is_err(), "accepted inconsistent {corrupt}");
        }
    }
}

#[test]
fn provider_proposal_cannot_contradict_recorded_response_history() {
    let child = Uuid::new_v4();
    let mut fixture = Fixture::new(Some(child));
    fixture.dispatch();
    let mut outcome = fixture.proposal_outcome();
    let OutputItem::ToolProposal(proposal) = &mut outcome.response.as_mut().unwrap().output[0]
    else {
        panic!()
    };
    proposal.arguments = r#"{"path":"wrong.txt"}"#.into();
    record_span(&mut fixture.store,fixture.session,fixture.request,json!({"version":1,"kind":"model_response","session":fixture.session,"request":fixture.request,"task":fixture.task,"child":child,"call":fixture.call,"outcome":outcome})).unwrap();
    assert!(
        fixture
            .export()
            .unwrap_err()
            .to_string()
            .contains("history/proposals")
    );
}

#[test]
fn completed_receipt_cannot_serve_an_unavailable_response_as_success() {
    let mut fixture = Fixture::new(None);
    fixture.dispatch();
    let outcome = fixture.outcome(Transport::Http, ResponseDialect::OpenAi);
    let report = fixture
        .store
        .artifacts()
        .put(&serde_json::to_vec(&fixture.report(&outcome)).unwrap())
        .unwrap();
    fixture
        .store
        .record_model_call(
            fixture.task,
            fixture.call,
            ModelCallReceipt {
                status: ModelCallStatus::Completed,
                tokens: Some(0),
                report,
            },
        )
        .unwrap();
    assert!(fixture.export().unwrap_err().to_string().contains("status"));
}

#[test]
fn shared_dispatch_payloads_charge_each_offline_request_materialization() {
    let child = Uuid::new_v4();
    let mut fixture = Fixture::new(Some(child));
    fixture.inference = InferenceRequest::new(
        ModelSettings::default(),
        vec![json!({"role":"user","content":"x".repeat(128*1024)})],
        vec![],
        "instructions".into(),
        fixture.session.to_string(),
        100,
    )
    .unwrap();
    for _ in 0..8 {
        record_dispatch(
            &mut fixture.store,
            fixture.session,
            fixture.request,
            fixture.task,
            Some(child),
            Uuid::new_v4(),
            &fixture.inference,
        )
        .unwrap();
    }
    let mut bundle = fixture.export().unwrap();
    let stored = bundle
        .records
        .iter()
        .map(|record| decoded_len(&record.event_base64).unwrap())
        .sum::<u64>()
        + bundle
            .artifacts
            .values()
            .filter_map(|payload| match payload {
                Payload::Present(encoded) => Some(decoded_len(encoded).unwrap()),
                _ => None,
            })
            .sum::<u64>();
    bundle.limits.bytes = 1024 * 1024;
    assert!(stored < bundle.limits.bytes);
    assert!(
        bundle
            .replay()
            .unwrap_err()
            .to_string()
            .contains("materialization")
    );
}

#[test]
fn child_followup_must_consume_the_recorded_provider_prefix() {
    let child = Uuid::new_v4();
    let mut fixture = Fixture::new(Some(child));
    fixture.dispatch();
    let outcome = fixture.proposal_outcome();
    record_span(&mut fixture.store,fixture.session,fixture.request,json!({"version":1,"kind":"model_response","session":fixture.session,"request":fixture.request,"task":fixture.task,"child":child,"call":fixture.call,"outcome":outcome})).unwrap();
    record_dispatch(
        &mut fixture.store,
        fixture.session,
        fixture.request,
        fixture.task,
        Some(child),
        Uuid::new_v4(),
        &fixture.inference,
    )
    .unwrap();
    assert!(
        fixture
            .export()
            .unwrap_err()
            .to_string()
            .contains("child dispatch")
    );
}

#[test]
fn fencing_does_not_rewrite_an_unknown_tool_settlement_as_success() {
    let mut fixture = Fixture::new(None);
    let input = fixture.store.artifacts().put(b"{}").unwrap();
    let state = fixture.store.load(fixture.task).unwrap();
    let (_, job) = fixture
        .store
        .start_execution_job(
            fixture.task,
            state.revision,
            false,
            60000,
            JobInvocation {
                session: fixture.session,
                request: fixture.request,
                call_id: None,
                capability: "read_file".into(),
                input,
                environment: input,
            },
        )
        .unwrap();
    let receipt = fixture.store.artifacts().put(&serde_json::to_vec(&json!({"task":fixture.task,"job":job,"session":fixture.session,"request":fixture.request,"status":"unknown","tool_result":{"error":"unknown execution"}})).unwrap()).unwrap();
    fixture
        .store
        .settle_execution_job(fixture.task, job, JobStatus::Unknown, receipt)
        .unwrap();
    fixture.store.fence_job(fixture.task, job, input).unwrap();
    let replay = fixture.export().unwrap().replay().unwrap();
    let tool = &replay.causality.tools[&job];
    assert_eq!(tool.status, JobStatus::Fenced);
    assert_eq!(tool.settlement_status, Some(JobStatus::Unknown));
    assert!(!replay.causality.complete);
}

#[test]
fn recorded_responses_cannot_cross_parent_child_or_session_boundaries() {
    let mut accepted_foreign = Vec::new();
    for boundary in ["same", "child", "session"] {
        let child = (boundary == "child").then(Uuid::new_v4);
        let mut fixture = Fixture::new(child);
        fixture.dispatch();
        let outcome = fixture.proposal_outcome();
        let items = outcome.response.as_ref().unwrap().history_items.clone();
        record_span(
            &mut fixture.store,
            fixture.session,
            fixture.request,
            json!({"version":1,"kind":"model_response","session":fixture.session,
                "request":fixture.request,"task":fixture.task,"child":child,
                "call":fixture.call,"outcome":outcome}),
        )
        .unwrap();
        let (session, request) = if boundary == "session" {
            let session = SessionId::new();
            let config = fixture.store.load_session(fixture.session).unwrap().config;
            fixture.store.create_session(session, config, None).unwrap();
            let request = Uuid::new_v4();
            let policy = fixture.store.load(fixture.task).unwrap().intake.unwrap();
            fixture
                .store
                .start_request(
                    session,
                    request,
                    "another request".into(),
                    Default::default(),
                    policy,
                )
                .unwrap();
            (session, request)
        } else {
            (fixture.session, fixture.request)
        };
        let revision = fixture.store.load_session(session).unwrap().revision;
        fixture
            .store
            .session_command(
                session,
                revision,
                Uuid::new_v5(&fixture.call, b"response"),
                SessionCommand::Response { request, items },
            )
            .unwrap();
        let result = fixture.export();
        if boundary == "same" {
            assert!(
                result.is_ok(),
                "same parent response should remain replayable: {result:?}"
            );
        } else if result.is_ok() {
            accepted_foreign.push(boundary);
        }
    }
    assert!(
        accepted_foreign.is_empty(),
        "accepted foreign responses as parent history: {accepted_foreign:?}"
    );
}

// Imported bundles can be rewritten wholesale. Recompute the envelope and journal
// hashes without calling write/export, so rejection must come from read/replay.
fn read_import(bundle: &TraceBundle) -> Result<TraceBundle, TraceError> {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("import.json");
    let envelope = json!({"digest": Digest::of_value(bundle).unwrap(), "bundle": bundle});
    fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    TraceBundle::read(&path)
}

fn assert_invalid_import(bundle: &TraceBundle, expected: &str) {
    for error in [
        bundle.replay().unwrap_err(),
        read_import(bundle).unwrap_err(),
    ] {
        assert!(
            matches!(&error, TraceError::Invalid(message) if message == expected),
            "expected {expected:?}, got {error:?}"
        );
    }
}

fn append_import_event(
    bundle: &mut TraceBundle,
    kind: &str,
    aggregate: Uuid,
    event: &impl Serialize,
) {
    let previous = bundle
        .records
        .iter()
        .rev()
        .find(|record| record.kind == kind && record.aggregate == aggregate);
    let revision = previous.map_or(1, |record| record.revision + 1);
    let bytes = serde_json::to_vec(event).unwrap();
    let hash = crate::store::aggregate_hash(
        kind,
        aggregate,
        revision,
        previous.map(|record| record.hash),
        &bytes,
    )
    .unwrap();
    bundle.through += 1;
    bundle.records.push(TraceRecord {
        sequence: bundle.through,
        aggregate,
        kind: kind.into(),
        revision,
        event_base64: STANDARD.encode(bytes),
        hash,
    });
}

fn import_artifact(bundle: &mut TraceBundle, value: &Value) -> Digest {
    let bytes = serde_json::to_vec(value).unwrap();
    let digest = Digest::of(&bytes);
    bundle
        .artifacts
        .insert(digest, Payload::Present(STANDARD.encode(bytes)));
    digest
}

fn append_import_span(bundle: &mut TraceBundle, fixture: &Fixture, span: &Value) {
    let digest = import_artifact(bundle, span);
    let operation = Uuid::new_v5(&fixture.session.0, &bundle.through.to_le_bytes());
    append_import_event(
        bundle,
        "session",
        fixture.session.0,
        &json!({"type":"command", "data":{
            "operation":operation,
            "command":SessionCommand::TraceRecorded { request: fixture.request, record: digest },
            "at_ms":0
        }}),
    );
}

#[test]
fn import_rejects_extreme_cursors_and_invalid_creation_order() {
    let fixture = Fixture::new(None);
    let bundle = fixture.export().unwrap();
    for sequence in [0, u64::MAX] {
        let mut forged = bundle.clone();
        forged.records[0].sequence = sequence;
        assert_invalid_import(&forged, "global journal cursor gap");
    }
    let mut forged = bundle.clone();
    forged.records[0].revision = u64::MAX;
    assert_invalid_import(&forged, "aggregate revision gap");
    for kind in ["session", "task"] {
        let creation = bundle
            .records
            .iter()
            .find(|record| record.kind == kind)
            .unwrap();
        let mut forged = bundle.clone();
        append_import_event(
            &mut forged,
            kind,
            creation.aggregate,
            &decode_json(&creation.event_base64).unwrap(),
        );
        assert_invalid_import(&forged, &format!("{kind} creation sequence"));
    }
    for (kind, event) in [
        (
            "session",
            json!({"type":"command","data":{
                "operation":Uuid::nil(),"command":SessionCommand::TraceRecorded {
                    request: fixture.request, record: Digest::of(b"absent")
                },"at_ms":0
            }}),
        ),
        ("task", json!(TaskEvent::CancellationRequested)),
    ] {
        let mut forged = bundle.clone();
        append_import_event(&mut forged, kind, Uuid::nil(), &event);
        assert_invalid_import(&forged, &format!("{kind} creation sequence"));
    }
}

#[test]
fn import_rejects_malformed_base64_and_event_shapes() {
    let fixture = Fixture::new(None);
    let bundle = fixture.export().unwrap();
    for encoded in ["A", "====", "AA=A"] {
        let mut forged = bundle.clone();
        forged.records[0].event_base64 = encoded.into();
        assert_invalid_import(&forged, "invalid base64 payload");
    }
    for event in [Value::Null, json!([]), json!({"type":"unknown"})] {
        let mut forged = bundle.clone();
        append_import_event(&mut forged, "session", fixture.session.0, &event);
        assert!(matches!(forged.replay(), Err(TraceError::Json(_))));
        assert!(matches!(read_import(&forged), Err(TraceError::Json(_))));
    }
}

#[test]
fn import_rejects_receipts_that_disagree_with_journal_links() {
    let fixture = Fixture::new(Some(Uuid::nil()));
    let bundle = fixture.export().unwrap();
    for kind in [
        "model_dispatch",
        "model_response",
        "child_terminal",
        "tool_dispatch",
    ] {
        for field in ["session", "request"] {
            let mut forged = bundle.clone();
            let mut span = fixture.dispatch.clone();
            span["kind"] = json!(kind);
            span["sequence"] = json!(u64::MAX);
            span[field] = json!(Uuid::nil());
            append_import_span(&mut forged, &fixture, &span);
            assert_invalid_import(
                &forged,
                &format!("recorded {field} disagrees with causal source"),
            );
        }
    }
}

#[test]
fn import_keeps_missing_child_dependencies_as_gaps() {
    let child = Uuid::nil();
    let mut fixture = Fixture::new(Some(child));
    fixture.dispatch();
    let next = Uuid::new_v5(&child, b"next");
    record_dispatch(
        &mut fixture.store,
        fixture.session,
        fixture.request,
        fixture.task,
        Some(child),
        next,
        &fixture.inference,
    )
    .unwrap();
    let bundle = fixture.export().unwrap();
    for missing in ["outcome", "input", "dispatch"] {
        let mut incomplete = bundle.clone();
        if missing != "outcome" {
            let digest = if missing == "input" {
                serde_json::from_value(fixture.dispatch["input"].clone()).unwrap()
            } else {
                Digest::of_value(&fixture.dispatch).unwrap()
            };
            assert!(incomplete.artifacts.remove(&digest).is_some());
            incomplete.exact = false;
        }
        for report in [
            incomplete.replay().unwrap(),
            read_import(&incomplete).unwrap().replay().unwrap(),
        ] {
            assert!(!report.causality.complete);
            let call = &report.causality.calls[&next];
            assert!(call.gaps.contains(&CausalGap::OutcomeMissing));
            if missing == "dispatch" {
                assert!(!report.causality.calls.contains_key(&fixture.call));
            } else {
                assert!(call.gaps.contains(&CausalGap::ChildContextUnavailable));
            }
        }
    }
}

#[test]
fn import_rejects_child_inputs_shorter_than_the_recorded_prefix_or_history() {
    let child = Uuid::nil();
    let mut fixture = Fixture::new(Some(child));
    fixture.dispatch();
    let outcome = fixture.proposal_outcome();
    record_span(
        &mut fixture.store,
        fixture.session,
        fixture.request,
        json!({"kind":"model_response","session":fixture.session,"request":fixture.request,
            "task":fixture.task,"child":child,"call":fixture.call,"outcome":outcome}),
    )
    .unwrap();
    let bundle = fixture.export().unwrap();
    for (input, error) in [
        (
            json!([]),
            "child dispatch dropped or changed its recorded input prefix",
        ),
        (
            fixture.inference.wire(Transport::Http)["input"].clone(),
            "child dispatch disagrees with recorded provider output",
        ),
    ] {
        let mut forged = bundle.clone();
        let mut dispatch = fixture.dispatch.clone();
        dispatch["call"] = json!(Uuid::new_v5(&child, b"next"));
        dispatch["input"] = json!(import_artifact(&mut forged, &input));
        // A missing template is a supported gap, not a reason to skip child continuity.
        dispatch["payload"] = json!(Digest::of(b"unavailable template"));
        append_import_span(&mut forged, &fixture, &dispatch);
        assert_invalid_import(&forged, error);
    }
}

#[test]
fn import_keeps_unavailable_context_sources_as_gaps() {
    let mut fixture = Fixture::new(None);
    fixture.dispatch();
    let outcome = fixture.outcome(Transport::Http, ResponseDialect::OpenAi);
    let mut report = fixture.report(&outcome);
    report["context"]["source"]["revision"] = json!(u64::MAX);
    fixture.record_outcome(report, Some(0));
    let bundle = fixture.export().unwrap();
    let imported = read_import(&bundle).unwrap().replay().unwrap();
    assert!(imported.exact);
    assert!(!imported.causality.complete);
    assert!(
        imported.causality.calls[&fixture.call]
            .gaps
            .contains(&CausalGap::ContextSourceUnavailable)
    );
}

#[test]
fn import_rejects_forged_context_ranges_before_slicing() {
    let mut fixture = Fixture::new(None);
    fixture.dispatch();
    let outcome = fixture.outcome(Transport::Http, ResponseDialect::OpenAi);
    let bundle = fixture.export().unwrap();
    for field in ["stable_input_items", "range", "input_range"] {
        let mut forged = bundle.clone();
        let mut report = fixture.report(&outcome);
        if field == "stable_input_items" {
            report["context"][field] = json!(usize::MAX);
        } else {
            report["context"]["segments"][0][field] = json!({"start":u64::MAX,"end":0});
        }
        let report = import_artifact(&mut forged, &report);
        append_import_event(
            &mut forged,
            "task",
            fixture.task.0,
            &TaskEvent::ModelCallRecorded {
                operation: fixture.call,
                receipt: ModelCallReceipt {
                    status: ModelCallStatus::Failed,
                    tokens: Some(0),
                    report,
                },
            },
        );
        assert_invalid_import(&forged, "report context disagrees with recorded source");
    }
}

#[test]
fn import_binds_tool_outputs_to_existing_proposals() {
    let mut fixture = Fixture::new(None);
    fixture.dispatch();
    let outcome = fixture.proposal_outcome();
    record_span(
        &mut fixture.store,
        fixture.session,
        fixture.request,
        json!({"kind":"model_response","session":fixture.session,"request":fixture.request,
            "task":fixture.task,"child":null,"call":fixture.call,"outcome":outcome}),
    )
    .unwrap();
    let state = fixture.store.load_session(fixture.session).unwrap();
    fixture
        .store
        .session_command(
            fixture.session,
            state.revision,
            Uuid::new_v5(&fixture.call, b"response"),
            SessionCommand::Response {
                request: fixture.request,
                items: outcome.response.unwrap().history_items,
            },
        )
        .unwrap();
    let prefix = fixture.export().unwrap();
    let mut forged = prefix.clone();
    append_import_event(
        &mut forged,
        "session",
        fixture.session.0,
        &SessionEvent::Command {
            operation: Uuid::nil(),
            command: SessionCommand::ToolResult {
                request: Uuid::nil(),
                call_id: "tool".into(),
                output: "recorded output".into(),
            },
            at_ms: 0,
        },
    );
    assert_invalid_import(
        &forged,
        "tool output request disagrees with provider proposal",
    );

    let state = fixture.store.load_session(fixture.session).unwrap();
    fixture
        .store
        .session_command(
            fixture.session,
            state.revision,
            Uuid::nil(),
            SessionCommand::ToolResult {
                request: fixture.request,
                call_id: "tool".into(),
                output: "recorded output".into(),
            },
        )
        .unwrap();
    let bundle = fixture.export().unwrap();
    let imported = read_import(&bundle).unwrap().replay().unwrap();
    assert_eq!(
        imported.causality.calls[&fixture.call].tool_outputs["tool"],
        Digest::of(b"recorded output")
    );

    let mut missing = bundle;
    let response = prefix
        .artifacts
        .iter()
        .find_map(|(digest, payload)| {
            let Payload::Present(encoded) = payload else {
                return None;
            };
            let value = decode_json(encoded).ok()?;
            (value["kind"] == "model_response").then_some(*digest)
        })
        .unwrap();
    missing.artifacts.remove(&response);
    missing.exact = false;
    let imported = read_import(&missing).unwrap().replay().unwrap();
    assert!(
        imported
            .causality
            .gaps
            .contains(&CausalGap::ToolLinkUnavailable)
    );
}

#[test]
fn import_keeps_materialized_context_as_an_explicit_gap() {
    let mut fixture = Fixture::new(None);
    let state = fixture.store.load_session(fixture.session).unwrap();
    let state = fixture
        .store
        .session_command(
            fixture.session,
            state.revision,
            Uuid::new_v5(&fixture.request, b"settled"),
            SessionCommand::TurnSettled {
                request: fixture.request,
                outcome: None,
                error: None,
            },
        )
        .unwrap();
    fixture.request = Uuid::nil();
    let state = fixture
        .store
        .session_command(
            fixture.session,
            state.revision,
            fixture.request,
            SessionCommand::Input {
                kind: RequestKind::Task,
                content: vec![json!({"role":"user","content":[{"type":"tact_image"}]})],
            },
        )
        .unwrap();
    let state = fixture
        .store
        .session_command(
            fixture.session,
            state.revision,
            Uuid::new_v5(&fixture.request, b"task"),
            SessionCommand::TaskLinked {
                request: fixture.request,
                task: fixture.task,
            },
        )
        .unwrap();
    fixture.projection = context::project(&state, 1024 * 1024).unwrap();
    let mut input = fixture.projection.input.clone();
    input.last_mut().unwrap()["content"] =
        json!([{"type":"input_image","image_url":"data:image/png;base64,AA=="}]);
    let live = input.split_off(fixture.projection.manifest.stable_input_items);
    let segments = fixture
        .projection
        .manifest
        .segments
        .iter()
        .filter(|segment| segment.role == ContextSegmentRole::StableHistory)
        .map(|segment| segment.input)
        .collect();
    fixture.inference = InferenceRequest::new_segmented(
        ModelSettings::default(),
        PromptInput::segmented(input, live, segments).unwrap(),
        vec![],
        "recorded instructions".into(),
        fixture.session.to_string(),
        100,
    )
    .unwrap();
    let wire = fixture.inference.wire(Transport::Http);
    fixture.replace_artifact("input", &wire["input"]);
    fixture.replace_artifact("tools", &wire["tools"]);
    fixture.replace_artifact("payload", &wire);
    fixture.dispatch["cache"] = json!(fixture.inference.cache_identity());
    fixture.dispatch["request"] = json!(fixture.request);
    fixture.dispatch();
    let outcome = fixture.outcome(Transport::Http, ResponseDialect::OpenAi);
    fixture.record_outcome(fixture.report(&outcome), Some(0));
    let bundle = fixture.export().unwrap();
    let imported = read_import(&bundle).unwrap().replay().unwrap();
    assert!(imported.exact);
    assert!(
        imported.causality.calls[&fixture.call]
            .gaps
            .contains(&CausalGap::ContextMaterializationUnavailable)
    );
}
