//! Scripted responses drive the real Host, dispatcher, journal and artifact store.
//! No live-provider cost or model-quality measurement is inferred from these fixtures.
use orvek_harness::{
    Channel, Digest,
    admission::{RepositoryProfile, RequestPolicy},
    contract::{DeliveryKind, Limits},
    controller::Host,
    inference::{
        Limits as InferenceLimits, ModelSettings, ResponsesClient, Route, Transport,
        auth::{Auth, SecretString},
    },
    session::{SessionAdmissionRequest, SessionCommand, SessionEvent, SessionId},
    state::Outcome,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn tool(id: &str, name: &str, args: Value) -> Value {
    json!({"type":"function_call","id":format!("fc_{id}"),"call_id":id,"name":name,"arguments":args.to_string(),"status":"completed"})
}
fn eval(id: &str, code: &str) -> Vec<Value> {
    vec![tool(id, "interpreter_eval", json!({"code":code}))]
}
fn done() -> Vec<Value> {
    vec![
        json!({"type":"message","id":"done","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Fixture finished","annotations":[]}]}),
    ]
}
async fn provider(replies: Vec<Vec<Value>>) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let mut replies = std::collections::VecDeque::from(replies);
    provider_with(move |_| {
        let output = replies.pop_front().expect("fixture response available");
        (output, replies.is_empty())
    })
    .await
}
async fn provider_with(
    mut reply: impl FnMut(&Value) -> (Vec<Value>, bool) + Send + 'static,
) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let mut requests = Vec::new();
        loop {
            let (mut socket, _) = timeout(Duration::from_secs(60), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                socket.read_exact(&mut b).await.unwrap();
                headers.push(b[0]);
            }
            let length = String::from_utf8(headers)
                .unwrap()
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|s| s.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            let mut body = vec![0; length];
            socket.read_exact(&mut body).await.unwrap();
            let request = serde_json::from_slice(&body).unwrap();
            let (output, last) = reply(&request);
            requests.push(request);
            let event = json!({"type":"response.completed","response":{"id":format!("r{}",requests.len()),"status":"completed","output":output,"usage":{"input_tokens":5,"output_tokens":1,"total_tokens":6}}});
            let payload = format!("event: response.completed\ndata: {event}\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            if last {
                break;
            }
        }
        requests
    });
    (endpoint, handle)
}
fn client(endpoint: &str) -> ResponsesClient {
    ResponsesClient::new(
        Auth::api_key(SecretString::new("fixture-key".into())).unwrap(),
        Route::new(Transport::Http, endpoint).unwrap(),
        InferenceLimits {
            max_attempts: 1,
            ..Default::default()
        },
    )
    .unwrap()
}
struct Fixture {
    directory: tempfile::TempDir,
    workspace: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        Self {
            directory,
            workspace,
        }
    }
    fn root(&self) -> PathBuf {
        self.directory.path().join("state")
    }
    fn native(&self, endpoint: &str) -> Host {
        Host::open_native(
            &self.root(),
            client(endpoint),
            Digest::of(b"fixture-config"),
        )
        .unwrap()
    }
    async fn session(&self, host: &Host) -> SessionId {
        host.create_session(SessionAdmissionRequest::new(
            self.workspace.clone(),
            ModelSettings::default(),
            orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            Channel::Stable,
        ))
        .await
        .unwrap()
        .id
    }
}
async fn run(host: &Host, session: SessionId, input: &str) -> orvek_harness::controller::TaskRun {
    timeout(
        Duration::from_secs(60),
        host.execute_request(
            session,
            Uuid::new_v4(),
            input.into(),
            Limits::default(),
            RequestPolicy {
                version: 1,
                delivery: DeliveryKind::Source,
                profile: RepositoryProfile {
                    version: 1,
                    name: "fixture".into(),
                    checks: Default::default(),
                },
            },
            CancellationToken::new(),
            Arc::new(|_| {}),
        ),
    )
    .await
    .unwrap()
    .unwrap()
}
async fn output(host: &Host, session: SessionId, id: &str) -> Value {
    let state = host.session(session).await.unwrap();
    let item = state
        .history
        .iter()
        .find(|v| v["type"] == "function_call_output" && v["call_id"] == id)
        .unwrap_or_else(|| panic!("missing {id}: {:?}", state.history));
    serde_json::from_str(item["output"].as_str().unwrap()).unwrap()
}
async fn artifact(host: &Host, digest: Digest) -> Value {
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
            break;
        }
    }
    serde_json::from_slice(&bytes).unwrap()
}
async fn assert_unknown_costs(host: &Host, expected_calls: usize) {
    let costs = host
        .journal_page(0, 256)
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
        .collect::<Vec<_>>();
    assert_eq!(costs.len(), expected_calls);
    assert!(costs.iter().all(Option::is_none));
}

fn rows() -> Vec<Value> {
    (0..120)
        .map(|i| json!({"id":i,"text":format!("PRIVATE_INTERMEDIATE_{i:04}_{}","x".repeat(32))}))
        .collect()
}
const LOAD: &str = "globalThis.text=''; let offset=0; while(true) { const page=(await host.call('read_file',{path:'rows.json',offset,max_bytes:4096})).result; text+=page.content.data; offset+=page.content.data.length; if(!page.truncated)break; } globalThis.rows=JSON.parse(text);";

#[tokio::test]
async fn interpreter_host_filters_large_values_without_inserting_intermediates_in_parent_prompt() {
    let fixture = Fixture::new();
    fs::write(
        fixture.workspace.join("rows.json"),
        serde_json::to_vec(&rows()).unwrap(),
    )
    .unwrap();
    let (endpoint,server)=provider(vec![eval("load",&format!("{LOAD} return {{count:rows.length}};")),eval("filter","globalThis.selected=rows.filter(r=>r.id%40===0).map(r=>r.id); host.checkpoint({selected}); return {selected};"),done()]).await;
    let host = fixture.native(&endpoint);
    let session = fixture.session(&host).await;
    let result = run(&host, session, "Select every fortieth record").await;
    assert_eq!(result.task.outcome, Some(Outcome::FinishedUnverified));
    assert_eq!(
        output(&host, session, "filter").await["output"]["value"]["selected"],
        json!([0, 40, 80])
    );
    let load = output(&host, session, "load").await;
    assert_eq!(load["output"]["value"]["count"], 120, "{load}");
    assert_eq!(result.task.jobs.len(), 3);
    for job in result.task.jobs.values() {
        let invocation = job.invocation.as_ref().unwrap();
        assert!(
            invocation
                .call_id
                .as_ref()
                .unwrap()
                .starts_with(load["cell"].as_str().unwrap())
        );
        assert_eq!(invocation.capability, "read_file");
    }
    let requests = server.await.unwrap();
    for request in &requests {
        assert!(!request.to_string().contains("PRIVATE_INTERMEDIATE_"));
    }
    let journal = host.journal_page(0, 256).await.unwrap();
    let inner =
        journal
            .iter()
            .filter_map(|r| serde_json::from_value::<SessionEvent>(r.event.clone()).ok())
            .filter_map(|event| match event {
                SessionEvent::Command {
                    command:
                        SessionCommand::Interpreter {
                            event:
                                orvek_harness::interpreter::InterpreterEvent::CallSettled {
                                    result, ..
                                },
                            ..
                        },
                    ..
                } => Some(result),
                _ => None,
            })
            .collect::<Vec<_>>();
    assert_eq!(inner.len(), 3);
    assert!(
        artifact(&host, inner[0])
            .await
            .to_string()
            .contains("PRIVATE_INTERMEDIATE_")
    );
    let state = host.session(session).await.unwrap();
    assert_eq!(
        state.tool_calls.len(),
        2,
        "inner calls are not fabricated provider proposals"
    );
}

#[tokio::test]
async fn interpreter_host_retained_bridge_uses_current_task_and_keeps_native_writes_children_unavailable()
 {
    let fixture = Fixture::new();
    fs::write(fixture.workspace.join("note"), "first").unwrap();
    let (endpoint,server)=provider(vec![eval("first","globalThis.saved=host.call; return await saved('read_file',{path:'note'});"),done(),eval("second","return await saved('read_file',{path:'note'});"),eval("denied","const errors=[];for(const name of ['write_file','exec_command','spawn_agent']) {try {await saved(name,{});}catch(e){errors.push(String(e));}} return errors;"),done()]).await;
    let host = fixture.native(&endpoint);
    let session = fixture.session(&host).await;
    let first = run(&host, session, "Read first").await;
    fs::write(fixture.workspace.join("note"), "second").unwrap();
    let second = run(&host, session, "Read second").await;
    assert_ne!(first.task.id, second.task.id);
    assert_eq!(second.task.jobs.len(), 1);
    let job = second.task.jobs.values().next().unwrap();
    assert_ne!(
        job.invocation.as_ref().unwrap().request,
        first
            .task
            .jobs
            .values()
            .next()
            .unwrap()
            .invocation
            .as_ref()
            .unwrap()
            .request
    );
    assert_eq!(
        output(&host, session, "second").await["output"]["value"]["result"]["content"]["data"],
        "second"
    );
    let errors = output(&host, session, "denied").await;
    assert_eq!(errors["output"]["value"].as_array().unwrap().len(), 3);
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("note")).unwrap(),
        "second"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn interpreter_host_restart_restores_only_checkpoint_data_without_repeating_jobs() {
    let fixture = Fixture::new();
    fs::write(fixture.workspace.join("note"), "saved-value").unwrap();
    let (endpoint,server)=provider(vec![eval("save","globalThis.live=await host.call('read_file',{path:'note'});host.checkpoint({answer:live.result.content.data});return {saved:true};"),done(),eval("restore","return {saved:restored.answer,live:typeof live};"),done()]).await;
    let host = fixture.native(&endpoint);
    let session = fixture.session(&host).await;
    let first = run(&host, session, "Checkpoint the read").await;
    assert_eq!(first.task.jobs.len(), 1);
    assert!(host.shutdown_if_idle().await);
    drop(host);
    fs::remove_file(fixture.workspace.join("note")).unwrap();
    let host = fixture.native(&endpoint);
    let second = run(&host, session, "Restore without redoing read").await;
    assert!(second.task.jobs.is_empty());
    let restored = output(&host, session, "restore").await;
    assert_eq!(
        restored["output"]["value"],
        json!({"saved":"saved-value","live":"undefined"})
    );
    assert_eq!(restored["state_lost"], true);
    server.await.unwrap();
}

#[tokio::test]
async fn interpreter_serial_and_composed_fixture_metrics_preserve_unknown_cost() {
    let fixture = Fixture::new();
    let data = rows();
    let bytes = serde_json::to_vec(&data).unwrap();
    fs::write(fixture.workspace.join("rows.json"), &bytes).unwrap();
    let selected = json!([0, 40, 80]);
    let mut metrics = Vec::new();
    for composed in [false, true] {
        let mut replies = Vec::new();
        if composed {
            replies.push(eval(
                "composed",
                &format!("{LOAD} return rows.filter(r=>r.id%40===0).map(r=>r.id);"),
            ));
        } else {
            for (i, offset) in (0..bytes.len()).step_by(4096).enumerate() {
                replies.push(vec![tool(
                    &format!("read{i}"),
                    "read_file",
                    json!({"path":"rows.json","offset":offset,"max_bytes":4096}),
                )]);
            }
        }
        replies.push(done());
        let (endpoint, server) = provider(replies).await;
        let sub = Fixture::new();
        fs::write(sub.workspace.join("rows.json"), &bytes).unwrap();
        let host = sub.native(&endpoint);
        let session = sub.session(&host).await;
        let started = Instant::now();
        let result = run(&host, session, "Select every fortieth record").await;
        let elapsed = started.elapsed().as_micros();
        if composed {
            assert_eq!(
                output(&host, session, "composed").await["output"]["value"],
                selected
            );
        } else {
            let mut raw = String::new();
            for i in 0..result.task.jobs.len() {
                raw.push_str(
                    output(&host, session, &format!("read{i}")).await["result"]["content"]["data"]
                        .as_str()
                        .unwrap(),
                );
            }
            let parsed: Vec<Value> = serde_json::from_str(&raw).unwrap();
            assert_eq!(
                json!(
                    parsed
                        .iter()
                        .filter(|r| r["id"].as_u64().unwrap() % 40 == 0)
                        .map(|r| r["id"].clone())
                        .collect::<Vec<_>>()
                ),
                selected
            );
        }
        let requests = server.await.unwrap();
        assert_unknown_costs(&host, requests.len()).await;
        metrics.push(json!({"mode":if composed {"composed"} else {"serial"},"correct":true,"provider_requests":requests.len(),"context_bytes":requests.iter().map(|r|r["input"].to_string().len()).sum::<usize>(),"latency_us":elapsed,"provider_cost":null,"quality_evidence":"scripted fixture only"}));
    }
    assert!(metrics[1]["provider_requests"].as_u64() < metrics[0]["provider_requests"].as_u64());
    assert!(metrics[1]["context_bytes"].as_u64() < metrics[0]["context_bytes"].as_u64());
    println!("T06_METRICS {}", json!(metrics));
}

fn fixture_result(request: &Value, id: &str) -> Option<Value> {
    request["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["type"] == "function_call_output" && v["call_id"] == id)
        .map(|v| serde_json::from_str(v["output"].as_str().unwrap()).unwrap())
}
fn child_request(request: &Value) -> bool {
    request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "submit_result")
}

#[tokio::test]
#[ignore = "requires local Docker, debian:bookworm-slim and ORVEK_EXECUTOR_HELPER"]
async fn interpreter_docker_filters_delegates_slices_and_returns_cited_result() {
    use orvek_harness::runtime::{DockerExecutor, ExecutionLimits};
    let program = format!(
        "{LOAD} const ids=rows.filter(r=>r.id%40===0).map(r=>r.id); const child=await host.call('spawn_agent',{{role:'reviewer',task:'Review only these selected IDs from rows.json: '+JSON.stringify(ids),model:'selected',output_schema:{{type:'object',properties:{{ids:{{type:'array'}},source:{{type:'string'}}}},required:['ids','source']}}}}); let waited; do {{waited=await host.call('wait_agent',{{agent_ids:[child.agent_id],timeout_ms:30000}});}} while(waited.timed_out); return {{ids,child:child.agent_id,evidence:waited.agents[0]}};"
    );
    let mut metrics = Vec::new();
    for composed in [false, true] {
        let fixture = Fixture::new();
        let bytes = serde_json::to_vec(&rows()).unwrap();
        fs::write(fixture.workspace.join("rows.json"), &bytes).unwrap();
        let pages = bytes.len().div_ceil(4096);
        let program = program.clone();
        let (endpoint,server)=provider_with(move |request| {
            if child_request(request) {
                return if fixture_result(request,"child_exec").is_none() {
                    (vec![tool("child_exec","exec_command",json!({"command":"sleep 2; printf 'rows.json IDs 0 40 80'"}))],false)
                } else {(vec![tool("child_submit","submit_result",json!({"result":{"ids":[0,40,80],"source":"rows.json"}}))],false)};
            }
            if composed {
                if fixture_result(request,"compose").is_none() {return (eval("compose",&program),false);}
            } else {
                let mut raw=String::new();
                for index in 0..pages {
                    let id=format!("serial_read_{index}");
                    let Some(result)=fixture_result(request,&id) else {return (vec![tool(&id,"read_file",json!({"path":"rows.json","offset":index*4096,"max_bytes":4096}))],false);};
                    raw.push_str(result["result"]["content"]["data"].as_str().unwrap());
                }
                let all:Vec<Value>=serde_json::from_str(&raw).unwrap();let ids=all.iter().filter(|row|row["id"].as_u64().unwrap()%40==0).map(|row|row["id"].clone()).collect::<Vec<_>>();
                assert_eq!(json!(ids),json!([0,40,80]));
                let Some(spawn)=fixture_result(request,"serial_spawn") else {return (vec![tool("serial_spawn","spawn_agent",json!({"role":"reviewer","task":format!("Review only these selected IDs from rows.json: {}",json!(ids)),"model":"selected","output_schema":{"type":"object","properties":{"ids":{"type":"array"},"source":{"type":"string"}},"required":["ids","source"]}}))],false);};
                if fixture_result(request,"serial_wait").is_none(){return (vec![tool("serial_wait","wait_agent",json!({"agent_ids":[spawn["agent_id"]],"timeout_ms":30000}))],false);}
            }
            (vec![tool("block","report_blocker",json!({"reason":"Fixture stops after collecting evidence; no completion claim"}))],true)
        }).await;
        let helper = std::env::var_os("ORVEK_EXECUTOR_HELPER").expect("explicit helper");
        let executor = DockerExecutor::connect_with_helper(
            "debian:bookworm-slim",
            std::path::Path::new(&helper),
            ExecutionLimits::default(),
        )
        .await
        .unwrap();
        let host = Host::open(&fixture.root(), client(&endpoint), executor).unwrap();
        let session = fixture.session(&host).await;
        let (pending_tx, pending_rx) = tokio::sync::oneshot::channel();
        let pending_tx = std::sync::Mutex::new(Some(pending_tx));
        let emit = Arc::new(move |event| {
            if matches!(event,orvek_harness::controller::HostUpdate::ToolStarted {name,..} if name=="wait_agent")
                && let Some(tx) = pending_tx.lock().unwrap().take()
            {
                let _ = tx.send(());
            }
        });
        let started = Instant::now();
        let execution = host.execute_request(
            session,
            Uuid::new_v4(),
            "Select every fortieth row, delegate only those IDs, cite the result".into(),
            Limits::default(),
            RequestPolicy {
                version: 1,
                delivery: DeliveryKind::Source,
                profile: RepositoryProfile {
                    version: 1,
                    name: "fixture".into(),
                    checks: Default::default(),
                },
            },
            CancellationToken::new(),
            emit,
        );
        let observe = async {
            pending_rx.await.unwrap();
            if composed {
                let state = host.session(session).await.unwrap();
                assert!(state.interpreter.cells.values().any(|cell| matches!(
                    cell.status,
                    orvek_harness::interpreter::CellStatus::Pending
                )));
                assert!(!state.interpreter.pending_calls.is_empty());
            }
        };
        let (result, ()) = timeout(Duration::from_secs(60), async {
            tokio::join!(execution, observe)
        })
        .await
        .unwrap();
        let result = result.unwrap();
        if composed {
            let output = output(&host, session, "compose").await;
            assert_eq!(
                output["output"]["value"]["ids"],
                json!([0, 40, 80]),
                "{output}"
            );
            assert_eq!(
                output["output"]["value"]["evidence"]["result"]["ids"],
                json!([0, 40, 80]),
                "{output}"
            );
            assert!(output["calls"].as_array().unwrap().len() >= 5);
        } else {
            assert_eq!(
                output(&host, session, "serial_wait").await["agents"][0]["result"]["ids"],
                json!([0, 40, 80])
            );
        }
        assert_eq!(result.task.outcome, Some(Outcome::Blocked));
        let child_job = result
            .task
            .jobs
            .values()
            .find(|j| {
                j.invocation
                    .as_ref()
                    .is_some_and(|i| i.capability == "exec_command")
            })
            .unwrap();
        assert_eq!(child_job.status, orvek_harness::state::JobStatus::Succeeded);
        let requests = server.await.unwrap();
        assert_unknown_costs(&host, requests.len()).await;
        if composed {
            assert!(
                requests
                    .iter()
                    .all(|request| !request.to_string().contains("PRIVATE_INTERMEDIATE_"))
            );
        }
        for child in requests.iter().filter(|request| child_request(request)) {
            assert!(child.to_string().contains("[0,40,80]"));
            assert!(!child.to_string().contains("PRIVATE_INTERMEDIATE_"));
        }
        metrics.push(json!({"mode":if composed {"composed"}else{"serial"},"correct":true,"provider_requests":requests.len(),"parent_requests":requests.iter().filter(|request|!child_request(request)).count(),"context_bytes":requests.iter().map(|r|r["input"].to_string().len()).sum::<usize>(),"latency_us":started.elapsed().as_micros(),"provider_cost":null,"quality_evidence":"scripted fixture with real Docker execution only"}));
    }
    println!("T06_DELEGATION_METRICS {}", json!(metrics));
    assert!(metrics[1]["parent_requests"].as_u64() < metrics[0]["parent_requests"].as_u64());
    assert!(metrics[1]["context_bytes"].as_u64() < metrics[0]["context_bytes"].as_u64());
}

#[tokio::test]
async fn interpreter_host_cancellation_preserves_settled_inner_receipt_and_invalidates_globals() {
    let fixture = Fixture::new();
    fs::write(fixture.workspace.join("note"), "read-once").unwrap();
    let (endpoint, server) = provider(vec![
        eval(
            "cancel",
            "globalThis.unsaved=42;await host.call('read_file',{path:'note'});while(true){}",
        ),
        eval("next", "return typeof unsaved;"),
        done(),
    ])
    .await;
    let host = fixture.native(&endpoint);
    let session = fixture.session(&host).await;
    let cancellation = CancellationToken::new();
    let trigger = cancellation.clone();
    let emit = Arc::new(move |event| {
        if matches!(event,orvek_harness::controller::HostUpdate::ToolFinished {name,..} if name=="read_file")
        {
            trigger.cancel();
        }
    });
    let result = timeout(
        Duration::from_secs(5),
        host.execute_request(
            session,
            Uuid::new_v4(),
            "Cancel a running cell".into(),
            Limits::default(),
            RequestPolicy {
                version: 1,
                delivery: DeliveryKind::Source,
                profile: RepositoryProfile {
                    version: 1,
                    name: "fixture".into(),
                    checks: Default::default(),
                },
            },
            cancellation,
            emit,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.task.outcome, Some(Outcome::Cancelled));
    assert_eq!(result.task.jobs.len(), 1);
    assert_eq!(
        result.task.jobs.values().next().unwrap().status,
        orvek_harness::state::JobStatus::Succeeded
    );
    assert_eq!(output(&host, session, "cancel").await["state_lost"], true);
    let next = run(&host, session, "Verify loss is explicit").await;
    assert!(next.task.jobs.is_empty());
    assert_eq!(
        output(&host, session, "next").await["output"]["value"],
        "undefined"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn interpreter_host_rejects_tampered_checkpoint_artifact_before_any_nested_execution() {
    let fixture = Fixture::new();
    let (endpoint, server) = provider(vec![
        eval("save", "host.checkpoint({answer:42});return true;"),
        done(),
        eval(
            "tampered",
            "return await host.call('read_file',{path:'missing'});",
        ),
    ])
    .await;
    let host = fixture.native(&endpoint);
    let session = fixture.session(&host).await;
    run(&host, session, "Save explicit data").await;
    let saved = host
        .session(session)
        .await
        .unwrap()
        .interpreter
        .checkpoint
        .unwrap();
    assert!(host.shutdown_if_idle().await);
    drop(host);
    let checkpoint = fixture
        .root()
        .join("artifacts")
        .join(saved.artifact.to_string());
    assert!(checkpoint.is_file(), "{}", checkpoint.display());
    fs::write(&checkpoint, b"{\"version\":999}").unwrap();
    let host = fixture.native(&endpoint);
    let result = host
        .execute_request(
            session,
            Uuid::new_v4(),
            "Reject tampering".into(),
            Limits::default(),
            RequestPolicy {
                version: 1,
                delivery: DeliveryKind::Source,
                profile: RepositoryProfile {
                    version: 1,
                    name: "fixture".into(),
                    checks: Default::default(),
                },
            },
            CancellationToken::new(),
            Arc::new(|_| {}),
        )
        .await;
    let result = result.unwrap();
    assert_eq!(result.task.outcome, Some(Outcome::Failed));
    assert!(
        result.message.contains("digest mismatch"),
        "{}",
        result.message
    );
    let state = host.session(session).await.unwrap();
    assert!(
        host.task(state.current_task.unwrap())
            .await
            .unwrap()
            .jobs
            .is_empty()
    );
    assert_eq!(server.await.unwrap().len(), 3);
}

#[tokio::test]
async fn interpreter_search_and_context_reads_keep_session_authorization() {
    let fixture = Fixture::new();
    fs::write(fixture.workspace.join("note"), "exact-search-needle").unwrap();
    let other = SessionId::new();
    let code = format!(
        "const found=await host.call('search',{{query:'exact-search-needle'}});const own=await host.call('read_context',{{start:0,limit:1}});let denied='';try{{await host.call('read_context',{{source_session:'{other}'}});}}catch(e){{denied=String(e);}}return {{hits:found.result.matches.length,request:own.items[0].item.content,denied}};"
    );
    let (endpoint, server) = provider(vec![eval("context", &code), done()]).await;
    let host = fixture.native(&endpoint);
    host.create_session_with_id(
        other,
        SessionAdmissionRequest::new(
            fixture.workspace.clone(),
            ModelSettings::default(),
            orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            Channel::Stable,
        ),
    )
    .await
    .unwrap();
    let session = fixture.session(&host).await;
    let result = run(&host, session, "Read only authorized context").await;
    assert_eq!(result.task.jobs.len(), 1);
    let output = output(&host, session, "context").await;
    assert_eq!(output["output"]["value"]["hits"], 1, "{output}");
    assert_eq!(
        output["output"]["value"]["request"],
        "Read only authorized context"
    );
    assert!(
        !output["output"]["value"]["denied"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    assert_eq!(output["calls"].as_array().unwrap().len(), 3);
    server.await.unwrap();
}

#[tokio::test]
async fn interpreter_encoding_never_runs_inherited_accessors() {
    let fixture = Fixture::new();
    fs::write(fixture.workspace.join("note"), "hidden-getter-read").unwrap();
    let script = r#"
      globalThis.hits = 0;
      Object.defineProperty(Object.prototype, 'get', {get() {
        hits++;
        delete Object.prototype.get;
        host.call('read_file', {path: 'note'});
        return undefined;
      }, configurable: true});
      let error = '';
      try { host.checkpoint({safe: 1}); } catch (e) { error = String(e); }
      delete Object.prototype.get;
      return {hits, error};
    "#;
    let (endpoint, server) = provider(vec![eval("poison", script), done()]).await;
    let host = fixture.native(&endpoint);
    let session = fixture.session(&host).await;
    let result = run(
        &host,
        session,
        "Checkpoint plain data without invoking accessors",
    )
    .await;
    let out = output(&host, session, "poison").await;
    assert_eq!(out["output"]["value"]["hits"], 0, "{out}");
    assert_eq!(out["output"]["value"]["error"], "");
    assert!(
        result.task.jobs.is_empty(),
        "an inherited accessor executed a host job: {:?}",
        result.task.jobs
    );
    server.await.unwrap();
}
