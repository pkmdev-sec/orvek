//! Real application bootstrap, Unix IPC, and captured provider requests.
use super::*;
use crate::app::config::ConfigOverrides;
use orvek_harness::{
    Channel,
    admission::{RepositoryProfile, RequestPolicy},
    auxiliary::{AuxiliaryContext, AuxiliaryKind, AuxiliaryLimits, AuxiliarySpec},
    contract::{DeliveryKind, Limits},
    inference::ModelSettings,
    session::{SessionAdmissionRequest, SessionId},
    submission::{SubmissionStatus, SubmitIntent},
    trace::{TraceBundle, TraceLimits},
};
use orvek_memory::MemoryStore;
use serde_json::{Value, json};
use std::{collections::BTreeSet, net::SocketAddr};
use tokio::{io::AsyncWriteExt, net::TcpListener, task::JoinHandle};

async fn provider(outputs: Vec<Vec<Value>>) -> (String, JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let mut requests = Vec::new();
        for output in outputs {
            let (mut socket, _) = timeout(Duration::from_secs(30), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                headers.push(byte[0]);
            }
            let length = String::from_utf8(headers)
                .unwrap()
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|n| n.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            let mut body = vec![0; length];
            socket.read_exact(&mut body).await.unwrap();
            requests.push(serde_json::from_slice(&body).unwrap());
            let event = json!({"type":"response.completed","response":{"id":format!("resp_{}", requests.len()),"status":"completed","output":output,"usage":{"input_tokens":5,"output_tokens":1,"total_tokens":6}}});
            let payload = format!("event: response.completed\ndata: {event}\n\n");
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len()).as_bytes()).await.unwrap();
        }
        requests
    });
    (endpoint, handle)
}

fn call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"type":"function_call","id":format!("fc_{id}"),"call_id":id,"name":name,"arguments":arguments.to_string(),"status":"completed"})
}

fn answer() -> Value {
    json!({"type":"message","id":"msg_done","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Done","annotations":[]}]})
}

struct Fixture {
    directory: tempfile::TempDir,
    config: Config,
}

impl Fixture {
    fn new(endpoint: &str, enabled: bool, sandbox: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        fs::create_dir(path.join("source")).unwrap();
        fs::create_dir_all(path.join("skills/check-note")).unwrap();
        fs::write(path.join("skills/check-note/SKILL.md"), "---\nname: check-note\ndescription: Check the fixture note.\n---\nSKILL-BODY-ONLY-ON-DEMAND\n").unwrap();
        fs::create_dir_all(path.join("skills/broken")).unwrap();
        fs::write(path.join("skills/broken/SKILL.md"), "invalid metadata").unwrap();
        let auth = path.join("auth-fixture");
        fs::write(&auth, "#!/bin/sh\nprintf fixture-provider-token\n").unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o700)).unwrap();
        let config_path = path.join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
[auth]
mode = "api-key"
command = {auth:?}
[agent]
workspace = {:?}
api_base_url = {endpoint:?}
execution = {:?}
[memory]
enabled = {enabled}
[skills]
enabled = {enabled}
roots = [{:?}]
"#,
                path.join("source"),
                if sandbox { "sandbox" } else { "host" },
                path.join("skills")
            ),
        )
        .unwrap();
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).unwrap();
        let config = Config::load_isolated(ConfigOverrides {
            path: Some(config_path),
            ..Default::default()
        })
        .unwrap();
        Self { directory, config }
    }

    fn configure_remote(&mut self, address: SocketAddr) {
        let remote_config = format!(
            "\n[memory.remote]\nendpoint = \"http://{address}/\"\nnamespace = \"fixture\"\nbearer_token = \"fixture-remote-bearer\"\nworkspace_roots = [{:?}]\n",
            self.config.agent().workspace()
        );
        let mut contents = fs::read_to_string(self.config.path()).unwrap();
        contents.push_str(&remote_config);
        fs::write(self.config.path(), contents).unwrap();
        self.config = Config::load_isolated(ConfigOverrides {
            path: Some(self.config.path().to_owned()),
            ..Default::default()
        })
        .unwrap();
    }

    async fn start(&self) -> (HostClient, JoinHandle<Result<()>>) {
        let config = self.config.clone();
        let mut server = tokio::spawn(async move { serve(&config).await });
        let client = HostClient::fixture(&state_directory(self.config.path()));
        timeout(Duration::from_secs(30), async {
            loop {
                if client.probe().await.is_ok() {
                    break;
                }
                if server.is_finished() {
                    panic!("host exited before binding IPC: {:?}", (&mut server).await);
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        (client, server)
    }

    async fn session(&self, client: &HostClient) -> SessionId {
        let id = SessionId::new();
        client
            .query(Command::CreateSession {
                id,
                request: SessionAdmissionRequest::new(
                    self.config.agent().workspace().to_owned(),
                    ModelSettings::default(),
                    32_768,
                    Channel::Stable,
                ),
            })
            .await
            .unwrap();
        id
    }
}

async fn stop(client: &HostClient, host: JoinHandle<Result<()>>) {
    assert!(matches!(
        client.query(Command::ShutdownIfIdle).await.unwrap(),
        Response::Shutdown { accepted: true }
    ));
    timeout(Duration::from_secs(10), host)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

async fn submit(client: &HostClient, session: SessionId, auxiliary: bool) -> SubmissionStatus {
    let intent = if auxiliary {
        SubmitIntent::Auxiliary {
            spec: AuxiliarySpec {
                kind: AuxiliaryKind::Conversation,
                context: AuxiliaryContext::CurrentConversation,
                review: None,
                limits: AuxiliaryLimits::default(),
            },
        }
    } else {
        SubmitIntent::NewTask {
            limits: Limits::default(),
            policy: RequestPolicy {
                version: 1,
                delivery: DeliveryKind::Source,
                profile: RepositoryProfile {
                    version: 1,
                    name: "fixture".into(),
                    checks: Default::default(),
                },
            },
        }
    };
    let request = Request::new(Command::Submit {
        session,
        content: vec![
            json!({"type":"input_text","text":"Use check-note and recall the fixture memory"}),
        ],
        intent,
    });
    client
        .call(&request, Duration::from_secs(10))
        .await
        .unwrap();
    timeout(Duration::from_secs(30), async {
        loop {
            let Response::Submission(submission) = client
                .query(Command::Submission {
                    session,
                    request: request.id,
                })
                .await
                .unwrap()
            else {
                panic!("missing submission")
            };
            if !submission.status.pending() {
                assert!(
                    matches!(submission.status, SubmissionStatus::Finished { .. }),
                    "{:?}",
                    submission.status
                );
                break submission.status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

async fn request_wiring(auxiliary: bool, sandbox: bool) {
    let finish = if sandbox && !auxiliary {
        call(
            "block",
            "report_blocker",
            json!({"reason":"fixture complete"}),
        )
    } else {
        answer()
    };
    let mut replies = vec![vec![
        call(
            "scan",
            "memory",
            json!({"operation":"scan","query":"fixture note"}),
        ),
        call(
            "read",
            "memory",
            json!({"operation":"read","keys":[{"id":1,"version":1}]}),
        ),
        call("skill", "read_skill", json!({"name":"check-note"})),
    ]];
    if !auxiliary {
        replies.push(vec![
            call(
                "put",
                "memory",
                json!({"operation":"put","content":"fixture new conclusion"}),
            ),
            call(
                "delete",
                "memory",
                json!({"operation":"delete","key":{"id":1,"version":1}}),
            ),
        ]);
    }
    replies.push(vec![finish]);
    let (endpoint, provider) = provider(replies).await;
    let fixture = Fixture::new(&endpoint, true, sandbox);
    let memory =
        crate::core::configured_memory_store(&fixture.config, fixture.config.agent().workspace())
            .unwrap()
            .unwrap();
    memory
        .put("fixture note uses blue ink", None)
        .await
        .unwrap();
    let (client, host) = fixture.start().await;
    let session = fixture.session(&client).await;
    submit(&client, session, auxiliary).await;
    stop(&client, host).await;
    let requests = provider.await.unwrap();
    let tools = requests[0]["tools"].as_array().unwrap();
    assert!(
        tools.iter().any(|tool| tool["name"] == "memory"),
        "configured memory never reached provider"
    );
    assert!(tools.iter().any(|tool| tool["name"] == "read_skill"));
    let instructions = requests[0]["instructions"].as_str().unwrap();
    assert!(instructions.contains("check-note"));
    assert!(
        instructions.contains("invalid skill metadata"),
        "{instructions}"
    );
    assert!(!instructions.contains("SKILL-BODY-ONLY-ON-DEMAND"));
    let outputs = requests[1]["input"].to_string();
    assert!(
        outputs.contains("fixture note uses blue ink"),
        "memory operation did not retrieve stored content: {outputs}"
    );
    assert!(outputs.contains("SKILL-BODY-ONLY-ON-DEMAND"));
    if !auxiliary {
        let records = memory.list().await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].content, "fixture new conclusion");
        assert!(
            requests[2]["input"]
                .to_string()
                .contains("fixture new conclusion")
        );
    }
    let bundle = TraceBundle::export(
        &state_directory(fixture.config.path()),
        None,
        TraceLimits::default(),
        &BTreeSet::new(),
        None,
    )
    .unwrap();
    let replay = bundle.replay().unwrap();
    assert!(
        replay.exact,
        "host context broke portable replay: {:?}",
        replay.unresolved
    );
}

#[tokio::test]
async fn configured_context_reaches_native_requests_over_ipc() {
    request_wiring(false, false).await;
}

#[tokio::test]
async fn configured_context_reaches_auxiliary_requests_over_ipc() {
    request_wiring(true, false).await;
}

#[tokio::test]
#[ignore = "requires a Docker daemon and ORVEK_EXECUTOR_IMAGE"]
async fn configured_context_reaches_sandbox_requests_over_ipc() {
    request_wiring(false, true).await;
}

#[tokio::test]
async fn disabled_context_is_absent_from_requests() {
    let (endpoint, provider) = provider(vec![vec![answer()]]).await;
    let fixture = Fixture::new(&endpoint, false, false);
    let (client, host) = fixture.start().await;
    submit(&client, fixture.session(&client).await, false).await;
    stop(&client, host).await;
    let requests = provider.await.unwrap();
    assert!(
        !requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| matches!(tool["name"].as_str(), Some("memory" | "read_skill")))
    );
    assert!(
        !requests[0]["instructions"]
            .as_str()
            .unwrap()
            .contains("Available local skills")
    );
    assert!(!fixture.config.memory_path().exists());
}

#[tokio::test]
async fn memory_operations_preserve_cas_and_only_changed_context_invalidates_cache() {
    let one = json!({"id":1,"version":1});
    let two = json!({"id":1,"version":2});
    let (endpoint, provider) = provider(vec![
        vec![call("scan", "memory", json!({"operation":"scan","query":"fixture"}))],
        vec![call("read", "memory", json!({"operation":"read","keys":[one.clone()]}))],
        vec![call("replace", "memory", json!({"operation":"put","replace":one.clone(),"content":"fixture note uses green ink"}))],
        vec![call("rescan", "memory", json!({"operation":"scan","query":"fixture"})), call("stale-put", "memory", json!({"operation":"put","replace":one.clone(),"content":"fixture stale write"}))],
        vec![call("stale-delete", "memory", json!({"operation":"delete","key":one})), call("read-new", "memory", json!({"operation":"read","keys":[two.clone()]}))],
        vec![call("delete", "memory", json!({"operation":"delete","key":two}))],
        vec![answer()],
    ]).await;
    let fixture = Fixture::new(&endpoint, true, false);
    let memory =
        crate::core::configured_memory_store(&fixture.config, fixture.config.agent().workspace())
            .unwrap()
            .unwrap();
    memory
        .put("fixture note uses blue ink", None)
        .await
        .unwrap();
    let (client, host) = fixture.start().await;
    let session = fixture.session(&client).await;
    submit(&client, session, false).await;
    let Response::Session(view) = client
        .query(Command::Session { id: session })
        .await
        .unwrap()
    else {
        panic!("session")
    };
    stop(&client, host).await;
    let requests = provider.await.unwrap();
    assert_eq!(requests[0]["instructions"], requests[1]["instructions"]);
    assert_eq!(requests[1]["instructions"], requests[2]["instructions"]);
    assert_ne!(requests[2]["instructions"], requests[3]["instructions"]);
    assert_eq!(requests[3]["instructions"], requests[4]["instructions"]);
    assert!(
        requests[4]["input"]
            .to_string()
            .contains("memory changed since it was read")
    );
    assert!(
        requests[5]["input"]
            .to_string()
            .contains("fixture note uses green ink")
    );
    assert!(memory.list().await.unwrap().is_empty());
    let store = orvek_harness::Store::open(&state_directory(fixture.config.path())).unwrap();
    let task = store.load(view.current_task.unwrap()).unwrap();
    let mut reports = task
        .model_receipts
        .values()
        .map(|receipt| {
            serde_json::from_slice::<Value>(
                &store
                    .public_artifacts()
                    .resolve(orvek_harness::artifacts::PublicArtifactRef::from_digest(
                        receipt.report,
                    ))
                    .unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let routing = &reports[0]["cache"]["routing"];
    let tools = &reports[0]["cache"]["tools"];
    assert!(
        reports
            .iter()
            .all(|report| &report["cache"]["routing"] == routing
                && &report["cache"]["tools"] == tools)
    );
    reports.sort_by_key(|report| report["cache"]["instructions"].to_string());
    reports.dedup_by_key(|report| report["cache"]["instructions"].to_string());
    assert_eq!(
        reports.len(),
        3,
        "unchanged metadata must not churn instruction identity"
    );
    let manifests = store
        .journal_page(0, 256)
        .unwrap()
        .into_iter()
        .filter_map(|record| {
            serde_json::from_value::<orvek_harness::session::SessionEvent>(record.event).ok()
        })
        .filter_map(|event| match event {
            orvek_harness::session::SessionEvent::Command {
                command: orvek_harness::session::SessionCommand::ContextPrepared { manifest, .. },
                ..
            } => Some(manifest),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(manifests.len(), 7);
    let versions = manifests
        .iter()
        .map(|digest| {
            serde_json::from_slice::<Value>(
                &store
                    .public_artifacts()
                    .resolve(orvek_harness::artifacts::PublicArtifactRef::from_digest(
                        *digest,
                    ))
                    .unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(versions[0]["memory"]["keys"][0]["version"], 1);
    assert_eq!(versions[3]["memory"]["keys"][0]["version"], 2);
    assert_eq!(versions[6]["memory"]["keys"], json!([]));
}

#[tokio::test]
async fn restarted_host_and_second_session_retrieve_the_same_memory_and_refreshed_skill() {
    let outputs = || {
        vec![
            call(
                "scan",
                "memory",
                json!({"operation":"scan","query":"fixture"}),
            ),
            call(
                "read",
                "memory",
                json!({"operation":"read","keys":[{"id":1,"version":1}]}),
            ),
            call("skill", "read_skill", json!({"name":"check-note"})),
        ]
    };
    let (endpoint, provider) =
        provider(vec![outputs(), vec![answer()], outputs(), vec![answer()]]).await;
    let fixture = Fixture::new(&endpoint, true, false);
    let memory =
        crate::core::configured_memory_store(&fixture.config, fixture.config.agent().workspace())
            .unwrap()
            .unwrap();
    memory
        .put("fixture note survives restart", None)
        .await
        .unwrap();
    let (client, host) = fixture.start().await;
    let first = fixture.session(&client).await;
    submit(&client, first, false).await;
    stop(&client, host).await;
    fs::write(
        fixture.directory.path().join("skills/check-note/SKILL.md"),
        "---\nname: check-note\ndescription: Refreshed skill catalog.\n---\nREFRESHED-BODY\n",
    )
    .unwrap();
    let (client, host) = fixture.start().await;
    let second = fixture.session(&client).await;
    assert_ne!(first, second);
    submit(&client, second, false).await;
    stop(&client, host).await;
    let requests = provider.await.unwrap();
    assert!(
        requests[1]["input"]
            .to_string()
            .contains("fixture note survives restart")
    );
    assert!(
        requests[3]["input"]
            .to_string()
            .contains("fixture note survives restart")
    );
    assert!(
        requests[2]["instructions"]
            .as_str()
            .unwrap()
            .contains("Refreshed skill catalog"),
        "{}",
        requests[2]["instructions"]
    );
    assert!(requests[3]["input"].to_string().contains("REFRESHED-BODY"));
}

#[tokio::test]
async fn auxiliary_memory_is_readonly_even_if_the_provider_proposes_mutations() {
    let (endpoint, provider) = provider(vec![
        vec![
            call(
                "scan",
                "memory",
                json!({"operation":"scan","query":"fixture"}),
            ),
            call(
                "put",
                "memory",
                json!({"operation":"put","content":"must not persist"}),
            ),
            call(
                "delete",
                "memory",
                json!({"operation":"delete","key":{"id":1,"version":1}}),
            ),
        ],
        vec![answer()],
    ])
    .await;
    let fixture = Fixture::new(&endpoint, true, false);
    let memory =
        crate::core::configured_memory_store(&fixture.config, fixture.config.agent().workspace())
            .unwrap()
            .unwrap();
    memory.put("fixture remains unchanged", None).await.unwrap();
    let (client, host) = fixture.start().await;
    submit(&client, fixture.session(&client).await, true).await;
    stop(&client, host).await;
    let requests = provider.await.unwrap();
    let memory_tool = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "memory")
        .unwrap();
    assert_eq!(
        memory_tool["parameters"]["oneOf"].as_array().unwrap().len(),
        2
    );
    assert!(
        requests[1]["input"]
            .to_string()
            .contains("memory mutation is only available to primary tasks")
    );
    let records = memory.list().await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].content, "fixture remains unchanged");
}

async fn remote_failure(sandbox: bool) {
    let remote = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = remote.local_addr().unwrap();
    let remote_server = tokio::spawn(async move {
        let (mut socket, _) = remote.accept().await.unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            headers.push(byte[0]);
        }
        socket
            .write_all(
                b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
    });
    let mut fixture = Fixture::new("http://127.0.0.1:1/v1", true, sandbox);
    let local = orvek_memory::SelectedMemoryStore::local(fixture.config.memory_path());
    local
        .put("local corpus must never stand in for remote", None)
        .await
        .unwrap();
    fixture.configure_remote(address);
    let (client, host) = fixture.start().await;
    let session = fixture.session(&client).await;
    let status = submit(&client, session, false).await;
    let SubmissionStatus::Finished {
        error: Some(error), ..
    } = status
    else {
        panic!("remote failure was hidden: {status:?}")
    };
    assert!(
        error.contains("memory backend is temporarily unavailable"),
        "{error}"
    );
    assert!(!error.contains("fixture-remote-bearer"));
    stop(&client, host).await;
    remote_server.await.unwrap();
    assert_eq!(local.list().await.unwrap().len(), 1);
    for entry in fs::read_dir(state_directory(fixture.config.path()).join("artifacts")).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("fixture-remote-bearer"));
    }
    let outside = fixture.directory.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let selected = crate::core::configured_memory_store(&fixture.config, &outside)
        .unwrap()
        .unwrap();
    assert!(matches!(
        selected,
        orvek_memory::SelectedMemoryStore::Local(_)
    ));
}

#[tokio::test]
async fn remote_failure_does_not_fall_back_to_local_memory_or_persist_credentials() {
    remote_failure(false).await;
}

#[tokio::test]
#[ignore = "requires a Docker daemon and ORVEK_EXECUTOR_HELPER"]
async fn sandbox_memory_selection_uses_the_admitted_workspace_not_the_temporary_copy() {
    remote_failure(true).await;
}

#[tokio::test]
async fn catalog_edits_refresh_at_the_next_recorded_provider_turn() {
    let mut fixture = Fixture::new("http://127.0.0.1:1/v1", true, false);
    let skill = fixture.directory.path().join("skills/check-note/SKILL.md");
    let original = fs::read(&skill).unwrap();
    let changed = "---\nname: check-note\ndescription: Changed during the task.\n---\nNEW-BODY\n";
    let (endpoint, provider) = provider(vec![
        vec![call("edit", "write_file", json!({"operation":"replace","path":skill,"expected":{"kind":"digest","digest":Digest::of(&original)},"content":changed}))],
        vec![call("skill", "read_skill", json!({"name":"check-note"}))],
        vec![answer()],
    ]).await;
    let config_path = fixture.config.path().to_owned();
    let contents = fs::read_to_string(&config_path)
        .unwrap()
        .replace("http://127.0.0.1:1/v1", &endpoint);
    fs::write(&config_path, contents).unwrap();
    fixture.config = Config::load_isolated(ConfigOverrides {
        path: Some(config_path),
        ..Default::default()
    })
    .unwrap();
    let (client, host) = fixture.start().await;
    submit(&client, fixture.session(&client).await, false).await;
    stop(&client, host).await;
    let requests = provider.await.unwrap();
    assert!(
        requests[0]["instructions"]
            .as_str()
            .unwrap()
            .contains("Check the fixture note.")
    );
    assert!(
        requests[1]["instructions"]
            .as_str()
            .unwrap()
            .contains("Changed during the task.")
    );
    assert!(
        !requests[1]["instructions"]
            .as_str()
            .unwrap()
            .contains("NEW-BODY")
    );
    assert!(requests[2]["input"].to_string().contains("NEW-BODY"));
    assert_eq!(requests[0]["tools"], requests[1]["tools"]);
    assert_eq!(
        requests[0]["prompt_cache_key"],
        requests[1]["prompt_cache_key"]
    );
}

#[tokio::test]
async fn cancellation_interrupts_a_stalled_remote_context_snapshot() {
    let remote = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = remote.local_addr().unwrap();
    let (started, received) = tokio::sync::oneshot::channel();
    let remote_server = tokio::spawn(async move {
        let (mut socket, _) = remote.accept().await.unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            headers.push(byte[0]);
        }
        started.send(()).unwrap();
        let mut byte = [0];
        let _ = socket.read(&mut byte).await;
    });
    let mut fixture = Fixture::new("http://127.0.0.1:1/v1", true, false);
    fixture.configure_remote(address);
    let (client, host) = fixture.start().await;
    let session = fixture.session(&client).await;
    let submitting_client = client.clone();
    let submission = tokio::spawn(async move { submit(&submitting_client, session, false).await });
    timeout(Duration::from_secs(5), received)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        client.query(Command::Cancel { session }).await.unwrap(),
        Response::Cancelled { requested: true }
    ));
    let status = timeout(Duration::from_secs(2), submission)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            status,
            SubmissionStatus::Finished {
                outcome: Some(orvek_harness::state::Outcome::Cancelled),
                ..
            }
        ),
        "{status:?}"
    );
    stop(&client, host).await;
    remote_server.abort();
}

#[tokio::test]
async fn scoped_evidence_and_post_run_proposals_reach_production_context_service() {
    use orvek_memory::{MemoryKind, MemoryScope, ProposalState};
    let draft = json!({"scope":"repository","kind":"code_claim","sources":[{"path":"feature.rs"}]});
    let (endpoint, provider) = provider(vec![
        vec![call("scan-claim","memory",json!({"operation":"scan","query":"fixture"}))],
        vec![call("claim","memory",json!({"operation":"put","content":"fixture feature is disabled","metadata":draft}))],
        vec![call("scan-lesson","memory",json!({"operation":"scan","query":"fixture"}))],
        vec![call("lesson","memory",json!({"operation":"propose_lesson","content":"fixture feature needs a behavior test","metadata":{"scope":"repository","kind":"procedure","sources":[{"path":"feature.rs"}]},"behavior_test":{"path":"behavior_test.rs"}}))],
        vec![answer()],
        vec![call("recall","memory",json!({"operation":"scan","query":"fixture feature"}))],
        vec![answer()],
    ]).await;
    let fixture = Fixture::new(&endpoint, true, false);
    let root = fixture.config.agent().workspace();
    fs::write(root.join("feature.rs"), "const FEATURE: bool = false;\n").unwrap();
    fs::write(root.join("behavior_test.rs"), "assert!(!FEATURE);\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Memory Test"],
        vec!["config", "user.email", "memory@example.invalid"],
        vec!["add", "."],
        vec!["commit", "-qm", "baseline"],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let memory = crate::core::configured_memory_store(&fixture.config, root)
        .unwrap()
        .unwrap();
    let (client, host) = fixture.start().await;
    let first = fixture.session(&client).await;
    submit(&client, first, false).await;
    // The task settles before its independent consolidation. Wait for the persisted proposal,
    // not an assistant message or the mere presence of a hook call.
    timeout(Duration::from_secs(5), async {
        loop {
            let records = memory.list().await.unwrap();
            if records.iter().any(|record| {
                matches!(
                    record.metadata.kind,
                    MemoryKind::LessonProposal {
                        state: ProposalState::Proposed,
                        ..
                    }
                )
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let records = memory.list().await.unwrap();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|record| matches!(
        record.metadata.scope,
        MemoryScope::Repository { .. }
    ) && record.metadata.producing_traces.len() == 1));
    assert_eq!(
        records[0].metadata.producing_traces[0].session,
        first.to_string()
    );
    fs::write(root.join("feature.rs"), "const FEATURE: bool = true;\n").unwrap();
    let second = SessionId::new();
    client
        .query(Command::CreateSession {
            id: second,
            request: SessionAdmissionRequest::new(
                root.to_owned(),
                ModelSettings {
                    model: orvek_harness::inference::Model::Terra,
                    ..ModelSettings::default()
                },
                32_768,
                Channel::Stable,
            ),
        })
        .await
        .unwrap();
    submit(&client, second, false).await;
    stop(&client, host).await;
    let requests = provider.await.unwrap();
    assert_eq!(
        requests[5]["model"],
        orvek_harness::inference::Model::Terra.as_str()
    );
    assert_ne!(requests[0]["model"], requests[5]["model"]);
    assert!(requests[6]["input"].to_string().contains("stale"));
    assert!(
        !requests[6]["instructions"]
            .to_string()
            .contains("fixture feature is disabled")
    );
    assert!(
        requests[4]["input"]
            .to_string()
            .contains("cited_not_executed")
    );
    assert_eq!(
        memory.list().await.unwrap().len(),
        2,
        "recall never relearns or rewrites records"
    );
}

#[tokio::test]
async fn foreign_scope_keys_are_absent_from_context_manifest_and_hidden_reads_do_not_count() {
    use orvek_harness::services::ContextService;
    use orvek_memory::{MemoryMetadata, MemoryScope};
    let (endpoint, provider) = provider(vec![
        vec![call(
            "hidden",
            "memory",
            json!({"operation":"read","keys":[{"id":1,"version":1}]}),
        )],
        vec![answer()],
    ])
    .await;
    let fixture = Fixture::new(&endpoint, true, false);
    let root = fixture.config.agent().workspace();
    let store = crate::core::configured_memory_store(&fixture.config, root)
        .unwrap()
        .unwrap();
    let hidden = store
        .put_with_metadata(
            "foreign repository detail",
            &MemoryMetadata {
                scope: MemoryScope::Repository {
                    identity: "different-repository".into(),
                },
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    let global = store.put("global preference", None).await.unwrap();
    let service = crate::core::context::ConfiguredContext::new(&fixture.config);
    let mut context = service.open(root).unwrap();
    let manifest = context.snapshot().await.unwrap();
    assert_eq!(
        manifest.memory.unwrap().keys,
        vec![serde_json::to_value(global.key).unwrap()]
    );
    let (client, host) = fixture.start().await;
    submit(&client, fixture.session(&client).await, false).await;
    stop(&client, host).await;
    let requests = provider.await.unwrap();
    let outputs = requests[1]["input"].to_string();
    assert!(!outputs.contains("foreign repository detail"));
    assert_eq!(store.list().await.unwrap()[0], hidden);
}
