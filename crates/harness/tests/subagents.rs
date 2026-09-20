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

struct Reply(Vec<u8>);
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
        Self(body)
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

async fn server(replies: Vec<Reply>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut socket).await;
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nX-LiteLLM-Response-Cost: 0.0001\r\nConnection: close\r\n\r\n";
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(&reply.0).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    });
    base
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
        Limits::default(),
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
    let base = server(vec![
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
        2,
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
async fn schema_invalid_submissions_are_rejected() {
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
    let base = server(vec![
        Reply::sse(completed(function_call_item(
            "call-1",
            "submit_result",
            r#"{"result":{"wrong":"shape"}}"#,
        ))),
        Reply::sse(completed(json!({
            "type": "message",
            "id": "msg-final",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": "gave up"}]
        }))),
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
        json!("failed"),
        "an invalid submission cannot complete the child"
    );
}
