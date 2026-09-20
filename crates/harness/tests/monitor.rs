//! Controlled wiring evidence, not live-model quality comparisons.
use base64::Engine;
use orvek_harness::{
    Channel, Digest,
    admission::{RepositoryProfile, RequestPolicy},
    contract::{DeliveryKind, Limits},
    controller::Host,
    inference::{
        Limits as InferenceLimits, ModelSettings, ResponsesClient, Route, Transport,
        auth::{Auth, SecretString},
    },
    monitor::{EpisodeState, Signature},
    session::{SessionAdmissionRequest, SessionId},
    state::Outcome,
    submission::SubmitIntent,
};
use serde_json::{Value, json};
use std::{fs, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    time::timeout,
};
use uuid::Uuid;
#[allow(dead_code)]
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

async fn admit(host: &Host, workspace: &std::path::Path) -> SessionId {
    host.create_session(SessionAdmissionRequest::new(
        workspace.to_owned(),
        ModelSettings::default(),
        orvek_harness::context::DEFAULT_WINDOW_TOKENS,
        Channel::Stable,
    ))
    .await
    .unwrap()
    .id
}
async fn run_read(host: &Arc<Host>, session: SessionId) {
    let request = Uuid::new_v4();
    host.submit(
        session,
        request,
        vec![json!({"type":"input_text","text":"Read the complete 4096-byte page"})],
        new_task_intent(),
    )
    .await
    .unwrap();
    assert_eq!(
        wait_submission(host, session, request).await.task.outcome,
        Some(Outcome::FinishedUnverified)
    );
}
fn read_reply() -> Vec<Value> {
    vec![function_call(
        "fc_read",
        "read",
        "read_file",
        json!({"path":"page","max_bytes":4096}),
    )]
}
async fn drain(host: &Host) {
    for _ in 0..100 {
        host.monitor_tick().await.unwrap();
        if host.monitor_report(0, 100).await.unwrap().status.cursor
            == host.info().await.unwrap().journal_sequence
        {
            return;
        }
    }
    panic!("monitor did not catch up");
}

#[tokio::test]
async fn seeded_release_runs_trace_diagnosis_frozen_checks_activation_and_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("page"), vec![b'a'; 4096]).unwrap();
    let (endpoint, server) = provider(vec![
        read_reply(),
        vec![final_message("done1")],
        read_reply(),
        vec![final_message("done2")],
        read_reply(),
        vec![final_message("done3")],
        read_reply(),
        vec![final_message("done4")],
    ])
    .await;
    let root = dir.path().join("state");
    let host =
        Arc::new(Host::open_native(&root, client(&endpoint), Digest::of(b"fixture")).unwrap());
    let baseline = host.monitor_report(0, 100).await.unwrap().status.active;
    let normal = admit(&host, &workspace).await;
    run_read(&host, normal).await;
    assert_eq!(
        tool_output(&host, normal, "read").await["result"]["content"]["bytes"],
        4096
    );
    host.set_monitor_sampling(2).await.unwrap();
    let bad = host
        .install_read_behavior(baseline, 4096, "seeded config regression".into())
        .await
        .unwrap();
    let trigger = admit(&host, &workspace).await;
    let inflight = admit(&host, &workspace).await;
    run_read(&host, trigger).await;
    assert!(
        tool_output(&host, trigger, "read").await["result"]["content"]["bytes"]
            .as_u64()
            .unwrap()
            < 4096
    );
    drain(&host).await;
    let report = host.monitor_report(0, 100).await.unwrap();
    assert_eq!(report.episodes.len(), 1);
    assert!(
        report
            .comparisons
            .iter()
            .any(|pair| pair.signature == Signature::ReadUnderfill
                && pair.before.opportunities == 1
                && pair.after.failures == 1)
    );
    let episode = &report.episodes[0];
    assert!(
        matches!(episode.state, EpisodeState::Promoted { .. }),
        "{episode:?}"
    );
    assert_ne!(report.status.active, bad);
    assert_eq!(report.status.previous, Some(bad));
    let result: Value =
        serde_json::from_slice(&artifact_bytes(&host, episode.result.unwrap()).await).unwrap();
    assert_eq!(result["control_failed"], true);
    assert_eq!(result["checks"].as_array().unwrap().len(), 14);
    assert!(result["model_comparisons"].is_null());
    let fixture: Value =
        serde_json::from_slice(&artifact_bytes(&host, episode.regression).await).unwrap();
    assert_eq!(fixture["max_bytes"], 4096);
    let adopted = host
        .create_session_with_id(
            trigger,
            SessionAdmissionRequest::new(
                workspace.clone(),
                ModelSettings::default(),
                orvek_harness::context::DEFAULT_WINDOW_TOKENS,
                Channel::Stable,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        adopted.admission().unwrap().native_read().unwrap().release,
        bad,
        "lost session admission acknowledgements keep the original version"
    );
    let new = admit(&host, &workspace).await;
    assert_eq!(
        host.session(new)
            .await
            .unwrap()
            .admission()
            .unwrap()
            .native_read()
            .unwrap()
            .release,
        report.status.active
    );
    assert_eq!(
        host.session(inflight)
            .await
            .unwrap()
            .admission()
            .unwrap()
            .native_read()
            .unwrap()
            .release,
        bad
    );
    run_read(&host, new).await;
    run_read(&host, inflight).await;
    assert_eq!(
        tool_output(&host, new, "read").await["result"]["content"]["bytes"],
        4096
    );
    assert!(
        tool_output(&host, inflight, "read").await["result"]["content"]["bytes"]
            .as_u64()
            .unwrap()
            < 4096
    );
    drain(&host).await;
    let before = host.monitor_report(0, 100).await.unwrap();
    drain(&host).await;
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(host.monitor_report(0, 100).await.unwrap()).unwrap()
    );
    assert_eq!(
        before.episodes.len(),
        1,
        "repair origins do not recursively produce episodes"
    );
    assert!(
        before
            .measures
            .iter()
            .any(|m| m.signature == Signature::ReadUnderfill
                && m.cohort.release == Some(bad)
                && m.opportunities == 2
                && m.failures == 2)
    );
    assert_eq!(before.status.sampling.considered, 2);
    assert_eq!(before.status.sampling.selected, 1);
    assert_eq!(before.status.sampling.skipped_sampling, 1);
    assert!(
        host.install_read_behavior(bad, 8192, "stale".into())
            .await
            .is_err()
    );
    host.rollback_read_behavior(before.status.active)
        .await
        .unwrap();
    let rolled_back = admit(&host, &workspace).await;
    assert_eq!(
        host.session(rolled_back)
            .await
            .unwrap()
            .admission()
            .unwrap()
            .native_read()
            .unwrap()
            .release,
        bad
    );
    assert_eq!(
        host.session(new)
            .await
            .unwrap()
            .admission()
            .unwrap()
            .native_read()
            .unwrap()
            .release,
        before.status.active
    );
    // The exact trace still replays; behavior activation never rewrites a session.
    let bundle = orvek_harness::trace::TraceBundle::export(
        &root,
        None,
        Default::default(),
        &Default::default(),
        None,
    )
    .unwrap();
    assert!(bundle.replay().unwrap().exact);
    if let Some(path) = std::env::var_os("ORVEK_MONITOR_EVIDENCE") {
        let path = std::path::PathBuf::from(path);
        fs::create_dir(&path).unwrap();
        bundle.write(&path.join("trace.json")).unwrap();
        fs::write(
            path.join("report.json"),
            serde_json::to_vec_pretty(&before).unwrap(),
        )
        .unwrap();
        for (name, digest) in [
            ("source-diff", episode.source_diff),
            ("regression", episode.regression),
            ("heldout", episode.heldout),
            ("candidate", episode.candidate.unwrap()),
            ("evaluation", episode.result.unwrap()),
        ] {
            fs::write(
                path.join(format!("{name}.json")),
                artifact_bytes(&host, digest).await,
            )
            .unwrap();
        }
    }
    let cursor = before.status.cursor;
    server.await.unwrap();
    drop(host);
    let reopened = Host::open_native(
        &root,
        client("http://127.0.0.1:1/v1/responses"),
        Digest::of(b"fixture"),
    )
    .unwrap();
    assert!(reopened.monitor_report(0, 100).await.unwrap().status.cursor >= cursor);
    drain(&reopened).await;
    assert_eq!(
        reopened
            .monitor_report(0, 100)
            .await
            .unwrap()
            .episodes
            .len(),
        1
    );
}

#[tokio::test]
async fn outage_and_docs_only_release_do_not_make_speculative_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("page"), vec![b'a'; 4096]).unwrap();
    let (endpoint, server) = scripted_provider(vec![
        ProviderReply::Response(read_reply()),
        ProviderReply::Response(vec![final_message("done")]),
        ProviderReply::Rejected(503),
        ProviderReply::Rejected(503),
        ProviderReply::Rejected(503),
    ])
    .await;
    let host = Arc::new(
        Host::open_native(
            &dir.path().join("state"),
            client(&endpoint),
            Digest::of(b"fixture"),
        )
        .unwrap(),
    );
    let baseline = host.monitor_report(0, 100).await.unwrap().status.active;
    fs::write(workspace.join("README.md"), "Unrelated documentation edit").unwrap();
    let docs = host
        .install_read_behavior(
            baseline,
            32768,
            "docs-only change; read behavior unchanged".into(),
        )
        .await
        .unwrap();
    run_read(&host, admit(&host, &workspace).await).await;
    let session = admit(&host, &workspace).await;
    let request = Uuid::new_v4();
    host.submit(
        session,
        request,
        vec![json!({"type":"input_text","text":"Read page"})],
        new_task_intent(),
    )
    .await
    .unwrap();
    wait_submission(&host, session, request).await;
    drain(&host).await;
    let report = host.monitor_report(0, 100).await.unwrap();
    assert_eq!(report.status.active, docs);
    assert!(report.episodes.is_empty());
    let calls = report
        .measures
        .iter()
        .find(|m| m.signature == Signature::ProviderError)
        .unwrap();
    assert!(calls.opportunities >= 3);
    assert!(calls.failures >= 1);
    assert!(report.interpretation.contains("outages remain uncertain"));
    // A provider may reject before charging: the actual receipt, not a zero fill, decides.
    let usage = report
        .measures
        .iter()
        .find(|m| m.signature == Signature::MissingUsage)
        .unwrap();
    assert_eq!(usage.opportunities, calls.opportunities);
    server.abort();
}

#[tokio::test]
async fn monitor_failure_leaves_running_user_task_and_other_tasks_independent() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let (started, observed) = oneshot::channel();
    let (endpoint, server) = scripted_provider(vec![
        ProviderReply::Stall(started),
        ProviderReply::Response(vec![final_message("other")]),
    ])
    .await;
    let root = dir.path().join("state");
    let host =
        Arc::new(Host::open_native(&root, client(&endpoint), Digest::of(b"fixture")).unwrap());
    let initial = host.monitor_report(0, 100).await.unwrap().status.active;
    let running = admit(&host, &workspace).await;
    let request = Uuid::new_v4();
    host.submit(
        running,
        request,
        vec![json!({"type":"input_text","text":"Wait for provider"})],
        new_task_intent(),
    )
    .await
    .unwrap();
    timeout(Duration::from_secs(10), observed)
        .await
        .unwrap()
        .unwrap();
    let active = host
        .install_read_behavior(initial, 4096, "new release while task is active".into())
        .await
        .unwrap();
    assert_eq!(
        host.session(running)
            .await
            .unwrap()
            .admission()
            .unwrap()
            .native_read()
            .unwrap()
            .release,
        initial
    );
    assert!(
        host.session(running)
            .await
            .unwrap()
            .active_request
            .is_some()
    );
    let future = admit(&host, &workspace).await;
    assert_eq!(
        host.session(future)
            .await
            .unwrap()
            .admission()
            .unwrap()
            .native_read()
            .unwrap()
            .release,
        active
    );
    let db = rusqlite::Connection::open(root.join("v1.sqlite3")).unwrap();
    db.execute("DELETE FROM monitor_state", []).unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let service = tokio::spawn(orvek_harness::ipc::serve(host.clone(), stop.clone()));
    assert!(host.monitor_tick().await.is_err());
    let other = Uuid::new_v4();
    host.submit(
        future,
        other,
        vec![json!({"type":"input_text","text":"Finish independently"})],
        new_task_intent(),
    )
    .await
    .unwrap();
    assert_eq!(
        wait_submission(&host, future, other).await.task.outcome,
        Some(Outcome::FinishedUnverified)
    );
    assert!(
        host.session(running)
            .await
            .unwrap()
            .active_request
            .is_some(),
        "monitor must not cancel user work"
    );
    host.cancel_submission(running, request).await.unwrap();
    wait_submission(&host, running, request).await;
    stop.cancel();
    timeout(Duration::from_secs(10), service)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap();
}
