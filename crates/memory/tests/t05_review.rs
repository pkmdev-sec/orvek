#![cfg(all(feature = "local", feature = "tool"))]
use orvek_memory::{
    LessonQuery, LocalMemoryStore, MemoryArchive, MemoryError, MemoryKey, MemoryKind,
    MemoryMetadata, MemoryOrigin, MemoryPermission, MemoryRecord, MemoryScan, MemoryScope,
    MemorySession, MemoryStore, ProposalState, SelectedMemoryStore, TraceReference,
    WorkspaceSources, finalize_lessons, propose_lesson,
    server::protocol::{ExportCursor, SyncReport},
};
use serde_json::json;
use std::{fs, path::Path, process::Command};
fn git(p: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    );
}
fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    git(d.path(), &["config", "user.name", "Audit"]);
    git(d.path(), &["config", "user.email", "audit@example.invalid"]);
    fs::write(d.path().join("feature.rs"), "version one\n").unwrap();
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-qm", "fixture"]);
    d
}
fn trace(s: &str) -> TraceReference {
    TraceReference {
        session: "session".into(),
        request: s.into(),
        task: s.into(),
    }
}
fn lesson(s: &WorkspaceSources, t: &str) -> MemoryMetadata {
    let ev = s.capture("feature.rs", None).unwrap();
    MemoryMetadata {
        scope: MemoryScope::Repository {
            identity: s.repository().into(),
        },
        kind: MemoryKind::LessonProposal {
            behavior_test: ev.clone(),
            state: ProposalState::Pending,
        },
        origin: MemoryOrigin::Model,
        evidence: vec![ev],
        producing_traces: vec![trace(t)],
        ..Default::default()
    }
}
#[tokio::test]
async fn archive_self_import_roundtrips() {
    let d = tempfile::tempdir().unwrap();
    let a = LocalMemoryStore::new(d.path().join("a/db"));
    let b = LocalMemoryStore::new(d.path().join("b/db"));
    a.put("same portable memory", None).await.unwrap();
    MemoryArchive::export(&a, &d.path().join("original"))
        .await
        .unwrap();
    let report = MemoryArchive::import(&d.path().join("original"), &a)
        .await
        .unwrap();
    println!(
        "self-import result {:?}; records={}",
        report,
        a.list().await.unwrap().len()
    );
    assert_eq!(
        a.list().await.unwrap().len(),
        1,
        "self import must not duplicate the original"
    );
    MemoryArchive::export(&a, &d.path().join("roundtrip"))
        .await
        .unwrap();
    let result = MemoryArchive::import(&d.path().join("roundtrip"), &b).await;
    println!(
        "re-export import result {result:?}; destination records={}",
        b.list().await.unwrap().len()
    );
    assert_eq!(result.unwrap().inserted, 1);
    assert_eq!(b.list().await.unwrap().len(), 1);
}
#[tokio::test]
async fn distinct_local_archives_keep_independent_scopes() {
    let d = tempfile::tempdir().unwrap();
    let a = LocalMemoryStore::new(d.path().join("a/db"));
    let b = LocalMemoryStore::new(d.path().join("b/db"));
    let c = LocalMemoryStore::new(d.path().join("c/db"));
    for (store, scope) in [(&a, "repo-a"), (&b, "repo-b")] {
        store
            .put_with_metadata(
                "run the tests",
                &MemoryMetadata {
                    scope: MemoryScope::Repository {
                        identity: scope.into(),
                    },
                    kind: MemoryKind::Procedure,
                    origin: MemoryOrigin::User,
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
    }
    MemoryArchive::export(&a, &d.path().join("a-archive"))
        .await
        .unwrap();
    MemoryArchive::export(&b, &d.path().join("b-archive"))
        .await
        .unwrap();
    MemoryArchive::import(&d.path().join("a-archive"), &c)
        .await
        .unwrap();
    let result = MemoryArchive::import(&d.path().join("b-archive"), &c).await;
    println!("distinct local store, different repository scope import result: {result:?}");
    assert_eq!(result.unwrap().inserted, 1);
    assert_eq!(c.list().await.unwrap().len(), 2);
}
#[tokio::test]
async fn delayed_callback_cannot_finalize_an_unsettled_new_run() {
    let d = tempfile::tempdir().unwrap();
    let r = repo();
    let s = WorkspaceSources::open(r.path()).unwrap();
    let store = LocalMemoryStore::new(d.path().join("memory/v1.sqlite3"));
    propose_lesson(&store, "test before delivery", lesson(&s, "A"))
        .await
        .unwrap();
    // A settles but its asynchronous callback has not run. B is still executing.
    propose_lesson(&store, "test before delivery", lesson(&s, "B"))
        .await
        .unwrap();
    assert_eq!(finalize_lessons(&store, &trace("A")).await.unwrap(), 0);
    let record = store.list().await.unwrap().remove(0);
    println!(
        "A callback after B nomination: {:?}; traces={:?}",
        record.metadata.kind, record.metadata.producing_traces
    );
    assert!(matches!(
        record.metadata.kind,
        MemoryKind::LessonProposal {
            state: ProposalState::Pending,
            ..
        }
    ));
    assert!(record.metadata.producing_traces.contains(&trace("B")));
}
#[tokio::test]
async fn repeated_changed_evidence_refreshes_active_citations() {
    let d = tempfile::tempdir().unwrap();
    let r = repo();
    let s = WorkspaceSources::open(r.path()).unwrap();
    let store = LocalMemoryStore::new(d.path().join("memory/v1.sqlite3"));
    propose_lesson(&store, "test before delivery", lesson(&s, "A"))
        .await
        .unwrap();
    finalize_lessons(&store, &trace("A")).await.unwrap();
    fs::write(r.path().join("feature.rs"), "version two\n").unwrap();
    let rec = propose_lesson(&store, "test before delivery", lesson(&s, "B"))
        .await
        .unwrap();
    println!(
        "freshly re-proposed lesson state {:?}; evidence={:?}",
        s.assess(&rec.metadata),
        rec.metadata.evidence
    );
    assert_eq!(
        s.assess(&rec.metadata),
        orvek_memory::EvidenceState::Current
    );
    assert_eq!(rec.metadata.evidence.len(), 1);
    assert_eq!(rec.metadata.historical_evidence.len(), 1);
    assert_ne!(
        rec.metadata.evidence[0],
        rec.metadata.historical_evidence[0]
    );
}
#[tokio::test]
async fn read_hidden_scope_does_not_mutate_telemetry() {
    let d = tempfile::tempdir().unwrap();
    let local = LocalMemoryStore::new(d.path().join("memory/v1.sqlite3"));
    let selected = SelectedMemoryStore::Local(local.clone());
    let rec = local
        .put_with_metadata(
            "private to other repo",
            &MemoryMetadata {
                scope: MemoryScope::Repository {
                    identity: "other-repo".into(),
                },
                kind: MemoryKind::Preference,
                origin: MemoryOrigin::User,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    let session = MemorySession::new(selected);
    let read = session
        .execute(
            json!({"operation":"read","keys":[rec.key.clone()]}),
            MemoryPermission::ReadOnly,
        )
        .await
        .unwrap();
    assert!(read["memories"].as_array().unwrap().is_empty());
    let after = local.list().await.unwrap().remove(0);
    println!(
        "hidden read: use count {} -> {}; probation {:?} -> {:?}",
        rec.use_count, after.use_count, rec.probation_until_ms, after.probation_until_ms
    );
    assert_eq!(after.use_count, 0);
    assert_eq!(after.probation_until_ms, rec.probation_until_ms);
}
#[test]
fn source_boundary_pins_the_admitted_root() {
    use std::os::unix::fs::symlink;
    let r = repo();
    let external = repo();
    let s = WorkspaceSources::open(r.path()).unwrap();
    git(external.path(), &["checkout", "--orphan", "different-root"]);
    fs::write(external.path().join("unrelated"), "other repository").unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "different root"]);
    let other = WorkspaceSources::open(external.path()).unwrap();
    assert_ne!(s.repository(), other.repository());
    let before = s.capture("feature.rs", None).unwrap();
    symlink(external.path(), r.path().join("link")).unwrap();
    assert!(s.capture("link/feature.rs", None).is_err());
    assert!(s.capture("../feature.rs", None).is_err());
    let container = tempfile::tempdir().unwrap();
    let moved = container.path().join("moved-repo");
    fs::rename(r.path(), &moved).unwrap();
    symlink(external.path(), r.path()).unwrap();
    let meta = MemoryMetadata {
        evidence: vec![before.clone()],
        ..Default::default()
    };
    println!(
        "unrelated repo via replaced root: cached={}, actual={}, freshness={:?}",
        s.repository(),
        WorkspaceSources::open(r.path()).unwrap().repository(),
        s.assess(&meta)
    );
    assert_eq!(s.assess(&meta), orvek_memory::EvidenceState::Current);
    fs::write(
        external.path().join("feature.rs"),
        "outside admitted root bytes",
    )
    .unwrap();
    let ev = s.capture("feature.rs", None).unwrap();
    assert_eq!(
        ev, before,
        "capture must stay on admitted A, including Git revision"
    );
    fs::remove_file(r.path()).unwrap();
    fs::rename(external.path(), r.path()).unwrap();
    assert_eq!(
        s.capture("feature.rs", None).unwrap(),
        before,
        "a replacement directory must not replace the pinned source view"
    );
}

#[tokio::test]
async fn archive_multihop_reexport_preserves_conflicts_and_all_owners() {
    let d = tempfile::tempdir().unwrap();
    let a = LocalMemoryStore::new(d.path().join("a/db"));
    let b = LocalMemoryStore::new(d.path().join("b/db"));
    let c = LocalMemoryStore::new(d.path().join("c/db"));
    let original = a.put("portable original", None).await.unwrap();
    MemoryArchive::export(&a, &d.path().join("a1"))
        .await
        .unwrap();
    MemoryArchive::import(&d.path().join("a1"), &b)
        .await
        .unwrap();
    let imported = b.list().await.unwrap().remove(0);
    assert_ne!(
        original.metadata.ownership_id,
        imported.metadata.ownership_id
    );
    MemoryArchive::export(&b, &d.path().join("b1"))
        .await
        .unwrap();
    MemoryArchive::import(&d.path().join("b1"), &c)
        .await
        .unwrap();
    MemoryArchive::export(&c, &d.path().join("c1"))
        .await
        .unwrap();
    assert_eq!(
        MemoryArchive::import(&d.path().join("c1"), &a)
            .await
            .unwrap()
            .inserted,
        0
    );
    let merged = a.list().await.unwrap().remove(0);
    assert_eq!(merged.metadata.transferred_from.len(), 3);
    assert!(
        merged
            .metadata
            .transferred_from
            .iter()
            .any(
                |source| source.ownership_id == *original.metadata.ownership_id.as_ref().unwrap()
                    && source.key == original.key
            )
    );
    assert!(
        merged
            .metadata
            .transferred_from
            .iter()
            .any(
                |source| source.ownership_id == *imported.metadata.ownership_id.as_ref().unwrap()
                    && source.key == imported.key
            )
    );
    assert_eq!(
        MemoryArchive::import(&d.path().join("c1"), &a)
            .await
            .unwrap()
            .skipped,
        1
    );
    assert_eq!(
        a.list().await.unwrap()[0],
        merged,
        "repeat import has no mutation"
    );
    // A fork with changed metadata but identical text is a distinct snapshot, not discarded.
    let mut changed = imported.metadata.clone();
    changed.origin = MemoryOrigin::User;
    let updated = b
        .put_with_metadata(&imported.content, &changed, Some(imported.key))
        .await
        .unwrap();
    MemoryArchive::export(&b, &d.path().join("b2"))
        .await
        .unwrap();
    assert_eq!(
        MemoryArchive::import(&d.path().join("b2"), &a)
            .await
            .unwrap()
            .inserted,
        1
    );
    let records = a.list().await.unwrap();
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .any(|record| record.metadata.origin == MemoryOrigin::User
                && record.metadata.imported_from.contains(&updated.key))
    );
    MemoryArchive::export(&a, &d.path().join("conflicts"))
        .await
        .unwrap();
    let target = LocalMemoryStore::new(d.path().join("target/db"));
    assert_eq!(
        MemoryArchive::import(&d.path().join("conflicts"), &target)
            .await
            .unwrap()
            .inserted,
        2
    );
}

#[tokio::test]
async fn independent_stores_with_equal_scope_text_and_numeric_keys_remain_distinct() {
    let d = tempfile::tempdir().unwrap();
    let a = LocalMemoryStore::new(d.path().join("a/db"));
    let b = LocalMemoryStore::new(d.path().join("b/db"));
    let target = LocalMemoryStore::new(d.path().join("target/db"));
    let first = a.put("same preference", None).await.unwrap();
    let second = b.put("same preference", None).await.unwrap();
    assert_eq!(first.key, second.key);
    assert_ne!(first.metadata.ownership_id, second.metadata.ownership_id);
    target.import_records(vec![first, second]).await.unwrap();
    assert_eq!(target.list().await.unwrap().len(), 2);
}

#[derive(Clone)]
struct PausedLessonPage {
    store: LocalMemoryStore,
    observed: std::sync::Arc<tokio::sync::Notify>,
    proceed: std::sync::Arc<tokio::sync::Notify>,
}
impl MemoryStore for PausedLessonPage {
    async fn lesson_page(
        &self,
        query: &LessonQuery,
        after: i64,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let page = self.store.lesson_page(query, after).await?;
        if after == 0 {
            self.observed.notify_one();
            self.proceed.notified().await;
        }
        Ok(page)
    }
    async fn scan(&self, query: &str, limit: usize) -> Result<MemoryScan, MemoryError> {
        self.store.scan(query, limit).await
    }
    async fn read(
        &self,
        ids: &[i64],
        keys: &[MemoryKey],
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        self.store.read(ids, keys).await
    }
    async fn list(&self) -> Result<Vec<MemoryRecord>, MemoryError> {
        self.store.list().await
    }
    async fn put(
        &self,
        content: &str,
        key: Option<MemoryKey>,
    ) -> Result<MemoryRecord, MemoryError> {
        self.store.put(content, key).await
    }
    async fn put_with_metadata(
        &self,
        content: &str,
        metadata: &MemoryMetadata,
        key: Option<MemoryKey>,
    ) -> Result<MemoryRecord, MemoryError> {
        self.store.put_with_metadata(content, metadata, key).await
    }
    async fn delete(&self, key: MemoryKey) -> Result<(), MemoryError> {
        self.store.delete(key).await
    }
    async fn sync(&self, records: &[MemoryRecord]) -> Result<SyncReport, MemoryError> {
        self.store.sync(records).await
    }
    async fn export_page(
        &self,
        ns: Option<&[String]>,
        cursor: Option<&ExportCursor>,
        limit: usize,
    ) -> Result<(Vec<MemoryRecord>, Option<ExportCursor>), MemoryError> {
        self.store.export_page(ns, cursor, limit).await
    }
}

#[tokio::test]
async fn finalization_cas_cannot_overwrite_a_nomination_after_query() {
    let d = tempfile::tempdir().unwrap();
    let r = repo();
    let sources = WorkspaceSources::open(r.path()).unwrap();
    let store = LocalMemoryStore::new(d.path().join("memory/v1.sqlite3"));
    let first = propose_lesson(&store, "test before delivery", lesson(&sources, "A"))
        .await
        .unwrap();
    let paused = PausedLessonPage {
        store: store.clone(),
        observed: Default::default(),
        proceed: Default::default(),
    };
    let worker = paused.clone();
    let task = tokio::spawn(async move { finalize_lessons(&worker, &trace("A")).await });
    paused.observed.notified().await;
    let second = propose_lesson(&store, "test before delivery", lesson(&sources, "B"))
        .await
        .unwrap();
    assert_eq!(second.key.version, first.key.version + 1);
    paused.proceed.notify_one();
    assert_eq!(task.await.unwrap().unwrap(), 0);
    assert_eq!(store.list().await.unwrap()[0], second);
    assert_eq!(finalize_lessons(&store, &trace("B")).await.unwrap(), 1);
    assert_eq!(finalize_lessons(&store, &trace("B")).await.unwrap(), 0);
}

#[test]
fn capture_rejects_a_replaced_git_history_in_the_pinned_directory() {
    let original = repo();
    let sources = WorkspaceSources::open(original.path()).unwrap();
    git(original.path(), &["checkout", "--orphan", "unrelated"]);
    fs::write(original.path().join("different-root"), "different history").unwrap();
    git(original.path(), &["add", "."]);
    git(original.path(), &["commit", "-qm", "unrelated root"]);
    assert_ne!(
        sources.repository(),
        WorkspaceSources::open(original.path())
            .unwrap()
            .repository()
    );
    assert!(sources.capture("feature.rs", None).is_err());
}
