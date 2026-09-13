use orvek_harness::{
    controller::Host,
    inference::{
        Limits, ModelSettings, ResponsesClient, Route, Transport,
        auth::{Auth, SecretString},
    },
    runtime::DockerExecutor,
    session::SessionConfig,
};
use rusqlite::{Connection, params};
use serde_json::json;

#[tokio::test]
#[ignore = "requires local Docker and configured ORVEK_EXECUTOR_HELPER"]
async fn legacy_import_creates_one_native_session_without_old_completion_or_execution() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("legacy.sqlite3");
    let connection = Connection::open(&database).unwrap();
    connection.execute_batch("PRAGMA user_version=3; CREATE TABLE sessions(session_id TEXT PRIMARY KEY,parent_session_id TEXT,workspace TEXT NOT NULL,model TEXT NOT NULL,effort TEXT NOT NULL,reasoning_mode TEXT NOT NULL,fast_mode INTEGER NOT NULL,application_version TEXT NOT NULL,started_at_ms INTEGER NOT NULL,updated_at_ms INTEGER NOT NULL,preview TEXT NOT NULL); CREATE TABLE events(event_id INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT NOT NULL,record_json BLOB NOT NULL,prompt_text TEXT,prompt_recorded_at_ms INTEGER,assistant_stream TEXT); CREATE TABLE resume_states(session_id TEXT PRIMARY KEY,state_zstd BLOB NOT NULL); INSERT INTO sessions VALUES ('old',NULL,'/old/workspace','gpt-5.6-sol','\"high\"','\"pro\"',0,'old',10,20,'Historical task');").unwrap();
    for (sequence, kind, payload) in [
        (
            1,
            "session.started",
            json!({"session_id":"old","workspace":"/old/workspace","model":"gpt-5.6-sol","effort":"high","reasoning_mode":"pro","fast_mode":false,"application_version":"old"}),
        ),
        (
            2,
            "user.submitted",
            json!({"id":1,"text":"repair the old behavior"}),
        ),
        (
            3,
            "run.completed",
            json!({"success":true,"certificate":"old success is not native evidence"}),
        ),
    ] {
        let record = json!({"schema_version":2,"sequence":sequence,"recorded_at_unix_ms":123,"source":"tact","type":kind,"payload":payload});
        connection
            .execute(
                "INSERT INTO events(session_id,record_json) VALUES ('old',?1)",
                params![serde_json::to_vec(&record).unwrap()],
            )
            .unwrap();
    }
    let before = std::fs::read(&database).unwrap();
    let client = ResponsesClient::new(
        Auth::api_key(SecretString::new("fixture".into())).unwrap(),
        Route::new(Transport::Http, "http://127.0.0.1:1/v1/responses").unwrap(),
        Limits {
            max_attempts: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let host = Host::open(
        &root.path().join("host"),
        client,
        DockerExecutor::connect("debian:bookworm-slim")
            .await
            .unwrap(),
    )
    .unwrap();
    let config = SessionConfig {
        workspace: source,
        model: ModelSettings::default(),
        instructions: String::new(),
    };
    let operation = uuid::Uuid::new_v4();
    let first = host
        .import_legacy_request(operation, database.clone(), "old".into(), config.clone())
        .await
        .unwrap();
    let repeated = host
        .import_legacy_request(operation, database.clone(), "old".into(), config.clone())
        .await
        .unwrap();
    assert_eq!(first, repeated);
    assert_eq!(first.current_task, None);
    assert_eq!(first.active_request, None);
    assert_eq!(first.outcome, None);
    assert_eq!(first.imported.as_ref().unwrap().source_session, "old");
    assert!(first.history.iter().all(|item| item["role"] == "user"
        && item["content"].as_array().is_some_and(|parts| {
            parts
                .iter()
                .all(|part| part["type"] == "input_text" && part["text"].is_string())
        })));
    assert_eq!(
        host.legacy_page(first.id, None, 32, 64 * 1024)
            .await
            .unwrap()
            .total_records,
        3
    );
    assert_eq!(std::fs::read(&database).unwrap(), before);
    std::fs::remove_file(&database).unwrap();
    std::fs::remove_dir(&config.workspace).unwrap();
    let recovered = host
        .import_legacy_request(operation, database.clone(), "old".into(), config.clone())
        .await
        .unwrap();
    assert_eq!(recovered.id, first.id);
    assert!(
        host.import_legacy_request(uuid::Uuid::new_v4(), database, "old".into(), config)
            .await
            .is_err()
    );

    assert_eq!(host.sessions(0, 64).await.unwrap().0.len(), 1);
}
