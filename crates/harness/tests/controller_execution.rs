use orvek_harness::{
    Channel, Digest, Store,
    contract::*,
    controller::{Host, HostUpdate},
    inference::{
        Limits as InferenceLimits, ModelSettings, ResponsesClient, Route, Transport,
        auth::{Auth, SecretString},
    },
    runtime::DockerExecutor,
    session::SessionAdmissionRequest,
    state::Outcome,
    verification::{CheckProgram, ControlFailure, Expectation, Probe},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

enum ProviderReply {
    Response(Vec<Value>),
    Rejected(u16),
}

async fn provider(outputs: Vec<Vec<Value>>) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    provider_replies(outputs.into_iter().map(ProviderReply::Response).collect()).await
}

async fn provider_replies(
    outputs: Vec<ProviderReply>,
) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (index, output) in outputs.into_iter().enumerate() {
            let (mut socket, _) = timeout(Duration::from_secs(20), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 32 * 1024);
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
            let output = match output {
                ProviderReply::Response(output) => output,
                ProviderReply::Rejected(status) => {
                    socket.write_all(format!("HTTP/1.1 {status} Rejected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
                    continue;
                }
            };
            let event = json!({"type":"response.completed","response":{"id":format!("resp_{index}"),"status":"completed","output":output,"usage":{"input_tokens":5,"output_tokens":1,"total_tokens":6}}});
            let payload = format!("event: response.completed\ndata: {event}\n\n");
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
        }
        requests
    });
    (endpoint, task)
}

fn final_message(id: &str) -> Value {
    json!({"type":"message","id":id,"role":"assistant","status":"completed","content":[{"type":"output_text","text":"Done","annotations":[]}]})
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn human_shell_keeps_private_changes_across_requests_without_model_calls_or_source_overwrite()
{
    use orvek_harness::{
        manual::ShellSpec,
        submission::SubmitIntent,
        workspace::{Entry, Snapshot},
    };
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.orvek/ct");
    fs::create_dir_all(&directory).unwrap();
    // Canonicalize before deriving any path under this root: the literal form
    // carries a `crates/harness/../../` detour, and the queued case binds a
    // Unix socket here, which macOS caps at 104 bytes (SUN_LEN).
    let directory = directory.canonicalize().unwrap();
    let root = tempfile::tempdir_in(directory).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("value"), "before").unwrap();
    let state_root = root.path().join("state");
    let client = ResponsesClient::new(
        Auth::api_key(SecretString::new("fixture".into())).unwrap(),
        Route::new(Transport::Http, "http://127.0.0.1:1/v1/responses").unwrap(),
        InferenceLimits {
            max_attempts: 1,
            ..InferenceLimits::default()
        },
    )
    .unwrap();
    let host = Arc::new(
        Host::open(
            &state_root,
            client,
            DockerExecutor::connect("debian:bookworm-slim")
                .await
                .unwrap(),
        )
        .unwrap(),
    );
    let session = host
        .create_session(SessionAdmissionRequest::new(
            source.clone(),
            ModelSettings::default(),
            orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            Channel::Stable,
        ))
        .await
        .unwrap();
    let request = uuid::Uuid::new_v4();
    let spec = ShellSpec {
        command: "printf after > value; printf done".into(),
        expected_task: None,
        scope_revision: None,
        timeout_ms: 10_000,
        output_bytes: 1024,
    };
    host.submit(
        session.id,
        request,
        vec![],
        SubmitIntent::Shell { spec: spec.clone() },
    )
    .await
    .unwrap();
    let report = wait_shell(&host, session.id, request).await;
    assert!(matches!(
        report.status,
        orvek_harness::runtime::ExecutionStatus::Exited(0)
    ));
    assert!(report.adopted);
    assert_eq!(artifact_bytes(&host, report.stdout).await, b"done");
    let state = host.session(session.id).await.unwrap();
    assert!(
        state.history.is_empty(),
        "local shell input/output are not automatically sent to a provider"
    );
    assert_eq!(state.current_task, None);
    assert_eq!(state.outcome, None);
    assert_eq!(state.branch.pending_shell, None);
    assert_eq!(
        host.submit(session.id, request, vec![], SubmitIntent::Shell { spec })
            .await
            .unwrap()
            .result,
        Some(Digest::of_value(&report).unwrap())
    );
    fs::write(source.join("note"), "user note").unwrap();
    let next = uuid::Uuid::new_v4();
    host.submit(
        session.id,
        next,
        vec![],
        SubmitIntent::Shell {
            spec: ShellSpec {
                command: "cat value; cat note".into(),
                expected_task: None,
                scope_revision: None,
                timeout_ms: 10_000,
                output_bytes: 1024,
            },
        },
    )
    .await
    .unwrap();
    let report = wait_shell(&host, session.id, next).await;
    assert_eq!(
        artifact_bytes(&host, report.stdout).await,
        b"afteruser note"
    );
    assert!(report.adopted);
    let snapshot: Snapshot =
        serde_json::from_slice(&artifact_bytes(&host, report.after.unwrap()).await).unwrap();
    assert!(
        matches!(snapshot.entries.get("value"), Some(Entry::File { content, .. }) if *content == Digest::of(b"after"))
    );
    assert_eq!(fs::read_to_string(source.join("value")).unwrap(), "before");
    assert_eq!(
        fs::read_to_string(source.join("note")).unwrap(),
        "user note"
    );
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn auxiliary_answer_is_durable_readonly_hidden_and_cannot_complete_the_coding_task() {
    use orvek_harness::{
        auxiliary::{
            AuxiliaryContext, AuxiliaryKind, AuxiliaryLimits, AuxiliaryReport, AuxiliarySpec,
            AuxiliaryStatus,
        },
        session::SessionCommand,
        submission::{SubmissionStatus, SubmitIntent},
    };
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.orvek/ct");
    fs::create_dir_all(&directory).unwrap();
    // Canonicalize before deriving any path under this root: the literal form
    // carries a `crates/harness/../../` detour, and the queued case binds a
    // Unix socket here, which macOS caps at 104 bytes (SUN_LEN).
    let directory = directory.canonicalize().unwrap();
    let root = tempfile::tempdir_in(directory).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("value"), "unchanged").unwrap();
    let state_root = root.path().join("state");
    let setup = Host::open(
        &state_root,
        ResponsesClient::new(
            Auth::api_key(SecretString::new("fixture".into())).unwrap(),
            Route::new(Transport::Http, "http://127.0.0.1:1/v1/responses").unwrap(),
            InferenceLimits {
                max_attempts: 1,
                ..InferenceLimits::default()
            },
        )
        .unwrap(),
        DockerExecutor::connect("debian:bookworm-slim")
            .await
            .unwrap(),
    )
    .unwrap();
    let session = setup
        .create_session(SessionAdmissionRequest::new(
            source.clone().canonicalize().unwrap(),
            ModelSettings::default(),
            orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            Channel::Stable,
        ))
        .await
        .unwrap();
    drop(setup);
    let mut store = Store::open(&state_root).unwrap();
    let policy = store
        .public_artifacts()
        .write(
            &serde_json::to_vec(&orvek_harness::admission::RequestPolicy {
                version: 1,
                delivery: DeliveryKind::Source,
                profile: orvek_harness::admission::RepositoryProfile {
                    version: 1,
                    name: "fixture".into(),
                    checks: BTreeMap::new(),
                },
            })
            .unwrap(),
        )
        .unwrap()
        .digest();
    let original = uuid::Uuid::new_v4();
    let (session, task, _) = store
        .start_request(
            session.id,
            original,
            "Implement the original coding objective".into(),
            Limits::default(),
            policy,
        )
        .unwrap();
    let before = store
        .stop(
            task.id,
            task.revision,
            Outcome::Blocked,
            "fixture prerequisite".into(),
        )
        .unwrap();
    let session = store
        .session_command(
            session.id,
            session.revision,
            uuid::Uuid::new_v4(),
            SessionCommand::TurnSettled {
                request: original,
                outcome: before.outcome,
                error: None,
            },
        )
        .unwrap();
    let original_history = session.history.clone();
    drop(store);
    let forbidden = json!({"type":"function_call","id":"fc_forbidden_aux","call_id":"call_forbidden_aux","name":"write_file","arguments":"{\"path\":\"value\",\"content\":\"must not run\"}","status":"completed"});
    let (endpoint, served) =
        provider(vec![vec![forbidden], vec![final_message("aux_answer")]]).await;
    let client = ResponsesClient::new(
        Auth::api_key(SecretString::new("fixture".into())).unwrap(),
        Route::new(Transport::Http, &endpoint).unwrap(),
        InferenceLimits {
            max_attempts: 1,
            ..InferenceLimits::default()
        },
    )
    .unwrap();
    let host = Arc::new(
        Host::open(
            &state_root,
            client,
            DockerExecutor::connect("debian:bookworm-slim")
                .await
                .unwrap(),
        )
        .unwrap(),
    );
    let request = uuid::Uuid::new_v4();
    let spec = AuxiliarySpec {
        kind: AuxiliaryKind::Question,
        context: AuxiliaryContext::Clean,
        review: None,
        limits: AuxiliaryLimits::default(),
    };
    let content = vec![
        json!({"type":"input_text","text":"Explain the selected context; do not implement it"}),
    ];
    host.submit(
        session.id,
        request,
        content.clone(),
        SubmitIntent::Auxiliary { spec: spec.clone() },
    )
    .await
    .unwrap();
    let settled = timeout(Duration::from_secs(30), async {
        loop {
            let submission = host.submission(session.id, request).await.unwrap();
            // Wait for the request to actually settle. Excluding only `Queued`
            // let this loop return a still-`Running` submission and then assert
            // on that transient state.
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
            SubmissionStatus::Finished {
                task: None,
                outcome: None,
                error: None
            }
        ),
        "{settled:?}"
    );
    let report: AuxiliaryReport = serde_json::from_slice(
        &artifact_bytes(&host, settled.result.expect("durable answer artifact")).await,
    )
    .unwrap();
    assert_eq!(report.status, AuxiliaryStatus::Completed);
    assert_eq!(report.model_calls, 2);
    assert_eq!(report.tokens, Some(12));
    assert_eq!(host.task(task.id).await.unwrap(), before);
    assert_eq!(
        host.session(session.id).await.unwrap().history,
        original_history
    );
    assert_eq!(
        fs::read_to_string(source.join("value")).unwrap(),
        "unchanged"
    );
    assert_eq!(
        host.submit(
            session.id,
            request,
            content,
            SubmitIntent::Auxiliary { spec }
        )
        .await
        .unwrap(),
        settled
    );
    let requests = served.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        !requests[0]["input"]
            .to_string()
            .contains("original coding objective")
    );
    assert!(requests[1]["input"].to_string().contains("not admitted"));
    assert!(
        !requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| matches!(
                tool["name"].as_str(),
                Some("exec_command" | "write_file" | "propose_completion")
            ))
    );
}

#[tokio::test]
#[ignore = "requires local Docker and pre-pulled debian:bookworm-slim"]
async fn controller_rejects_premature_finish_then_delivers_an_actually_verified_fix() {
    exercise(DeliveryKind::Source, Mode::OrdinaryAction).await;
}

#[tokio::test]
#[ignore = "requires local Docker and pre-pulled debian:bookworm-slim"]
async fn controller_delivers_a_patch_that_reproduces_the_verified_source() {
    exercise(DeliveryKind::Patch, Mode::Ordinary).await;
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn ordinary_action_is_classified_then_admitted_as_one_task() {
    exercise(DeliveryKind::Source, Mode::OrdinaryAction).await;
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn ordinary_information_is_answered_without_changing_the_current_task() {
    exercise(DeliveryKind::Source, Mode::OrdinaryInformation).await;
}

/// Like `provider`, but lets a case choose the terminal event's `usage` field,
/// including omitting it, instead of the fixed six-token accounting. Omitting it
/// is how an unknown-billing classifier response is reproduced.
async fn provider_with_usage(
    outputs: Vec<(Vec<Value>, Option<Value>)>,
) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (index, (output, usage)) in outputs.into_iter().enumerate() {
            let (mut socket, _) = timeout(Duration::from_secs(20), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 32 * 1024);
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
            let mut response =
                json!({"id":format!("resp_{index}"),"status":"completed","output":output});
            if let Some(usage) = usage {
                response["usage"] = usage;
            }
            let event = json!({"type":"response.completed","response":response});
            let payload = format!("event: response.completed\ndata: {event}\n\n");
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
        }
        requests
    });
    (endpoint, task)
}

struct ClassifierRun {
    session: orvek_harness::session::SessionId,
    request: uuid::Uuid,
    settled: orvek_harness::submission::Submission,
    state: orvek_harness::session::SessionState,
    requests: Vec<Value>,
    records: Vec<orvek_harness::auxiliary::AuxiliaryRecord>,
}

/// Drive one ordinary submission through classification against a single
/// scripted classifier response, then settle and read back durable state.
async fn run_ordinary_classifier(output: Vec<Value>, usage: Option<Value>) -> ClassifierRun {
    use orvek_harness::submission::{Schedule, SubmitIntent};
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.orvek/ct");
    fs::create_dir_all(&directory).unwrap();
    let directory = directory.canonicalize().unwrap();
    let root = tempfile::tempdir_in(directory).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    let state_root = root.path().join("state");
    let (endpoint, served) = provider_with_usage(vec![(output, usage)]).await;
    let client = ResponsesClient::new(
        Auth::api_key(SecretString::new("fixture-key".into())).unwrap(),
        Route::new(Transport::Http, &endpoint).unwrap(),
        InferenceLimits {
            max_attempts: 1,
            ..InferenceLimits::default()
        },
    )
    .unwrap();
    let host = Arc::new(
        Host::open(
            &state_root,
            client,
            DockerExecutor::connect("debian:bookworm-slim")
                .await
                .unwrap(),
        )
        .unwrap(),
    );
    let session = host
        .create_session(SessionAdmissionRequest::new(
            source.clone(),
            ModelSettings::default(),
            orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            Channel::Stable,
        ))
        .await
        .unwrap();
    let policy = orvek_harness::admission::RequestPolicy {
        version: 1,
        delivery: DeliveryKind::Source,
        profile: orvek_harness::admission::RepositoryProfile {
            version: 1,
            name: "malformed classifier fixture".into(),
            checks: BTreeMap::new(),
        },
    };
    let request = uuid::Uuid::new_v4();
    let content = vec![json!({"type":"input_text","text":"Fix addition"})];
    let intent = SubmitIntent::Ordinary {
        limits: Limits::default(),
        policy,
        schedule: Schedule::Queue,
    };
    host.submit(session.id, request, content.clone(), intent.clone())
        .await
        .unwrap();
    host.start_queued().await.unwrap();
    let settled = timeout(Duration::from_secs(60), async {
        loop {
            let submission = host.submission(session.id, request).await.unwrap();
            if !submission.status.pending() {
                break submission;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let requests = served.await.unwrap();
    // Resubmitting the identical ID, input and intent is recognized as the same
    // request and answered from the durable record, never reclassified.
    let replay = host
        .submit(session.id, request, content, intent)
        .await
        .unwrap();
    assert_eq!(
        replay, settled,
        "resubmitting the same ordinary request must not redispatch the classifier"
    );
    let mut records = Vec::new();
    for digest in &settled.records {
        records.push(serde_json::from_slice(&artifact_bytes(&host, *digest).await).unwrap());
    }
    let state = host.session(session.id).await.unwrap();
    ClassifierRun {
        session: session.id,
        request,
        settled,
        state,
        requests,
        records,
    }
}

/// Every malformed classifier outcome has to fail closed identically: no task,
/// no fabricated answer, no redispatch, the original submission ID kept, and a
/// durable intended/observed record pair that still shows what was actually
/// billed.
fn assert_classifier_failed_closed(
    run: &ClassifierRun,
    expected_kind: &str,
    expected_tokens: Option<u64>,
) {
    use orvek_harness::{auxiliary::AuxiliaryRecord, state::ModelCallStatus};
    assert_eq!(run.settled.id, run.request, "the submission keeps its ID");
    let orvek_harness::submission::SubmissionStatus::Finished {
        task,
        outcome,
        error,
    } = run.settled.status.clone()
    else {
        panic!("must fail closed into Finished: {:?}", run.settled.status);
    };
    assert_eq!(task, None, "no task may be admitted");
    assert_eq!(outcome, None);
    assert!(
        error
            .as_deref()
            .is_some_and(|error| error.contains("ordinary classification failed closed")),
        "{error:?}"
    );
    assert_eq!(run.settled.result, None, "no answer may be fabricated");
    assert_eq!(run.requests.len(), 1, "the classifier is dispatched once");
    assert_eq!(run.records.len(), 2, "{:?}", run.records);
    assert!(matches!(
        run.records[0],
        AuxiliaryRecord::ClassificationIntended { .. }
    ));
    let AuxiliaryRecord::ClassificationObserved { receipt, kind, .. } = &run.records[1] else {
        panic!("expected an observed record: {:?}", run.records[1]);
    };
    assert_eq!(
        receipt.status,
        ModelCallStatus::Completed,
        "the provider call itself succeeded; only the decision failed closed"
    );
    assert_eq!(
        receipt.tokens, expected_tokens,
        "unknown billing must not be normalized to zero spend"
    );
    assert_eq!(kind.as_str(), expected_kind);
    assert_eq!(run.state.current_task, None);
    assert!(run.state.tasks_by_request.is_empty());
    assert_eq!(
        run.state.active_request, None,
        "a fail-closed classification must settle the turn, not wedge the session"
    );
    let _ = run.session;
}

fn classifier_message(text: String) -> Vec<Value> {
    vec![
        json!({"type":"message","id":"msg_classify","role":"assistant","status":"completed","content":[{"type":"output_text","text":text,"annotations":[]}]}),
    ]
}

fn six_tokens() -> Option<Value> {
    Some(json!({"input_tokens":5,"output_tokens":1,"total_tokens":6}))
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn ordinary_classifier_rejects_unparseable_json_without_redispatch() {
    let run =
        run_ordinary_classifier(classifier_message("not json at all".into()), six_tokens()).await;
    assert_classifier_failed_closed(&run, "", Some(6));
    assert!(
        run.state.history.is_empty(),
        "an unresolved request records no conversation history"
    );
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn ordinary_classifier_rejects_unknown_fields_without_redispatch() {
    // `ClassifierOutput` denies unknown fields, so an extra key alongside a
    // valid kind is still a hard rejection rather than a tolerated decision.
    let run = run_ordinary_classifier(
        classifier_message(json!({"kind":"action","confidence":0.97}).to_string()),
        six_tokens(),
    )
    .await;
    assert_classifier_failed_closed(&run, "", Some(6));
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn ordinary_classifier_rejects_an_unknown_kind_without_redispatch() {
    let run = run_ordinary_classifier(
        classifier_message(json!({"kind":"chitchat"}).to_string()),
        six_tokens(),
    )
    .await;
    assert_classifier_failed_closed(&run, "", Some(6));
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn ordinary_classifier_rejects_a_tool_proposal_without_executing_it() {
    // The classifier request offers no tools, but a misbehaving model can still
    // return a function call. It must not be executed or treated as a decision.
    let run = run_ordinary_classifier(
        vec![
            json!({"type":"function_call","id":"fc_classify","call_id":"call_classify","name":"read_file","arguments":"{}","status":"completed"}),
        ],
        six_tokens(),
    )
    .await;
    assert_classifier_failed_closed(&run, "", Some(6));
    assert_eq!(
        run.requests[0]["tools"],
        json!([]),
        "the classifier is never offered tools, so the proposal was unsolicited"
    );
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn ordinary_classifier_with_unknown_billing_fails_closed_despite_a_valid_kind() {
    // The body is a perfectly valid `action` decision, but the terminal event
    // omits usage entirely. A recognized kind is not enough to admit a task
    // when the spend cannot be accounted, and the durable record keeps both
    // facts: the kind it understood and the billing it never learned.
    let run = run_ordinary_classifier(
        classifier_message(json!({"kind":"action"}).to_string()),
        None,
    )
    .await;
    assert_classifier_failed_closed(&run, "action", None);
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn ordinary_action_continues_an_incomplete_task_and_charges_its_classifier() {
    use orvek_harness::submission::{Schedule, SubmissionStatus, SubmitIntent};
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.orvek/ct");
    fs::create_dir_all(&directory).unwrap();
    let directory = directory.canonicalize().unwrap();
    let root = tempfile::tempdir_in(directory).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    let before = "#!/bin/sh\nprintf '3\\n'\n";
    let after = "#!/bin/sh\nprintf '%s\\n' \"$(($1 + $2))\"\n";
    fs::write(source.join("add"), before).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(source.join("add"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let state_root = root.path().join("state");
    let program = CheckProgram {
        version: 1,
        probes: vec![Probe::Command {
            id: "sum".into(),
            command: "./add 2 2".into(),
            exit_code: 0,
            stdout: Some(Expectation::Equals("4\n".into())),
            stderr: None,
        }],
        control_failure: Some(ControlFailure {
            probe: "sum".into(),
            stdout: Some(Expectation::Equals("3\n".into())),
            stderr: None,
        }),
    };
    let limits = Limits {
        model_calls: 12,
        tokens: 4000,
        ..Limits::default()
    };
    let draft = orvek_harness::admission::Proposal {
        outcome: "add both operands".into(),
        scope: "addition command".into(),
        requirements: vec![Requirement {
            id: "sum".into(),
            behavior: "2 plus 2 yields 4".into(),
            origin: Origin::User("Fix addition".into()),
            checks: vec!["sum".into()],
            depends_on: vec![],
        }],
        checks: BTreeMap::from([(
            "sum".into(),
            orvek_harness::admission::ProposedCheck {
                purpose: "observe exact arithmetic behavior".into(),
                kind: CheckKind::Behavior,
                program: program.clone(),
                baseline_failure: true,
                control_omission: None,
            },
        )]),
        protected_behavior: vec![],
        assumptions: vec![],
        open_questions: vec![],
    };
    let classify = |id: &str| {
        vec![
            json!({"type":"message","id":id,"role":"assistant","status":"completed","content":[{"type":"output_text","text":json!({"kind":"action"}).to_string(),"annotations":[]}]}),
        ]
    };
    let outputs = vec![
        classify("msg_classify_1"),
        vec![
            json!({"type":"function_call","id":"fc_contract","call_id":"call_contract","name":"propose_contract","arguments":serde_json::to_string(&draft).unwrap(),"status":"completed"}),
        ],
        vec![final_message("msg_early")],
        vec![
            json!({"type":"function_call","id":"fc_write","call_id":"call_write","name":"write_file","arguments":serde_json::to_string(&json!({"operation":"replace","path":"add","expected":{"kind":"digest","digest":Digest::of(before.as_bytes())},"content":after})).unwrap(),"status":"completed"}),
        ],
        vec![
            json!({"type":"function_call","id":"fc_blocker","call_id":"call_blocker","name":"report_blocker","arguments":"{\"reason\":\"fixture prerequisite is temporarily absent\"}","status":"completed"}),
        ],
        classify("msg_classify_2"),
        vec![final_message("msg_final")],
        vec![final_message("msg_spare_1")],
        vec![final_message("msg_spare_2")],
    ];
    // The handle is intentionally dropped rather than awaited: the exact number
    // of provider turns the controller needs is not the property under test, so
    // spare responses must not make the fixture hang.
    let (endpoint, _served) = provider(outputs).await;
    let client = ResponsesClient::new(
        Auth::api_key(SecretString::new("fixture-key".into())).unwrap(),
        Route::new(Transport::Http, &endpoint).unwrap(),
        InferenceLimits {
            max_attempts: 1,
            ..InferenceLimits::default()
        },
    )
    .unwrap();
    let host = Arc::new(
        Host::open(
            &state_root,
            client,
            DockerExecutor::connect("debian:bookworm-slim")
                .await
                .unwrap(),
        )
        .unwrap(),
    );
    let session = host
        .create_session(SessionAdmissionRequest::new(
            source.clone(),
            ModelSettings::default(),
            orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            Channel::Stable,
        ))
        .await
        .unwrap();
    let policy = orvek_harness::admission::RequestPolicy {
        version: 1,
        delivery: DeliveryKind::Source,
        profile: orvek_harness::admission::RepositoryProfile {
            version: 1,
            name: "fixed addition fixture".into(),
            checks: BTreeMap::new(),
        },
    };
    let ordinary = |text: &str| {
        (
            vec![json!({"type":"input_text","text":text})],
            SubmitIntent::Ordinary {
                limits,
                policy: policy.clone(),
                schedule: Schedule::Queue,
            },
        )
    };
    let settle = |request: uuid::Uuid| {
        let host = host.clone();
        async move {
            timeout(Duration::from_secs(120), async {
                loop {
                    let submission = host.submission(session.id, request).await.unwrap();
                    if !submission.status.pending() {
                        break submission;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap()
        }
    };

    let first = uuid::Uuid::new_v4();
    let (content, intent) = ordinary("Fix addition");
    host.submit(session.id, first, content, intent)
        .await
        .unwrap();
    host.start_queued().await.unwrap();
    let settled = settle(first).await;
    let SubmissionStatus::Finished {
        task: Some(task_id),
        outcome,
        ..
    } = settled.status
    else {
        panic!("an ordinary action must admit a task even when it blocks: {settled:?}");
    };
    assert_eq!(
        outcome,
        Some(Outcome::Blocked),
        "the fixture reports a blocker so the task is left incomplete: {settled:?}"
    );
    let after_first = host.task(task_id).await.unwrap();
    let first_classifier = uuid::Uuid::new_v5(&first, b"ordinary-classifier");
    assert!(
        after_first.model_receipts.contains_key(&first_classifier),
        "the classifier that admitted this task must be charged to it"
    );

    // A second ordinary action, while that task is still incomplete.
    let second = uuid::Uuid::new_v4();
    let (content, intent) = ordinary("The prerequisite is available now; continue");
    host.submit(session.id, second, content, intent)
        .await
        .unwrap();
    host.start_queued().await.unwrap();
    let settled = settle(second).await;
    let SubmissionStatus::Finished {
        task: Some(continued),
        ..
    } = settled.status
    else {
        panic!("the continuation must resolve to a task: {settled:?}");
    };

    // Identity and every immutable fact are retained across the continuation.
    assert_eq!(
        continued, task_id,
        "an ordinary action must continue the incomplete task, never spawn a second one"
    );
    let after_second = host.task(task_id).await.unwrap();
    assert_eq!(
        after_second.request, after_first.request,
        "the immutable original request must not change"
    );
    assert_eq!(after_second.initial_limits, after_first.initial_limits);
    assert_eq!(
        after_second.started_ms, after_first.started_ms,
        "continuing must not reset the task's start time"
    );
    assert_eq!(
        after_second.baseline, after_first.baseline,
        "continuing must not re-snapshot the established baseline"
    );
    assert_eq!(after_second.origin, after_first.origin);
    assert_eq!(
        after_second.contract.as_ref().unwrap().requirements,
        after_first.contract.as_ref().unwrap().requirements,
        "the scope admitted on the first turn is retained"
    );

    // The second classifier is charged to that same task, and the first charge
    // survives untouched.
    let second_classifier = uuid::Uuid::new_v5(&second, b"ordinary-classifier");
    assert!(
        after_second.model_receipts.contains_key(&second_classifier),
        "the continuation's classifier must be charged to the continued task"
    );
    assert!(
        after_second.model_receipts.contains_key(&first_classifier),
        "the original classifier charge must survive the continuation"
    );
    assert!(after_second.usage.model_calls > after_first.usage.model_calls);

    let state = host.session(session.id).await.unwrap();
    assert_eq!(state.tasks_by_request.get(&first), Some(&task_id));
    assert_eq!(
        state.tasks_by_request.get(&second),
        Some(&task_id),
        "both ordinary requests resolve to the one task"
    );
    assert_eq!(
        fs::read_to_string(source.join("add")).unwrap(),
        before,
        "the user's source must not be overwritten by execution"
    );
}

/// Accepts exactly one request and then never answers it, so the classifier
/// call stays in flight until the host cancels it. Resolves `dispatched` once
/// the request is fully read, and reports how many requests it ever received.
async fn stalling_provider() -> (
    String,
    tokio::sync::oneshot::Receiver<()>,
    tokio::task::JoinHandle<usize>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    let (dispatched_tx, dispatched_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let Ok(Ok((mut socket, _))) = timeout(Duration::from_secs(20), listener.accept()).await
        else {
            return 0usize;
        };
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            if socket.read_exact(&mut byte).await.is_err() {
                return 0;
            }
            headers.push(byte[0]);
            assert!(headers.len() < 32 * 1024);
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
        if socket.read_exact(&mut bytes).await.is_err() {
            return 0;
        }
        let _ = dispatched_tx.send(());
        // Deliberately send no response. This read returns once the host drops
        // the connection, which is what cancelling the call does.
        let mut discard = [0u8; 1];
        let _ = socket.read(&mut discard).await;
        1
    });
    (endpoint, dispatched_rx, task)
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn ordinary_cancellation_during_classification_settles_without_admitting_a_task() {
    use orvek_harness::submission::{Schedule, SubmitIntent};
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.orvek/ct");
    fs::create_dir_all(&directory).unwrap();
    let directory = directory.canonicalize().unwrap();
    let root = tempfile::tempdir_in(directory).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    let state_root = root.path().join("state");
    let (endpoint, dispatched, served) = stalling_provider().await;
    let client = ResponsesClient::new(
        Auth::api_key(SecretString::new("fixture-key".into())).unwrap(),
        Route::new(Transport::Http, &endpoint).unwrap(),
        InferenceLimits {
            max_attempts: 1,
            ..InferenceLimits::default()
        },
    )
    .unwrap();
    let host = Arc::new(
        Host::open(
            &state_root,
            client,
            DockerExecutor::connect("debian:bookworm-slim")
                .await
                .unwrap(),
        )
        .unwrap(),
    );
    let session = host
        .create_session(SessionAdmissionRequest::new(
            source.clone(),
            ModelSettings::default(),
            orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            Channel::Stable,
        ))
        .await
        .unwrap();
    let policy = orvek_harness::admission::RequestPolicy {
        version: 1,
        delivery: DeliveryKind::Source,
        profile: orvek_harness::admission::RepositoryProfile {
            version: 1,
            name: "cancelled classifier fixture".into(),
            checks: BTreeMap::new(),
        },
    };
    let request = uuid::Uuid::new_v4();
    host.submit(
        session.id,
        request,
        vec![json!({"type":"input_text","text":"Fix addition"})],
        SubmitIntent::Ordinary {
            limits: Limits::default(),
            policy,
            schedule: Schedule::Queue,
        },
    )
    .await
    .unwrap();
    host.start_queued().await.unwrap();

    // Only cancel once the call is genuinely on the wire, so this exercises the
    // in-flight window rather than the easier pre-dispatch one.
    timeout(Duration::from_secs(20), dispatched)
        .await
        .unwrap()
        .unwrap();
    host.cancel_submission(session.id, request).await.unwrap();

    let settled = timeout(Duration::from_secs(60), async {
        loop {
            let submission = host.submission(session.id, request).await.unwrap();
            if !submission.status.pending() {
                break submission;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let dispatches = timeout(Duration::from_secs(30), served)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(settled.id, request);
    assert!(
        matches!(
            settled.status,
            orvek_harness::submission::SubmissionStatus::Finished {
                task: None,
                outcome: None,
                ..
            }
        ),
        "a cancelled classification must never admit a task: {settled:?}"
    );
    assert_eq!(settled.result, None, "no answer may be fabricated");
    assert_eq!(
        dispatches, 1,
        "cancelling must not cause the classifier to be redispatched"
    );
    let state = host.session(session.id).await.unwrap();
    assert_eq!(
        state.active_request, None,
        "a cancelled classification must settle the turn, not wedge the session"
    );
    assert_eq!(state.current_task, None);
    assert!(state.tasks_by_request.is_empty());
}

#[tokio::test]
#[ignore = "requires local Docker and pre-pulled debian:bookworm-slim"]
async fn explicit_completion_tool_gets_a_durable_result_before_session_settles() {
    exercise(DeliveryKind::Source, Mode::ExplicitCompletion).await;
}

#[tokio::test]
#[ignore = "requires local Docker and pre-pulled debian:bookworm-slim"]
async fn resumed_task_keeps_its_original_baseline_working_changes_and_monotonic_usage() {
    exercise(DeliveryKind::Source, Mode::Resume).await;
}

#[tokio::test]
#[ignore = "requires local Docker and pre-pulled debian:bookworm-slim"]
async fn natural_request_can_write_and_execute_before_pinning_its_contract() {
    exercise(DeliveryKind::Source, Mode::Natural).await;
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn disconnected_submission_and_followup_keep_one_task_and_its_original_checks() {
    exercise(DeliveryKind::Source, Mode::Queued).await;
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn unresolved_questions_allow_research_but_not_completion() {
    exercise(DeliveryKind::Source, Mode::OpenQuestions).await;
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn rate_limit_retry_admits_a_fresh_call_and_then_delivers() {
    exercise(DeliveryKind::Source, Mode::RateLimitRetry).await;
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn repeated_rate_limits_stop_without_fabricated_spend_or_completion() {
    exercise(DeliveryKind::Source, Mode::RateLimitExhaustion).await;
}

enum Mode {
    RateLimitRetry,
    RateLimitExhaustion,
    OpenQuestions,
    Ordinary,
    OrdinaryAction,
    ExplicitCompletion,
    Resume,
    Natural,
    Queued,
    OrdinaryInformation,
}

async fn exercise(delivery: DeliveryKind, mode: Mode) {
    let questions = matches!(mode, Mode::OpenQuestions);
    let rate_limit_retry = matches!(mode, Mode::RateLimitRetry);
    let rate_limit_exhaustion = matches!(mode, Mode::RateLimitExhaustion);
    let completion_tool = matches!(mode, Mode::ExplicitCompletion);
    let resume = matches!(mode, Mode::Resume);
    let queued = matches!(mode, Mode::Queued);
    let natural = matches!(mode, Mode::Natural | Mode::Queued);
    let ordinary_action = matches!(mode, Mode::OrdinaryAction);
    let ordinary_information = matches!(mode, Mode::OrdinaryInformation);
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.orvek/ct");
    fs::create_dir_all(&directory).unwrap();
    // Canonicalize before deriving any path under this root: the literal form
    // carries a `crates/harness/../../` detour, and the queued case binds a
    // Unix socket here, which macOS caps at 104 bytes (SUN_LEN).
    let directory = directory.canonicalize().unwrap();
    let root = tempfile::tempdir_in(directory).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    let before = "#!/bin/sh\nprintf '3\\n'\n";
    let after = "#!/bin/sh\nprintf '%s\\n' \"$(($1 + $2))\"\n";
    fs::write(source.join("add"), before).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(source.join("add"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Keep this segment short: the queued case binds `<state_root>/host.sock`,
    // and macOS caps a Unix socket path at 104 bytes (SUN_LEN). Matches the
    // shorter `state` name the other tests in this file already use.
    let state_root = root.path().join("state");
    let store = Store::open(&state_root).unwrap();
    let program = CheckProgram {
        version: 1,
        probes: vec![Probe::Command {
            id: "sum".into(),
            command: "./add 2 2".into(),
            exit_code: 0,
            stdout: Some(Expectation::Equals("4\n".into())),
            stderr: None,
        }],
        control_failure: Some(ControlFailure {
            probe: "sum".into(),
            stdout: Some(Expectation::Equals("3\n".into())),
            stderr: None,
        }),
    };
    let verifier = store
        .public_artifacts()
        .write(&serde_json::to_vec(&program).unwrap())
        .unwrap()
        .digest();
    drop(store);
    let contract = Contract {
        request: "Fix addition".into(),
        outcome: "add both arguments".into(),
        scope: "add command".into(),
        requirements: vec![Requirement {
            id: "sum".into(),
            behavior: "add both arguments".into(),
            origin: Origin::User("Fix addition".into()),
            checks: vec!["sum".into()],
            depends_on: vec![],
        }],
        checks: BTreeMap::from([(
            "sum".into(),
            CheckDefinition {
                purpose: "observe command output".into(),
                kind: CheckKind::Behavior,
                verifier,
                command: vec!["tact-verify".into()],
                timeout_ms: 60_000,
                minimum_assertions: 2,
                control: ControlRequirement::BaselineFailure,
                control_source: None,
                baseline: BaselinePolicy::MustPass,
                flake: FlakePolicy::RejectAnyFailure,
            },
        )]),
        protected_behavior: vec![],
        assumptions: vec![],
        open_questions: if questions {
            vec!["Which additional operands matter?".into()]
        } else {
            vec![]
        },
        delivery,
        limits: Limits {
            model_calls: if queued || ordinary_action {
                8
            } else if ordinary_information {
                2
            } else if natural {
                6
            } else {
                4
            },
            tokens: 1000,
            ..Limits::default()
        },
    };
    let write = json!({"type":"function_call","id":"fc_write","call_id":"call_write","name":"write_file","arguments":serde_json::to_string(&json!({"operation":"replace","path":"add","expected":{"kind":"digest","digest":Digest::of(before.as_bytes())},"content":after})).unwrap(),"status":"completed"});
    let mut outputs = vec![
        vec![final_message("msg_early")],
        vec![write.clone()],
        if completion_tool {
            vec![
                json!({"type":"function_call","id":"fc_complete","call_id":"call_complete","name":"propose_completion","arguments":"{}","status":"completed"}),
            ]
        } else {
            vec![final_message("msg_final")]
        },
    ];
    if ordinary_action || ordinary_information {
        let kind = if ordinary_action {
            "action"
        } else {
            "information"
        };
        outputs.insert(
            0,
            vec![json!({"type":"message","id":"msg_classify","role":"assistant","status":"completed","content":[{"type":"output_text","text":json!({"kind":kind}).to_string(),"annotations":[]}]})],
        );
    }
    if ordinary_information {
        outputs.push(vec![final_message("msg_information_answer")]);
    }
    if resume {
        outputs.insert(2, vec![json!({"type":"function_call","id":"fc_blocker","call_id":"call_blocker","name":"report_blocker","arguments":"{\"reason\":\"fixture prerequisite is temporarily absent\"}","status":"completed"})]);
    }
    let policy = orvek_harness::admission::RequestPolicy {
        version: 1,
        delivery,
        profile: orvek_harness::admission::RepositoryProfile {
            version: 1,
            name: "fixed addition fixture".into(),
            checks: BTreeMap::new(),
        },
    };
    if ordinary_action {
        let draft = orvek_harness::admission::Proposal {
            outcome: contract.outcome.clone(),
            scope: contract.scope.clone(),
            requirements: contract.requirements.clone(),
            checks: BTreeMap::from([(
                "sum".into(),
                orvek_harness::admission::ProposedCheck {
                    purpose: "observe exact arithmetic behavior".into(),
                    kind: CheckKind::Behavior,
                    program: program.clone(),
                    baseline_failure: true,
                    control_omission: None,
                },
            )]),
            protected_behavior: vec![],
            assumptions: vec![],
            open_questions: vec![],
        };
        outputs.insert(
            1,
            vec![json!({"type":"function_call","id":"fc_contract","call_id":"call_contract","name":"propose_contract","arguments":serde_json::to_string(&draft).unwrap(),"status":"completed"})],
        );
    }
    if natural {
        let draft = orvek_harness::admission::Proposal {
            outcome: contract.outcome.clone(),
            scope: contract.scope.clone(),
            requirements: contract.requirements.clone(),
            checks: BTreeMap::from([(
                "sum".into(),
                orvek_harness::admission::ProposedCheck {
                    purpose: "observe exact arithmetic behavior".into(),
                    kind: CheckKind::Behavior,
                    program: program.clone(),
                    baseline_failure: true,
                    control_omission: None,
                },
            )]),
            protected_behavior: vec![],
            assumptions: vec![],
            open_questions: vec![],
        };
        let early_write = json!({"type":"function_call","id":"fc_early_write","call_id":"call_early_write","name":"write_file","arguments":serde_json::to_string(&json!({"operation":"replace","path":"discovery-note","expected":{"kind":"absent"},"content":"discovery write"})).unwrap(),"status":"completed"});
        let early_exec = json!({"type":"function_call","id":"fc_early_exec","call_id":"call_early_exec","name":"exec_command","arguments":serde_json::to_string(&json!({"command":"test \"$(cat discovery-note)\" = \"discovery write\" && rm discovery-note && printf early-workspace-ok"})).unwrap(),"status":"completed"});
        outputs.insert(0, vec![
            json!({"type":"reasoning","id":"rs_discovery","summary":[{"type":"summary_text","text":"Inspect the workspace before proposing the contract."}]}),
            early_write,
            early_exec,
        ]);
        outputs.insert(1, vec![json!({"type":"function_call","id":"fc_contract","call_id":"call_contract","name":"propose_contract","arguments":serde_json::to_string(&draft).unwrap(),"status":"completed"})]);
        if queued {
            let mut addition = draft;
            addition.outcome = "preserve zero identity".into();
            addition.scope = "zero operands".into();
            addition.requirements[0].id = "zero".into();
            addition.requirements[0].behavior = "zero plus zero yields zero".into();
            addition.requirements[0].origin = Origin::User("Also verify zero plus zero".into());
            addition.requirements[0].checks = vec!["zero".into()];
            let mut check = addition.checks.remove("sum").unwrap();
            check.purpose = "observe zero operands".into();
            check.program.probes = vec![Probe::Command {
                id: "sum".into(),
                command: "./add 0 0".into(),
                exit_code: 0,
                stdout: Some(Expectation::Equals("0\n".into())),
                stderr: None,
            }];
            addition.checks.insert("zero".into(), check);
            outputs.push(vec![json!({"type":"function_call","id":"fc_followup","call_id":"call_followup","name":"propose_contract","arguments":serde_json::to_string(&addition).unwrap(),"status":"completed"})]);
            outputs.push(vec![final_message("msg_followup_done")]);
        }
    }
    if questions {
        outputs = vec![
            vec![
                json!({"type":"function_call","id":"fc_research","call_id":"call_research","name":"exec_command","arguments":serde_json::to_string(&json!({"command":"printf research-ok > question-notes; cat question-notes"})).unwrap(),"status":"completed"}),
            ],
            vec![
                json!({"type":"function_call","id":"fc_question_blocker","call_id":"call_question_blocker","name":"report_blocker","arguments":"{\"reason\":\"Need the requested operand range\"}","status":"completed"}),
            ],
        ];
    }
    let (endpoint, served) = if rate_limit_exhaustion {
        provider_replies(vec![
            ProviderReply::Rejected(429),
            ProviderReply::Rejected(429),
            ProviderReply::Rejected(429),
        ])
        .await
    } else if rate_limit_retry {
        let mut replies = vec![ProviderReply::Rejected(429)];
        replies.extend(outputs.into_iter().map(ProviderReply::Response));
        provider_replies(replies).await
    } else {
        provider(outputs).await
    };
    let auth = Auth::api_key(SecretString::new("fixture-key".into())).unwrap();
    let route = Route::new(Transport::Http, &endpoint).unwrap();
    let client = ResponsesClient::new(
        auth,
        route,
        InferenceLimits {
            max_attempts: 1,
            ..InferenceLimits::default()
        },
    )
    .unwrap();
    let executor = DockerExecutor::connect("debian:bookworm-slim")
        .await
        .unwrap();
    let mut host = Arc::new(Host::open(&state_root, client, executor).unwrap());
    let session = host
        .create_session(SessionAdmissionRequest::new(
            source.clone(),
            ModelSettings::default(),
            orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            Channel::Stable,
        ))
        .await
        .unwrap();
    let updates = Arc::new(Mutex::new(Vec::new()));
    let recorded = updates.clone();
    let request_id = uuid::Uuid::new_v4();
    let sink: orvek_harness::controller::EventSink =
        Arc::new(move |update| recorded.lock().unwrap().push(update));
    let mut result = if queued {
        use orvek_harness::{ipc, submission::SubmitIntent};
        let stop = CancellationToken::new();
        let mut service = tokio::spawn(ipc::serve(host.clone(), stop.clone()));
        let socket = host.state_directory().join("host.sock");
        timeout(Duration::from_secs(5), async {
            while !socket.exists() {
                // Report why the listener never appeared. Spinning only on the
                // socket path turns any `serve` failure into an opaque timeout.
                if service.is_finished() {
                    panic!(
                        "ipc::serve exited before binding: {:?}",
                        (&mut service).await
                    );
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let request = ipc::Request {
            version: ipc::PROTOCOL_VERSION,
            id: request_id,
            command: ipc::Command::Submit {
                session: session.id,
                content: vec![json!({"type":"input_text","text":"Fix addition"})],
                intent: SubmitIntent::NewTask {
                    limits: contract.limits,
                    policy: policy.clone(),
                },
            },
        };
        let mut disconnected = tokio::net::UnixStream::connect(&socket).await.unwrap();
        ipc::write_frame(&mut disconnected, &request).await.unwrap();
        drop(disconnected);
        let accepted = ipc::call(&socket, &request, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(
            matches!(accepted, ipc::Response::Submission(_)),
            "{accepted:?}"
        );
        let first = wait_submission(&host, session.id, request_id).await;
        assert_eq!(first.task.outcome, Some(Outcome::Complete), "{first:?}");
        let repeated = ipc::call(&socket, &request, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(matches!(repeated, ipc::Response::Submission(_)));
        let followup = uuid::Uuid::new_v4();
        host.submit(
            session.id,
            followup,
            vec![json!({"type":"input_text","text":"Also verify zero plus zero"})],
            SubmitIntent::Continue {
                task: first.task.id,
                scope_revision: first.task.scope_revision,
                schedule: orvek_harness::submission::Schedule::Queue,
            },
        )
        .await
        .unwrap();
        let result = wait_submission(&host, session.id, followup).await;
        assert_eq!(result.task.id, first.task.id);
        assert_eq!(result.task.baseline, first.task.baseline);
        assert_eq!(result.task.request, first.task.request);
        assert_eq!(result.task.initial_limits, first.task.initial_limits);
        assert_eq!(result.task.started_ms, first.task.started_ms);
        assert!(result.task.usage.tokens > first.task.usage.tokens);
        assert_eq!(
            result.task.contract.as_ref().unwrap().checks.get("sum"),
            first.task.contract.as_ref().unwrap().checks.get("sum")
        );
        assert!(
            result
                .task
                .contract
                .as_ref()
                .unwrap()
                .checks
                .contains_key("zero")
        );
        assert_eq!(result.task.certificates.len(), 2);
        let parent = host.session(session.id).await.unwrap();
        assert_eq!(parent.branch.pending_task, None);
        let seed = parent.branch.workspace.as_ref().unwrap();
        assert_eq!(seed.source, result.task.candidate.as_ref().unwrap().source);
        assert_eq!(Some(seed.origin), result.task.origin);
        let fork = host
            .fork_session(orvek_harness::session::SessionId::new(), parent.cursor())
            .await
            .unwrap();
        let handoff = host
            .handoff_session(orvek_harness::session::SessionId::new(), parent.cursor())
            .await
            .unwrap();
        assert_eq!(fork.history, parent.history);
        assert!(handoff.history.is_empty());
        for branch in [&fork, &handoff] {
            assert_eq!(branch.current_task, None);
            assert_eq!(branch.outcome, None);
            assert_eq!(branch.branch.workspace, parent.branch.workspace);
            let source: orvek_harness::workspace::Snapshot = serde_json::from_slice(
                &artifact_bytes(&host, branch.branch.workspace.as_ref().unwrap().source).await,
            )
            .unwrap();
            assert!(
                matches!(source.entries.get("add"), Some(orvek_harness::workspace::Entry::File { content, .. }) if *content == Digest::of(after.as_bytes()))
            );
        }
        assert_eq!(
            host.task(result.task.id).await.unwrap(),
            result.task,
            "new contexts cannot reset the original task or its spend"
        );
        stop.cancel();
        service.await.unwrap().unwrap();
        result
    } else if ordinary_action {
        host.submit(
            session.id,
            request_id,
            vec![json!({"type":"input_text","text":"Fix addition"})],
            orvek_harness::submission::SubmitIntent::Ordinary {
                limits: contract.limits,
                policy: policy.clone(),
                schedule: orvek_harness::submission::Schedule::Queue,
            },
        )
        .await
        .unwrap();
        host.start_queued().await.unwrap();
        let settled = timeout(Duration::from_secs(90), async {
            loop {
                let submission = host.submission(session.id, request_id).await.unwrap();
                if !submission.status.pending() {
                    break submission;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let orvek_harness::submission::SubmissionStatus::Finished {
            task: Some(task), ..
        } = settled.status
        else {
            panic!("ordinary action must produce a task: {settled:?}")
        };
        let task = host.task(task).await.unwrap();
        orvek_harness::controller::TaskRun {
            session: session.id,
            task,
            message: String::new(),
        }
    } else if ordinary_information {
        host.submit(
            session.id,
            request_id,
            vec![json!({"type":"input_text","text":"Fix addition"})],
            orvek_harness::submission::SubmitIntent::Ordinary {
                limits: contract.limits,
                policy: policy.clone(),
                schedule: orvek_harness::submission::Schedule::Queue,
            },
        )
        .await
        .unwrap();
        host.start_queued().await.unwrap();
        let settled = timeout(Duration::from_secs(90), async {
            loop {
                let submission = host.submission(session.id, request_id).await.unwrap();
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
                    ..
                }
            ),
            "{settled:?}"
        );
        let report: orvek_harness::auxiliary::AuxiliaryReport = serde_json::from_slice(
            &artifact_bytes(&host, settled.result.expect("durable answer")).await,
        )
        .unwrap();
        assert_eq!(
            report.status,
            orvek_harness::auxiliary::AuxiliaryStatus::Completed
        );
        // The classifier and the answer are both charged to this ordinary
        // submission, so its report aggregates both: one classification call
        // plus one answer call. This mirrors the action path, where the
        // classifier is charged to the adopted task and counted in its five
        // model calls. Reporting only the answer would hide real spend.
        assert_eq!(report.model_calls, 2);
        assert_eq!(report.tokens, Some(12));
        let state = host.session(session.id).await.unwrap();
        assert_eq!(state.current_task, None);
        assert_eq!(state.tasks_by_request, BTreeMap::new());
        assert_eq!(state.history.len(), 1);
        assert_eq!(state.active_request, None);
        assert_eq!(fs::read_to_string(source.join("add")).unwrap(), before);
        return;
    } else if natural {
        host.execute_request(
            session.id,
            request_id,
            "Fix addition".into(),
            contract.limits,
            policy.clone(),
            CancellationToken::new(),
            sink,
        )
        .await
        .unwrap()
    } else {
        host.execute_contract_request(
            session.id,
            request_id,
            "Fix addition".into(),
            contract.clone(),
            CancellationToken::new(),
            sink,
        )
        .await
        .unwrap()
    };
    if rate_limit_exhaustion {
        assert_eq!(result.task.outcome, Some(Outcome::Failed), "{result:?}");
        assert_eq!(result.task.usage.model_calls, 3);
        assert_eq!(result.task.usage.tokens, 0);
        assert_eq!(result.task.model_receipts.len(), 3);
        assert!(
            result
                .task
                .model_receipts
                .values()
                .all(|receipt| receipt.tokens == Some(0))
        );
        assert!(result.task.certificates.is_empty());
        assert_eq!(served.await.unwrap().len(), 3);
        return;
    }
    if rate_limit_retry {
        assert_eq!(result.task.outcome, Some(Outcome::Complete), "{result:?}");
        assert_eq!(result.task.usage.model_calls, 4);
        assert_eq!(result.task.usage.tokens, 18);
        assert_eq!(result.task.model_receipts.len(), 4);
        assert_eq!(
            result
                .task
                .model_receipts
                .values()
                .filter(|receipt| receipt.tokens == Some(0))
                .count(),
            1
        );
        assert_eq!(result.task.certificates.len(), 1);
        let certificate = &result.task.certificates[0];
        assert_eq!(
            fs::read_to_string(
                state_root
                    .join("deliveries")
                    .join(result.task.id.to_string())
                    .join(certificate.source.to_string())
                    .join("add")
            )
            .unwrap(),
            after
        );
        assert_eq!(served.await.unwrap().len(), 4);
        return;
    }
    if questions {
        assert_eq!(result.task.outcome, Some(Outcome::Blocked));
        assert_eq!(result.task.usage.model_calls, 2);
        assert!(result.task.certificates.is_empty());
        assert!(
            orvek_harness::completion::evaluate(&result.task, orvek_harness::store::now_ms())
                .is_err()
        );
        let requests = served.await.unwrap();
        assert!(requests[1]["input"].to_string().contains("research-ok"));
        return;
    }
    let user_edit = "#!/bin/sh\nprintf 'user is editing a different version\\n'\n";
    if resume {
        assert_eq!(result.task.outcome, Some(Outcome::Blocked));
        fs::write(source.join("add"), user_edit).unwrap();
        drop(host);
        let client = ResponsesClient::new(
            Auth::api_key(SecretString::new("fixture-key".into())).unwrap(),
            Route::new(Transport::Http, &endpoint).unwrap(),
            InferenceLimits {
                max_attempts: 1,
                ..InferenceLimits::default()
            },
        )
        .unwrap();
        host = Arc::new(
            Host::open(
                &state_root,
                client,
                DockerExecutor::connect("debian:bookworm-slim")
                    .await
                    .unwrap(),
            )
            .unwrap(),
        );
        let recorded = updates.clone();
        let resume_request = uuid::Uuid::new_v4();
        let revision = result.task.revision;
        result = host
            .resume_task_request(
                session.id,
                resume_request,
                result.task.id,
                revision,
                "The prerequisite is available; continue".into(),
                CancellationToken::new(),
                Arc::new(move |update| recorded.lock().unwrap().push(update)),
            )
            .await
            .unwrap();
        let replay = host
            .resume_task_request(
                session.id,
                resume_request,
                result.task.id,
                revision,
                "The prerequisite is available; continue".into(),
                CancellationToken::new(),
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        assert_eq!(replay.task, result.task);
    }
    let calls = if ordinary_action {
        5
    } else if queued {
        7
    } else if natural {
        5
    } else if resume {
        4
    } else {
        3
    };
    assert_eq!(result.task.outcome, Some(Outcome::Complete), "{result:?}");
    assert_eq!(result.task.usage.model_calls, calls);
    assert_eq!(
        fs::read_to_string(source.join("add")).unwrap(),
        if resume { user_edit } else { before },
        "source workspace must not be overwritten by agent execution"
    );
    let certificate = &result.task.certificates[0];
    let directory = state_root
        .join("deliveries")
        .join(result.task.id.to_string());
    if delivery == DeliveryKind::Source {
        assert_eq!(
            fs::read_to_string(directory.join(certificate.source.to_string()).join("add")).unwrap(),
            after
        );
    } else {
        let patch = fs::read(directory.join(format!("{}.patch", certificate.artifact))).unwrap();
        assert_eq!(Digest::of(&patch), certificate.artifact);
        assert!(String::from_utf8_lossy(&patch).contains("diff --git"));
        assert!(result.task.candidate.as_ref().unwrap().provenance.is_some());
    }
    if completion_tool {
        let session = host.session(session.id).await.unwrap();
        assert!(session.history.iter().any(
            |item| item["type"] == "function_call_output" && item["call_id"] == "call_complete"
        ));
        assert_eq!(session.active_request, None);
    }
    let replay = if queued || ordinary_action {
        let submission = host
            .submit(
                session.id,
                request_id,
                vec![json!({"type":"input_text","text":"Fix addition"})],
                if queued {
                    orvek_harness::submission::SubmitIntent::NewTask {
                        limits: contract.limits,
                        policy,
                    }
                } else {
                    orvek_harness::submission::SubmitIntent::Ordinary {
                        limits: contract.limits,
                        policy,
                        schedule: orvek_harness::submission::Schedule::Queue,
                    }
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            submission.status,
            orvek_harness::submission::SubmissionStatus::Finished {
                outcome: Some(Outcome::Complete),
                ..
            }
        ));
        result.clone()
    } else if natural {
        host.execute_request(
            session.id,
            request_id,
            "Fix addition".into(),
            contract.limits,
            policy,
            CancellationToken::new(),
            Arc::new(|_| {}),
        )
        .await
        .unwrap()
    } else {
        host.execute_contract_request(
            session.id,
            request_id,
            "Fix addition".into(),
            contract,
            CancellationToken::new(),
            Arc::new(|_| {}),
        )
        .await
        .unwrap()
    };
    assert_eq!(replay.task.id, result.task.id);
    assert_eq!(replay.task.revision, result.task.revision);
    assert_eq!(replay.task.usage.model_calls, calls);
    let requests = served.await.unwrap();
    let provider_requests = calls;
    assert_eq!(requests.len(), provider_requests as usize);
    if natural {
        assert!(
            requests[0]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "write_file")
        );
        let early_results = requests[1]["input"].as_array().unwrap();
        for call in ["call_early_write", "call_early_exec"] {
            let result = early_results
                .iter()
                .find(|item| item["type"] == "function_call_output" && item["call_id"] == call)
                .unwrap();
            assert!(
                !result["output"].as_str().unwrap().contains("\"error\""),
                "{result}"
            );
        }
        assert!(
            requests[1]["input"]
                .to_string()
                .contains("early-workspace-ok")
        );
        assert!(result.task.contract_admission.is_some());
        assert!(result.task.intake.is_some());
    }
    assert!(
        requests[if natural {
            3
        } else if ordinary_action {
            4
        } else {
            1
        }]["input"]
            .to_string()
            .contains("host rejected completion")
    );
    assert_eq!(
        updates
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(
                event,
                HostUpdate::Finished {
                    outcome: Outcome::Complete,
                    ..
                }
            ))
            .count(),
        if queued || ordinary_action { 0 } else { 1 }
    );
    assert!(
        result
            .task
            .evidence
            .iter()
            .any(|e| e.observation.status == orvek_harness::state::CheckStatus::Failed)
    );
    assert!(
        result
            .task
            .evidence
            .iter()
            .any(|e| e.observation.status == orvek_harness::state::CheckStatus::Passed)
    );
    if queued {
        let shell = uuid::Uuid::new_v4();
        host.submit(
            session.id,
            shell,
            vec![],
            orvek_harness::submission::SubmitIntent::Shell {
                spec: orvek_harness::manual::ShellSpec {
                    command: "printf broken > add; printf 'Done: this is only command output'"
                        .into(),
                    expected_task: Some(result.task.id),
                    scope_revision: Some(result.task.scope_revision),
                    timeout_ms: 10_000,
                    output_bytes: 4096,
                },
            },
        )
        .await
        .unwrap();
        let report = wait_shell(&host, session.id, shell).await;
        assert!(report.adopted, "{report:?}");
        let changed = host.task(result.task.id).await.unwrap();
        assert_eq!(changed.outcome, Some(Outcome::Blocked));
        assert_eq!(changed.request, result.task.request);
        assert_eq!(changed.initial_limits, result.task.initial_limits);
        assert_eq!(
            changed.usage, result.task.usage,
            "shell must not invoke a model or reset spend"
        );
        assert_eq!(changed.certificates, result.task.certificates);
        assert!(changed.workspace_override.is_some());
        assert!(
            orvek_harness::completion::evaluate(&changed, orvek_harness::store::now_ms()).is_err()
        );
        let (view, _) = host
            .inspect_task_review(changed.id, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(view.snapshot, changed.workspace_override);
        let resumed = host
            .resume_task_request(
                session.id,
                uuid::Uuid::new_v4(),
                changed.id,
                changed.revision,
                "Inspect the user shell edits".into(),
                CancellationToken::new(),
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        assert_eq!(resumed.task.workspace_override, None);
        assert_eq!(
            fs::read_to_string(
                state_root
                    .join("workspaces")
                    .join(changed.id.to_string())
                    .join("working/add")
            )
            .unwrap(),
            "broken"
        );
        assert_eq!(fs::read_to_string(source.join("add")).unwrap(), before);
        assert_ne!(resumed.task.outcome, Some(Outcome::Complete));
    }
}

async fn artifact_bytes(host: &Host, digest: Digest) -> Vec<u8> {
    use base64::Engine;
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

async fn wait_shell(
    host: &Host,
    session: orvek_harness::session::SessionId,
    request: uuid::Uuid,
) -> orvek_harness::manual::ShellReport {
    timeout(Duration::from_secs(30), async {
        loop {
            let submission = host.submission(session, request).await.unwrap();
            if !submission.status.pending() {
                return serde_json::from_slice(
                    &artifact_bytes(
                        host,
                        submission.result.unwrap_or_else(|| {
                            panic!("shell has no report: {:?}", submission.status)
                        }),
                    )
                    .await,
                )
                .unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

async fn wait_submission(
    host: &Host,
    session: orvek_harness::session::SessionId,
    request: uuid::Uuid,
) -> orvek_harness::controller::TaskRun {
    timeout(Duration::from_secs(90), async {
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
