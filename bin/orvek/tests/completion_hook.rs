//! Real CLI and terminal IPC clients share the configured host notification path.
use orvek_harness::{
    Channel,
    admission::{RepositoryProfile, RequestPolicy},
    contract::{DeliveryKind, Limits},
    inference::ModelSettings,
    ipc::{self, Command, Request, Response, WatchFrame},
    session::{SessionAdmissionRequest, SessionId},
    submission::{SubmissionStatus, SubmitIntent},
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UnixStream},
    process::{Child, Command as Process},
    time::{sleep, timeout},
};
use uuid::Uuid;

async fn provider(output: Vec<Value>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let output = output.clone();
            tokio::spawn(async move {
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if socket.read_exact(&mut byte).await.is_err() {
                        return;
                    }
                    header.push(byte[0]);
                }
                let length = String::from_utf8(header)
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
                let event = json!({"type":"response.completed","response":{"id":"resp_hook","status":"completed","output":output,"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}});
                let payload = format!("event: response.completed\ndata: {event}\n\n");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    endpoint
}

fn done() -> Vec<Value> {
    vec![
        json!({"type":"message","id":"msg_done","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Done","annotations":[]}]}),
    ]
}

struct Fixture {
    _directory: tempfile::TempDir,
    config: PathBuf,
    workspace: PathBuf,
    socket: PathBuf,
    host: Option<Child>,
}

impl Fixture {
    fn new(endpoint: &str, hook: &str, sandbox: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("note"), "baseline").unwrap();
        let config = directory.path().join("config.toml");
        fs::write(&config, format!("[auth]\nmode = \"api-key\"\napi_key_env = \"ORVEK_HOOK_TEST_KEY\"\n[agent]\nexecution = {:?}\napi_base_url = {:?}\ncompletion_hook = {}\n", if sandbox { "sandbox" } else { "host" }, endpoint, serde_json::to_string(hook).unwrap())).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        let socket = directory.path().join("host/v1/host.sock");
        Self {
            _directory: directory,
            config,
            workspace,
            socket,
            host: None,
        }
    }

    fn seed_unclaimed(&self, command: &str) {
        use orvek_harness::{
            Store,
            controller::notification::DeliveryEvent,
            session::{SessionCommand, SessionConfig},
            state::Outcome,
        };
        let session = SessionId::new();
        let request = Uuid::new_v4();
        {
            // Crash cut: intent and terminal task are durable, but TurnSettled and
            // the notification claim have not been written. Startup must finish both.
            let mut store = Store::open(self.socket.parent().unwrap()).unwrap();
            let state = store
                .create_session(
                    session,
                    SessionConfig {
                        workspace: self.workspace.canonicalize().unwrap(),
                        model: ModelSettings::default(),
                        instructions: String::new(),
                        context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
                    },
                    None,
                )
                .unwrap();
            store
                .session_command(
                    session,
                    state.revision,
                    Uuid::new_v4(),
                    SessionCommand::CompletionHook(DeliveryEvent::Armed {
                        request,
                        command: command.into(),
                    }),
                )
                .unwrap();
            let policy = RequestPolicy {
                version: 1,
                delivery: DeliveryKind::Source,
                profile: RepositoryProfile {
                    version: 1,
                    name: "fixture".into(),
                    checks: Default::default(),
                },
            };
            let intake = store
                .public_artifacts()
                .write(&serde_json::to_vec(&policy).unwrap())
                .unwrap()
                .digest();
            let (_, task, _) = store
                .start_request(
                    session,
                    request,
                    "Finish the task".into(),
                    Limits::default(),
                    intake,
                )
                .unwrap();
            store
                .stop(
                    task.id,
                    task.revision,
                    Outcome::BudgetExhausted,
                    "fixture allowance exhausted".into(),
                )
                .unwrap();
        }
    }

    fn command(&self) -> Process {
        let mut command = Process::new(env!("CARGO_BIN_EXE_orvek"));
        command
            .args(["--config"])
            .arg(&self.config)
            .arg("--workspace")
            .arg(&self.workspace)
            .env("ORVEK_HOOK_TEST_KEY", "fixture-key")
            .env("ORVEK_HOME", self.config.parent().unwrap())
            .kill_on_drop(true);
        command
    }

    async fn start(&mut self) {
        let log = fs::File::create(self.config.with_extension("host.log")).unwrap();
        self.host = Some(
            self.command()
                .arg("host")
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        timeout(Duration::from_secs(30), async {
            loop {
                if UnixStream::connect(&self.socket).await.is_ok() {
                    return;
                }
                if let Some(status) = self.host.as_mut().unwrap().try_wait().unwrap() {
                    panic!(
                        "host exited {status}: {}",
                        fs::read_to_string(self.config.with_extension("host.log")).unwrap()
                    );
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }

    async fn query(&self, command: Command) -> Response {
        self.request(&Request::new(command)).await
    }

    async fn request(&self, request: &Request) -> Response {
        let mut socket = UnixStream::connect(&self.socket).await.unwrap();
        ipc::write_frame(&mut socket, request).await.unwrap();
        timeout(Duration::from_secs(30), ipc::read_frame(&mut socket))
            .await
            .unwrap()
            .unwrap()
    }

    async fn hook_events(&self) -> Vec<Value> {
        let Response::Journal(records) = self
            .query(Command::Journal {
                after: 0,
                limit: 256,
            })
            .await
        else {
            panic!("missing journal")
        };
        records
            .into_iter()
            .filter_map(|record| {
                let command = &record.event["data"]["command"];
                (command["type"] == "completion_hook").then(|| command["data"].clone())
            })
            .collect()
    }

    async fn stop(&mut self) {
        assert!(matches!(
            self.query(Command::ShutdownIfIdle).await,
            Response::Shutdown { accepted: true }
        ));
        timeout(Duration::from_secs(10), self.host.as_mut().unwrap().wait())
            .await
            .unwrap()
            .unwrap();
        self.host = None;
    }

    async fn submit(&self) -> (SessionId, Request) {
        let response = self
            .query(Command::CreateSession {
                id: SessionId::new(),
                request: SessionAdmissionRequest::new(
                    self.workspace.clone(),
                    ModelSettings::default(),
                    orvek_harness::context::DEFAULT_WINDOW_TOKENS,
                    Channel::Stable,
                ),
            })
            .await;
        let Response::Session(session) = response else {
            panic!("{response:?}")
        };
        let request = Request::new(Command::Submit {
            session: session.id,
            content: vec![json!({"type":"input_text","text":"Finish the task"})],
            intent: SubmitIntent::NewTask {
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
            },
        });
        assert!(matches!(
            self.request(&request).await,
            Response::Submission(_)
        ));
        (session.id, request)
    }

    async fn settled(&self, session: SessionId, request: Uuid) -> SubmissionStatus {
        timeout(Duration::from_secs(40), async {
            loop {
                let Response::Submission(submission) =
                    self.query(Command::Submission { session, request }).await
                else {
                    panic!("missing submission")
                };
                if !submission.status.pending() {
                    return submission.status;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn configured_hook_runs_for_headless_and_terminal_ipc_without_reconnect_duplicates() {
    let endpoint = provider(done()).await;
    let mut fixture = Fixture::new(
        &endpoint,
        "cat > \"$ORVEK_COMPLETION_ID.json\"; printf '%s\\n' \"$ORVEK_COMPLETION_ID\" >> deliveries",
        false,
    );
    fixture.start().await;
    let output = timeout(
        Duration::from_secs(40),
        fixture.command().args(["run", "Finish the task"]).output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fixture.workspace.join("deliveries").exists(),
        "configured completion_hook was ignored by the headless host"
    );
    let (session, request) = fixture.submit().await;
    fixture.settled(session, request.id).await;
    for _ in 0..2 {
        let mut socket = UnixStream::connect(&fixture.socket).await.unwrap();
        ipc::write_frame(
            &mut socket,
            &Request::new(Command::Watch {
                after: 0,
                session: Some(session),
            }),
        )
        .await
        .unwrap();
        loop {
            let frame: WatchFrame = ipc::read_frame(&mut socket).await.unwrap();
            if matches!(frame, WatchFrame::Ready { .. }) {
                break;
            }
        }
        assert!(matches!(
            fixture.request(&request).await,
            Response::Submission(_)
        ));
    }
    fixture.stop().await;
    fixture.start().await;
    fixture.stop().await;
    let delivered = fs::read_to_string(fixture.workspace.join("deliveries")).unwrap();
    let ids = delivered.lines().collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    for id in ids {
        let payload: Value = serde_json::from_slice(
            &fs::read(fixture.workspace.join(format!("{id}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(payload["delivery_id"], id);
        assert_eq!(payload["outcome"], "finished_unverified");
    }
}

#[tokio::test]
async fn hook_failure_does_not_change_the_task_outcome_or_leak_output() {
    let endpoint = provider(done()).await;
    let mut fixture = Fixture::new(
        &endpoint,
        "printf '%s%s' hook-private- output; printf '%s%s' hook-private- error >&2; printf once >> deliveries; exit 7",
        false,
    );
    fixture.start().await;
    let output = timeout(
        Duration::from_secs(40),
        fixture.command().args(["run", "Finish the task"]).output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("hook-private-output"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("hook-private-error"));
    let events = fixture.hook_events().await;
    assert_eq!(
        events.last().unwrap()["data"]["result"],
        json!({"type":"failed","data":{"exit_code":7}})
    );
    fixture.stop().await;
    fixture.start().await;
    fixture.stop().await;
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("deliveries")).unwrap(),
        "once"
    );
}

#[tokio::test]
async fn hook_timeout_is_unknown_and_is_not_retried_after_restart() {
    let endpoint = provider(done()).await;
    let mut fixture = Fixture::new(
        &endpoint,
        "printf once >> deliveries; sleep 30; printf leaked > escaped",
        false,
    );
    fixture.start().await;
    let (session, request) = fixture.submit().await;
    let status = fixture.settled(session, request.id).await;
    assert!(matches!(
        status,
        SubmissionStatus::Finished {
            outcome: Some(orvek_harness::state::Outcome::FinishedUnverified),
            ..
        }
    ));
    let events = fixture.hook_events().await;
    assert_eq!(
        events.last().unwrap()["data"]["result"],
        json!({"type":"unknown","data":{"reason":"timed_out"}})
    );
    fixture.stop().await;
    fixture.start().await;
    fixture.stop().await;
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("deliveries")).unwrap(),
        "once"
    );
    assert!(!fixture.workspace.join("escaped").exists());
}

#[tokio::test]
async fn crash_after_external_effect_keeps_delivery_unknown_without_retry() {
    let endpoint = provider(done()).await;
    let mut fixture = Fixture::new(
        &endpoint,
        "printf once >> deliveries; kill -KILL $PPID; exit 0",
        false,
    );
    fixture.start().await;
    let (_session, _request) = fixture.submit().await;
    timeout(
        Duration::from_secs(20),
        fixture.host.as_mut().unwrap().wait(),
    )
    .await
    .unwrap()
    .unwrap();
    fixture.host = None;
    fixture.start().await;
    let events = fixture.hook_events().await;
    assert_eq!(
        events.len(),
        2,
        "armed and claimed only; no acknowledged result"
    );
    assert_eq!(events[1]["type"], "claimed");
    fixture.stop().await;
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("deliveries")).unwrap(),
        "once"
    );
}

#[tokio::test]
async fn cancelling_a_task_still_delivers_its_terminal_hook() {
    let endpoint = provider(vec![json!({"type":"function_call","id":"fc_wait","call_id":"call_wait","name":"exec_command","arguments":"{\"command\":\"printf ready > model-ready; sleep 30\"}","status":"completed"})]).await;
    let mut fixture = Fixture::new(
        &endpoint,
        "printf '%s' \"$ORVEK_OUTCOME\" > deliveries",
        false,
    );
    fixture.start().await;
    let (session, request) = fixture.submit().await;
    timeout(Duration::from_secs(15), async {
        while !fixture.workspace.join("model-ready").exists() {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    fixture
        .query(Command::CancelSubmission {
            session,
            request: request.id,
        })
        .await;
    let status = fixture.settled(session, request.id).await;
    assert!(matches!(
        status,
        SubmissionStatus::Finished {
            outcome: Some(orvek_harness::state::Outcome::Cancelled),
            ..
        }
    ));
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("deliveries")).unwrap(),
        "cancelled"
    );
    fixture.stop().await;
}

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn sandbox_headless_and_terminal_ipc_use_the_same_host_hook() {
    let endpoint = provider(vec![json!({"type":"function_call","id":"fc_blocker","call_id":"call_blocker","name":"report_blocker","arguments":"{\"reason\":\"fixture prerequisite is absent\"}","status":"completed"})]).await;
    let mut fixture = Fixture::new(
        &endpoint,
        "printf '%s\\n' \"$ORVEK_OUTCOME\" >> deliveries",
        true,
    );
    fixture.start().await;
    let output = timeout(
        Duration::from_secs(60),
        fixture.command().args(["run", "Finish the task"]).output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        output.status.code(),
        Some(20),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (session, request) = fixture.submit().await;
    assert!(matches!(
        fixture.settled(session, request.id).await,
        SubmissionStatus::Finished {
            outcome: Some(orvek_harness::state::Outcome::Blocked),
            ..
        }
    ));
    fixture.stop().await;
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("deliveries")).unwrap(),
        "blocked\nblocked\n"
    );
}

#[tokio::test]
async fn restart_delivers_an_unclaimed_intent_after_recovering_settlement() {
    let endpoint = provider(done()).await;
    let mut fixture = Fixture::new(&endpoint, "exit 99", false);
    fixture.seed_unclaimed(r#"printf '%s' "$ORVEK_OUTCOME" >> deliveries"#);
    fixture.start().await;
    let events = timeout(Duration::from_secs(10), async {
        loop {
            let events = fixture.hook_events().await;
            if events
                .last()
                .is_some_and(|event| event["type"] == "finished")
            {
                break events;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        events.last().unwrap()["data"]["result"],
        json!({"type":"succeeded"})
    );
    fixture.stop().await;
    fixture.start().await;
    fixture.stop().await;
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("deliveries")).unwrap(),
        "budget_exhausted",
        "recovery must use the pinned command, not new configuration"
    );
}

#[tokio::test]
async fn controller_failure_settles_before_its_notification() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            header.push(byte[0]);
        }
        let length = String::from_utf8(header)
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
        socket
            .write_all(
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
    });
    let mut fixture = Fixture::new(
        &endpoint,
        "printf '%s' \"$ORVEK_OUTCOME\" > deliveries",
        false,
    );
    fixture.start().await;
    let (session, request) = fixture.submit().await;
    assert!(matches!(
        fixture.settled(session, request.id).await,
        SubmissionStatus::Finished {
            outcome: Some(orvek_harness::state::Outcome::Failed),
            ..
        }
    ));
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("deliveries")).unwrap(),
        "failed"
    );
    let Response::Journal(records) = fixture
        .query(Command::Journal {
            after: 0,
            limit: 256,
        })
        .await
    else {
        panic!("missing journal")
    };
    let settled = records
        .iter()
        .position(|r| r.event["data"]["command"]["type"] == "turn_settled")
        .unwrap();
    let claimed = records
        .iter()
        .position(|r| r.event["data"]["command"]["data"]["type"] == "claimed")
        .unwrap();
    assert!(settled < claimed);
    fixture.stop().await;
}

#[tokio::test]
async fn recovering_a_blocked_hook_keeps_ipc_responsive() {
    let endpoint = provider(done()).await;
    let mut fixture = Fixture::new(&endpoint, "exit 99", false);
    fixture.seed_unclaimed("printf ready > hook-started; sleep 30");
    let socket = fixture.socket.clone();
    let started = fixture.workspace.join("hook-started");
    let observer = async {
        timeout(Duration::from_secs(30), async {
            while !started.exists() {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let stream = UnixStream::connect(&socket).await;
        assert!(stream.is_ok(), "a pending hook blocked the IPC listener");
        let mut stream = stream.unwrap();
        ipc::write_frame(&mut stream, &Request::new(Command::Info))
            .await
            .unwrap();
        let response: Response = timeout(Duration::from_secs(2), ipc::read_frame(&mut stream))
            .await
            .expect("a pending hook blocked host queries")
            .unwrap();
        assert!(matches!(response, Response::Info(_)));
    };
    tokio::join!(fixture.start(), observer);
    // Shutdown cancels recovery. The durable pre-spawn claim remains unknown.
    fixture.stop().await;
    fixture.start().await;
    let events = fixture.hook_events().await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[1]["type"], "claimed");
    fixture.stop().await;
}

#[tokio::test]
async fn diagnostic_headless_resume_emits_feedback_before_its_watch_cursor() {
    use orvek_harness::{Store, session::SessionCommand};
    let endpoint = provider(done()).await;
    let mut fixture = Fixture::new(&endpoint, "true", false);
    fixture.start().await;
    let Response::Session(view) = fixture
        .query(Command::CreateSession {
            id: SessionId::new(),
            request: SessionAdmissionRequest::new(
                fixture.workspace.clone(),
                ModelSettings::default(),
                orvek_harness::context::DEFAULT_WINDOW_TOKENS,
                Channel::Stable,
            ),
        })
        .await
    else {
        panic!("expected admitted session")
    };
    let session = view.id;
    fixture.stop().await;
    {
        let mut store = Store::open(fixture.socket.parent().unwrap()).unwrap();
        let mut state = store.load_session(session).unwrap();
        for index in 0..65 {
            state = store
                .session_command(
                    session,
                    state.revision,
                    Uuid::new_v4(),
                    SessionCommand::Feedback {
                        message: format!("Warning: saved diagnostic {index}"),
                    },
                )
                .unwrap();
        }
    }
    fixture.start().await;
    let output = timeout(
        Duration::from_secs(40),
        fixture
            .command()
            .args(["--resume", &session.to_string(), "run", "Finish the task"])
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    fixture.stop().await;
    assert!(
        output.status.success(),
        "status={} stderr={} stdout={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let feedback = events
        .iter()
        .filter(|event| event["type"] == "session_feedback")
        .collect::<Vec<_>>();
    assert_eq!(
        feedback.len(),
        65,
        "headless must not skip feedback already included in its starting session snapshot"
    );
    assert!(
        feedback
            .iter()
            .all(|event| event["data"]["session"] == json!(session))
    );
    assert_eq!(
        feedback.first().unwrap()["data"]["message"],
        "Warning: saved diagnostic 0"
    );
    assert_eq!(
        feedback.last().unwrap()["data"]["message"],
        "Warning: saved diagnostic 64"
    );
}
