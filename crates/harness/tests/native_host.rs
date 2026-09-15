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
        Limits as InferenceLimits, ModelSettings, ResponsesClient, Route, Transport,
        auth::{Auth, SecretString},
    },
    input,
    session::{SessionAdmissionRequest, SessionId},
    state::{JobInvocation, JobStatus, Outcome},
    submission::{Schedule, SubmitIntent, WorkIntent},
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};
use uuid::Uuid;

async fn provider(outputs: Vec<Vec<Value>>) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for output in outputs {
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
            let event = json!({"type":"response.completed","response":{"id":format!("resp_{}", requests.len()),"status":"completed","output":output,"usage":{"input_tokens":5,"output_tokens":1,"total_tokens":6}}});
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
