use orvek_harness::{
    Store,
    inference::ModelSettings,
    session::{SessionConfig, SessionId},
};
use serde_json::Value;
use std::process::Command;

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orvek"))
        .env_remove("OPENAI_API_KEY")
        .env("ORVEK_CONFIG", "/definitely/missing/orvek-trace-test.toml")
        .env("ORVEK_API_BASE_URL", "http://127.0.0.1:1")
        .args(args)
        .output()
        .unwrap()
}
#[test]
fn trace_cli_exports_replays_reviews_and_prefixes_without_config_or_credentials() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let mut store = Store::open(&source).unwrap();
    store
        .create_session(
            SessionId::new(),
            SessionConfig {
                workspace: source.clone(),
                model: ModelSettings::default(),
                instructions: String::new(),
                context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            },
            None,
        )
        .unwrap();
    let output = root.path().join("bundle.json");
    let result = cli(&[
        "trace",
        "export",
        "--host-root",
        source.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    drop(store);
    std::fs::remove_dir_all(&source).unwrap();
    for action in ["replay", "review"] {
        let result = cli(&["trace", action, output.to_str().unwrap()]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let value: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(value["exact"], true, "{value}");
    }
    let prefixes = root.path().join("prefixes");
    let result = cli(&[
        "trace",
        "prefixes",
        output.to_str().unwrap(),
        "--output",
        prefixes.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        !cli(&[
            "trace",
            "reexecute",
            output.to_str().unwrap(),
            "--task",
            "00000000-0000-0000-0000-000000000000"
        ])
        .status
        .success(),
        "execution requires the explicit experiment flag"
    );
    std::fs::write(&output, b"{}").unwrap();
    assert!(
        !cli(&["trace", "replay", output.to_str().unwrap()])
            .status
            .success()
    );
}

fn experiment_trace(
    root: &std::path::Path,
) -> (std::path::PathBuf, orvek_harness::state::TaskId, SessionId) {
    use orvek_harness::trace::TraceBundle;
    use uuid::Uuid;
    let old_workspace = root.join("old");
    std::fs::create_dir(&old_workspace).unwrap();
    let mut store = Store::open(&root.join("source-host")).unwrap();
    let session = SessionId::new();
    store
        .create_session(
            session,
            SessionConfig {
                workspace: old_workspace,
                model: ModelSettings::default(),
                instructions: String::new(),
                context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            },
            None,
        )
        .unwrap();
    let policy = store.public_artifacts().write(br#"{"version":1,"delivery":"source","profile":{"version":1,"name":"fixture","checks":{}}}"#).unwrap().digest();
    let (_, task, _) = store
        .start_request(
            session,
            Uuid::new_v4(),
            "inspect".into(),
            Default::default(),
            policy,
        )
        .unwrap();
    let bundle = root.join("trace.json");
    TraceBundle::export(
        &root.join("source-host"),
        None,
        Default::default(),
        &Default::default(),
        None,
    )
    .unwrap()
    .write(&bundle)
    .unwrap();
    (bundle, task.id, session)
}

#[test]
fn reexecution_refuses_a_nonempty_workspace_before_starting_a_host() {
    let root = tempfile::tempdir().unwrap();
    let (bundle, task, _) = experiment_trace(root.path());
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let workspace = root.path().join("occupied");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("existing"), "do not touch").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orvek"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", root.path())
        .args([
            "--config",
            config.to_str().unwrap(),
            "--workspace",
            workspace.to_str().unwrap(),
            "trace",
            "reexecute",
            bundle.to_str().unwrap(),
            "--task",
            &task.to_string(),
            "--experimental",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("new empty --workspace"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root.path().join("host").exists());
    assert_eq!(
        std::fs::read_to_string(workspace.join("existing")).unwrap(),
        "do not touch"
    );
}

#[tokio::test]
async fn reexecution_cli_submits_fresh_intent_through_the_real_host() {
    use orvek_harness::trace::TraceBundle;
    use serde_json::json;
    use std::time::Duration;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        process::Command as ProcessCommand,
        time::timeout,
    };
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let (bundle, old_task, old_session) = experiment_trace(root.path());
    let workspace = root.path().join("fresh");
    std::fs::create_dir(&workspace).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move {
        timeout(Duration::from_secs(30), async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                headers.push(byte[0]);
            }
            let length: usize = String::from_utf8(headers).unwrap().lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|value| value.trim().parse().unwrap())).unwrap();
            let mut body = vec![0;length];
            socket.read_exact(&mut body).await.unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            let event = json!({"type":"response.completed","response":{"id":"resp_fresh","status":"completed","output":[{"type":"message","id":"msg_fresh","role":"assistant","status":"completed","content":[{"type":"output_text","text":"fresh answer","annotations":[]}]}],"usage":{"input_tokens":5,"output_tokens":5,"total_tokens":10}}});
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nevent: response.completed\ndata: {event}\n\n");
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
            request
        }).await.unwrap()
    });
    let config = root.path().join("config.toml");
    std::fs::write(&config,format!("[auth]\nmode = \"api-key\"\n[agent]\nworkspace = {:?}\napi_base_url = {:?}\n[skills]\nenabled = false\n[memory]\nenabled = false\n[subagents]\nenabled = false\n",workspace,endpoint)).unwrap();
    let command = || {
        let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_orvek"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", root.path())
            .env("OPENAI_API_KEY", "local-fixture-not-a-credential")
            .arg("--config")
            .arg(&config)
            .kill_on_drop(true);
        command
    };
    let mut host = command()
        .arg("host")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    timeout(Duration::from_secs(20), async {
        let mut ticks = tokio::time::interval(Duration::from_millis(10));
        while !root.path().join("host/v1/host.sock").exists() {
            assert!(
                host.try_wait().unwrap().is_none(),
                "foreground fixture host exited"
            );
            ticks.tick().await;
        }
    })
    .await
    .unwrap();
    let output = timeout(
        Duration::from_secs(30),
        command()
            .args(["trace", "reexecute"])
            .arg(&bundle)
            .arg("--task")
            .arg(old_task.to_string())
            .arg("--experimental")
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    host.kill().await.unwrap();
    host.wait().await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = provider.await.unwrap();
    assert!(request["input"].to_string().contains("inspect"));
    assert!(!request.to_string().contains(&old_task.to_string()));
    let fresh = TraceBundle::export(
        &root.path().join("host/v1"),
        None,
        Default::default(),
        &Default::default(),
        None,
    )
    .unwrap()
    .replay()
    .unwrap();
    assert_eq!(fresh.sessions.len(), 1);
    assert!(!fresh.sessions.contains_key(&old_session));
    assert_eq!(fresh.tasks.len(), 1);
    assert!(!fresh.tasks.contains_key(&old_task));
    let task = fresh.tasks.values().next().unwrap();
    assert_eq!(task.request, "inspect");
    assert!(task.certificates.is_empty());
    assert!(task.evidence.is_empty());
    assert_eq!(
        task.outcome,
        Some(orvek_harness::state::Outcome::FinishedUnverified)
    );
}
