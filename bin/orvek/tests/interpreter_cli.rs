//! Real binary + detached host + IPC, using a local scripted provider only.
use orvek_harness::{
    ipc::{self, Command, Request, Response},
    session::SessionId,
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UnixStream},
    process::{Child, Command as Process},
    time::{sleep, timeout},
};

fn eval(id: &str, code: &str) -> Vec<Value> {
    vec![
        json!({"type":"function_call","id":format!("fc_{id}"),"call_id":id,"name":"interpreter_eval","arguments":json!({"code":code}).to_string(),"status":"completed"}),
    ]
}
fn done() -> Vec<Value> {
    vec![
        json!({"type":"message","id":"done","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Selected evidence is ready","annotations":[]}]}),
    ]
}
async fn provider(replies: Vec<Vec<Value>>) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let mut requests = Vec::new();
        for output in replies {
            let (mut socket, _) = timeout(Duration::from_secs(60), listener.accept())
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
            let event = json!({"type":"response.completed","response":{"id":format!("r{}",requests.len()),"status":"completed","output":output,"usage":{"input_tokens":5,"output_tokens":1,"total_tokens":6}}});
            let payload = format!("event: response.completed\ndata: {event}\n\n");
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",payload.len()).as_bytes()).await.unwrap();
        }
        requests
    });
    (endpoint, handle)
}
struct Fixture {
    _directory: tempfile::TempDir,
    config: PathBuf,
    workspace: PathBuf,
    socket: PathBuf,
    host: Option<Child>,
}
impl Fixture {
    fn new(endpoint: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let config = directory.path().join("config.toml");
        fs::write(&config,format!("[auth]\nmode = \"api-key\"\napi_key_env = \"ORVEK_INTERPRETER_TEST_KEY\"\n[agent]\nexecution = \"host\"\napi_base_url = {endpoint:?}\n")).unwrap();
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
    fn command(&self) -> Process {
        let mut command = Process::new(env!("CARGO_BIN_EXE_orvek"));
        command
            .args(["--config"])
            .arg(&self.config)
            .arg("--workspace")
            .arg(&self.workspace)
            .env("ORVEK_INTERPRETER_TEST_KEY", "fixture-key")
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
                    break;
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
        let mut socket = UnixStream::connect(&self.socket).await.unwrap();
        ipc::write_frame(&mut socket, &Request::new(command))
            .await
            .unwrap();
        timeout(Duration::from_secs(30), ipc::read_frame(&mut socket))
            .await
            .unwrap()
            .unwrap()
    }
    async fn tool_result(&self, id: &str) -> Value {
        let Response::Journal(records) = self
            .query(Command::Journal {
                after: 0,
                limit: 256,
            })
            .await
        else {
            panic!("missing journal")
        };
        let record = records
            .iter()
            .find(|record| {
                record.event["data"]["command"]["type"] == "tool_result"
                    && record.event["data"]["command"]["data"]["call_id"] == id
            })
            .unwrap();
        serde_json::from_str(
            record.event["data"]["command"]["data"]["output"]
                .as_str()
                .unwrap(),
        )
        .unwrap()
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
    async fn run(&self, resume: Option<SessionId>) -> Vec<Value> {
        let mut command = self.command();
        if let Some(session) = resume {
            command.args(["--resume", &session.to_string()]);
        }
        command.args(["run", "Select evidence from the fixture"]);
        let output = timeout(Duration::from_secs(60), command.output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

#[tokio::test]
async fn cli_interpreter_journals_inner_calls_and_restores_without_repeating_reads() {
    let (endpoint,server)=provider(vec![eval("load","globalThis.full=JSON.parse((await host.call('read_file',{path:'rows.json'})).result.content.data);host.checkpoint({selected:full.filter(r=>r.id===7).map(r=>r.id)});return {count:full.length};"),eval("select","return {selected:full.filter(r=>r.id===7).map(r=>r.id)};"),done(),eval("restore","return {selected:restored.selected,live:typeof full};"),done()]).await;
    let mut fixture = Fixture::new(&endpoint);
    let rows = (0..30)
        .map(|id| json!({"id":id,"text":format!("CLI_PRIVATE_ROW_{id}")}))
        .collect::<Vec<_>>();
    fs::write(
        fixture.workspace.join("rows.json"),
        serde_json::to_vec(&rows).unwrap(),
    )
    .unwrap();
    fixture.start().await;
    let first = fixture.run(None).await;
    let session: SessionId = serde_json::from_value(
        first.iter().find(|v| v["type"] == "session").unwrap()["data"]["id"].clone(),
    )
    .unwrap();
    assert!(first.iter().any(|v| v.to_string().contains("call_started")));
    assert!(!json!(first).to_string().contains("CLI_PRIVATE_ROW_"));
    let selected = fixture.tool_result("select").await;
    assert_eq!(selected["output"]["value"]["selected"], json!([7]));
    fixture.stop().await;
    fs::remove_file(fixture.workspace.join("rows.json")).unwrap();
    fixture.start().await;
    let second = fixture.run(Some(session)).await;
    assert!(
        json!(second)
            .to_string()
            .contains("Unsaved globals and live handles are lost")
    );
    let restored = fixture.tool_result("restore").await;
    assert_eq!(
        restored["output"]["value"],
        json!({"selected":[7],"live":"undefined"})
    );
    fixture.stop().await;
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 5);
    assert!(
        requests
            .iter()
            .all(|request| !request.to_string().contains("CLI_PRIVATE_ROW_"))
    );
}

#[tokio::test]
async fn cli_interpreter_crash_invalidates_pending_cell_without_replaying_inner_calls() {
    let (endpoint,server)=provider(vec![
        eval("save_before_crash","const read=await host.call('read_file',{path:'note'});host.checkpoint({answer:read.result.content.data});return {saved:true};"),
        eval("crash","await host.call('read_file',{path:'note'});while(true){}"),
        eval("after_crash","return {answer:restored.answer,live:typeof read};"),done(),
    ]).await;
    let mut fixture = Fixture::new(&endpoint);
    fs::write(fixture.workspace.join("note"), "durable-data").unwrap();
    fixture.start().await;
    let cli = fixture
        .command()
        .args(["run", "Checkpoint then hold the cell"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let session = timeout(Duration::from_secs(20), async {
        loop {
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
                .filter(|record| {
                    record.event["data"]["command"]["type"] == "interpreter"
                        && record.event["data"]["command"]["data"]["event"]["kind"]
                            == "call_settled"
                })
                .count();
            if settled == 2 {
                let started = records
                    .iter()
                    .find(|record| {
                        record.event["data"]["command"]["data"]["event"]["outer_call"] == "crash"
                    })
                    .unwrap();
                break serde_json::from_value::<SessionId>(json!(started.aggregate)).unwrap();
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    fixture.host.as_mut().unwrap().start_kill().unwrap();
    fixture.host.as_mut().unwrap().wait().await.unwrap();
    fixture.host = None;
    fs::remove_file(fixture.workspace.join("note")).unwrap();
    fixture.start().await;
    let original = timeout(Duration::from_secs(20), cli.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !original.status.success(),
        "crashed request must not become success"
    );
    fixture.run(Some(session)).await;
    let restored = fixture.tool_result("after_crash").await;
    assert_eq!(
        restored["output"]["value"],
        json!({"answer":"durable-data","live":"undefined"})
    );
    assert_eq!(restored["state_lost"], true);
    let Response::Journal(records) = fixture
        .query(Command::Journal {
            after: 0,
            limit: 256,
        })
        .await
    else {
        panic!("missing journal")
    };
    assert_eq!(
        records
            .iter()
            .filter(
                |record| record.event["data"]["command"]["type"] == "interpreter"
                    && record.event["data"]["command"]["data"]["event"]["kind"] == "call_started"
            )
            .count(),
        2,
        "host restart must not replay either read"
    );
    fixture.stop().await;
    assert_eq!(server.await.unwrap().len(), 4);
    let store = orvek_harness::Store::open(fixture.socket.parent().unwrap()).unwrap();
    let state = store.load_session(session).unwrap();
    assert_eq!(
        state
            .interpreter
            .cells
            .values()
            .filter(|cell| matches!(
                cell.status,
                orvek_harness::interpreter::CellStatus::Interrupted
            ))
            .count(),
        1
    );
}
