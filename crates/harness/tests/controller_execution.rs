use orvek_harness::{
    Digest, Store,
    contract::*,
    controller::{Host, HostUpdate},
    inference::{
        Limits as InferenceLimits, ModelSettings, ResponsesClient, Route, Transport,
        auth::{Auth, SecretString},
    },
    runtime::DockerExecutor,
    session::SessionConfig,
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

async fn provider(outputs: Vec<Vec<Value>>) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
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
    let root = tempfile::tempdir_in(directory).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("value"), "before").unwrap();
    let state_root = root.path().join("state");
    let store = Store::open(&state_root).unwrap();
    let artifacts = store.artifacts().clone();
    drop(store);
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
        .create_session(SessionConfig {
            workspace: source.clone(),
            model: ModelSettings::default(),
            instructions: String::new(),
        })
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
    let report = wait_shell(&host, &artifacts, session.id, request).await;
    assert!(matches!(
        report.status,
        orvek_harness::runtime::ExecutionStatus::Exited(0)
    ));
    assert!(report.adopted);
    assert_eq!(artifacts.read(report.stdout).unwrap(), b"done");
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
    let report = wait_shell(&host, &artifacts, session.id, next).await;
    assert_eq!(artifacts.read(report.stdout).unwrap(), b"afteruser note");
    assert!(report.adopted);
    let snapshot = Snapshot::load(report.after.unwrap(), &artifacts).unwrap();
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
        session::{SessionCommand, SessionId},
        submission::{SubmissionStatus, SubmitIntent},
    };
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.orvek/ct");
    fs::create_dir_all(&directory).unwrap();
    let root = tempfile::tempdir_in(directory).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("value"), "unchanged").unwrap();
    let state_root = root.path().join("state");
    let mut store = Store::open(&state_root).unwrap();
    let session = store
        .create_session(
            SessionId::new(),
            SessionConfig {
                workspace: source.clone().canonicalize().unwrap(),
                model: ModelSettings::default(),
                instructions: String::new(),
            },
            None,
        )
        .unwrap();
    let policy = store
        .artifacts()
        .put(
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
        .unwrap();
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
    let artifacts = store.artifacts().clone();
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
            if !matches!(
                submission.status,
                orvek_harness::submission::SubmissionStatus::Queued
            ) {
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
        &artifacts
            .read(settled.result.expect("durable answer artifact"))
            .unwrap(),
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
    exercise(DeliveryKind::Source, Mode::Ordinary).await;
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn ordinary_action_is_classified_then_admitted_as_one_task() {
    exercise(DeliveryKind::Source, Mode::OrdinaryInformation).await;
}

#[tokio::test]
#[ignore = "requires local Docker and pre-pulled debian:bookworm-slim"]
async fn controller_delivers_a_patch_that_reproduces_the_verified_source() {
    exercise(DeliveryKind::Patch, Mode::Ordinary).await;
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
async fn natural_request_cannot_edit_until_its_behavioral_contract_is_pinned() {
    exercise(DeliveryKind::Source, Mode::Natural).await;
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn disconnected_submission_and_followup_keep_one_task_and_its_original_checks() {
    exercise(DeliveryKind::Source, Mode::Queued).await;
}

enum Mode {
    Ordinary,
    ExplicitCompletion,
    Resume,
    Natural,
    Queued,
    OrdinaryInformation,
}

async fn exercise(delivery: DeliveryKind, mode: Mode) {
    let completion_tool = matches!(mode, Mode::ExplicitCompletion);
    let resume = matches!(mode, Mode::Resume);
    let queued = matches!(mode, Mode::Queued);
    let natural = matches!(mode, Mode::Natural | Mode::Queued);
    let ordinary_information = matches!(mode, Mode::OrdinaryInformation);
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.orvek/ct");
    fs::create_dir_all(&directory).unwrap();
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
    let state_root = root.path().join("protected-state");
    let store = Store::open(&state_root).unwrap();
    let artifacts = store.artifacts().clone();
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
        .artifacts()
        .put(&serde_json::to_vec(&program).unwrap())
        .unwrap();
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
        open_questions: vec![],
        delivery,
        limits: Limits {
            model_calls: if queued {
                8
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
    if ordinary_information {
        outputs.insert(
            0,
            vec![json!({"type":"message","id":"msg_classify","role":"assistant","status":"completed","content":[{"type":"output_text","text":"{\"kind\":\"action\"}","annotations":[]}]})],
        );
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
        let mut forbidden = write;
        forbidden["call_id"] = json!("call_forbidden");
        forbidden["id"] = json!("fc_forbidden");
        outputs.insert(0, vec![forbidden]);
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
    let (endpoint, served) = provider(outputs).await;
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
        .create_session(SessionConfig {
            workspace: source.clone(),
            model: ModelSettings::default(),
            instructions: String::new(),
        })
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
        let service = tokio::spawn(ipc::serve(host.clone(), stop.clone()));
        let socket = host.state_directory().join("host.sock");
        timeout(Duration::from_secs(5), async {
            while !socket.exists() {
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
            let source = orvek_harness::workspace::Snapshot::load(
                branch.branch.workspace.as_ref().unwrap().source,
                &artifacts,
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
        timeout(Duration::from_secs(90), async {
            loop {
                let _submission = host.submission(session.id, request_id).await.unwrap();
                tokio::time::sleep(Duration::from_millis(10)).await;
                let state = host.session(session.id).await.unwrap();
                let submission = host.submission(session.id, request_id).await.unwrap();
                if matches!(
                    submission.status,
                    orvek_harness::submission::SubmissionStatus::Running
                ) && state.current_task.is_some()
                {
                    break submission;
                }
            }
        })
        .await
        .unwrap();
        host.execute_resolved_submission(session.id, request_id, CancellationToken::new(), sink)
            .await
            .unwrap()
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
    let calls = if ordinary_information {
        4
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
    let replay = if queued {
        let submission = host
            .submit(
                session.id,
                request_id,
                vec![json!({"type":"input_text","text":"Fix addition"})],
                orvek_harness::submission::SubmitIntent::NewTask {
                    limits: contract.limits,
                    policy,
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
    assert_eq!(requests.len(), calls as usize);
    if natural {
        assert!(
            !requests[0]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "write_file")
        );
        assert!(requests[1]["input"].to_string().contains("not admitted"));
        assert!(result.task.contract_admission.is_some());
        assert!(result.task.intake.is_some());
    }
    assert!(
        requests[if natural { 3 } else { 1 }]["input"]
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
        if queued { 0 } else { 1 }
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
        let report = wait_shell(&host, &artifacts, session.id, shell).await;
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

async fn wait_shell(
    host: &Host,
    artifacts: &orvek_harness::artifacts::ArtifactStore,
    session: orvek_harness::session::SessionId,
    request: uuid::Uuid,
) -> orvek_harness::manual::ShellReport {
    timeout(Duration::from_secs(30), async {
        loop {
            let submission = host.submission(session, request).await.unwrap();
            if !submission.status.pending() {
                return serde_json::from_slice(
                    &artifacts
                        .read(submission.result.unwrap_or_else(|| {
                            panic!("shell has no report: {:?}", submission.status)
                        }))
                        .unwrap(),
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
