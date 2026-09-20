//! End-to-end subagent engine test: a scripted provider drives a child that
//! reads the workspace, records a journaled task job, and submits a
//! schema-validated result observed through the lifecycle event stream.

use orvek_harness::{
    Digest, Store,
    capabilities::{ToolContext, ToolRun, WorkspaceTools},
    controller::subagents::{ChildRun, ChildToolBackend, SubagentEvent, Subagents},
    inference::{Limits, Model, ModelSettings, ResponsesClient, Route, Transport, UsdCost},
    runtime::ExecutionEnvironment,
    session::{SessionCommand, SessionEvent},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct Reply {
    status: u16,
    body: Vec<u8>,
}
impl Reply {
    fn sse(events: Vec<Value>) -> Self {
        let body = events
            .into_iter()
            .map(|value| {
                format!(
                    "event: {}\r\ndata: {value}\r\n\r\n",
                    value["type"].as_str().unwrap()
                )
            })
            .collect::<String>()
            .into_bytes();
        Self { status: 200, body }
    }
}

async fn read_request(socket: &mut TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.unwrap(), 1);
        bytes.push(byte[0]);
    }
    let text = String::from_utf8(bytes.clone()).unwrap();
    let length = text
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
    body
}

async fn server(replies: Vec<Reply>) -> (String, tokio::task::JoinHandle<Vec<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let served = tokio::spawn(async move {
        let mut captures = Vec::new();
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            captures.push(read_request(&mut socket).await);
            let cost = if reply.status == 200 {
                "X-LiteLLM-Response-Cost: 0.0001\r\n"
            } else {
                ""
            };
            let head = format!(
                "HTTP/1.1 {} Fixture\r\nContent-Type: text/event-stream; charset=utf-8\r\n{cost}Connection: close\r\n\r\n",
                reply.status
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(&reply.body).await.unwrap();
            socket.shutdown().await.unwrap();
        }
        captures
    });
    (base, served)
}

/// File-only backend using the real workspace tools without contacting Docker.
struct StubTools;

impl ChildToolBackend for StubTools {
    fn definitions(&self) -> Vec<Value> {
        vec![json!({
            "type": "function",
            "name": "read_file",
            "description": "Read a bounded byte range of a workspace file.",
            "parameters": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }
        })]
    }

    fn environment(&self) -> ExecutionEnvironment {
        fixture_environment()
    }

    fn execute(
        &self,
        name: String,
        arguments: Value,
        context: ToolContext,
        cancellation: CancellationToken,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolRun> + Send + 'static>> {
        Box::pin(async move {
            ToolRun {
                result: WorkspaceTools::execute_file_tool(
                    &name,
                    arguments,
                    &context,
                    &cancellation,
                ),
                execution: None,
                diagnostic: None,
            }
        })
    }
}

fn fixture_environment() -> ExecutionEnvironment {
    ExecutionEnvironment {
        daemon_id: "fixture-daemon".into(),
        endpoint: "unix:///fixture/docker.sock".into(),
        protocol_version: 1,
        image_id: "sha256:fixture".into(),
        memory_bytes: 1024,
        pids: 32,
        cpus: 1,
        network: "none".into(),
        helper_digest: Digest::of(b"fixture helper"),
        architecture: "aarch64".into(),
        workspace_bytes: 1024,
        workspace_inodes: 32,
        cache_bytes: 1024,
        cache_inodes: 32,
        temporary_bytes: 1024,
        temporary_inodes: 32,
        source_transport: "fixture".into(),
        writable_mount_options: String::new(),
    }
}

fn client(base: &str) -> ResponsesClient {
    ResponsesClient::new(
        orvek_harness::inference::auth::Auth::api_key(
            orvek_harness::inference::auth::SecretString::new("test-key".to_owned()),
        )
        .unwrap(),
        Route::new(Transport::Http, &format!("{base}/responses")).unwrap(),
        Limits {
            max_attempts: 1,
            ..Limits::default()
        },
    )
    .unwrap()
}

fn function_call_item(call_id: &str, name: &str, arguments: &str) -> Value {
    json!({
        "type": "function_call",
        "id": format!("fc-{call_id}"),
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
        "status": "completed"
    })
}

fn completed(output: Value) -> Vec<Value> {
    vec![json!({
        "type": "response.completed",
        "response": {
            "id": "response-fixture",
            "status": "completed",
            "output": [output],
            "usage": {"input_tokens": 5, "output_tokens": 5, "total_tokens": 10}
        }
    })]
}

#[tokio::test]
async fn child_reads_the_workspace_records_a_job_and_submits_a_valid_result() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("answer.txt"), b"42").unwrap();

    let mut store = Store::open(&root.path().join("state")).unwrap();
    let session = orvek_harness::session::SessionId::new();
    store
        .create_session(
            session,
            orvek_harness::session::SessionConfig {
                workspace: workspace.clone(),
                model: ModelSettings::default(),
                instructions: String::new(),
                context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            },
            None,
        )
        .unwrap();
    let policy = store
        .public_artifacts()
        .write(
            &serde_json::to_vec(&json!({
                "version": 1,
                "delivery": "source",
                "profile": {"version": 1, "name": "fixture", "checks": {}}
            }))
            .unwrap(),
        )
        .unwrap()
        .digest();
    let request = Uuid::new_v4();
    let (_, task, _) = store
        .start_request(session, request, "probe".into(), Default::default(), policy)
        .unwrap();
    // Turn 1: the child reads the workspace; turn 2: it submits its result.
    let (base, served) = server(vec![
        Reply {
            status: 429,
            body: Vec::new(),
        },
        Reply::sse(completed(function_call_item(
            "call-1",
            "read_file",
            r#"{"path":"answer.txt"}"#,
        ))),
        Reply::sse(completed(function_call_item(
            "call-2",
            "submit_result",
            r#"{"result":{"answer":"42","source":"answer.txt"}}"#,
        ))),
    ])
    .await;

    let engine = Subagents::new();
    let mut events = engine.subscribe();
    let mut run = ChildRun {
        session,
        request,
        task: task.id,
        scope_revision: task.scope_revision,
        working: workspace.clone(),
        model: ModelSettings::default(),
        provider: Arc::new(client(&base)),
        tools: std::sync::Arc::new(StubTools),
        store: Arc::new(tokio::sync::Mutex::new(store)),
    };
    let spawn = engine
        .execute(
            "spawn_agent",
            json!({
                "role": "researcher",
                "task": "Read answer.txt and report its content.",
                "model": "selected",
                "output_schema": {
                    "type": "object",
                    "properties": {"answer": {"type": "string"}},
                    "required": ["answer"]
                }
            }),
            &run,
            CancellationToken::new(),
        )
        .await;
    let agent = spawn["agent_id"].as_str().expect("spawned").to_owned();

    let waited = loop {
        let waited = engine
            .execute(
                "wait_agent",
                json!({"agent_ids": [agent]}),
                &run,
                CancellationToken::new(),
            )
            .await;
        if waited["timed_out"] == json!(false) {
            break waited;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let reported = &waited["agents"][0];
    assert_eq!(
        reported["status"],
        json!("completed"),
        "child failed: {:?}",
        reported["error"]
    );
    assert_eq!(
        reported["result"]["answer"],
        json!("42"),
        "the child read the file and submitted a schema-valid result"
    );

    let spawned = events.try_recv().expect("spawned event");
    assert!(matches!(spawned, SubagentEvent::Spawned { .. }));
    let returned = events.try_recv().expect("returned event");
    let output = match returned {
        SubagentEvent::Returned { output, .. } => output,
        other => panic!("expected Returned, got {other:?}"),
    };
    assert_ne!(output, Digest::of(b""));

    engine.set_policy(true, false, 8);
    run.model.model = Model::Luna;
    let rejected = engine
        .execute(
            "spawn_agent",
            json!({
                "role": "researcher",
                "task": "must not run",
                "model": "selected",
                "output_schema": {"type": "object"}
            }),
            &run,
            CancellationToken::new(),
        )
        .await;
    assert_eq!(
        rejected["error"],
        "Luna subagents are disabled by configuration"
    );

    let bundle = orvek_harness::trace::TraceBundle::export(
        &root.path().join("state"),
        None,
        Default::default(),
        &Default::default(),
        None,
    )
    .unwrap();
    let replay = bundle.replay().unwrap();
    assert!(replay.exact, "{:?}", replay.unresolved);
    let dispatches = replay
        .spans
        .iter()
        .filter(|span| span["span"]["kind"] == "model_dispatch")
        .collect::<Vec<_>>();
    assert_eq!(dispatches.len(), 3);
    assert!(dispatches.iter().all(|span| span["span"]["child"] == agent));
    let captures = served.await.unwrap();
    assert_eq!(captures.len(), 3);
    assert_eq!(
        captures[0], captures[1],
        "a fresh call retries the same body"
    );
    let mut calls = std::collections::BTreeSet::new();
    for (dispatch, captured) in dispatches.iter().zip(captures) {
        let dispatch = &dispatch["span"];
        assert!(calls.insert(dispatch["call"].as_str().unwrap()));
        assert_eq!(dispatch["payload_kind"], "logical_http_template");
        let response = replay
            .spans
            .iter()
            .find(|span| {
                span["span"]["kind"] == "model_response" && span["span"]["call"] == dispatch["call"]
            })
            .unwrap();
        let response = &response["span"];
        for field in ["session", "request", "task", "child", "call"] {
            assert_eq!(response[field], dispatch[field]);
        }
        let prepared = &response["outcome"]["request"];
        assert_eq!(prepared["status"], "prepared");
        assert_eq!(prepared["transport"], "http");
        assert_eq!(prepared["dialect"], "open_ai");
        assert_eq!(prepared["body"].as_str().unwrap().as_bytes(), captured);
        assert!(!response.to_string().contains("test-key"));
    }

    let tool = replay
        .spans
        .iter()
        .find(|span| span["span"]["kind"] == "tool_dispatch")
        .unwrap();
    assert_eq!(tool["span"]["tool_call"], "call-1");
    assert_eq!(tool["span"]["call"], dispatches[1]["span"]["call"]);
    assert_eq!(replay.causality.calls.len(), 3);
    assert!(
        replay
            .causality
            .calls
            .values()
            .all(|call| call.prepared_body_checked)
    );
    assert!(replay.causality.calls.values().all(|call| {
        call.gaps
            .contains(&orvek_harness::trace::CausalGap::ChildOriginUnavailable)
    }));
    assert_eq!(replay.causality.tools.len(), 1);
    assert!(
        replay
            .causality
            .tools
            .values()
            .all(|tool| matches!(tool.result, orvek_harness::trace::RecordedStatus::Served))
    );
    assert_eq!(replay.cost.calls, 3);
    assert!(replay.cost.complete);
    assert_eq!(replay.cost.total_tokens, Some(20));
    assert!(dispatches[0]["span"]["source_revision"].is_string());
    let prefixes = bundle
        .prefixes()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(prefixes.len(), 3);
    for prefix in prefixes {
        assert_eq!(prefix.prefix.through + 1, prefix.before_sequence);
        assert!(prefix.decision["logical_request_payload"]["input"].is_array());
        let replay = prefix.prefix.replay().unwrap();
        assert!(
            replay
                .spans
                .iter()
                .all(|span| span["span"]["kind"] != "child_terminal")
        );
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        for payload in prefix.prefix.artifacts.values() {
            if let orvek_harness::trace::Payload::Present(encoded) = payload {
                let bytes = STANDARD.decode(encoded).unwrap();
                assert!(
                    !String::from_utf8_lossy(&bytes).contains(r#""call_id":"call-2""#),
                    "future response leaked into an N-1 prefix"
                );
            }
        }
    }

    // The child's read was journaled as a read-only task job.
    let store = run.store.lock().await;
    let state = store.load(task.id).unwrap();
    assert!(
        state.jobs.iter().any(|(_id, job)| {
            job.invocation
                .as_ref()
                .is_some_and(|invocation| invocation.capability == "read_file")
        }),
        "child tool execution is journaled"
    );
    let costs = store
        .journal_page(0, 256)
        .unwrap()
        .into_iter()
        .filter_map(|record| serde_json::from_value::<SessionEvent>(record.event).ok())
        .filter_map(|event| match event {
            SessionEvent::Command {
                command: SessionCommand::ProviderCost { cost_usd, .. },
                ..
            } => cost_usd,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        costs.len(),
        3,
        "each child model call has a durable cost receipt"
    );
    assert_eq!(
        costs
            .into_iter()
            .try_fold(UsdCost::ZERO, UsdCost::checked_add)
            .unwrap()
            .to_string(),
        "$0.0002"
    );
}

#[tokio::test]
async fn invalid_submission_can_be_repaired_with_the_callers_schema() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let mut store = Store::open(&root.path().join("state")).unwrap();
    let session = orvek_harness::session::SessionId::new();
    store
        .create_session(
            session,
            orvek_harness::session::SessionConfig {
                workspace: workspace.clone(),
                model: ModelSettings::default(),
                instructions: String::new(),
                context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            },
            None,
        )
        .unwrap();
    let policy = store
        .public_artifacts()
        .write(
            &serde_json::to_vec(&json!({
                "version": 1,
                "delivery": "source",
                "profile": {"version": 1, "name": "fixture", "checks": {}}
            }))
            .unwrap(),
        )
        .unwrap()
        .digest();
    let (_, task, _) = store
        .start_request(
            session,
            Uuid::new_v4(),
            "probe".into(),
            Default::default(),
            policy,
        )
        .unwrap();

    // The child submits a result missing the required field, then a plain
    // message ends its turn without a valid submission.
    let (base, served) = server(vec![
        Reply::sse(completed(function_call_item(
            "call-1",
            "submit_result",
            r#"{"result":{"wrong":"shape"}}"#,
        ))),
        Reply::sse(completed(function_call_item(
            "call-2",
            "submit_result",
            r#"{"result":{"caller_verdict":"accepted"}}"#,
        ))),
    ])
    .await;

    let engine = Subagents::new();
    let run = ChildRun {
        session,
        request: Uuid::new_v4(),
        task: task.id,
        scope_revision: task.scope_revision,
        working: workspace.clone(),
        model: ModelSettings::default(),
        provider: Arc::new(client(&base)),
        tools: std::sync::Arc::new(StubTools),
        store: Arc::new(tokio::sync::Mutex::new(store)),
    };
    let spawn = engine
        .execute(
            "spawn_agent",
            json!({
                "role": "researcher",
                "task": "irrelevant",
                "model": "selected",
                "output_schema": {
                    "type": "object",
                    "properties": {"caller_verdict": {"type": "string", "enum": ["accepted", "rejected"]}},
                    "required": ["caller_verdict"]
                }
            }),
            &run,
            CancellationToken::new(),
        )
        .await;
    let agent = spawn["agent_id"].as_str().expect("spawned").to_owned();

    let waited = loop {
        let waited = engine
            .execute(
                "wait_agent",
                json!({"agent_ids": [agent]}),
                &run,
                CancellationToken::new(),
            )
            .await;
        if waited["timed_out"] == json!(false) {
            break waited;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let reported = &waited["agents"][0];
    assert_eq!(
        reported["status"],
        json!("completed"),
        "invalid submission must receive feedback and permit repair"
    );
    let captures = served.await.unwrap();
    let first: Value = serde_json::from_slice(&captures[0]).unwrap();
    let submit = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "submit_result")
        .unwrap();
    assert_eq!(
        submit["parameters"]["properties"]["result"]["required"],
        json!(["caller_verdict"])
    );
    assert_eq!(
        submit["parameters"]["properties"]["result"]["properties"]["caller_verdict"]["enum"],
        json!(["accepted", "rejected"])
    );
    let second: Value = serde_json::from_slice(&captures[1]).unwrap();
    assert!(second["input"].to_string().contains("caller_verdict"));
    assert!(second["input"].to_string().contains("resubmit"));
}

fn fixture_run(root: &std::path::Path, base: &str) -> ChildRun {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("answer.txt"), "42").unwrap();
    let mut store = Store::open(&root.join("state")).unwrap();
    let session = orvek_harness::session::SessionId::new();
    store
        .create_session(
            session,
            orvek_harness::session::SessionConfig {
                workspace: workspace.clone(),
                model: ModelSettings::default(),
                instructions: String::new(),
                context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            },
            None,
        )
        .unwrap();
    let policy = store
        .public_artifacts()
        .write(
            &serde_json::to_vec(&json!({
                "version":1,"delivery":"source","profile":{"version":1,"name":"fixture","checks":{}}
            }))
            .unwrap(),
        )
        .unwrap()
        .digest();
    let request = Uuid::new_v4();
    let (_, task, _) = store
        .start_request(
            session,
            request,
            "inspect repository".into(),
            Default::default(),
            policy,
        )
        .unwrap();
    ChildRun {
        session,
        request,
        task: task.id,
        scope_revision: task.scope_revision,
        working: workspace,
        model: ModelSettings::default(),
        provider: Arc::new(client(base)),
        tools: Arc::new(StubTools),
        store: Arc::new(tokio::sync::Mutex::new(store)),
    }
}

fn spawn_arguments() -> Value {
    json!({"role":"independent reviewer","task":"Find the answer and report it.","model":"selected",
        "output_schema":{"type":"object","properties":{"answer":{"type":"string","enum":["42"]}},"required":["answer"]}})
}

async fn spawn_and_wait(engine: &Subagents, run: &ChildRun, args: Value) -> Value {
    let spawned = engine
        .execute("spawn_agent", args, run, CancellationToken::new())
        .await;
    assert!(spawned["agent_id"].is_string(), "{spawned}");
    let waited = engine
        .execute(
            "wait_agent",
            json!({"agent_ids":[spawned["agent_id"]],"timeout_ms":5000}),
            run,
            CancellationToken::new(),
        )
        .await;
    assert_eq!(waited["timed_out"], false, "{waited}");
    waited["agents"][0].clone()
}

fn prose(text: &str) -> Value {
    json!({"type":"message","id":"prose","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]})
}

#[tokio::test]
async fn prose_is_unsubmitted_and_provider_failure_is_failed() {
    for (reply, expected) in [
        (
            Reply::sse(completed(prose("The answer is 42"))),
            "unsubmitted",
        ),
        (
            Reply {
                status: 500,
                body: vec![],
            },
            "failed",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let (base, served) = server(vec![reply]).await;
        let run = fixture_run(root.path(), &base);
        let engine = Subagents::new();
        let result = spawn_and_wait(&engine, &run, spawn_arguments()).await;
        assert_eq!(result["status"], expected);
        assert!(result["result"].is_null());
        assert!(result["result_digest"].is_null());
        if expected == "unsubmitted" {
            assert_eq!(result["unsubmitted_answer"], "The answer is 42");
            assert!(matches!(
                engine.snapshot(run.session).await[1],
                SubagentEvent::Unsubmitted { .. }
            ));
        }
        served.await.unwrap();
    }
}

#[tokio::test]
async fn repeated_invalid_submissions_do_not_add_an_early_stop_cap() {
    let root = tempfile::tempdir().unwrap();
    let mut replies = (0..5)
        .map(|n| {
            Reply::sse(completed(function_call_item(
                &format!("invalid-{n}"),
                "submit_result",
                r#"{"result":{"answer":"wrong"}}"#,
            )))
        })
        .collect::<Vec<_>>();
    replies.push(Reply::sse(completed(function_call_item(
        "valid",
        "submit_result",
        r#"{"result":{"answer":"42"}}"#,
    ))));
    let (base, served) = server(replies).await;
    let run = fixture_run(root.path(), &base);
    let result = spawn_and_wait(&Subagents::new(), &run, spawn_arguments()).await;
    assert_eq!(result["status"], "completed", "{result}");
    let captures = served.await.unwrap();
    assert_eq!(captures.len(), 6);
    assert_eq!(
        serde_json::from_slice::<Value>(&captures[5]).unwrap()["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .count(),
        5
    );
}

async fn record_parent_discovery(run: &ChildRun) {
    let mut store = run.store.lock().await;
    let state = store.load_session(run.session).unwrap();
    let state = store
        .session_command(
            run.session,
            state.revision,
            Uuid::new_v4(),
            SessionCommand::Response {
                request: run.request,
                items: vec![function_call_item(
                    "parent-read",
                    "read_file",
                    r#"{"path":"answer.txt"}"#,
                )],
            },
        )
        .unwrap();
    let state = store
        .session_command(
            run.session,
            state.revision,
            Uuid::new_v4(),
            SessionCommand::ToolResult {
                request: run.request,
                call_id: "parent-read".into(),
                output: r#"{"answer":"42","source":"answer.txt"}"#.into(),
            },
        )
        .unwrap();
    store
        .session_command(
            run.session,
            state.revision,
            Uuid::new_v4(),
            SessionCommand::Response {
                request: run.request,
                items: vec![
                    prose("PARENT_CONCLUSION answer=42 source=answer.txt"),
                    function_call_item("unfinished-spawn", "spawn_agent", "{}"),
                ],
            },
        )
        .unwrap();
}

#[tokio::test]
async fn isolated_and_forked_workers_have_pinned_context_but_identical_permissions() {
    let mut measurements = Vec::new();
    for mode in ["isolated", "fork_at_cursor"] {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let served = tokio::spawn(async move {
            let mut captures = Vec::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let request: Value =
                    serde_json::from_slice(&read_request(&mut socket).await).unwrap();
                let knows_answer = request["input"].to_string().contains("42");
                let output = if knows_answer {
                    function_call_item("done", "submit_result", r#"{"result":{"answer":"42"}}"#)
                } else {
                    function_call_item("discover", "read_file", r#"{"path":"answer.txt"}"#)
                };
                captures.push(request);
                let reply = Reply::sse(completed(output));
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await.unwrap();
                socket.write_all(&reply.body).await.unwrap();
                socket.shutdown().await.unwrap();
                if knows_answer {
                    break;
                }
            }
            captures
        });
        let run = fixture_run(root.path(), &base);
        record_parent_discovery(&run).await;
        let cutoff = run
            .store
            .lock()
            .await
            .load_session(run.session)
            .unwrap()
            .cursor();
        let engine = Subagents::new();
        let mut args = spawn_arguments();
        if mode == "fork_at_cursor" {
            args["context_mode"] = json!(mode);
        }
        let spawned = engine
            .execute("spawn_agent", args, &run, CancellationToken::new())
            .await;
        assert!(spawned["agent_id"].is_string(), "{spawned}");
        {
            let mut store = run.store.lock().await;
            let state = store.load_session(run.session).unwrap();
            store
                .session_command(
                    run.session,
                    state.revision,
                    Uuid::new_v4(),
                    SessionCommand::Feedback {
                        message: "FUTURE_PARENT_RECORD".into(),
                    },
                )
                .unwrap();
        }
        let waited = engine
            .execute(
                "wait_agent",
                json!({"agent_ids":[spawned["agent_id"]],"timeout_ms":5000}),
                &run,
                CancellationToken::new(),
            )
            .await;
        let result = &waited["agents"][0];
        assert_eq!(result["status"], "completed", "{waited}");
        let captures = served.await.unwrap();
        let first = &captures[0];
        assert_eq!(
            first["input"].to_string().contains("PARENT_CONCLUSION"),
            mode == "fork_at_cursor"
        );
        assert!(!first["input"].to_string().contains("unfinished-spawn"));
        assert!(!first["input"].to_string().contains("FUTURE_PARENT_RECORD"));
        assert_eq!(result["context"]["mode"], mode);
        assert_eq!(result["context"]["parent"], json!(cutoff));
        assert_eq!(result["context"]["frozen_workspace"], false);
        assert!(
            first["instructions"]
                .as_str()
                .unwrap()
                .contains("not a frozen snapshot")
        );
        assert_eq!(
            first["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["read_file", "submit_result"]
        );
        if mode == "fork_at_cursor" {
            assert_eq!(
                result["context"]["excluded_calls"],
                json!(["unfinished-spawn"])
            );
        }
        let store = run.store.lock().await;
        let parent = store.load_session_cursor(&cutoff).unwrap();
        assert_eq!(
            result["context"]["source_history"],
            json!(Digest::of_value(&parent.history).unwrap())
        );
        measurements.push((
            mode,
            store.load(run.task).unwrap().jobs.len(),
            result["result"].clone(),
        ));
    }
    assert_eq!(
        measurements[0].1, 1,
        "isolated worker rediscovered the source"
    );
    assert_eq!(measurements[1].1, 0, "fork used inherited discovery");
    assert_eq!(measurements[0].2, measurements[1].2);
    eprintln!(
        "deterministic discovery: isolated=1 read, fork=0 reads; exact scripted answers equal; live model quality unmeasured"
    );
}

#[tokio::test]
async fn messages_are_durably_accepted_then_consumed_before_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let served = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let first = read_request(&mut socket).await;
        ready_tx.send(()).unwrap();
        release_rx.await.unwrap();
        let reply = Reply::sse(completed(function_call_item(
            "read",
            "read_file",
            r#"{"path":"answer.txt"}"#,
        )));
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        socket.write_all(&reply.body).await.unwrap();
        socket.shutdown().await.unwrap();
        let (mut socket, _) = listener.accept().await.unwrap();
        let second = read_request(&mut socket).await;
        let reply = Reply::sse(completed(function_call_item(
            "done",
            "submit_result",
            r#"{"result":{"answer":"42"}}"#,
        )));
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        socket.write_all(&reply.body).await.unwrap();
        socket.shutdown().await.unwrap();
        (first, second)
    });
    let run = fixture_run(root.path(), &base);
    let engine = Subagents::new();
    let spawned = engine
        .execute(
            "spawn_agent",
            spawn_arguments(),
            &run,
            CancellationToken::new(),
        )
        .await;
    ready_rx.await.unwrap();
    let delivered = engine.execute("send_agent_message", json!({"agent_id":spawned["agent_id"],"message":"STEERING_EVIDENCE","purpose":"context","priority":"urgent"}), &run, CancellationToken::new()).await;
    assert_eq!(delivered["delivered"], true);
    let before = run.store.lock().await.journal_page(0, 256).unwrap();
    let accepted = before
        .iter()
        .filter(|r| r.event.to_string().contains("message_accepted"))
        .count();
    let consumed = before
        .iter()
        .filter(|r| r.event.to_string().contains("message_consumed"))
        .count();
    assert_eq!((accepted, consumed), (1, 0));
    release_tx.send(()).unwrap();
    let waited = engine
        .execute(
            "wait_agent",
            json!({"agent_ids":[spawned["agent_id"]],"timeout_ms":5000}),
            &run,
            CancellationToken::new(),
        )
        .await;
    assert_eq!(waited["agents"][0]["status"], "completed");
    let (first, second) = served.await.unwrap();
    assert!(
        !String::from_utf8(first)
            .unwrap()
            .contains("STEERING_EVIDENCE")
    );
    assert!(
        String::from_utf8(second)
            .unwrap()
            .contains("STEERING_EVIDENCE")
    );
    let mut store = run.store.lock().await;
    let events = store.journal_page(0, 256).unwrap();
    let accepted = events
        .iter()
        .position(|r| r.event.to_string().contains("message_accepted"))
        .unwrap();
    let consumed = events
        .iter()
        .position(|r| r.event.to_string().contains("message_consumed"))
        .unwrap();
    assert!(accepted < consumed);
    assert_eq!(
        events
            .iter()
            .filter(|r| r.event.to_string().contains("message_consumed"))
            .count(),
        1
    );
    let recovered = Subagents::recover(&mut store).unwrap();
    drop(store);
    let replay = recovered
        .execute(
            "wait_agent",
            json!({"agent_ids":[spawned["agent_id"]]}),
            &run,
            CancellationToken::new(),
        )
        .await;
    assert_eq!(waited, replay);
}

#[tokio::test]
async fn lifecycle_survives_fresh_process_crash_cuts_and_production_host_reopen() {
    use orvek_harness::controller::{
        Host,
        subagents::lifecycle::{Event, Outcome},
    };
    const ROOT: &str = "ORVEK_T08_LIFECYCLE_ROOT";
    const STAGE: &str = "ORVEK_T08_LIFECYCLE_STAGE";
    const CUT: &str = "ORVEK_T08_LIFECYCLE_CUT";
    if let Ok(root) = std::env::var(ROOT) {
        let root = std::path::PathBuf::from(root);
        let cut = std::env::var(CUT).unwrap();
        if std::env::var(STAGE).unwrap() == "write" {
            if cut == "actual_completion" {
                let (base, served) = server(vec![Reply::sse(completed(function_call_item(
                    "done",
                    "submit_result",
                    r#"{"result":{"answer":"42"}}"#,
                )))])
                .await;
                let run = fixture_run(&root, &base);
                let engine = Subagents::new();
                let result = spawn_and_wait(&engine, &run, spawn_arguments()).await;
                assert_eq!(result["status"], "completed");
                served.await.unwrap();
                std::fs::write(root.join("identity.json"), serde_json::to_vec(&json!({"session":run.session,"request":run.request,"task":run.task,"agent":result["agent_id"],"result":result["result_digest"]})).unwrap()).unwrap();
                std::fs::write(
                    root.join("recovered.json"),
                    serde_json::to_vec(&json!({"agents":[result],"timed_out":false})).unwrap(),
                )
                .unwrap();
                std::process::exit(73);
            }
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let run = fixture_run(&root, &base);
            let engine = Subagents::new();
            let spawned = engine
                .execute(
                    "spawn_agent",
                    spawn_arguments(),
                    &run,
                    CancellationToken::new(),
                )
                .await;
            let agent = Uuid::parse_str(spawned["agent_id"].as_str().unwrap()).unwrap();
            // Model the persistence cuts while the actual child provider remains unresponsive.
            // In particular, no child Returned event can be published in this process.
            let mut store = run.store.lock().await;
            let mut result = None;
            if cut != "after_spawn" {
                result = Some(
                    store
                        .public_artifacts()
                        .write(br#"{"answer":"42"}"#)
                        .unwrap()
                        .digest(),
                );
            }
            if cut == "before_publication" {
                let event = Event::Terminal {
                    agent,
                    outcome: Outcome::SchemaValid {
                        result: result.unwrap(),
                    },
                };
                let operation = Uuid::new_v5(&agent, b"child-terminal");
                let state = store.load_session(run.session).unwrap();
                let state = store
                    .session_command(
                        run.session,
                        state.revision,
                        operation,
                        SessionCommand::ChildLifecycle(Box::new(event.clone())),
                    )
                    .unwrap();
                store
                    .session_command(
                        run.session,
                        state.revision,
                        operation,
                        SessionCommand::ChildLifecycle(Box::new(event)),
                    )
                    .unwrap();
                assert!(
                    store
                        .session_command(
                            run.session,
                            state.revision,
                            operation,
                            SessionCommand::ChildLifecycle(Box::new(Event::Terminal {
                                agent,
                                outcome: Outcome::Failed {
                                    reason: "must not replace completion".into()
                                }
                            }))
                        )
                        .is_err()
                );
                assert_eq!(
                    engine.snapshot(run.session).await.len(),
                    1,
                    "terminal not published to registry"
                );
            }
            std::fs::write(root.join("identity.json"), serde_json::to_vec(&json!({"session":run.session,"request":run.request,"task":run.task,"agent":agent,"result":result})).unwrap()).unwrap();
            std::process::exit(73);
        }
        let identity: Value =
            serde_json::from_slice(&std::fs::read(root.join("identity.json")).unwrap()).unwrap();
        let session = serde_json::from_value(identity["session"].clone()).unwrap();
        // This is the production Host::open_backend recovery path, with a closed local endpoint.
        let host = Host::open_native(
            &root.join("state"),
            client("http://127.0.0.1:1"),
            Digest::of(b"fixture"),
        )
        .unwrap();
        let snapshot = host.subagent_snapshot(session).await;
        assert_eq!(snapshot.len(), 2);
        if matches!(cut.as_str(), "before_publication" | "actual_completion") {
            assert!(matches!(snapshot[1], SubagentEvent::Returned { .. }));
        } else {
            assert!(matches!(snapshot[1], SubagentEvent::Cancelled { .. }));
        }
        drop(host);
        let mut store = Store::open(&root.join("state")).unwrap();
        let engine = Subagents::recover(&mut store).unwrap();
        let task = store
            .load(serde_json::from_value(identity["task"].clone()).unwrap())
            .unwrap();
        let run = ChildRun {
            session,
            request: serde_json::from_value(identity["request"].clone()).unwrap(),
            task: task.id,
            scope_revision: task.scope_revision,
            working: root.join("workspace"),
            model: ModelSettings::default(),
            provider: Arc::new(client("http://127.0.0.1:1")),
            tools: Arc::new(StubTools),
            store: Arc::new(tokio::sync::Mutex::new(store)),
        };
        let first = engine
            .execute(
                "wait_agent",
                json!({"agent_ids":[identity["agent"]]}),
                &run,
                CancellationToken::new(),
            )
            .await;
        let second = engine
            .execute(
                "wait_agent",
                json!({"agent_ids":[identity["agent"]]}),
                &run,
                CancellationToken::new(),
            )
            .await;
        let listed = engine
            .execute("list_agents", json!({}), &run, CancellationToken::new())
            .await;
        assert_eq!(first, second);
        assert_eq!(first["agents"], listed["agents"]);
        let child = &first["agents"][0];
        if matches!(cut.as_str(), "before_publication" | "actual_completion") {
            assert_eq!(child["status"], "completed");
            assert_eq!(child["result"], json!({"answer":"42"}));
            assert_eq!(child["result_digest"], identity["result"]);
        } else {
            assert_eq!(child["status"], "interrupted");
            assert!(
                child["result"].is_null(),
                "unlinked orphan artifact is not authoritative"
            );
            assert!(child["result_digest"].is_null());
        }
        let store = run.store.lock().await;
        let journal = store.journal_page(0, 256).unwrap();
        assert_eq!(
            journal
                .iter()
                .filter(|r| r.event["data"]["command"]["type"] == "child_lifecycle"
                    && r.event["data"]["command"]["data"]["kind"] == "terminal")
                .count(),
            1
        );
        assert_eq!(
            journal
                .iter()
                .filter(|r| r.event["data"]["command"]["type"] == "child_lifecycle"
                    && r.event["data"]["command"]["data"]["kind"] == "spawned")
                .count(),
            1
        );
        let result_path = root.join("recovered.json");
        if result_path.exists() {
            assert_eq!(
                serde_json::from_slice::<Value>(&std::fs::read(&result_path).unwrap()).unwrap(),
                first
            );
        }
        std::fs::write(result_path, serde_json::to_vec(&first).unwrap()).unwrap();
        return;
    }
    for cut in [
        "after_spawn",
        "after_result_storage",
        "before_publication",
        "actual_completion",
    ] {
        let root = tempfile::tempdir().unwrap();
        for stage in ["write", "read", "read"] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "lifecycle_survives_fresh_process_crash_cuts_and_production_host_reopen",
                    "--nocapture",
                ])
                .env(ROOT, root.path())
                .env(STAGE, stage)
                .env(CUT, cut)
                .status()
                .unwrap();
            assert_eq!(
                status.code(),
                Some(if stage == "write" { 73 } else { 0 }),
                "{cut}/{stage}"
            );
        }
    }
}
