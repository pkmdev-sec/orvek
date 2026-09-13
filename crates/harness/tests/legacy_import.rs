use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};
use orvek_harness::{
    Digest,
    import::{ImportError, ImportLimits, LegacyArchive},
};
use tempfile::TempDir;

struct Fixture {
    directory: TempDir,
    path: PathBuf,
    connection: Connection,
}
impl Fixture {
    fn new(version: u32) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v2.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE sessions(session_id TEXT PRIMARY KEY,parent_session_id TEXT,workspace TEXT NOT NULL,model TEXT NOT NULL,effort TEXT NOT NULL,reasoning_mode TEXT NOT NULL,fast_mode INTEGER NOT NULL,application_version TEXT NOT NULL,started_at_ms INTEGER NOT NULL,updated_at_ms INTEGER NOT NULL,preview TEXT NOT NULL);CREATE TABLE events(event_id INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT NOT NULL,record_json BLOB NOT NULL,prompt_text TEXT,prompt_recorded_at_ms INTEGER,assistant_stream TEXT);CREATE INDEX events_by_session ON events(session_id,event_id);CREATE TABLE resume_states(session_id TEXT PRIMARY KEY,state_zstd BLOB NOT NULL);").unwrap();
        connection
            .pragma_update(None, "user_version", version)
            .unwrap();
        Self {
            directory,
            path,
            connection,
        }
    }
    fn session(&self, id: &str, parent: Option<(&str, u64)>) {
        add_session(&self.connection, id, parent);
    }
    fn record(&self, id: &str, record: Value) {
        raw_record(&self.connection, id, record.to_string().as_bytes());
    }
    fn resume(&self, id: &str, wrapper: u32, vendor: u32, extra: &str) -> Vec<u8> {
        let raw=format!("{{\"format_version\":{wrapper},\"snapshot\": {{\"version\":{vendor},\"model\":\"gpt-5.6-sol\",\"opaque\":\"{extra}\"}},\"instructions\":\"untrusted old instructions\",\"skills_catalog_present\":true}}").into_bytes();
        let compressed = zstd::encode_all(raw.as_slice(), 3).unwrap();
        self.connection
            .execute(
                "INSERT OR REPLACE INTO resume_states VALUES (?1,?2)",
                params![id, compressed],
            )
            .unwrap();
        raw
    }
    fn open(&self) -> LegacyArchive {
        LegacyArchive::open(&self.path, ImportLimits::default()).unwrap()
    }
}
fn add_session(connection: &Connection, id: &str, parent: Option<(&str, u64)>) {
    connection.execute("INSERT INTO sessions VALUES (?1,?2,'/old/workspace','gpt-5.6-sol','\"high\"','\"pro\"',1,'fixture',100,200,'preview')",params![id,parent.map(|p|p.0)]).unwrap();
    let mut payload = json!({"session_id":id,"model":"gpt-5.6-sol","effort":"high","reasoning_mode":"pro","fast_mode":true,"workspace":"/old/workspace","application_version":"fixture"});
    if let Some((id, cutoff)) = parent {
        payload["parent_session_id"] = id.into();
        payload["parent_sequence"] = cutoff.into();
    }
    raw_record(
        connection,
        id,
        event(1, "tact", "session.started", payload)
            .to_string()
            .as_bytes(),
    );
}
fn raw_record(connection: &Connection, id: &str, bytes: &[u8]) {
    connection
        .execute(
            "INSERT INTO events(session_id,record_json) VALUES (?1,?2)",
            params![id, bytes],
        )
        .unwrap();
}
fn event(sequence: u64, source: &str, kind: &str, payload: Value) -> Value {
    let mut record = json!({"schema_version":2,"sequence":sequence,"recorded_at_unix_ms":123,"source":source,"type":kind,"payload":payload});
    if source == "agent" {
        record["agent"] =
            json!({"protocol_version":99,"request_id":"old-request","sequence":sequence});
    }
    record
}
fn prompt(sequence: u64, text: &str) -> Value {
    event(
        sequence,
        "tact",
        "user.submitted",
        json!({"id":sequence,"text":text}),
    )
}

#[test]
fn exports_raw_history_and_resume_bytes_without_claiming_task_success() {
    let fixture = Fixture::new(2);
    fixture.session("root", None);
    let raw=b"{ \"schema_version\":2,\"sequence\":2,\"recorded_at_unix_ms\":124,\"source\":\"tact\",\"type\":\"user.submitted\",\"payload\":{\"text\":\"original spacing\",\"id\":2}}";
    raw_record(&fixture.connection, "root", raw);
    fixture.record(
        "root",
        event(
            3,
            "agent",
            "run.completed",
            json!({"success":true,"certificate":"forged"}),
        ),
    );
    let decoded = fixture.resume("root", 2, 1, "opaque original state");
    let before = fs::read(&fixture.path).unwrap();
    let archive = fixture.open();
    let exported = archive.export("root").unwrap();
    assert_eq!(archive.database_version(), 2);
    assert_eq!(exported.records[1].raw_json, raw);
    assert_eq!(exported.records[1].raw_digest, Digest::of(raw));
    assert_eq!(exported.records[2].kind, "run.completed");
    assert_eq!(exported.records[2].agent_protocol_version, Some(99));
    assert_eq!(exported.lineage[0].metadata.effort.raw, "\"high\"");
    assert_eq!(
        exported.lineage[0].metadata.effort.value.as_deref(),
        Some("high")
    );
    assert_eq!(
        exported.lineage[0].metadata.reasoning_mode.value.as_deref(),
        Some("pro")
    );
    let resume = exported.resume.unwrap();
    assert_eq!(resume.decoded_json, decoded);
    assert_eq!(
        zstd::decode_all(resume.compressed_zstd.as_slice()).unwrap(),
        decoded
    );
    assert_eq!(
        resume.snapshot_digest,
        Digest::of(
            b"{\"version\":1,\"model\":\"gpt-5.6-sol\",\"opaque\":\"opaque original state\"}"
        )
    );
    assert_eq!(archive.snapshot_id(), Digest::of(archive.snapshot_bytes()));
    assert_eq!(fs::read(&fixture.path).unwrap(), before);
}

#[test]
fn retry_identity_is_content_based_and_excludes_unrelated_sessions() {
    let fixture = Fixture::new(2);
    fixture.session("root", None);
    fixture.record("root", prompt(2, "original"));
    let first = fixture.open();
    let first_export = first.export("root").unwrap();
    let retry = fixture.open();
    assert_eq!(first.snapshot_id(), retry.snapshot_id());
    assert_eq!(
        first_export.import_id,
        retry.export("root").unwrap().import_id
    );
    let copied = fixture.directory.path().join("different-name.sqlite");
    fs::copy(&fixture.path, &copied).unwrap();
    let copied = LegacyArchive::open(&copied, ImportLimits::default()).unwrap();
    assert_eq!(
        first_export.import_id,
        copied.export("root").unwrap().import_id
    );
    fixture.session("unrelated", None);
    fixture.record("unrelated", prompt(2, "other data"));
    let changed = fixture.open();
    assert_ne!(first.snapshot_id(), changed.snapshot_id());
    assert_eq!(
        first_export.import_id,
        changed.export("root").unwrap().import_id
    );
    fixture.record("root", prompt(3, "new selected data"));
    assert_ne!(
        first_export.import_id,
        fixture.open().export("root").unwrap().import_id
    );
    assert_eq!(first.export("root").unwrap().records.len(), 2);
}

#[test]
fn branches_include_only_exact_ancestor_cutoffs_in_order() {
    let fixture = Fixture::new(2);
    fixture.session("root", None);
    fixture.record("root", prompt(2, "inherited root"));
    fixture.record("root", prompt(3, "root future"));
    fixture.session("fork", Some(("root", 2)));
    fixture.record("fork", prompt(2, "inherited fork"));
    fixture.record("fork", prompt(3, "fork future"));
    fixture.session("leaf", Some(("fork", 2)));
    fixture.record("leaf", prompt(2, "leaf"));
    let exported = fixture.open().export("leaf").unwrap();
    assert_eq!(
        exported
            .lineage
            .iter()
            .map(|s| s.metadata.session_id.as_str())
            .collect::<Vec<_>>(),
        ["root", "fork", "leaf"]
    );
    assert_eq!(
        exported
            .lineage
            .iter()
            .map(|s| s.through_sequence)
            .collect::<Vec<_>>(),
        [Some(2), Some(2), None]
    );
    assert_eq!(exported.records.len(), 6);
    assert_eq!(exported.lineage[2].first_record_index, 4);
    assert!(
        exported
            .records
            .iter()
            .all(|r| !String::from_utf8_lossy(&r.raw_json).contains("future"))
    );
}

#[test]
fn zero_cutoff_preserves_lineage_without_inheriting_ancestor_context() {
    let fixture = Fixture::new(2);
    fixture.session("root", None);
    fixture.record("root", prompt(2, "not inherited"));
    fixture.session("fork", Some(("root", 0)));
    let exported = fixture.open().export("fork").unwrap();
    assert_eq!(exported.lineage[0].record_count, 0);
    assert_eq!(exported.lineage[0].through_sequence, Some(0));
    assert_eq!(exported.records.len(), 1);
    assert_eq!(exported.records[0].session_id, "fork");
}

#[test]
fn malformed_parent_tail_after_a_valid_cutoff_is_preserved_but_not_decoded() {
    let fixture = Fixture::new(2);
    fixture.session("parent", None);
    fixture.record("parent", prompt(2, "inherited"));
    raw_record(&fixture.connection, "parent", b"not-json");
    fixture.session("fork", Some(("parent", 2)));
    let archive = fixture.open();
    assert_eq!(archive.export("fork").unwrap().records.len(), 3);
    assert!(matches!(
        archive.export("parent"),
        Err(ImportError::CorruptRecord)
    ));
    let restored = fixture.directory.path().join("snapshot.sqlite");
    fs::write(&restored, archive.snapshot_bytes()).unwrap();
    let connection = Connection::open(restored).unwrap();
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT record_json FROM events WHERE record_json=?1",
            [b"not-json".as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bytes, b"not-json");
}

#[test]
fn missing_ancestors_cycles_and_unpersisted_cutoffs_are_rejected() {
    let fixture = Fixture::new(2);
    fixture.session("missing", Some(("absent", 1)));
    assert!(matches!(
        fixture.open().export("missing"),
        Err(ImportError::MissingAncestor)
    ));
    fixture.session("cycle-a", Some(("cycle-b", 1)));
    fixture.session("cycle-b", Some(("cycle-a", 1)));
    assert!(matches!(
        fixture.open().export("cycle-a"),
        Err(ImportError::LineageCycle)
    ));
    fixture.session("parent", None);
    fixture.record("parent", prompt(3, "sequence gap"));
    fixture.session("gap", Some(("parent", 2)));
    assert!(matches!(
        fixture.open().export("gap"),
        Err(ImportError::InvalidForkCutoff)
    ));
    fixture.session("beyond", Some(("parent", 4)));
    assert!(matches!(
        fixture.open().export("beyond"),
        Err(ImportError::InvalidForkCutoff)
    ));
}

#[test]
fn schema_three_archives_are_preserved_opaquely_in_the_consistent_snapshot() {
    let fixture = Fixture::new(3);
    fixture.session("root", None);
    fixture.resume("root", 2, 2, "snapshot variant two");
    fixture.connection.execute_batch("CREATE TABLE compaction_archives(archive_id TEXT PRIMARY KEY,payload_zstd BLOB NOT NULL,format_hint TEXT);").unwrap();
    let opaque = b"opaque archive format is not interpreted";
    fixture
        .connection
        .execute(
            "INSERT INTO compaction_archives VALUES ('archive-1',?1,'vendor-private')",
            [opaque.as_slice()],
        )
        .unwrap();
    let archive = fixture.open();
    let item = archive
        .schema()
        .iter()
        .find(|o| o.name == "compaction_archives")
        .unwrap();
    assert!(item.opaque);
    assert_eq!(item.opaque_row_count, Some(1));
    assert_eq!(
        archive
            .export("root")
            .unwrap()
            .resume
            .unwrap()
            .snapshot_version,
        2
    );
    let copy = fixture.directory.path().join("restored.sqlite3");
    fs::write(&copy, archive.snapshot_bytes()).unwrap();
    let connection = Connection::open(copy).unwrap();
    let preserved: Vec<u8> = connection
        .query_row("SELECT payload_zstd FROM compaction_archives", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(preserved, opaque);
}

#[test]
fn unsupported_database_record_wrapper_and_snapshot_versions_are_explicit_errors() {
    for version in [0, 1, 4, 99] {
        let fixture = Fixture::new(version);
        assert!(
            matches!(LegacyArchive::open(&fixture.path,ImportLimits::default()),Err(ImportError::UnsupportedDatabaseVersion(found)) if found==version)
        );
    }
    let fixture = Fixture::new(2);
    fixture.session("root", None);
    let mut newer = prompt(2, "new envelope");
    newer["schema_version"] = 3.into();
    fixture.record("root", newer);
    assert!(matches!(
        fixture.open().export("root"),
        Err(ImportError::UnsupportedRecordVersion(3))
    ));
    let fixture = Fixture::new(2);
    fixture.session("root", None);
    fixture.resume("root", 3, 1, "new wrapper");
    assert!(matches!(
        fixture.open().export("root"),
        Err(ImportError::UnsupportedResumeVersion(3))
    ));
    fixture.resume("root", 2, 3, "new snapshot");
    assert!(matches!(
        fixture.open().export("root"),
        Err(ImportError::UnsupportedSnapshotVersion(3))
    ));
}

#[test]
fn duplicate_sequences_invalid_start_and_truncated_records_cannot_be_imported() {
    for raw in [
        b"{truncated".as_slice(),
        b"{\"schema_version\":2}".as_slice(),
    ] {
        let fixture = Fixture::new(2);
        fixture.session("root", None);
        raw_record(&fixture.connection, "root", raw);
        assert!(matches!(
            fixture.open().export("root"),
            Err(ImportError::CorruptRecord)
        ));
    }
    let fixture = Fixture::new(2);
    fixture.session("root", None);
    fixture.record("root", prompt(1, "duplicate"));
    assert!(matches!(
        fixture.open().export("root"),
        Err(ImportError::CorruptRecord)
    ));
    let fixture = Fixture::new(2);
    fixture.session("root", None);
    fixture
        .connection
        .execute(
            "UPDATE events SET record_json=?1",
            [prompt(1, "missing start").to_string().as_bytes()],
        )
        .unwrap();
    assert!(matches!(
        fixture.open().export("root"),
        Err(ImportError::InvalidLineage)
    ));
}

#[test]
fn resume_decompression_rejects_truncation_non_json_and_expansion_bombs() {
    let fixture = Fixture::new(2);
    fixture.session("root", None);
    fixture.resume("root", 2, 1, "ok");
    let mut compressed: Vec<u8> = fixture
        .connection
        .query_row("SELECT state_zstd FROM resume_states", [], |r| r.get(0))
        .unwrap();
    compressed.truncate(compressed.len() - 3);
    fixture
        .connection
        .execute("UPDATE resume_states SET state_zstd=?1", [compressed])
        .unwrap();
    assert!(matches!(
        fixture.open().export("root"),
        Err(ImportError::CorruptResume)
    ));
    fixture
        .connection
        .execute(
            "UPDATE resume_states SET state_zstd=?1",
            [zstd::encode_all(b"not json".as_slice(), 3).unwrap()],
        )
        .unwrap();
    assert!(matches!(
        fixture.open().export("root"),
        Err(ImportError::CorruptResume)
    ));
    fixture.resume("root", 2, 1, &"x".repeat(200000));
    let limited = LegacyArchive::open(
        &fixture.path,
        ImportLimits {
            max_decoded_resume_bytes: 1024,
            ..ImportLimits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        limited.export("root"),
        Err(ImportError::Limit("decoded resume bytes"))
    ));
    let limited = LegacyArchive::open(
        &fixture.path,
        ImportLimits {
            max_compressed_resume_bytes: 16,
            ..ImportLimits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        limited.export("root"),
        Err(ImportError::Limit("compressed resume bytes"))
    ));
}

#[test]
fn byte_row_lineage_and_archive_limits_fail_instead_of_truncating_silently() {
    let fixture = Fixture::new(2);
    fixture.session("root", None);
    fixture.record("root", prompt(2, &"x".repeat(2000)));
    fixture.session("child", Some(("root", 1)));
    assert!(matches!(
        LegacyArchive::open(
            &fixture.path,
            ImportLimits {
                max_snapshot_bytes: 4096,
                ..ImportLimits::default()
            }
        ),
        Err(ImportError::Limit("snapshot bytes"))
    ));
    for (limits, bound) in [
        (
            ImportLimits {
                max_records: 1,
                ..ImportLimits::default()
            },
            "record rows",
        ),
        (
            ImportLimits {
                max_record_bytes: 1024,
                ..ImportLimits::default()
            },
            "record bytes",
        ),
        (
            ImportLimits {
                max_selected_bytes: 512,
                ..ImportLimits::default()
            },
            "selected bytes",
        ),
    ] {
        assert!(
            matches!(LegacyArchive::open(&fixture.path,limits).unwrap().export("root"),Err(ImportError::Limit(found)) if found==bound)
        );
    }
    let limited = LegacyArchive::open(
        &fixture.path,
        ImportLimits {
            max_sessions: 1,
            ..ImportLimits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        limited.sessions(),
        Err(ImportError::Limit("session rows"))
    ));
    let limited = LegacyArchive::open(
        &fixture.path,
        ImportLimits {
            max_lineage_depth: 1,
            ..ImportLimits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        limited.export("child"),
        Err(ImportError::Limit("lineage depth"))
    ));
    fixture
        .connection
        .execute_batch(
            "CREATE TABLE opaque_archive(id INTEGER);INSERT INTO opaque_archive VALUES (1),(2);",
        )
        .unwrap();
    assert!(matches!(
        LegacyArchive::open(
            &fixture.path,
            ImportLimits {
                max_archive_rows: 1,
                ..ImportLimits::default()
            }
        ),
        Err(ImportError::Limit("opaque archive rows"))
    ));
}

#[test]
fn missing_or_corrupt_source_is_not_created_or_rewritten() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.sqlite3");
    assert!(LegacyArchive::open(&missing, ImportLimits::default()).is_err());
    assert!(!missing.exists());
    let corrupt = directory.path().join("corrupt.sqlite3");
    fs::write(&corrupt, b"not sqlite").unwrap();
    assert!(LegacyArchive::open(&corrupt, ImportLimits::default()).is_err());
    assert_eq!(fs::read(corrupt).unwrap(), b"not sqlite");
}

#[test]
fn active_wal_writer_cannot_mix_transcript_and_resume_generations_in_a_snapshot() {
    let fixture = Fixture::new(2);
    fixture
        .connection
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    fixture
        .connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    fixture.session("root", None);
    let ready = Arc::new(Barrier::new(2));
    let path = fixture.path.clone();
    let signal = ready.clone();
    let writer = thread::spawn(move || {
        let mut connection = Connection::open(path).unwrap();
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        connection
            .pragma_update(None, "wal_autocheckpoint", 0)
            .unwrap();
        for generation in 1..=100u64 {
            let transaction = connection.transaction().unwrap();
            raw_record(
                &transaction,
                "root",
                prompt(generation + 1, &format!("generation {generation}"))
                    .to_string()
                    .as_bytes(),
            );
            let state = json!({"format_version":2,"snapshot":{"version":2,"generation":generation},"instructions":"historical only","skills_catalog_present":false});
            let compressed = zstd::encode_all(state.to_string().as_bytes(), 3).unwrap();
            transaction
                .execute(
                    "INSERT OR REPLACE INTO resume_states VALUES ('root',?1)",
                    [compressed],
                )
                .unwrap();
            transaction.commit().unwrap();
            if generation == 1 {
                signal.wait();
            }
            thread::yield_now();
        }
    });
    ready.wait();
    let archive = fixture.open();
    let before = archive.export("root").unwrap();
    writer.join().unwrap();
    let resume: Value =
        serde_json::from_slice(&before.resume.as_ref().unwrap().decoded_json).unwrap();
    let generation = resume["snapshot"]["generation"].as_u64().unwrap();
    assert_eq!(before.records.last().unwrap().sequence, generation + 1);
    assert_eq!(before.records.len() as u64, generation + 1);
    assert_eq!(archive.export("root").unwrap().import_id, before.import_id);
    let latest = fixture.open().export("root").unwrap();
    assert_eq!(latest.records.len(), 101);
}

#[test]
fn uncommitted_wal_generation_is_excluded_while_the_writer_remains_active() {
    let fixture = Fixture::new(2);
    fixture
        .connection
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    fixture.session("root", None);
    fixture.resume("root", 2, 2, "committed original");
    let begun = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let thread_begun = begun.clone();
    let thread_release = release.clone();
    let path = fixture.path.clone();
    let writer = thread::spawn(move || {
        let mut connection = Connection::open(path).unwrap();
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        raw_record(
            &transaction,
            "root",
            prompt(2, "pending write").to_string().as_bytes(),
        );
        transaction
            .execute(
                "UPDATE resume_states SET state_zstd=?1",
                [b"uncommitted malformed data".as_slice()],
            )
            .unwrap();
        thread_begun.wait();
        thread_release.wait();
        transaction.rollback().unwrap();
    });
    begun.wait();
    let archive = fixture.open();
    let exported = archive.export("root").unwrap();
    assert_eq!(exported.records.len(), 1);
    assert!(
        String::from_utf8_lossy(&exported.resume.unwrap().decoded_json)
            .contains("committed original")
    );
    release.wait();
    writer.join().unwrap();
}

#[test]
fn unknown_legacy_setting_text_is_preserved_without_defaulting_the_model_policy() {
    let fixture = Fixture::new(2);
    fixture.session("root", None);
    fixture.connection.execute("UPDATE sessions SET effort='unknown legacy encoding',reasoning_mode='\"future-mode\"',model='historical-model'",[]).unwrap();
    let exported = fixture.open().export("root").unwrap();
    let metadata = &exported.lineage[0].metadata;
    assert_eq!(metadata.effort.raw, "unknown legacy encoding");
    assert!(metadata.effort.value.is_none());
    assert_eq!(
        metadata.reasoning_mode.value.as_deref(),
        Some("future-mode")
    );
    assert_eq!(metadata.model, "historical-model");
}

#[test]
fn required_tables_cannot_be_replaced_with_executable_views() {
    let fixture = Fixture::new(2);
    fixture.connection.execute_batch("DROP TABLE resume_states; CREATE VIEW resume_states AS SELECT 'root' AS session_id,load_extension('untrusted') AS state_zstd;").unwrap();
    assert!(matches!(
        LegacyArchive::open(&fixture.path, ImportLimits::default()),
        Err(ImportError::Database)
    ));
}
