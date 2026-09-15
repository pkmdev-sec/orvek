use orvek_harness::{
    Digest, Store, StoreError,
    import::{ImportLimits, LegacyArchive, PreparedImport, PublicationLimits, prepare_import},
    inference::ModelSettings,
    session::{SessionCommand, SessionConfig},
};
use rusqlite::{Connection, params};
use serde_json::json;
use uuid::Uuid;

struct Fixture {
    root: tempfile::TempDir,
    database: std::path::PathBuf,
    config: SessionConfig,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("legacy.sqlite3");
        let connection = Connection::open(&database).unwrap();
        connection.execute_batch("PRAGMA user_version=3; CREATE TABLE sessions(session_id TEXT PRIMARY KEY,parent_session_id TEXT,workspace TEXT NOT NULL,model TEXT NOT NULL,effort TEXT NOT NULL,reasoning_mode TEXT NOT NULL,fast_mode INTEGER NOT NULL,application_version TEXT NOT NULL,started_at_ms INTEGER NOT NULL,updated_at_ms INTEGER NOT NULL,preview TEXT NOT NULL); CREATE TABLE events(event_id INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT NOT NULL,record_json BLOB NOT NULL,prompt_text TEXT,prompt_recorded_at_ms INTEGER,assistant_stream TEXT); CREATE TABLE resume_states(session_id TEXT PRIMARY KEY,state_zstd BLOB NOT NULL); INSERT INTO sessions VALUES ('old',NULL,'/old/workspace','gpt-5.6-sol','\"high\"','\"pro\"',0,'old',10,20,'Historical task');").unwrap();
        append(
            &connection,
            1,
            "session.started",
            json!({"session_id":"old","workspace":"/old/workspace","model":"gpt-5.6-sol","effort":"high","reasoning_mode":"pro","fast_mode":false,"application_version":"old"}),
        );
        append(
            &connection,
            2,
            "user.submitted",
            json!({"id":1,"text":"original request"}),
        );
        append(
            &connection,
            3,
            "run.completed",
            json!({"success":true,"certificate":"not native authority"}),
        );
        drop(connection);
        let workspace = root.path().join("work");
        std::fs::create_dir(&workspace).unwrap();
        let config = SessionConfig {
            workspace: workspace.canonicalize().unwrap(),
            model: ModelSettings::default(),
            instructions: "fixture instructions".into(),
            context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
        };
        let store = Store::open(&root.path().join("host")).unwrap();
        Self {
            root,
            database,
            config,
            store,
        }
    }
    fn prepare(&self) -> PreparedImport {
        let archive = LegacyArchive::open(&self.database, ImportLimits::default()).unwrap();
        prepare_import(
            &archive,
            self.store.public_artifacts(),
            "old",
            PublicationLimits::default(),
        )
        .unwrap()
    }
}
fn append(connection: &Connection, sequence: u64, kind: &str, payload: serde_json::Value) {
    let record = json!({"schema_version":2,"sequence":sequence,"recorded_at_unix_ms":123,"source":"tact","type":kind,"payload":payload});
    connection
        .execute(
            "INSERT INTO events(session_id,record_json) VALUES ('old',?1)",
            params![serde_json::to_vec(&record).unwrap()],
        )
        .unwrap();
}
#[test]
fn retry_binding_survives_restart_and_missing_original_source() {
    let mut fixture = Fixture::new();
    let operation = Uuid::new_v4();
    let fingerprint = Digest::of(b"exact request");
    let prepared = fixture.prepare();
    let first = fixture
        .store
        .commit_legacy_import(operation, fingerprint, fixture.config.clone(), prepared)
        .unwrap();
    assert_eq!(
        first.imported.as_ref().unwrap().first_operation,
        Some(operation)
    );
    assert!(first.current_task.is_none());
    assert!(first.outcome.is_none());
    std::fs::remove_file(&fixture.database).unwrap();
    let root = fixture.root.path().join("host");
    drop(fixture.store);
    let store = Store::open(&root).unwrap();
    assert_eq!(
        store
            .lookup_legacy_import(operation, fingerprint)
            .unwrap()
            .unwrap()
            .id,
        first.id
    );
    assert!(matches!(
        store.lookup_legacy_import(operation, Digest::of(b"changed input")),
        Err(StoreError::Invalid(_))
    ));
}
#[test]
fn fresh_operations_reuse_identical_selected_bytes_and_preserve_native_progress() {
    let mut fixture = Fixture::new();
    let prepared = fixture.prepare();
    let first = fixture
        .store
        .commit_legacy_import(
            Uuid::new_v4(),
            Digest::of(b"first path"),
            fixture.config.clone(),
            prepared,
        )
        .unwrap();
    let progressed = fixture
        .store
        .session_command(
            first.id,
            first.revision,
            Uuid::new_v4(),
            SessionCommand::Feedback {
                message: "preserved native progress".into(),
            },
        )
        .unwrap();
    let other = Uuid::new_v4();
    let fingerprint = Digest::of(b"another path to identical source");
    let prepared = fixture.prepare();
    let reused = fixture
        .store
        .commit_legacy_import(other, fingerprint, fixture.config.clone(), prepared)
        .unwrap();
    assert_eq!(reused.id, first.id);
    assert_eq!(reused.history, progressed.history);
    assert_eq!(reused.admission(), progressed.admission());
    assert!(reused.revision > progressed.revision);
    let retry = fixture
        .store
        .lookup_legacy_import(other, fingerprint)
        .unwrap()
        .unwrap();
    assert_eq!(retry, reused);
    let fork_id = Default::default();
    fixture
        .store
        .create_handoff_session(fork_id, reused.fork_cursor())
        .unwrap();
    assert_eq!(
        fixture
            .store
            .lookup_legacy_import(other, fingerprint)
            .unwrap()
            .unwrap()
            .id,
        first.id
    );
}
#[test]
fn selected_source_changes_produce_a_new_target_only_for_fresh_operations() {
    let mut fixture = Fixture::new();
    let operation = Uuid::new_v4();
    let fingerprint = Digest::of(b"same source path");
    let prepared = fixture.prepare();
    let first = fixture
        .store
        .commit_legacy_import(operation, fingerprint, fixture.config.clone(), prepared)
        .unwrap();
    let source = Connection::open(&fixture.database).unwrap();
    append(
        &source,
        4,
        "user.submitted",
        json!({"id":2,"text":"new source record"}),
    );
    drop(source);
    assert_eq!(
        fixture
            .store
            .lookup_legacy_import(operation, fingerprint)
            .unwrap()
            .unwrap()
            .id,
        first.id
    );
    let fresh = fixture.prepare();
    let second = fixture
        .store
        .commit_legacy_import(Uuid::new_v4(), fingerprint, fixture.config.clone(), fresh)
        .unwrap();
    assert_ne!(first.id, second.id);
    assert_ne!(
        first.imported.unwrap().import_id,
        second.imported.unwrap().import_id
    );
    assert!(second.outcome.is_none());
    assert!(second.current_task.is_none());
}
#[test]
fn corrupted_binding_cannot_be_hidden_by_changing_the_operation_field() {
    let mut fixture = Fixture::new();
    let operation = Uuid::new_v4();
    let fingerprint = Digest::of(b"request");
    let prepared = fixture.prepare();
    let first = fixture
        .store
        .commit_legacy_import(operation, fingerprint, fixture.config.clone(), prepared)
        .unwrap();
    let connection = Connection::open(fixture.root.path().join("host/v1.sqlite3")).unwrap();
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT event FROM events WHERE aggregate=?1 AND revision=1",
            [first.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["data"]["imported"]["first_operation"] = json!(Uuid::new_v4());
    connection
        .execute(
            "UPDATE events SET event=?1 WHERE aggregate=?2 AND revision=1",
            params![serde_json::to_vec(&value).unwrap(), first.id.to_string()],
        )
        .unwrap();
    assert!(matches!(
        fixture.store.lookup_legacy_import(operation, fingerprint),
        Err(StoreError::Integrity(_))
    ));
}
#[test]
fn first_import_operation_cannot_be_reused_for_an_unrelated_session_command() {
    let mut fixture = Fixture::new();
    let operation = Uuid::new_v4();
    let prepared = fixture.prepare();
    let first = fixture
        .store
        .commit_legacy_import(
            operation,
            Digest::of(b"request"),
            fixture.config.clone(),
            prepared,
        )
        .unwrap();
    assert!(matches!(
        fixture.store.session_command(
            first.id,
            first.revision,
            operation,
            SessionCommand::Feedback {
                message: "must not be applied".into(),
            }
        ),
        Err(StoreError::Invalid(_))
    ));
}
