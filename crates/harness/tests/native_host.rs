//! Native primary-mode tests: a scripted provider drives tasks whose primary
//! tools run directly on this machine, without Docker, snapshots or verified
//! completion. Isolated-mode behavior stays covered by the Docker suites.

use base64::Engine;
use orvek_harness::{
    Channel, Digest, Store,
    admission::{RepositoryProfile, RequestPolicy},
    contract::{DeliveryKind, Limits},
    controller::Host,
    inference::{
        Limits as InferenceLimits, ModelSettings, ResponsesClient, Route, Transport, UsdCost,
        auth::{Auth, SecretString},
    },
    input,
    session::{SessionAdmissionRequest, SessionCommand, SessionEvent, SessionId},
    state::{JobInvocation, JobStatus, Outcome},
    submission::{Schedule, SubmitIntent, WorkIntent},
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    time::timeout,
};
use uuid::Uuid;

enum ProviderReply {
    Response(Vec<Value>),
    Rejected(u16),
    Stall(oneshot::Sender<()>),
}

async fn scripted_provider(
    replies: Vec<ProviderReply>,
) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        let mut stalled = Vec::new();
        for reply in replies {
            let (mut socket, _) = timeout(Duration::from_secs(20), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                headers.push(byte[0]);
            }
            let headers = String::from_utf8(headers).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|n| n.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            let mut bytes = vec![0; length];
            socket.read_exact(&mut bytes).await.unwrap();
            requests.push(serde_json::from_slice(&bytes).unwrap());

            match reply {
                ProviderReply::Response(output) => {
                    let event = json!({"type":"response.completed","response":{"id":format!("resp_{}", requests.len()),"status":"completed","output":output,"usage":{"input_tokens":5,"output_tokens":1,"total_tokens":6}}});
                    let payload = format!("event: response.completed\ndata: {event}\n\n");
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nX-LiteLLM-Response-Cost: 0.0001\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                }
                ProviderReply::Rejected(status) => {
                    socket
                        .write_all(
                            format!(
                                "HTTP/1.1 {status} Rejected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            )
                            .as_bytes(),
                        )
                        .await
                        .unwrap();
                }
                ProviderReply::Stall(started) => {
                    started.send(()).unwrap();
                    stalled.push(tokio::spawn(async move {
                        let mut byte = [0];
                        while socket.read(&mut byte).await.unwrap_or(0) != 0 {}
                    }));
                }
            }
        }
        for connection in stalled {
            connection.await.unwrap();
        }
        requests
    });
    (endpoint, task)
}

async fn provider(outputs: Vec<Vec<Value>>) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    scripted_provider(outputs.into_iter().map(ProviderReply::Response).collect()).await
}

fn final_message(id: &str) -> Value {
    json!({"type":"message","id":id,"role":"assistant","status":"completed","content":[{"type":"output_text","text":"Done","annotations":[]}]})
}

fn information_classification() -> Vec<Value> {
    vec![
        json!({"type":"message","id":"msg_classify","role":"assistant","status":"completed","content":[{"type":"output_text","text":"{\"kind\":\"information\"}","annotations":[]}]}),
    ]
}

fn function_call(id: &str, call_id: &str, name: &str, arguments: Value) -> Value {
    json!({"type":"function_call","id":id,"call_id":call_id,"name":name,"arguments":serde_json::to_string(&arguments).unwrap(),"status":"completed"})
}

fn client(endpoint: &str) -> ResponsesClient {
    ResponsesClient::new(
        Auth::api_key(SecretString::new("fixture-key".into())).unwrap(),
        Route::new(Transport::Http, endpoint).unwrap(),
        InferenceLimits {
            max_attempts: 1,
            ..InferenceLimits::default()
        },
    )
    .unwrap()
}

fn policy() -> RequestPolicy {
    RequestPolicy {
        version: 1,
        delivery: DeliveryKind::Source,
        profile: RepositoryProfile {
            version: 1,
            name: "fixture".into(),
            checks: Default::default(),
        },
    }
}

fn new_task_intent() -> SubmitIntent {
    SubmitIntent::NewTask {
        limits: Limits::default(),
        policy: policy(),
    }
}

async fn wait_submission(
    host: &Host,
    session: SessionId,
    request: Uuid,
) -> orvek_harness::controller::TaskRun {
    timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(submission) = host.submission(session, request).await {
                match submission.status {
                    orvek_harness::submission::SubmissionStatus::Finished {
                        task: Some(task),
                        error,
                        ..
                    } => {
                        return orvek_harness::controller::TaskRun {
                            session,
                            task: host.task(task).await.unwrap(),
                            message: error.unwrap_or_default(),
                        };
                    }
                    orvek_harness::submission::SubmissionStatus::Queued
                    | orvek_harness::submission::SubmissionStatus::Running => {}
                    state => panic!("submission did not produce a task: {state:?}"),
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

async fn artifact_bytes(host: &Host, digest: Digest) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let chunk = host
            .read_artifact(digest, bytes.len(), 64 * 1024)
            .await
            .unwrap();
        bytes.extend(
            base64::engine::general_purpose::STANDARD
                .decode(chunk["data"].as_str().unwrap())
                .unwrap(),
        );
        if chunk["next"].is_null() {
            return bytes;
        }
    }
}

/// Recorded tool result for one model call, read back from the session
/// history's `function_call_output` entries.
async fn tool_output(host: &Host, session: SessionId, call_id: &str) -> Value {
    let state = host.session(session).await.unwrap();
    let entry = state
        .history
        .iter()
        .rev()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == *call_id)
        .unwrap();
    serde_json::from_str(entry["output"].as_str().unwrap()).unwrap()
}

async fn provider_costs(host: &Host) -> Vec<Option<UsdCost>> {
    host.journal_page(0, 256)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|record| serde_json::from_value::<SessionEvent>(record.event).ok())
        .filter_map(|event| match event {
            SessionEvent::Command {
                command: SessionCommand::ProviderCost { cost_usd, .. },
                ..
            } => Some(cost_usd),
            _ => None,
        })
        .collect()
}

struct Fixture {
    directory: tempfile::TempDir,
    source: PathBuf,
    outside: PathBuf,
}

impl Fixture {
    /// A real workspace directory on this machine plus a second directory
    /// outside it. No Docker daemon is ever contacted.
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let outside = directory.path().join("outside");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(source.join("note"), "before").unwrap();
        Self {
            directory,
            source: source.canonicalize().unwrap(),
            outside: outside.canonicalize().unwrap(),
        }
    }

    fn state_root(&self) -> PathBuf {
        self.directory.path().join("state")
    }

    async fn open_host(&self, endpoint: &str) -> Arc<Host> {
        Arc::new(
            Host::open_native(&self.state_root(), client(endpoint), Digest::of(b"config")).unwrap(),
        )
    }

    async fn admit_session(&self, host: &Host) -> SessionId {
        host.create_session(SessionAdmissionRequest::new(
            self.directory.path().join("source"),
            ModelSettings::default(),
            orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            Channel::Stable,
        ))
        .await
        .unwrap()
        .id
    }
}

#[tokio::test]
async fn native_primary_tools_edit_the_live_workspace_and_prose_finishes_unverified() {
    let fixture = Fixture::new();
    let outside = fixture.outside.display().to_string();
    let outputs = vec![
        vec![
            function_call(
                "fc_exec",
                "call_exec",
                "exec_command",
                json!({"command": format!("printf host-exec > exec.txt && printf outside > '{outside}/outside.txt'")}),
            ),
            function_call(
                "fc_write",
                "call_write",
                "write_file",
                json!({"operation":"replace","path":format!("{outside}/native-abs.txt"),"expected":{"kind":"absent"},"content":"absolute write"}),
            ),
            function_call("fc_read", "call_read", "read_file", json!({"path":"note"})),
            function_call(
                "fc_read_outside",
                "call_read_outside",
                "read_file",
                json!({"path":format!("{outside}/outside.txt")}),
            ),
        ],
        vec![final_message("msg_done")],
    ];
    let (endpoint, server) = provider(outputs).await;
    let host = fixture.open_host(&endpoint).await;
    let session = fixture.admit_session(&host).await;
    let request = Uuid::new_v4();
    host.submit(
        session,
        request,
        vec![json!({"type":"input_text","text":"Fix the note"})],
        new_task_intent(),
    )
    .await
    .unwrap();
    let run = wait_submission(&host, session, request).await;

    assert_eq!(run.task.outcome, Some(Outcome::FinishedUnverified));
    assert_eq!(run.task.certificates, Vec::new());
    assert_eq!(run.task.delivery, None);
    assert_eq!(run.task.baseline, None);
    assert_eq!(run.task.candidate, None);

    // Every artifact landed in the live directories, not a sandbox copy.
    assert_eq!(
        fs::read_to_string(fixture.source.join("exec.txt")).unwrap(),
        "host-exec"
    );
    assert_eq!(
        fs::read_to_string(fixture.outside.join("outside.txt")).unwrap(),
        "outside"
    );
    assert_eq!(
        fs::read_to_string(fixture.outside.join("native-abs.txt")).unwrap(),
        "absolute write"
    );
    assert_eq!(
        fs::read_to_string(fixture.source.join("note")).unwrap(),
        "before"
    );

    // Tool envelopes report the native backend and the real cwd.
    let read = tool_output(&host, session, "call_read").await;
    assert_eq!(read["backend"], "native_host");
    assert_eq!(
        read["cwd"].as_str().unwrap(),
        fixture.source.to_str().unwrap()
    );
    assert_eq!(read["result"]["content"]["data"], "before");
    let outside_read = tool_output(&host, session, "call_read_outside").await;
    assert_eq!(outside_read["result"]["content"]["data"], "outside");

    // Nothing was materialized or snapshotted under the host state root.
    assert!(
        !host
            .state_directory()
            .join("workspaces")
            .try_exists()
            .unwrap()
    );
    let costs = provider_costs(&host).await;
    assert_eq!(costs, vec![Some("0.0001".parse().unwrap()); 2]);
    server.await.unwrap();
}

#[tokio::test]
async fn native_sloppiness_tool_analyzes_the_live_workspace_without_a_baseline() {
    let fixture = Fixture::new();
    fs::write(
        fixture.source.join("lib.rs"),
        "fn redundant(value: bool) -> bool { if value { true } else { false } }\n",
    )
    .unwrap();
    let outputs = vec![
        vec![function_call(
            "fc_sloppiness",
            "call_sloppiness",
            "measure_sloppiness",
            json!({}),
        )],
        vec![final_message("msg_done")],
    ];
    let (endpoint, server) = provider(outputs).await;
    let host = fixture.open_host(&endpoint).await;
    fs::write(
        fixture.state_root().join("not-workspace.rs"),
        "fn must_not_be_measured() {}\n",
    )
    .unwrap();
    let session = fixture.admit_session(&host).await;
    let request = Uuid::new_v4();
    host.submit(
        session,
        request,
        vec![json!({"type":"input_text","text":"Inspect the source"})],
        new_task_intent(),
    )
    .await
    .unwrap();

    let run = wait_submission(&host, session, request).await;
    assert_eq!(run.task.outcome, Some(Outcome::FinishedUnverified));
    let report = tool_output(&host, session, "call_sloppiness").await;
    assert_eq!(report["current"]["version"], 2);
    assert_eq!(report["current"]["language"], "rust");
    assert_eq!(report["current"]["languages"]["rust"], 1);
    assert_eq!(report["current"]["files"], 1);
    assert_eq!(report["current"]["erosion"]["functions"], 1);
    assert!(
        report["current"]["verbosity"]["ast_flagged_lines"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(report.get("baseline").is_none());
    assert!(report.get("delta").is_none());

    let requests = server.await.unwrap();
    assert!(
        requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "measure_sloppiness")
    );
}

#[tokio::test]
async fn native_task_retries_a_pre_generation_authentication_rejection() {
    let fixture = Fixture::new();
    let (endpoint, server) = scripted_provider(vec![
        ProviderReply::Rejected(401),
        ProviderReply::Response(vec![final_message("msg_recovered")]),
    ])
    .await;
    let host = fixture.open_host(&endpoint).await;
    let session = fixture.admit_session(&host).await;
    let request = Uuid::new_v4();
    host.submit(
        session,
        request,
        vec![json!({"type":"input_text","text":"Recover this turn"})],
        new_task_intent(),
    )
    .await
    .unwrap();
    let run = wait_submission(&host, session, request).await;

    assert_eq!(run.task.outcome, Some(Outcome::FinishedUnverified));
    assert_eq!(run.task.model_receipts.len(), 2);
    assert!(run.task.model_receipts.values().any(|receipt| {
        receipt.status == orvek_harness::state::ModelCallStatus::Failed && receipt.tokens == Some(0)
    }));
    let bundle = orvek_harness::trace::TraceBundle::export(
        host.state_directory(),
        None,
        Default::default(),
        &Default::default(),
        None,
    )
    .unwrap();
    let replay = bundle.replay().unwrap();
    assert!(replay.exact, "{:?}", replay.unresolved);
    assert_eq!(replay.tasks[&run.task.id], run.task);
    assert_eq!(
        replay.tasks[&run.task.id].outcome,
        Some(Outcome::FinishedUnverified)
    );
    assert!(replay.tasks[&run.task.id].certificates.is_empty());
    let prefixes = bundle
        .prefixes()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(prefixes.len(), 2);
    assert!(replay.causality.complete, "{:?}", replay.causality);
    assert!(
        replay
            .causality
            .calls
            .values()
            .all(|call| call.prepared_body_checked)
    );
    for prefix in prefixes {
        assert_eq!(prefix.decision["payload_kind"], "logical_http_template");
        assert_eq!(prefix.decision["wire"]["status"], "unavailable");
        assert!(prefix.decision["logical_request_payload"]["input"].is_array());
        assert!(prefix.decision.get("request_payload").is_none());
    }
    assert_eq!(
        replay
            .spans
            .iter()
            .filter(|span| span["span"]["kind"] == "model_dispatch")
            .count(),
        2
    );
    let captures = server.await.unwrap();
    assert_eq!(captures.len(), 2);
    let dispatches = replay
        .spans
        .iter()
        .filter(|span| span["span"]["kind"] == "model_dispatch")
        .collect::<Vec<_>>();
    assert_ne!(dispatches[0]["span"]["call"], dispatches[1]["span"]["call"]);
    // A prefix ending after dispatch intent models a crash before outcome persistence.
    let interrupted = orvek_harness::trace::TraceBundle::export(
        host.state_directory(),
        Some(dispatches[0]["sequence"].as_u64().unwrap()),
        Default::default(),
        &Default::default(),
        None,
    )
    .unwrap();
    let interrupted = interrupted.replay().unwrap();
    assert!(interrupted.exact);
    assert!(!interrupted.causality.complete);
    assert!(interrupted.causality.calls.values().any(|call| {
        call.gaps
            .contains(&orvek_harness::trace::CausalGap::OutcomeMissing)
    }));
    assert!(
        !interrupted
            .spans
            .iter()
            .any(|span| span["span"]["kind"] == "model_response")
    );
    let intent = interrupted
        .spans
        .iter()
        .find(|span| span["span"]["kind"] == "model_dispatch")
        .unwrap();
    assert_eq!(
        intent["span"]["wire"],
        json!({"status":"unavailable","reason":"outcome_not_recorded"})
    );
    for (dispatch, captured) in dispatches.iter().zip(captures) {
        assert_eq!(dispatch["span"]["payload_kind"], "logical_http_template");
        assert_eq!(dispatch["span"]["wire"]["status"], "unavailable");
        let call: Uuid = serde_json::from_value(dispatch["span"]["call"].clone()).unwrap();
        let receipt = &run.task.model_receipts[&call];
        let report: Value =
            serde_json::from_slice(&artifact_bytes(&host, receipt.report).await).unwrap();
        let prepared = &report["outcome"]["request"];
        assert_eq!(prepared["transport"], "http");
        assert_eq!(prepared["dialect"], "open_ai");
        assert_eq!(
            serde_json::from_str::<Value>(prepared["body"].as_str().unwrap()).unwrap(),
            captured
        );
    }
}

#[tokio::test]
async fn ordinary_classification_and_answer_recover_from_authentication_rejections() {
    let fixture = Fixture::new();
    let (endpoint, server) = scripted_provider(vec![
        ProviderReply::Rejected(401),
        ProviderReply::Response(information_classification()),
        ProviderReply::Rejected(401),
        ProviderReply::Response(vec![final_message("msg_recovered_answer")]),
    ])
    .await;
    let host = fixture.open_host(&endpoint).await;
    let session = fixture.admit_session(&host).await;
    let request = Uuid::new_v4();
    host.submit(
        session,
        request,
        vec![json!({"type":"input_text","text":"Explain this"})],
        SubmitIntent::Ordinary {
            limits: Limits::default(),
            policy: policy(),
            schedule: Schedule::Queue,
        },
    )
    .await
    .unwrap();

    let settled = timeout(Duration::from_secs(10), async {
        loop {
            let submission = host.submission(session, request).await.unwrap();
            if !submission.status.pending() {
                break submission;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(
            settled.status,
            orvek_harness::submission::SubmissionStatus::Finished {
                task: None,
                outcome: None,
                error: None,
            }
        ),
        "unexpected settled status: {:?}",
        settled.status
    );
    let costs = provider_costs(&host).await;
    assert_eq!(costs.len(), 4);
    assert_eq!(
        costs
            .iter()
            .filter(|cost| **cost == Some(UsdCost::ZERO))
            .count(),
        2
    );
    assert_eq!(
        costs
            .into_iter()
            .flatten()
            .try_fold(UsdCost::ZERO, UsdCost::checked_add)
            .unwrap()
            .to_string(),
        "$0.0002"
    );
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 4);
    for classifier in &requests[..2] {
        assert_eq!(classifier["text"]["format"]["type"], "json_schema");
        assert_eq!(classifier["text"]["format"]["strict"], true);
        assert_eq!(
            classifier["text"]["format"]["schema"]["additionalProperties"],
            false
        );
    }
    for answer in &requests[2..] {
        assert!(answer["text"].get("format").is_none());
    }
}

#[tokio::test]
async fn ordinary_information_answer_can_be_cancelled_and_followed_by_new_input() {
    let fixture = Fixture::new();
    let (started, answer_started) = oneshot::channel();
    let (endpoint, server) = scripted_provider(vec![
        ProviderReply::Response(information_classification()),
        ProviderReply::Stall(started),
        ProviderReply::Response(vec![final_message("msg_after_cancel")]),
    ])
    .await;
    let host = fixture.open_host(&endpoint).await;
    let session = fixture.admit_session(&host).await;
    let request = Uuid::new_v4();
    host.submit(
        session,
        request,
        vec![json!({"type":"input_text","text":"Explain the current state"})],
        SubmitIntent::Ordinary {
            limits: Limits::default(),
            policy: policy(),
            schedule: Schedule::Queue,
        },
    )
    .await
    .unwrap();

    answer_started.await.unwrap();
    assert!(host.cancel(session).await);
    timeout(Duration::from_secs(10), async {
        loop {
            let submission = host.submission(session, request).await.unwrap();
            if !submission.status.pending() {
                assert!(matches!(
                    submission.status,
                    orvek_harness::submission::SubmissionStatus::Finished {
                        task: None,
                        outcome: None,
                        ..
                    }
                ));
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let followup = Uuid::new_v4();
    host.submit(
        session,
        followup,
        vec![json!({"type":"input_text","text":"Start fresh work"})],
        new_task_intent(),
    )
    .await
    .unwrap();
    let continued = wait_submission(&host, session, followup).await;
    assert_eq!(continued.task.outcome, Some(Outcome::FinishedUnverified));
    assert_eq!(server.await.unwrap().len(), 3);
}

#[tokio::test]
async fn cancelled_provider_turn_can_continue_on_the_same_native_task() {
    let fixture = Fixture::new();
    let (started, answer_started) = oneshot::channel();
    let (endpoint, server) = scripted_provider(vec![
        ProviderReply::Stall(started),
        ProviderReply::Response(vec![final_message("msg_resumed")]),
    ])
    .await;
    let host = fixture.open_host(&endpoint).await;
    let session = fixture.admit_session(&host).await;
    let request = Uuid::new_v4();
    host.submit(
        session,
        request,
        vec![json!({"type":"input_text","text":"Start interruptible work"})],
        new_task_intent(),
    )
    .await
    .unwrap();

    answer_started.await.unwrap();
    assert!(host.cancel(session).await);
    let cancelled = wait_submission(&host, session, request).await;
    assert_eq!(cancelled.task.outcome, Some(Outcome::Cancelled));
    assert!(
        cancelled
            .task
            .model_receipts
            .values()
            .any(
                |receipt| receipt.status == orvek_harness::state::ModelCallStatus::Cancelled
                    && receipt.tokens.is_none()
            )
    );

    let followup = Uuid::new_v4();
    host.submit(
        session,
        followup,
        vec![json!({"type":"input_text","text":"Continue after cancellation"})],
        SubmitIntent::Continue {
            task: cancelled.task.id,
            scope_revision: cancelled.task.scope_revision,
            schedule: Schedule::Queue,
        },
    )
    .await
    .unwrap();
    let continued = wait_submission(&host, session, followup).await;

    assert_eq!(continued.task.id, cancelled.task.id);
    assert_eq!(continued.task.outcome, Some(Outcome::FinishedUnverified));
    assert_eq!(server.await.unwrap().len(), 2);
}

#[tokio::test]
async fn native_primary_work_ignores_verification_budgets_and_evidence_invalidation() {
    let fixture = Fixture::new();
    let outputs = vec![
        vec![function_call(
            "fc_exec",
            "call_exec",
            "exec_command",
            json!({"command":"sleep 0.02; printf finished > long-running.txt"}),
        )],
        vec![final_message("msg_done")],
        vec![final_message("msg_followup_done")],
    ];
    let (endpoint, server) = provider(outputs).await;
    let host = fixture.open_host(&endpoint).await;
    let session = fixture.admit_session(&host).await;
    let request = Uuid::new_v4();
    host.submit(
        session,
        request,
        vec![json!({"type":"input_text","text":"Complete the native work"})],
        SubmitIntent::NewTask {
            limits: Limits {
                model_calls: 1,
                tokens: 1,
                elapsed_ms: 1,
                ..Limits::default()
            },
            policy: policy(),
        },
    )
    .await
    .unwrap();
    let run = wait_submission(&host, session, request).await;

    assert_eq!(run.task.outcome, Some(Outcome::FinishedUnverified));
    assert!(run.task.usage.model_calls > run.task.limits().model_calls);
    assert!(run.task.usage.tokens > run.task.limits().tokens);
    assert_eq!(run.task.generation, 1);
    assert_eq!(
        fs::read_to_string(fixture.source.join("long-running.txt")).unwrap(),
        "finished"
    );

    let followup = Uuid::new_v4();
    host.submit(
        session,
        followup,
        vec![json!({"type":"input_text","text":"Continue after the limits are exhausted"})],
        SubmitIntent::Continue {
            task: run.task.id,
            scope_revision: run.task.scope_revision,
            schedule: Schedule::Queue,
        },
    )
    .await
    .unwrap();
    let continued = wait_submission(&host, session, followup).await;
    assert_eq!(continued.task.id, run.task.id);
    assert_eq!(continued.task.outcome, Some(Outcome::FinishedUnverified));
    assert!(continued.task.usage.model_calls > run.task.usage.model_calls);
    server.await.unwrap();
}

#[tokio::test]
async fn native_receipts_record_host_backend_cwd_and_command_metadata() {
    let fixture = Fixture::new();
    let outputs = vec![
        vec![
            function_call(
                "fc_ok",
                "call_ok",
                "exec_command",
                json!({"command":"true"}),
            ),
            function_call(
                "fc_fail",
                "call_fail",
                "exec_command",
                json!({"command":"exit 3"}),
            ),
        ],
        vec![function_call(
            "fc_finish",
            "call_finish",
            "propose_completion",
            json!({}),
        )],
    ];
    let (endpoint, _server) = provider(outputs).await;
    let host = fixture.open_host(&endpoint).await;
    let session = fixture.admit_session(&host).await;
    let request = Uuid::new_v4();
    host.submit(
        session,
        request,
        vec![json!({"type":"input_text","text":"Run the checks"})],
        new_task_intent(),
    )
    .await
    .unwrap();
    let run = wait_submission(&host, session, request).await;

    // The explicit finish is accepted without checks, certificates or delivery.
    assert_eq!(run.task.outcome, Some(Outcome::FinishedUnverified));
    assert_eq!(run.task.certificates, Vec::new());
    assert_eq!(run.task.delivery, None);
    let finish = tool_output(&host, session, "call_finish").await;
    assert_eq!(finish, json!({"accepted":true,"finished_unverified":true}));

    let mut receipts = Vec::new();
    for job in run.task.jobs.values() {
        let receipt: Value =
            serde_json::from_slice(&artifact_bytes(&host, job.execution_receipt.unwrap()).await)
                .unwrap();
        assert_eq!(receipt["backend"], "host");
        assert_eq!(receipt["execution"]["metadata"]["backend"], "native_host");
        assert_eq!(
            receipt["execution"]["metadata"]["cwd"].as_str().unwrap(),
            fixture.source.to_str().unwrap()
        );
        receipts.push((
            job.status,
            receipt["execution"]["metadata"]["command"]
                .as_str()
                .unwrap()
                .to_owned(),
            receipt["execution"]["status"].clone(),
        ));
    }
    receipts.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(
        receipts,
        vec![
            (
                JobStatus::Failed,
                "exit 3".to_owned(),
                json!({"kind":"exited","detail":3}),
            ),
            (
                JobStatus::Succeeded,
                "true".to_owned(),
                json!({"kind":"exited","detail":0}),
            ),
        ]
    );
}

/// A native job whose outcome was never confirmed stays unknown across a host
/// restart: reconciliation neither fences it through Docker nor replays it,
/// and the session still admits fresh work.
#[tokio::test]
async fn restart_keeps_unknown_native_jobs_unfenced_and_never_replays_them() {
    let fixture = Fixture::new();
    // Admit the session on a first native host, then simulate a crash that
    // happened while an exec job was running and never settled.
    let session = {
        let idle = fixture.open_host("http://127.0.0.1:1/v1/responses").await;
        fixture.admit_session(&idle).await
    };
    let (interrupted_task, continue_request) = {
        let mut store = Store::open(&fixture.state_root()).unwrap();
        let intake = store
            .public_artifacts()
            .write(&serde_json::to_vec(&policy()).unwrap())
            .unwrap()
            .digest();
        let request = Uuid::new_v4();
        let (_, task, created) = store
            .start_request(
                session,
                request,
                "Interrupted native work".into(),
                Limits::default(),
                intake,
            )
            .unwrap();
        assert!(created);
        let native_environment = store
            .public_artifacts()
            .write(
                &json!({"backend":"native_host","os":"fixture-os","arch":"fixture-arch"})
                    .to_string()
                    .into_bytes(),
            )
            .unwrap()
            .digest();
        let invocation = JobInvocation {
            session,
            request,
            call_id: None,
            capability: "exec_command".into(),
            input: intake,
            environment: native_environment,
        };
        store
            .start_execution_job(task.id, task.revision, false, 60_000, invocation)
            .unwrap();
        let continue_input = input::prepare(
            vec![json!({"type":"input_text","text":"continue the interrupted work"})],
            store.public_artifacts(),
        )
        .unwrap()
        .artifact;
        let continue_request = Uuid::new_v4();
        store
            .submit(
                session,
                continue_request,
                continue_input,
                WorkIntent::Continue {
                    task: task.id,
                    scope_revision: task.scope_revision,
                    schedule: Schedule::Queue,
                },
            )
            .unwrap();
        (task.id, continue_request)
    };

    // Restart: recovery marks the running job unknown without any fencing.
    let outputs = vec![vec![final_message("msg_fresh")]];
    let (endpoint, server) = provider(outputs).await;
    let host = fixture.open_host(&endpoint).await;
    host.start_queued().await.unwrap();
    timeout(Duration::from_secs(20), async {
        loop {
            let submission = host.submission(session, continue_request).await.unwrap();
            if let orvek_harness::submission::SubmissionStatus::Finished { outcome, error, .. } =
                &submission.status
            {
                assert_eq!(*outcome, None);
                assert!(
                    error.as_deref().unwrap_or_default().contains("reconcile unfinished jobs"),
                    "continuation must stop at the unresolved-job gate, not fence or replay: {submission:?}"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    // The unknown job survived untouched: still unknown, never fenced.
    let task = host.task(interrupted_task).await.unwrap();
    assert_eq!(task.outcome, Some(Outcome::Blocked));
    let job = task.jobs.values().next().unwrap();
    assert_eq!(job.status, JobStatus::Unknown);
    assert_eq!(job.fence_receipt, None);

    // Fresh work on the same session is unaffected by the unknown job.
    let request = Uuid::new_v4();
    host.submit(
        session,
        request,
        vec![json!({"type":"input_text","text":"Fresh native work"})],
        new_task_intent(),
    )
    .await
    .unwrap();
    let fresh = wait_submission(&host, session, request).await;
    assert_eq!(fresh.task.outcome, Some(Outcome::FinishedUnverified));
    server.await.unwrap();
}

#[tokio::test]
async fn native_sessions_admit_workspaces_that_contain_the_state_root() {
    // Launching from a home-style directory whose tree contains the state
    // root is ordinary native usage: no snapshot exists to contaminate.
    let fixture = Fixture::new();
    let (endpoint, server) = provider(vec![vec![final_message("msg_home")]]).await;
    let host = fixture.open_host(&endpoint).await;
    let session = host
        .create_session(SessionAdmissionRequest::new(
            fixture.directory.path().canonicalize().unwrap(),
            ModelSettings::default(),
            orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            Channel::Stable,
        ))
        .await
        .expect("ancestor workspace must be admitted on a native host")
        .id;
    let request = Uuid::new_v4();
    host.submit(
        session,
        request,
        vec![json!({"type":"input_text","text":"Status only"})],
        new_task_intent(),
    )
    .await
    .unwrap();
    let run = wait_submission(&host, session, request).await;
    assert_eq!(run.task.outcome, Some(Outcome::FinishedUnverified));
    drop(server);
}

#[tokio::test]
async fn native_host_rejects_sandbox_only_shell_input() {
    let fixture = Fixture::new();
    let host = fixture.open_host("http://127.0.0.1:1/v1/responses").await;
    let session = fixture.admit_session(&host).await;
    let error = host
        .submit(
            session,
            Uuid::new_v4(),
            vec![json!({"type":"input_text","text":"! echo hi"})],
            SubmitIntent::Shell {
                spec: orvek_harness::manual::ShellSpec {
                    command: "echo hi".into(),
                    expected_task: None,
                    scope_revision: None,
                    timeout_ms: 5_000,
                    output_bytes: 1_024,
                },
            },
        )
        .await
        .expect_err("sandbox shell must be refused on a native host");
    assert!(error.to_string().contains("no sandbox shell"), "{error}");
}

#[tokio::test]
async fn trace_reexecution_uses_only_intent_and_fresh_admitted_identities() {
    let original = Fixture::new();
    let (endpoint, server) = provider(vec![vec![final_message("original")]]).await;
    let host = original.open_host(&endpoint).await;
    let session = original.admit_session(&host).await;
    let request = Uuid::new_v4();
    host.submit(
        session,
        request,
        vec![json!({"type":"input_text","text":"Describe the empty workspace"})],
        new_task_intent(),
    )
    .await
    .unwrap();
    let first = wait_submission(&host, session, request).await;
    server.await.unwrap();
    let bundle = orvek_harness::trace::TraceBundle::export(
        host.state_directory(),
        None,
        Default::default(),
        &Default::default(),
        None,
    )
    .unwrap();
    let intent = bundle.reexecution_intent(first.task.id).unwrap();
    let fresh = Fixture::new();
    let (endpoint, server) = provider(vec![vec![final_message("fresh")]]).await;
    let host = fresh.open_host(&endpoint).await;
    let new_session = fresh.admit_session(&host).await;
    let new_request = Uuid::new_v4();
    host.submit(
        new_session,
        new_request,
        vec![json!({"type":"input_text","text":intent})],
        new_task_intent(),
    )
    .await
    .unwrap();
    let second = wait_submission(&host, new_session, new_request).await;
    let requests = server.await.unwrap();
    assert_ne!(session, new_session);
    assert_ne!(request, new_request);
    assert_ne!(first.task.id, second.task.id);
    assert!(
        first
            .task
            .model_receipts
            .keys()
            .all(|id| !second.task.model_receipts.contains_key(id))
    );
    assert_eq!(second.task.request, first.task.request);
    assert_eq!(second.task.outcome, Some(Outcome::FinishedUnverified));
    assert!(second.task.evidence.is_empty());
    assert!(
        requests
            .iter()
            .all(|request| !request.to_string().contains(&first.task.id.to_string()))
    );
}
