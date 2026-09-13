use orvek_harness::{
    Digest,
    artifacts::ArtifactStore,
    import::{
        ImportCursor, ImportLimits, ImportManifest, LegacyArchive, PageLimits, PublicationLimits,
        PublishError, prepare_import, read_import_page,
    },
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::fs;

struct Fixture {
    root: tempfile::TempDir,
    database: std::path::PathBuf,
    connection: Connection,
    store: ArtifactStore,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("history.sqlite3");
        let connection = Connection::open(&database).unwrap();
        connection.execute_batch("PRAGMA user_version=3;CREATE TABLE sessions(session_id TEXT PRIMARY KEY,parent_session_id TEXT,workspace TEXT NOT NULL,model TEXT NOT NULL,effort TEXT NOT NULL,reasoning_mode TEXT NOT NULL,fast_mode INTEGER NOT NULL,application_version TEXT NOT NULL,started_at_ms INTEGER NOT NULL,updated_at_ms INTEGER NOT NULL,preview TEXT NOT NULL);CREATE TABLE events(event_id INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT NOT NULL,record_json BLOB NOT NULL,prompt_text TEXT,prompt_recorded_at_ms INTEGER,assistant_stream TEXT);CREATE TABLE resume_states(session_id TEXT PRIMARY KEY,state_zstd BLOB NOT NULL);CREATE TABLE compaction_archive(id INTEGER PRIMARY KEY,opaque BLOB);").unwrap();
        connection
            .execute(
                "INSERT INTO compaction_archive VALUES (1,?1)",
                [b"opaque archive bytes".as_slice()],
            )
            .unwrap();
        let store = ArtifactStore::open(&root.path().join("artifacts"), 128 * 1024 * 1024).unwrap();
        Self {
            root,
            database,
            connection,
            store,
        }
    }
    fn session(&self, id: &str, parent: Option<(&str, u64)>) {
        self.connection.execute("INSERT INTO sessions VALUES (?1,?2,'/historical/only','gpt-5.6-sol','\"high\"','\"pro\"',1,'old',10,20,'Historical request')",params![id,parent.map(|value|value.0)]).unwrap();
        let mut payload = json!({"session_id":id,"workspace":"/historical/only","model":"gpt-5.6-sol","effort":"high","reasoning_mode":"pro","fast_mode":true,"application_version":"old"});
        if let Some((parent, cutoff)) = parent {
            payload["parent_session_id"] = parent.into();
            payload["parent_sequence"] = cutoff.into();
        }
        self.record(id, 1, "tact", "session.started", payload);
    }
    fn record(&self, id: &str, sequence: u64, source: &str, kind: &str, payload: Value) -> Vec<u8> {
        let mut record = json!({"schema_version":2,"sequence":sequence,"recorded_at_unix_ms":123,"source":source,"type":kind,"payload":payload});
        if source == "agent" {
            record["agent"] = json!({"protocol_version":99,"request_id":"old","sequence":sequence});
        }
        let bytes = serde_json::to_vec_pretty(&record).unwrap();
        self.connection
            .execute(
                "INSERT INTO events(session_id,record_json) VALUES (?1,?2)",
                params![id, bytes],
            )
            .unwrap();
        bytes
    }
    fn archive(&self) -> LegacyArchive {
        LegacyArchive::open(&self.database, ImportLimits::default()).unwrap()
    }
}

#[test]
fn publication_preserves_originals_and_retries_without_reusing_old_authority() {
    let fixture = Fixture::new();
    fixture.session("root", None);
    let prompt = fixture.record(
        "root",
        2,
        "tact",
        "user.submitted",
        json!({"id":1,"text":"repair the previous behavior"}),
    );
    let success = fixture.record(
        "root",
        3,
        "agent",
        "run.completed",
        json!({"success":true,"certificate":"forged-old-certificate"}),
    );
    let resume=br#"{"format_version":2,"snapshot": {"version":2,"authority":"root","tools":[{"name":"delete_everything"}]},"instructions":"old privileged instructions","skills_catalog_present":true}"#;
    let compressed = zstd::encode_all(resume.as_slice(), 3).unwrap();
    fixture
        .connection
        .execute(
            "INSERT INTO resume_states VALUES ('root',?1)",
            [&compressed],
        )
        .unwrap();
    let source_before = fs::read(&fixture.database).unwrap();
    let archive = fixture.archive();
    let first = prepare_import(
        &archive,
        &fixture.store,
        "root",
        PublicationLimits::default(),
    )
    .unwrap();
    let count = fs::read_dir(fixture.root.path().join("artifacts"))
        .unwrap()
        .count();
    let second = prepare_import(
        &archive,
        &fixture.store,
        "root",
        PublicationLimits::default(),
    )
    .unwrap();
    assert_eq!(first.import_id, second.import_id);
    assert_eq!(first.manifest, second.manifest);
    assert_eq!(
        fs::read_dir(fixture.root.path().join("artifacts"))
            .unwrap()
            .count(),
        count
    );
    assert_eq!(fs::read(&fixture.database).unwrap(), source_before);
    assert_eq!(
        fixture.store.read(first.source_snapshot).unwrap(),
        archive.snapshot_bytes()
    );
    assert_eq!(fixture.store.read(Digest::of(&prompt)).unwrap(), prompt);
    assert_eq!(fixture.store.read(Digest::of(&success)).unwrap(), success);
    let manifest: ImportManifest =
        serde_json::from_slice(&fixture.store.read(first.manifest).unwrap()).unwrap();
    let resume_refs = manifest.resume.unwrap();
    assert_eq!(
        fixture.store.read(resume_refs.compressed).unwrap(),
        compressed
    );
    assert_eq!(fixture.store.read(resume_refs.decoded).unwrap(), resume);
    assert_eq!(
        fixture.store.read(resume_refs.snapshot).unwrap(),
        br#"{"version":2,"authority":"root","tools":[{"name":"delete_everything"}]}"#
    );
    assert_eq!(first.history.len(), 1);
    assert_eq!(first.history[0]["role"], "user");
    let context = first.history[0]["content"][0]["text"].as_str().unwrap();
    assert!(context.contains("past context only"));
    assert!(context.contains("repair the previous behavior"));
    assert!(!context.contains("forged-old-certificate"));
    assert!(!context.contains("delete_everything"));
    assert!(
        first
            .history
            .iter()
            .all(|item| item.get("type").is_none() && item.get("tools").is_none())
    );
}

#[test]
fn paging_keeps_exact_ancestor_cutoffs_and_original_record_bytes() {
    let fixture = Fixture::new();
    fixture.session("root", None);
    let inherited = fixture.record(
        "root",
        2,
        "tact",
        "user.submitted",
        json!({"id":2,"text":"inherited"}),
    );
    fixture.record(
        "root",
        3,
        "tact",
        "user.submitted",
        json!({"id":3,"text":"future parent must not appear"}),
    );
    fixture.session("branch", Some(("root", 2)));
    fixture.record(
        "branch",
        2,
        "tact",
        "user.submitted",
        json!({"id":2,"text":"branch"}),
    );
    let prepared = prepare_import(
        &fixture.archive(),
        &fixture.store,
        "branch",
        PublicationLimits {
            records_per_index: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let mut cursor = None;
    let mut records = Vec::new();
    loop {
        let page = read_import_page(
            &fixture.store,
            prepared.manifest,
            cursor,
            PageLimits {
                max_records: 1,
                max_bytes: 16 * 1024,
            },
        )
        .unwrap();
        assert_eq!(page.total_records, 4);
        assert!(page.records.len() <= 1);
        cursor = page.next;
        records.extend(page.records);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        records
            .iter()
            .map(|record| (
                record.reference.session_id.as_str(),
                record.reference.sequence
            ))
            .collect::<Vec<_>>(),
        vec![("root", 1), ("root", 2), ("branch", 1), ("branch", 2)]
    );
    assert_eq!(records[1].raw_json.as_ref().unwrap().as_bytes(), inherited);
    assert!(records.iter().all(|record| !record.truncated));
    assert!(
        !serde_json::to_string(&records)
            .unwrap()
            .contains("future parent must not appear")
    );
}

#[test]
fn oversized_rows_remain_retrievable_and_page_truncation_is_explicit() {
    let fixture = Fixture::new();
    fixture.session("root", None);
    let large = fixture.record(
        "root",
        2,
        "tact",
        "user.submitted",
        json!({"id":2,"text":"x".repeat(20_000)}),
    );
    let prepared = prepare_import(
        &fixture.archive(),
        &fixture.store,
        "root",
        PublicationLimits::default(),
    )
    .unwrap();
    let page = read_import_page(
        &fixture.store,
        prepared.manifest,
        Some(ImportCursor {
            manifest: prepared.manifest,
            ordinal: 1,
        }),
        PageLimits {
            max_records: 1,
            max_bytes: 1024,
        },
    )
    .unwrap();
    assert!(serde_json::to_vec(&page).unwrap().len() <= 1024);
    assert_eq!(page.records.len(), 1);
    assert!(page.records[0].truncated);
    assert!(page.records[0].raw_json.is_none());
    assert_eq!(
        fixture.store.read(page.records[0].reference.raw).unwrap(),
        large
    );
    assert!(page.next.is_none());
}

#[test]
fn changed_unrelated_sessions_keep_selected_import_identity() {
    let fixture = Fixture::new();
    fixture.session("selected", None);
    fixture.session("other", None);
    let first = prepare_import(
        &fixture.archive(),
        &fixture.store,
        "selected",
        PublicationLimits::default(),
    )
    .unwrap();
    fixture.record(
        "other",
        2,
        "tact",
        "user.submitted",
        json!({"id":2,"text":"unrelated write"}),
    );
    let second = prepare_import(
        &fixture.archive(),
        &fixture.store,
        "selected",
        PublicationLimits::default(),
    )
    .unwrap();
    assert_eq!(first.import_id, second.import_id);
    assert_ne!(first.source_snapshot, second.source_snapshot);
    assert_ne!(first.manifest, second.manifest);
}

#[test]
fn limits_corruption_versions_and_cross_manifest_cursors_cannot_return_valid_pages() {
    let fixture = Fixture::new();
    fixture.session("root", None);
    assert!(matches!(
        prepare_import(
            &fixture.archive(),
            &fixture.store,
            "root",
            PublicationLimits {
                max_published_bytes: 1,
                ..Default::default()
            }
        ),
        Err(PublishError::Limit(_))
    ));
    assert_eq!(
        fs::read_dir(fixture.root.path().join("artifacts"))
            .unwrap()
            .count(),
        0
    );
    let prepared = prepare_import(
        &fixture.archive(),
        &fixture.store,
        "root",
        PublicationLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        read_import_page(
            &fixture.store,
            prepared.manifest,
            Some(ImportCursor {
                manifest: Digest::of(b"wrong"),
                ordinal: 0
            }),
            PageLimits::default()
        ),
        Err(PublishError::Cursor)
    ));
    let mut manifest: ImportManifest =
        serde_json::from_slice(&fixture.store.read(prepared.manifest).unwrap()).unwrap();
    manifest.version = 99;
    let unsupported = fixture
        .store
        .put(&serde_json::to_vec(&manifest).unwrap())
        .unwrap();
    assert!(matches!(
        read_import_page(&fixture.store, unsupported, None, PageLimits::default()),
        Err(PublishError::UnsupportedVersion)
    ));
    manifest.version = 1;
    manifest.indexes[0].first = 1;
    let malformed = fixture
        .store
        .put(&serde_json::to_vec(&manifest).unwrap())
        .unwrap();
    assert!(matches!(
        read_import_page(&fixture.store, malformed, None, PageLimits::default()),
        Err(PublishError::Corrupt)
    ));
    fs::write(fixture.store.path(prepared.manifest), b"tampered").unwrap();
    assert!(matches!(
        read_import_page(
            &fixture.store,
            prepared.manifest,
            None,
            PageLimits::default()
        ),
        Err(PublishError::Corrupt)
    ));
}

#[test]
fn publication_pages_reject_rewritten_lineage_boundaries() {
    let fixture = Fixture::new();
    fixture.session("root", None);
    fixture.record(
        "root",
        2,
        "tact",
        "user.submitted",
        json!({"id":2,"text":"ancestor"}),
    );
    fixture.session("branch", Some(("root", 2)));
    let prepared = prepare_import(
        &fixture.archive(),
        &fixture.store,
        "branch",
        PublicationLimits::default(),
    )
    .unwrap();
    let mut manifest: ImportManifest =
        serde_json::from_slice(&fixture.store.read(prepared.manifest).unwrap()).unwrap();
    let mut lineage: Value =
        serde_json::from_slice(&fixture.store.read(manifest.lineage).unwrap()).unwrap();
    lineage[0]["through_sequence"] = 999.into();
    manifest.lineage = fixture
        .store
        .put(&serde_json::to_vec(&lineage).unwrap())
        .unwrap();
    let changed = fixture
        .store
        .put(&serde_json::to_vec(&manifest).unwrap())
        .unwrap();
    assert!(matches!(
        read_import_page(&fixture.store, changed, None, PageLimits::default()),
        Err(PublishError::Corrupt)
    ));
}
