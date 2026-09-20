//! Fresh-process CLI and asynchronous IPC-worker acceptance; no real model calls.
use orvek_harness::{
    ipc::{self, Command, Request, Response},
    monitor::EpisodeState,
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UnixStream},
    process::{Child, Command as Process},
    time::{sleep, timeout},
};

struct Fixture {
    _dir: tempfile::TempDir,
    config: PathBuf,
    workspace: PathBuf,
    socket: PathBuf,
    host: Option<Child>,
}
impl Fixture {
    fn command(&self) -> Process {
        let mut cmd = Process::new(env!("CARGO_BIN_EXE_orvek"));
        cmd.arg("--config")
            .arg(&self.config)
            .arg("--workspace")
            .arg(&self.workspace)
            .env("ORVEK_MONITOR_TEST_KEY", "fixture-key")
            .env("ORVEK_HOME", self.config.parent().unwrap())
            .kill_on_drop(true);
        cmd
    }
    async fn cli(&self, args: &[&str]) -> Value {
        let output = timeout(Duration::from_secs(30), self.command().args(args).output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    async fn query(&self, command: Command) -> Response {
        ipc::call(&self.socket, &Request::new(command), Duration::from_secs(5))
            .await
            .unwrap()
    }
    async fn start(&mut self) {
        let log = fs::File::create(self.config.with_extension("log")).unwrap();
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
                        "{status}: {}",
                        fs::read_to_string(self.config.with_extension("log")).unwrap()
                    );
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn report(&self) -> orvek_harness::monitor::MonitorReport {
        let Response::Monitor(report) = self
            .query(Command::MonitorReport {
                offset: 0,
                limit: 100,
            })
            .await
        else {
            panic!("not report");
        };
        *report
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
}

#[tokio::test]
async fn cli_release_real_task_background_repair_and_restart_rollback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move {
        let mut count = 0;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
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
                        .map(|s| s.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            let mut bytes = vec![0; length];
            socket.read_exact(&mut bytes).await.unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            let is_followup = request["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["type"] == "function_call_output");
            let output = if is_followup {
                vec![
                    json!({"type":"message","id":"msg_done","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Done","annotations":[]}]}),
                ]
            } else {
                vec![
                    json!({"type":"function_call","id":"fc_read","call_id":"read","name":"read_file","arguments":"{\"path\":\"page\",\"max_bytes\":4096}","status":"completed"}),
                ]
            };
            count += 1;
            let event = json!({"type":"response.completed","response":{"id":format!("resp_{count}"),"status":"completed","output":output,"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}});
            let body = format!("event: response.completed\ndata: {event}\n\n");
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("page"), vec![b'x'; 4096]).unwrap();
    let config = dir.path().join("config.toml");
    fs::write(
        &config,
        format!(
            r#"[auth]
mode = "api-key"
api_key_env = "ORVEK_MONITOR_TEST_KEY"
[agent]
execution = "host"
api_base_url = {endpoint:?}
"#
        ),
    )
    .unwrap();
    let mut fixture = Fixture {
        config,
        workspace,
        socket: dir.path().join("host/v1/host.sock"),
        _dir: dir,
        host: None,
    };
    fixture.start().await;
    let status = fixture.cli(&["monitor", "report"]).await;
    let initial = status["data"]["status"]["active"].as_str().unwrap();
    let installed = fixture
        .cli(&[
            "monitor",
            "install-read",
            initial,
            "4096",
            "controlled regression",
        ])
        .await;
    let bad = installed["data"].as_str().unwrap();
    let output = timeout(
        Duration::from_secs(30),
        fixture.command().args(["run", "Read page"]).output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = timeout(Duration::from_secs(15), async {
        loop {
            let report = fixture.report().await;
            if report
                .episodes
                .iter()
                .any(|e| matches!(e.state, EpisodeState::Promoted { .. }))
            {
                break report;
            }
            sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(report.episodes.len(), 1);
    assert_eq!(report.episodes[0].regressed.to_string(), bad);
    let active = report.status.active;
    let cursor = report.status.cursor;
    fixture.host.as_mut().unwrap().kill().await.unwrap();
    fixture.host = None;
    fixture.start().await;
    assert_eq!(fixture.report().await.status.active, active);
    assert!(fixture.report().await.status.cursor >= cursor);
    fixture
        .cli(&["monitor", "rollback", &active.to_string()])
        .await;
    assert_eq!(fixture.report().await.status.active.to_string(), bad);
    fixture.cli(&["monitor", "sampling", "2"]).await;
    assert_eq!(fixture.report().await.status.sample_every, 2);
    fixture.stop().await;
    provider.abort();
}
