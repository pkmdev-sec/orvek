use super::{
    Credential, MAX_JSON_BODY_BYTES, MemoryServer, ServerBuildError,
    protocol::{
        self, DeleteRequest, ErrorResponse, ExportCursor, ExportRequest, ExportResponse,
        ListResponse, PutRequest, PutResponse, ReadRequest, ReadResponse, RemoteErrorCode,
        RemoteRole, ScanRequest, ScanResponse, SessionResponse, SyncReport, SyncRequest,
    },
};
use crate::{
    MemoryCandidate, MemoryError, MemoryKey, MemoryLimits, MemoryMetadata, MemoryRecord,
    MemoryScan, MemoryStore, RemoteClientError, RemoteMemoryClient, RemoteToken,
    model::normalize_identity,
};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, Response, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Notify, Semaphore};
use tower::ServiceExt;

const ALICE_TOKEN: &str = "alice-test-token-000000000001";
const BOB_TOKEN: &str = "bob-test-token-00000000000002";
const READER_TOKEN: &str = "reader-test-token-00000000004";

fn credential(namespace: &str, role: RemoteRole, token: &str) -> Credential {
    Credential::new(namespace.to_owned(), role, token.to_owned()).unwrap()
}

#[derive(Clone, Default)]
struct TestMemoryDatabase {
    state: Arc<Mutex<TestMemoryState>>,
}

#[derive(Default)]
struct TestMemoryState {
    records: BTreeMap<(String, i64), MemoryRecord>,
    next_ids: BTreeMap<String, i64>,
}

#[derive(Clone)]
struct TestMemoryStore {
    database: TestMemoryDatabase,
    namespace: String,
}

impl TestMemoryDatabase {
    fn bind(&self, namespace: String) -> TestMemoryStore {
        TestMemoryStore {
            database: self.clone(),
            namespace,
        }
    }
}

fn memory_app(credentials: Vec<Credential>) -> Router {
    let database = TestMemoryDatabase::default();
    MemoryServer::new(move |namespace| database.bind(namespace), credentials)
        .unwrap()
        .router()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn prune_expired(state: &mut TestMemoryState, now: i64) {
    state.records.retain(|_, record| {
        !record
            .probation_until_ms
            .is_some_and(|expiry| expiry <= now && record.use_count == 0)
    });
}

fn visible_records(state: &TestMemoryState) -> Vec<MemoryRecord> {
    state.records.values().cloned().collect()
}

impl MemoryStore for TestMemoryStore {
    async fn lesson_page(
        &self,
        query: &crate::LessonQuery,
        after: i64,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let mut state = self.database.state.lock().unwrap();
        prune_expired(&mut state, now_ms());
        Ok(state
            .records
            .values()
            .filter(|record| {
                record.key.namespace.as_deref() == Some(&self.namespace)
                    && record.key.id > after
                    && query.matches(record)
            })
            .take(protocol::MAX_EXPORT_PAGE_RECORDS)
            .cloned()
            .collect())
    }
    async fn read_scoped(
        &self,
        ids: &[i64],
        keys: &[MemoryKey],
        repository: Option<&str>,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let keys = {
            let state = self.database.state.lock().unwrap();
            state
                .records
                .values()
                .filter(|record| {
                    record.metadata.visible_in(repository)
                        && (keys.contains(&record.key)
                            || record.key.namespace.as_deref() == Some(&self.namespace)
                                && ids.contains(&record.key.id))
                })
                .map(|record| record.key.clone())
                .collect::<Vec<_>>()
        };
        self.read(&[], &keys).await
    }

    async fn scan(&self, query: &str, limit: usize) -> Result<MemoryScan, MemoryError> {
        let now = now_ms();
        let query = normalize_identity(query);
        let terms = query.split_whitespace().collect::<Vec<_>>();
        let mut state = self.database.state.lock().unwrap();
        prune_expired(&mut state, now);
        let mut seen = HashSet::new();
        let mut candidates = visible_records(&state)
            .into_iter()
            .filter(|record| {
                let content = normalize_identity(&record.content);
                terms.iter().all(|term| content.contains(term))
                    && seen.insert(normalize_identity(&record.content))
            })
            .map(|record| MemoryCandidate {
                metadata: Default::default(),
                key: record.key,
                preview: record.content,
                score: 1.0,
            })
            .take(limit.min(MemoryLimits::PRODUCTION.scan_results))
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.key
                .namespace
                .cmp(&right.key.namespace)
                .then_with(|| left.key.id.cmp(&right.key.id))
        });
        for candidate in &candidates {
            if let Some(record) = state
                .records
                .get_mut(&(candidate.key.namespace.clone().unwrap(), candidate.key.id))
            {
                record.last_scanned_at_ms = Some(now);
                record.scan_count = record.scan_count.saturating_add(1);
            }
        }
        Ok(MemoryScan {
            abstained: candidates.is_empty(),
            candidates,
        })
    }

    async fn scan_scoped(
        &self,
        query: &str,
        limit: usize,
        repository: Option<&str>,
    ) -> Result<MemoryScan, MemoryError> {
        let records = self
            .list()
            .await?
            .into_iter()
            .filter(|record| record.metadata.visible_in(repository))
            .collect::<Vec<_>>();
        Ok(MemoryScan::rank(query, &records, limit))
    }
    async fn read(
        &self,
        ids: &[i64],
        keys: &[MemoryKey],
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let now = now_ms();
        let mut state = self.database.state.lock().unwrap();
        prune_expired(&mut state, now);
        let references = ids
            .iter()
            .map(|id| (self.namespace.clone(), *id, None))
            .chain(keys.iter().filter_map(|key| {
                key.namespace
                    .clone()
                    .map(|namespace| (namespace, key.id, Some(key.version)))
            }));
        let mut seen = HashSet::new();
        let mut records = Vec::new();
        for (namespace, id, version) in references {
            let Some(record) = state.records.get_mut(&(namespace, id)) else {
                continue;
            };
            if version.is_some_and(|version| version != record.key.version)
                || !seen.insert(normalize_identity(&record.content))
            {
                continue;
            }
            record.last_used_at_ms = Some(now);
            record.use_count = record.use_count.saturating_add(1);
            record.probation_until_ms = None;
            records.push(record.clone());
        }
        Ok(records)
    }

    async fn list(&self) -> Result<Vec<MemoryRecord>, MemoryError> {
        let mut state = self.database.state.lock().unwrap();
        prune_expired(&mut state, now_ms());
        Ok(visible_records(&state)
            .into_iter()
            .take(MemoryLimits::PRODUCTION.records)
            .collect())
    }

    async fn put(
        &self,
        content: &str,
        replacement: Option<MemoryKey>,
    ) -> Result<MemoryRecord, MemoryError> {
        self.put_with_metadata(content, &crate::MemoryMetadata::default(), replacement)
            .await
    }
    async fn put_with_metadata(
        &self,
        content: &str,
        metadata: &crate::MemoryMetadata,
        replacement: Option<MemoryKey>,
    ) -> Result<MemoryRecord, MemoryError> {
        metadata.validate()?;
        let identity = metadata.identity(content);
        if identity.is_empty() {
            return Err(MemoryError::EmptyContent);
        }
        let now = now_ms();
        let mut state = self.database.state.lock().unwrap();
        prune_expired(&mut state, now);
        if state.records.values().any(|record| {
            record.key.namespace.as_deref() == Some(&self.namespace)
                && record.metadata.identity(&record.content) == identity
                && replacement
                    .as_ref()
                    .is_none_or(|key| key.id != record.key.id)
        }) {
            return Err(MemoryError::Duplicate);
        }

        let namespace_records = state
            .records
            .values()
            .filter(|record| record.key.namespace.as_deref() == Some(&self.namespace))
            .collect::<Vec<_>>();
        let replacing_bytes = replacement
            .as_ref()
            .and_then(|key| state.records.get(&(self.namespace.clone(), key.id)))
            .map_or(0, |record| record.content.len());
        let total_bytes = namespace_records
            .iter()
            .map(|record| record.content.len())
            .sum::<usize>()
            .saturating_sub(replacing_bytes)
            .saturating_add(content.len());
        if total_bytes > MemoryLimits::PRODUCTION.total_content_bytes {
            return Err(MemoryError::ContentCapacity {
                maximum_bytes: MemoryLimits::PRODUCTION.total_content_bytes,
            });
        }

        let (id, version, created_at_ms) = if let Some(key) = replacement {
            if key.namespace.as_deref() != Some(&self.namespace) {
                return Err(MemoryError::RemoteReadOnly);
            }
            let existing = state
                .records
                .get(&(self.namespace.clone(), key.id))
                .ok_or(MemoryError::NotFound)?;
            if existing.key.version != key.version {
                return Err(MemoryError::Conflict);
            }
            (key.id, key.version + 1, existing.created_at_ms)
        } else {
            if namespace_records.len() >= MemoryLimits::PRODUCTION.records {
                return Err(MemoryError::RecordCapacity {
                    maximum: MemoryLimits::PRODUCTION.records,
                });
            }
            let next = state.next_ids.entry(self.namespace.clone()).or_insert(1);
            let id = *next;
            *next = next.saturating_add(1);
            (id, 1, now)
        };
        let mut metadata = metadata.clone();
        metadata.ownership_id = state
            .records
            .get(&(self.namespace.clone(), id))
            .and_then(|record| record.metadata.ownership_id.clone())
            .or_else(|| {
                static NEXT: AtomicUsize = AtomicUsize::new(1);
                Some(format!("{:032x}", NEXT.fetch_add(1, Ordering::Relaxed)))
            });
        let memory = MemoryRecord {
            metadata,
            key: MemoryKey::remote(self.namespace.clone(), id, version),
            content: content.to_owned(),
            created_at_ms,
            updated_at_ms: now,
            last_scanned_at_ms: None,
            scan_count: 0,
            last_used_at_ms: None,
            use_count: 0,
            probation_until_ms: Some(
                now.saturating_add(MemoryLimits::PRODUCTION.probation_duration_ms),
            ),
        };
        state
            .records
            .insert((self.namespace.clone(), id), memory.clone());
        Ok(memory)
    }

    async fn delete(&self, key: MemoryKey) -> Result<(), MemoryError> {
        if key.namespace.as_deref() != Some(&self.namespace) {
            return Err(MemoryError::RemoteReadOnly);
        }
        let mut state = self.database.state.lock().unwrap();
        let index = (self.namespace.clone(), key.id);
        if let Some(record) = state.records.get(&index)
            && record.key.version != key.version
        {
            return Err(MemoryError::Conflict);
        }
        state.records.remove(&index);
        Ok(())
    }

    async fn sync(&self, memories: &[MemoryRecord]) -> Result<SyncReport, MemoryError> {
        let mut identities = HashSet::new();
        if memories.len() > MemoryLimits::PRODUCTION.records {
            return Err(MemoryError::RecordCapacity {
                maximum: MemoryLimits::PRODUCTION.records,
            });
        }
        let total_bytes = memories.iter().try_fold(0usize, |total, memory| {
            if memory.key.id <= 0
                || memory.key.version == 0
                || !identities.insert(normalize_identity(&memory.content))
            {
                return Err(MemoryError::Conflict);
            }
            total
                .checked_add(memory.content.len())
                .ok_or(MemoryError::ContentCapacity {
                    maximum_bytes: MemoryLimits::PRODUCTION.total_content_bytes,
                })
        })?;
        if total_bytes > MemoryLimits::PRODUCTION.total_content_bytes {
            return Err(MemoryError::ContentCapacity {
                maximum_bytes: MemoryLimits::PRODUCTION.total_content_bytes,
            });
        }

        let mut state = self.database.state.lock().unwrap();
        let existing = state
            .records
            .iter()
            .filter(|((namespace, _), _)| namespace == &self.namespace)
            .map(|((_, id), record)| (*id, record.clone()))
            .collect::<BTreeMap<_, _>>();
        let incoming_ids = memories
            .iter()
            .map(|memory| memory.key.id)
            .collect::<HashSet<_>>();
        let mut report = SyncReport {
            deleted: existing
                .keys()
                .filter(|id| !incoming_ids.contains(id))
                .count(),
            ..SyncReport::default()
        };
        state
            .records
            .retain(|(namespace, _), _| namespace != &self.namespace);
        for memory in memories {
            let mut memory = memory.clone();
            memory.key.namespace = Some(self.namespace.clone());
            match existing.get(&memory.key.id) {
                None => report.inserted += 1,
                Some(previous) if previous == &memory => report.unchanged += 1,
                Some(_) => report.replaced += 1,
            }
            state
                .records
                .insert((self.namespace.clone(), memory.key.id), memory);
        }
        let next = memories
            .iter()
            .map(|memory| memory.key.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        state
            .next_ids
            .entry(self.namespace.clone())
            .and_modify(|current| *current = (*current).max(next))
            .or_insert(next.max(1));
        Ok(report)
    }

    async fn export_page(
        &self,
        namespaces: Option<&[String]>,
        cursor: Option<&ExportCursor>,
        limit: usize,
    ) -> Result<(Vec<MemoryRecord>, Option<ExportCursor>), MemoryError> {
        let mut state = self.database.state.lock().unwrap();
        prune_expired(&mut state, now_ms());
        let selected = namespaces.map(|values| values.iter().collect::<HashSet<_>>());
        let mut records = state
            .records
            .iter()
            .filter(|((namespace, id), _)| {
                selected
                    .as_ref()
                    .is_none_or(|selected| selected.contains(namespace))
                    && cursor.is_none_or(|cursor| {
                        (namespace.as_str(), *id) > (cursor.namespace.as_str(), cursor.id)
                    })
            })
            .map(|(_, record)| record.clone())
            .collect::<Vec<_>>();
        let limit = limit.min(protocol::MAX_EXPORT_PAGE_RECORDS);
        let has_more = records.len() > limit;
        records.truncate(limit);
        let next_cursor = has_more.then(|| {
            let key = &records.last().unwrap().key;
            ExportCursor {
                namespace: key.namespace.clone().unwrap(),
                id: key.id,
            }
        });
        Ok((records, next_cursor))
    }
}

fn record(id: i64, version: u64, content: &str) -> MemoryRecord {
    MemoryRecord {
        metadata: Default::default(),
        key: MemoryKey::local(id, version),
        content: content.to_owned(),
        created_at_ms: 10,
        updated_at_ms: 10 + i64::try_from(version).unwrap(),
        last_scanned_at_ms: None,
        scan_count: 0,
        last_used_at_ms: None,
        use_count: 0,
        probation_until_ms: None,
    }
}

fn request<T: Serialize>(
    method: &str,
    path: &str,
    token: Option<&str>,
    namespace: Option<&str>,
    body: Option<&T>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(format!("/{path}"));
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if let Some(namespace) = namespace {
        builder = builder.header(protocol::NAMESPACE_HEADER, namespace);
    }
    let body = body
        .map(|body| Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap_or_else(Body::empty);
    builder
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .unwrap()
}

async fn send<T: Serialize>(
    app: &Router,
    path: &str,
    token: &str,
    namespace: &str,
    body: &T,
) -> Response<Body> {
    app.clone()
        .oneshot(request(
            "POST",
            path,
            Some(token),
            Some(namespace),
            Some(body),
        ))
        .await
        .unwrap()
}

async fn json<T: DeserializeOwned>(response: Response<Body>) -> T {
    serde_json::from_slice(&response_bytes(response).await).unwrap()
}

async fn response_bytes(response: Response<Body>) -> Vec<u8> {
    to_bytes(response.into_body(), MAX_JSON_BODY_BYTES)
        .await
        .unwrap()
        .to_vec()
}

async fn put(app: &Router, namespace: &str, token: &str, content: &str) -> MemoryRecord {
    let response = send(
        app,
        protocol::PUT_PATH,
        token,
        namespace,
        &PutRequest {
            metadata: Default::default(),
            content: content.to_owned(),
            replacement: None,
        },
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    json::<PutResponse>(response).await.memory
}

#[tokio::test]
async fn authentication_namespace_and_role_are_enforced() {
    let app = memory_app(vec![
        credential("alice", RemoteRole::Writer, ALICE_TOKEN),
        credential("reader", RemoteRole::Reader, READER_TOKEN),
    ]);

    let missing = app
        .clone()
        .oneshot(request::<()>(
            "GET",
            protocol::SESSION_PATH,
            None,
            Some("alice"),
            None,
        ))
        .await
        .unwrap();
    assert_error(
        missing,
        StatusCode::UNAUTHORIZED,
        RemoteErrorCode::Unauthorized,
    )
    .await;

    let mismatch = app
        .clone()
        .oneshot(request::<()>(
            "GET",
            protocol::SESSION_PATH,
            Some(ALICE_TOKEN),
            Some("reader"),
            None,
        ))
        .await
        .unwrap();
    assert_error(
        mismatch,
        StatusCode::FORBIDDEN,
        RemoteErrorCode::NamespaceMismatch,
    )
    .await;

    let session = app
        .clone()
        .oneshot(request::<()>(
            "GET",
            protocol::SESSION_PATH,
            Some(ALICE_TOKEN),
            Some("alice"),
            None,
        ))
        .await
        .unwrap();
    let session = json::<SessionResponse>(session).await;
    assert_eq!(session.namespace, "alice");
    assert_eq!(session.role, RemoteRole::Writer);

    let denied = send(
        &app,
        protocol::PUT_PATH,
        READER_TOKEN,
        "reader",
        &PutRequest {
            metadata: Default::default(),
            content: "reader cannot write".to_owned(),
            replacement: None,
        },
    )
    .await;
    assert_error(denied, StatusCode::FORBIDDEN, RemoteErrorCode::Forbidden).await;
    let denied = send(
        &app,
        protocol::DELETE_PATH,
        READER_TOKEN,
        "reader",
        &DeleteRequest {
            key: MemoryKey::remote("reader".to_owned(), 1, 1),
        },
    )
    .await;
    assert_error(denied, StatusCode::FORBIDDEN, RemoteErrorCode::Forbidden).await;
    let denied = send(
        &app,
        protocol::SYNC_PATH,
        READER_TOKEN,
        "reader",
        &SyncRequest {
            memories: Vec::new(),
        },
    )
    .await;
    assert_error(denied, StatusCode::FORBIDDEN, RemoteErrorCode::Forbidden).await;
}

#[test]
fn duplicate_bearer_tokens_are_rejected_without_exposing_tokens() {
    let result = MemoryServer::new(
        |namespace| TestMemoryDatabase::default().bind(namespace),
        [
            credential("alice", RemoteRole::Writer, ALICE_TOKEN),
            credential("bob", RemoteRole::Reader, ALICE_TOKEN),
        ],
    );
    assert!(matches!(
        result,
        Err(ServerBuildError::DuplicateBearerToken)
    ));
    let diagnostic = format!("{:?}", result.err().unwrap());
    assert!(!diagnostic.contains(ALICE_TOKEN));
}

#[tokio::test]
async fn scan_read_and_list_return_only_caller_visible_records() {
    let app = memory_app(vec![
        credential("alice", RemoteRole::Writer, ALICE_TOKEN),
        credential("bob", RemoteRole::Writer, BOB_TOKEN),
    ]);
    let alice = put(&app, "alice", ALICE_TOKEN, "alice private indexing note").await;
    let bob = put(&app, "bob", BOB_TOKEN, "bob visible concurrent sqlite note").await;

    let listed = send(&app, protocol::LIST_PATH, ALICE_TOKEN, "alice", &()).await;
    let listed = json::<ListResponse>(listed).await.memories;
    assert_eq!(
        listed.iter().map(|memory| &memory.key).collect::<Vec<_>>(),
        [&alice.key, &bob.key]
    );

    let scanned = send(
        &app,
        protocol::SCAN_PATH,
        ALICE_TOKEN,
        "alice",
        &ScanRequest {
            scope: None,
            query: "concurrent sqlite".to_owned(),
            limit: 5,
        },
    )
    .await;
    let scanned = json::<ScanResponse>(scanned).await.candidates;
    assert_eq!(scanned.len(), 1);
    assert_eq!(scanned[0].key, bob.key);

    let read = send(
        &app,
        protocol::READ_PATH,
        ALICE_TOKEN,
        "alice",
        &ReadRequest {
            scope: None,
            ids: vec![alice.key.id],
            keys: vec![bob.key.clone()],
        },
    )
    .await;
    let read = json::<ReadResponse>(read).await.memories;
    assert_eq!(read.len(), 2);
    assert!(read.iter().any(|memory| memory.key == alice.key));
    assert!(read.iter().any(|memory| memory.key == bob.key));
}

#[tokio::test]
async fn put_replace_and_delete_are_server_authored() {
    let app = memory_app(vec![credential("alice", RemoteRole::Writer, ALICE_TOKEN)]);
    let inserted = put(&app, "alice", ALICE_TOKEN, "initial server-authored note").await;
    assert_eq!(inserted.key.namespace.as_deref(), Some("alice"));
    assert_eq!(inserted.key.version, 1);

    let replaced = send(
        &app,
        protocol::PUT_PATH,
        ALICE_TOKEN,
        "alice",
        &PutRequest {
            metadata: Default::default(),
            content: "replacement server-authored note".to_owned(),
            replacement: Some(inserted.key.clone()),
        },
    )
    .await;
    let replaced = json::<PutResponse>(replaced).await.memory;
    assert_eq!(replaced.key.id, inserted.key.id);
    assert_eq!(replaced.key.version, inserted.key.version + 1);

    let deleted = send(
        &app,
        protocol::DELETE_PATH,
        ALICE_TOKEN,
        "alice",
        &DeleteRequest {
            key: replaced.key.clone(),
        },
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::OK);
    let read = send(
        &app,
        protocol::READ_PATH,
        ALICE_TOKEN,
        "alice",
        &ReadRequest {
            scope: None,
            ids: vec![replaced.key.id],
            keys: Vec::new(),
        },
    )
    .await;
    assert!(json::<ReadResponse>(read).await.memories.is_empty());
}

#[tokio::test]
async fn foreign_keys_cannot_be_mutated() {
    let app = memory_app(vec![
        credential("alice", RemoteRole::Writer, ALICE_TOKEN),
        credential("bob", RemoteRole::Writer, BOB_TOKEN),
    ]);
    let alice = put(&app, "alice", ALICE_TOKEN, "alice owns this note").await;
    let local_shaped_delete = send(
        &app,
        protocol::DELETE_PATH,
        ALICE_TOKEN,
        "alice",
        &DeleteRequest {
            key: MemoryKey::local(alice.key.id, alice.key.version),
        },
    )
    .await;
    assert_error(
        local_shaped_delete,
        StatusCode::FORBIDDEN,
        RemoteErrorCode::Forbidden,
    )
    .await;

    let bob = put(&app, "bob", BOB_TOKEN, "bob owns this note").await;

    let replace = send(
        &app,
        protocol::PUT_PATH,
        ALICE_TOKEN,
        "alice",
        &PutRequest {
            metadata: Default::default(),
            content: "alice cannot replace bob".to_owned(),
            replacement: Some(bob.key.clone()),
        },
    )
    .await;
    assert_error(replace, StatusCode::FORBIDDEN, RemoteErrorCode::Forbidden).await;

    let delete = send(
        &app,
        protocol::DELETE_PATH,
        ALICE_TOKEN,
        "alice",
        &DeleteRequest { key: bob.key },
    )
    .await;
    assert_error(delete, StatusCode::FORBIDDEN, RemoteErrorCode::Forbidden).await;
}

#[tokio::test]
async fn concurrent_puts_allocate_distinct_monotonic_ids() {
    let app = memory_app(vec![credential("alice", RemoteRole::Writer, ALICE_TOKEN)]);
    let seed = put(&app, "alice", ALICE_TOKEN, "initialize concurrent database").await;
    assert_eq!(seed.key.id, 1);
    let mut tasks = Vec::new();
    for index in 0..16 {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            put(
                &app,
                "alice",
                ALICE_TOKEN,
                &format!("concurrent note number {index}"),
            )
            .await
            .key
            .id
        }));
    }
    let mut ids = Vec::new();
    for task in tasks {
        ids.push(task.await.unwrap());
    }
    ids.sort_unstable();
    assert_eq!(ids, (2..=17).collect::<Vec<_>>());
}

#[tokio::test]
async fn sync_replaces_the_callers_complete_snapshot() {
    let app = memory_app(vec![credential("alice", RemoteRole::Writer, ALICE_TOKEN)]);
    let first = SyncRequest {
        memories: vec![
            record(10, 1, "first snapshot note"),
            record(20, 1, "second snapshot note"),
        ],
    };
    let report =
        json::<SyncReport>(send(&app, protocol::SYNC_PATH, ALICE_TOKEN, "alice", &first).await)
            .await;
    assert_eq!(
        report,
        SyncReport {
            inserted: 2,
            ..SyncReport::default()
        }
    );

    let second = SyncRequest {
        memories: vec![
            record(20, 2, "second snapshot revised"),
            record(30, 1, "third snapshot note"),
        ],
    };
    let report =
        json::<SyncReport>(send(&app, protocol::SYNC_PATH, ALICE_TOKEN, "alice", &second).await)
            .await;
    assert_eq!(report.inserted, 1);
    assert_eq!(report.replaced, 1);
    assert_eq!(report.deleted, 1);

    let exported = export_page(
        &app,
        ALICE_TOKEN,
        "alice",
        Some(vec!["alice".to_owned()]),
        None,
        10,
    )
    .await;
    assert_eq!(
        exported
            .memories
            .iter()
            .map(|memory| memory.key.id)
            .collect::<Vec<_>>(),
        [20, 30]
    );
}

#[tokio::test]
async fn sync_rejects_duplicate_snapshot_ids_without_mutation() {
    let app = memory_app(vec![credential("alice", RemoteRole::Writer, ALICE_TOKEN)]);
    let response = send(
        &app,
        protocol::SYNC_PATH,
        ALICE_TOKEN,
        "alice",
        &SyncRequest {
            memories: vec![record(10, 1, "first value"), record(10, 2, "second value")],
        },
    )
    .await;
    assert_error(
        response,
        StatusCode::BAD_REQUEST,
        RemoteErrorCode::BadRequest,
    )
    .await;

    let exported = export_page(
        &app,
        ALICE_TOKEN,
        "alice",
        Some(vec!["alice".to_owned()]),
        None,
        10,
    )
    .await;
    assert!(exported.memories.is_empty());
}

fn structurally_invalid_remote_records() -> Vec<MemoryRecord> {
    let mut reversed = record(1, 1, "reversed timestamps");
    reversed.updated_at_ms = reversed.created_at_ms - 1;

    let mut oversized_counter = record(2, 1, "oversized counter");
    oversized_counter.last_scanned_at_ms = Some(oversized_counter.updated_at_ms);
    oversized_counter.scan_count = i64::MAX as u64 + 1;

    let mut invalid_metadata = record(3, 1, "invalid metadata");
    invalid_metadata.metadata.ownership_id = Some("invalid".to_owned());

    let mut incoherent_telemetry = record(4, 1, "incoherent telemetry");
    incoherent_telemetry.use_count = 1;

    for memory in [
        &mut reversed,
        &mut oversized_counter,
        &mut invalid_metadata,
        &mut incoherent_telemetry,
    ] {
        memory.key.namespace = Some("alice".to_owned());
    }
    vec![
        reversed,
        oversized_counter,
        invalid_metadata,
        incoherent_telemetry,
    ]
}

#[tokio::test]
async fn sync_rejects_structural_and_secret_snapshots_before_store_binding() {
    let bindings = Arc::new(AtomicUsize::new(0));
    let factory_bindings = bindings.clone();
    let app = MemoryServer::new(
        move |namespace| {
            factory_bindings.fetch_add(1, Ordering::SeqCst);
            TestMemoryDatabase::default().bind(namespace)
        },
        [credential("alice", RemoteRole::Writer, ALICE_TOKEN)],
    )
    .unwrap()
    .router();

    let mut cases = structurally_invalid_remote_records();
    let mut secret = record(5, 1, "password=hunter2");
    secret.key.namespace = Some("alice".to_owned());
    cases.push(secret);
    for memory in cases {
        let response = send(
            &app,
            protocol::SYNC_PATH,
            ALICE_TOKEN,
            "alice",
            &SyncRequest {
                memories: vec![memory],
            },
        )
        .await;
        assert_error(
            response,
            StatusCode::BAD_REQUEST,
            RemoteErrorCode::BadRequest,
        )
        .await;
    }
    assert_eq!(bindings.load(Ordering::SeqCst), 0);
}

type ExportPageRequest = (Option<ExportCursor>, usize);

#[derive(Clone)]
struct ExportSequenceStore {
    pages: Arc<Mutex<Vec<ExportPageRequest>>>,
    records: Arc<Vec<MemoryRecord>>,
}

impl MemoryStore for ExportSequenceStore {
    async fn scan(&self, _query: &str, _limit: usize) -> Result<MemoryScan, MemoryError> {
        Ok(MemoryScan {
            abstained: true,
            candidates: Vec::new(),
        })
    }
    async fn read(
        &self,
        _ids: &[i64],
        _keys: &[MemoryKey],
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        Ok(Vec::new())
    }
    async fn list(&self) -> Result<Vec<MemoryRecord>, MemoryError> {
        Ok(Vec::new())
    }
    async fn put(
        &self,
        _content: &str,
        _replacement: Option<MemoryKey>,
    ) -> Result<MemoryRecord, MemoryError> {
        unreachable!()
    }
    async fn delete(&self, _key: MemoryKey) -> Result<(), MemoryError> {
        Ok(())
    }
    async fn sync(&self, _memories: &[MemoryRecord]) -> Result<SyncReport, MemoryError> {
        Ok(SyncReport::default())
    }
    async fn export_page(
        &self,
        _namespaces: Option<&[String]>,
        cursor: Option<&ExportCursor>,
        limit: usize,
    ) -> Result<(Vec<MemoryRecord>, Option<ExportCursor>), MemoryError> {
        self.pages.lock().unwrap().push((cursor.cloned(), limit));
        let after = cursor.map_or(0, |cursor| cursor.id);
        let mut records = self
            .records
            .iter()
            .filter(|record| record.key.id > after)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let has_more = self.records.iter().any(|record| {
            records
                .last()
                .is_some_and(|last| record.key.id > last.key.id)
        });
        let next = has_more.then(|| ExportCursor {
            namespace: "alice".to_owned(),
            id: records.last().unwrap().key.id,
        });
        Ok((std::mem::take(&mut records), next))
    }
}

#[tokio::test]
async fn export_advances_past_all_secret_and_trailing_secret_pages() {
    let pages = Arc::new(Mutex::new(Vec::new()));
    let mut secret_one = record(1, 1, "password=hunter2");
    secret_one.key.namespace = Some("alice".to_owned());
    let mut visible = record(2, 1, "visible export record");
    visible.key.namespace = Some("alice".to_owned());
    let mut secret_three = record(3, 1, "token=abcdefghijklmnop");
    secret_three.key.namespace = Some("alice".to_owned());
    let records = Arc::new(vec![secret_one, visible, secret_three]);
    let factory_pages = pages.clone();
    let factory_records = records.clone();
    let app = MemoryServer::new(
        move |_namespace| ExportSequenceStore {
            pages: factory_pages.clone(),
            records: factory_records.clone(),
        },
        [credential("alice", RemoteRole::Writer, ALICE_TOKEN)],
    )
    .unwrap()
    .router();

    let response = export_page(&app, ALICE_TOKEN, "alice", None, None, 1).await;
    assert_eq!(
        response
            .memories
            .iter()
            .map(|record| record.key.id)
            .collect::<Vec<_>>(),
        [2]
    );
    assert_eq!(
        response.next_cursor,
        Some(ExportCursor {
            namespace: "alice".to_owned(),
            id: 2,
        })
    );
    let terminal = export_page(&app, ALICE_TOKEN, "alice", None, response.next_cursor, 1).await;
    assert!(terminal.memories.is_empty());
    assert_eq!(terminal.next_cursor, None);
    assert_eq!(
        *pages.lock().unwrap(),
        [
            (None, 1),
            (
                Some(ExportCursor {
                    namespace: "alice".to_owned(),
                    id: 1
                }),
                1
            ),
            (
                Some(ExportCursor {
                    namespace: "alice".to_owned(),
                    id: 2
                }),
                1
            ),
        ]
    );
}

#[tokio::test]
async fn export_paginates_all_or_selected_without_visibility_filtering_or_deduplication() {
    let app = memory_app(vec![
        credential("alice", RemoteRole::Writer, ALICE_TOKEN),
        credential("bob", RemoteRole::Writer, BOB_TOKEN),
    ]);
    put(&app, "alice", ALICE_TOKEN, "same normalized content").await;
    put(&app, "alice", ALICE_TOKEN, "alice second export note").await;
    put(&app, "bob", BOB_TOKEN, "same normalized content").await;

    let mut cursor = None;
    let mut all = Vec::new();
    loop {
        let page = export_page(&app, ALICE_TOKEN, "alice", None, cursor, 1).await;
        all.extend(page.memories);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(all.len(), 3);
    assert_eq!(
        all.iter()
            .filter(|memory| memory.content == "same normalized content")
            .count(),
        2
    );
    assert_eq!(all[0].key.namespace.as_deref(), Some("alice"));

    let selected = export_page(
        &app,
        ALICE_TOKEN,
        "alice",
        Some(vec!["bob".to_owned()]),
        None,
        10,
    )
    .await;
    assert_eq!(selected.memories.len(), 1);
    assert_eq!(selected.memories[0].key.namespace.as_deref(), Some("bob"));
}

async fn export_page(
    app: &Router,
    token: &str,
    namespace: &str,
    namespaces: Option<Vec<String>>,
    cursor: Option<ExportCursor>,
    limit: usize,
) -> ExportResponse {
    json(
        send(
            app,
            protocol::EXPORT_PATH,
            token,
            namespace,
            &ExportRequest {
                namespaces,
                cursor,
                limit,
            },
        )
        .await,
    )
    .await
}

#[tokio::test]
async fn body_and_request_bounds_are_content_free_client_errors() {
    let app = memory_app(vec![credential("alice", RemoteRole::Writer, ALICE_TOKEN)]);

    let cases = [
        send(
            &app,
            protocol::SCAN_PATH,
            ALICE_TOKEN,
            "alice",
            &ScanRequest {
                scope: None,
                query: "q".repeat(MemoryLimits::PRODUCTION.query_bytes + 1),
                limit: 1,
            },
        )
        .await,
        send(
            &app,
            protocol::PUT_PATH,
            ALICE_TOKEN,
            "alice",
            &PutRequest {
                metadata: Default::default(),
                content: "c".repeat(MemoryLimits::PRODUCTION.content_bytes + 1),
                replacement: None,
            },
        )
        .await,
    ];
    let [query_error, content_error] = cases;
    assert_error(
        query_error,
        StatusCode::PAYLOAD_TOO_LARGE,
        RemoteErrorCode::QueryTooLarge,
    )
    .await;
    assert_error(
        content_error,
        StatusCode::PAYLOAD_TOO_LARGE,
        RemoteErrorCode::ContentTooLarge,
    )
    .await;

    let invalid_limit = send(
        &app,
        protocol::SCAN_PATH,
        ALICE_TOKEN,
        "alice",
        &ScanRequest {
            scope: None,
            query: "bounded".to_owned(),
            limit: MemoryLimits::PRODUCTION.scan_results + 1,
        },
    )
    .await;
    assert_error(
        invalid_limit,
        StatusCode::BAD_REQUEST,
        RemoteErrorCode::BadRequest,
    )
    .await;

    let invalid_export = send(
        &app,
        protocol::EXPORT_PATH,
        ALICE_TOKEN,
        "alice",
        &ExportRequest {
            namespaces: None,
            cursor: None,
            limit: 0,
        },
    )
    .await;
    assert_error(
        invalid_export,
        StatusCode::BAD_REQUEST,
        RemoteErrorCode::BadRequest,
    )
    .await;

    let too_many = send(
        &app,
        protocol::READ_PATH,
        ALICE_TOKEN,
        "alice",
        &ReadRequest {
            scope: None,
            ids: (1..=i64::try_from(MemoryLimits::PRODUCTION.records + 1).unwrap()).collect(),
            keys: Vec::new(),
        },
    )
    .await;
    assert_error(
        too_many,
        StatusCode::BAD_REQUEST,
        RemoteErrorCode::BadRequest,
    )
    .await;

    put(&app, "alice", ALICE_TOKEN, "duplicate response marker").await;
    let duplicate = send(
        &app,
        protocol::PUT_PATH,
        ALICE_TOKEN,
        "alice",
        &PutRequest {
            metadata: Default::default(),
            content: "duplicate response marker".to_owned(),
            replacement: None,
        },
    )
    .await;
    assert_error(duplicate, StatusCode::CONFLICT, RemoteErrorCode::Duplicate).await;

    let oversized = Request::builder()
        .method("POST")
        .uri(format!("/{}", protocol::PUT_PATH))
        .header(header::AUTHORIZATION, format!("Bearer {ALICE_TOKEN}"))
        .header(protocol::NAMESPACE_HEADER, "alice")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(vec![b'x'; MAX_JSON_BODY_BYTES + 1]))
        .unwrap();
    let oversized = app.oneshot(oversized).await.unwrap();
    assert_error(
        oversized,
        StatusCode::PAYLOAD_TOO_LARGE,
        RemoteErrorCode::BadRequest,
    )
    .await;
}

async fn assert_error(response: Response<Body>, status: StatusCode, code: RemoteErrorCode) {
    assert_eq!(response.status(), status);
    let bytes = response_bytes(response).await;
    let decoded: ErrorResponse = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded.code, code);
    assert_eq!(bytes, serde_json::to_vec(&ErrorResponse { code }).unwrap());
}

#[derive(Clone)]
struct RetryState {
    list_calls: Arc<AtomicUsize>,
    put_calls: Arc<AtomicUsize>,
}

async fn retrying_list(
    axum::extract::State(state): axum::extract::State<RetryState>,
) -> Response<Body> {
    if state.list_calls.fetch_add(1, Ordering::SeqCst) == 0 {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                code: RemoteErrorCode::Unavailable,
            }),
        )
            .into_response();
    }
    Json(ListResponse {
        memories: Vec::new(),
    })
    .into_response()
}

#[derive(Clone, Default)]
struct BookmarkState {
    requests: Arc<Mutex<Vec<Option<String>>>>,
}

async fn bookmarked_list(
    axum::extract::State(state): axum::extract::State<BookmarkState>,
    headers: axum::http::HeaderMap,
) -> Response<Body> {
    let bookmark = headers
        .get(protocol::BOOKMARK_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    state.requests.lock().unwrap().push(bookmark.clone());

    tokio::time::sleep(Duration::from_millis(50)).await;
    let response_bookmark = match bookmark.as_deref() {
        None => "bookmark-1",
        Some("bookmark-1") => "bookmark-2",
        Some("bookmark-2") => "bookmark-3",
        _ => return StatusCode::CONFLICT.into_response(),
    };
    let mut response = Json(ListResponse {
        memories: Vec::new(),
    })
    .into_response();
    response.headers_mut().insert(
        protocol::BOOKMARK_HEADER,
        response_bookmark.parse().unwrap(),
    );
    response
}

async fn unavailable_put(
    axum::extract::State(state): axum::extract::State<RetryState>,
) -> Response<Body> {
    state.put_calls.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ErrorResponse {
            code: RemoteErrorCode::Unavailable,
        }),
    )
        .into_response()
}

async fn rate_limited() -> StatusCode {
    StatusCode::TOO_MANY_REQUESTS
}

async fn oversized_list() -> Json<ListResponse> {
    Json(ListResponse {
        memories: (1..=MemoryLimits::PRODUCTION.records + 1)
            .map(|id| {
                let mut memory = record(id as i64, 1, "visible");
                memory.key = MemoryKey::remote("alice".to_owned(), id as i64, 1);
                memory
            })
            .collect(),
    })
}

async fn unsafe_scan() -> Json<ScanResponse> {
    Json(ScanResponse {
        candidates: vec![MemoryCandidate {
            metadata: Default::default(),
            key: MemoryKey::remote("alice".to_owned(), 1, 1),
            preview: "password=hunter2".to_owned(),
            score: 1.0,
        }],
    })
}

async fn oversized_scan() -> Json<ScanResponse> {
    Json(ScanResponse {
        candidates: (1..=2)
            .map(|id| MemoryCandidate {
                metadata: Default::default(),
                key: MemoryKey::remote("alice".to_owned(), id, 1),
                preview: format!("candidate {id}"),
                score: 1.0,
            })
            .collect(),
    })
}

async fn ambiguous_version_scan() -> Json<ScanResponse> {
    Json(ScanResponse {
        candidates: (1..=2)
            .map(|version| MemoryCandidate {
                metadata: Default::default(),
                key: MemoryKey::remote("alice".to_owned(), 1, version),
                preview: format!("version {version}"),
                score: 1.0,
            })
            .collect(),
    })
}

async fn ascending_score_scan() -> Json<ScanResponse> {
    Json(ScanResponse {
        candidates: (1..=2)
            .map(|id| MemoryCandidate {
                metadata: Default::default(),
                key: MemoryKey::remote("alice".to_owned(), id, 1),
                preview: format!("candidate {id}"),
                score: id as f64,
            })
            .collect(),
    })
}

async fn oversized_export() -> Json<ExportResponse> {
    Json(ExportResponse {
        memories: (1..=2)
            .map(|id| {
                let mut memory = record(id, 1, &format!("memory {id}"));
                memory.key = MemoryKey::remote("alice".to_owned(), id, 1);
                memory
            })
            .collect(),
        next_cursor: None,
    })
}

async fn impossible_sync_report() -> Json<SyncReport> {
    Json(SyncReport {
        inserted: 2,
        replaced: 0,
        unchanged: 0,
        deleted: 0,
    })
}

async fn unrelated_put() -> Json<PutResponse> {
    let mut memory = record(2, 1, "different content");
    memory.key = MemoryKey::remote("alice".to_owned(), 2, 1);
    Json(PutResponse { memory })
}

async fn equivalent_content_read() -> Json<ReadResponse> {
    let mut alice = record(1, 1, "shared operating note");
    alice.key = MemoryKey::remote("alice".to_owned(), 1, 1);
    let mut bob = record(1, 1, "shared operating note");
    bob.key = MemoryKey::remote("bob".to_owned(), 1, 1);
    Json(ReadResponse {
        memories: vec![alice, bob],
    })
}

async fn ambiguous_version_read() -> Json<ReadResponse> {
    Json(ReadResponse {
        memories: (1..=3)
            .map(|version| {
                let mut memory = record(1, version, &format!("version {version}"));
                memory.key = MemoryKey::remote("alice".to_owned(), 1, version);
                memory
            })
            .collect(),
    })
}

async fn ambiguous_version_list() -> Json<ListResponse> {
    Json(ListResponse {
        memories: (1..=2)
            .map(|version| {
                let mut memory = record(1, version, &format!("version {version}"));
                memory.key = MemoryKey::remote("alice".to_owned(), 1, version);
                memory
            })
            .collect(),
    })
}

#[tokio::test]
async fn client_retries_safe_operations_but_does_not_replay_put_responses() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let state = RetryState {
        list_calls: Arc::new(AtomicUsize::new(0)),
        put_calls: Arc::new(AtomicUsize::new(0)),
    };
    let app = Router::new()
        .route(&format!("/{}", protocol::LIST_PATH), post(retrying_list))
        .route(&format!("/{}", protocol::PUT_PATH), post(unavailable_put))
        .with_state(state.clone());
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    assert!(client.list().await.unwrap().is_empty());
    assert_eq!(state.list_calls.load(Ordering::SeqCst), 2);
    let error = client.put("one-shot put", None).await.unwrap_err();
    let MemoryError::Unavailable { source } = error else {
        panic!("expected unavailable error, got {error:?}");
    };
    assert!(matches!(
        source.downcast_ref::<RemoteClientError>(),
        Some(RemoteClientError::Rejected {
            code: RemoteErrorCode::Unavailable
        })
    ));
    assert_eq!(state.put_calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn client_carries_bookmarks_across_concurrent_clones() {
    let state = BookmarkState::default();
    let app = Router::new()
        .route(&format!("/{}", protocol::LIST_PATH), post(bookmarked_list))
        .with_state(state.clone());
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();
    let clone = client.clone();

    let (first, second) = tokio::join!(client.list(), clone.list());
    assert!(first.unwrap().is_empty());
    assert!(second.unwrap().is_empty());
    assert!(client.list().await.unwrap().is_empty());
    assert_eq!(
        *state.requests.lock().unwrap(),
        vec![
            None,
            Some("bookmark-1".to_owned()),
            Some("bookmark-2".to_owned()),
        ]
    );
    task.abort();
}

#[tokio::test]
async fn client_preserves_empty_content_and_exhausted_rate_limit_errors() {
    let client = RemoteMemoryClient::new(
        "http://127.0.0.1:1/",
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        client.put("   ", None).await,
        Err(MemoryError::EmptyContent)
    ));

    let app = Router::new().route(&format!("/{}", protocol::LIST_PATH), post(rate_limited));
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        client.list().await,
        Err(MemoryError::Unavailable { .. })
    ));
    task.abort();
}

#[tokio::test]
async fn client_reports_a_missing_versioned_session_route_as_incompatible() {
    let (endpoint, task) = live_server(Router::new()).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    assert!(matches!(
        client.session().await,
        Err(RemoteClientError::IncompatibleProtocol)
    ));
    task.abort();
}

#[tokio::test]
async fn client_rejects_an_unbounded_list_window() {
    let app = Router::new().route(&format!("/{}", protocol::LIST_PATH), post(oversized_list));
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    let error = client.list().await.unwrap_err();
    let MemoryError::Backend { source } = error else {
        panic!("expected backend error, got {error:?}");
    };
    assert!(matches!(
        source.downcast_ref::<RemoteClientError>(),
        Some(RemoteClientError::InvalidResponse)
    ));
    task.abort();
}

#[tokio::test]
async fn client_suppresses_unsafe_scan_previews() {
    let app = Router::new().route(&format!("/{}", protocol::SCAN_PATH), post(unsafe_scan));
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    assert!(
        client
            .scan("password", 5)
            .await
            .unwrap()
            .candidates
            .is_empty()
    );
    task.abort();
}

async fn invalid_list(
    axum::extract::State(memory): axum::extract::State<MemoryRecord>,
) -> Json<ListResponse> {
    Json(ListResponse {
        memories: vec![memory],
    })
}

async fn invalid_read(
    axum::extract::State(memory): axum::extract::State<MemoryRecord>,
) -> Json<ReadResponse> {
    Json(ReadResponse {
        memories: vec![memory],
    })
}

async fn invalid_export(
    axum::extract::State(memory): axum::extract::State<MemoryRecord>,
) -> Json<ExportResponse> {
    Json(ExportResponse {
        memories: vec![memory],
        next_cursor: None,
    })
}

async fn secret_and_malformed_scan() -> Json<ScanResponse> {
    Json(ScanResponse {
        candidates: vec![
            MemoryCandidate {
                metadata: Default::default(),
                key: MemoryKey::remote("alice".to_owned(), 1, 1),
                preview: "password=hunter2".to_owned(),
                score: 2.0,
            },
            MemoryCandidate {
                metadata: Default::default(),
                key: MemoryKey::local(2, 1),
                preview: "malformed candidate".to_owned(),
                score: 1.0,
            },
        ],
    })
}

fn assert_invalid_remote_response(error: MemoryError) {
    let MemoryError::Backend { source } = error else {
        panic!("expected backend error, got {error:?}");
    };
    assert!(matches!(
        source.downcast_ref::<RemoteClientError>(),
        Some(RemoteClientError::InvalidResponse)
    ));
}

#[tokio::test]
async fn client_rejects_each_structural_record_class_at_decode_boundaries() {
    for memory in structurally_invalid_remote_records() {
        for boundary in [
            protocol::READ_PATH,
            protocol::LIST_PATH,
            protocol::EXPORT_PATH,
        ] {
            let app = match boundary {
                protocol::READ_PATH => {
                    Router::new().route(&format!("/{boundary}"), post(invalid_read))
                }
                protocol::LIST_PATH => {
                    Router::new().route(&format!("/{boundary}"), post(invalid_list))
                }
                protocol::EXPORT_PATH => {
                    Router::new().route(&format!("/{boundary}"), post(invalid_export))
                }
                _ => unreachable!(),
            }
            .with_state(memory.clone());
            let (endpoint, task) = live_server(app).await;
            let client = RemoteMemoryClient::new(
                &endpoint,
                "alice".to_owned(),
                RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
            )
            .unwrap();

            let error = match boundary {
                protocol::READ_PATH => client.read(&[memory.key.id], &[]).await.unwrap_err(),
                protocol::LIST_PATH => client.list().await.unwrap_err(),
                protocol::EXPORT_PATH => client.export_page(None, None, 1).await.unwrap_err(),
                _ => unreachable!(),
            };
            assert_invalid_remote_response(error);
            task.abort();
        }
    }
}

#[tokio::test]
async fn client_rejects_malformed_candidates_even_when_secret_candidates_are_suppressed() {
    let app = Router::new().route(
        &format!("/{}", protocol::SCAN_PATH),
        post(secret_and_malformed_scan),
    );
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    assert_invalid_remote_response(client.scan("candidate", 2).await.unwrap_err());
    task.abort();
}

#[tokio::test]
async fn direct_remote_writes_preflight_all_invalid_inputs_without_http_requests() {
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = requests.clone();
    let app = Router::new().fallback(move || {
        let observed = observed.clone();
        async move {
            observed.fetch_add(1, Ordering::SeqCst);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    });
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    assert!(matches!(
        client.put("password=hunter2", None).await,
        Err(MemoryError::SecretRejected)
    ));
    let invalid_metadata = MemoryMetadata {
        ownership_id: Some("invalid".to_owned()),
        ..MemoryMetadata::default()
    };
    assert!(matches!(
        client
            .put_with_metadata("malformed metadata", &invalid_metadata, None)
            .await,
        Err(MemoryError::InvalidMetadata)
    ));

    let mut secret = record(1, 1, "token=abcdefghijklmnop");
    secret.key = MemoryKey::local(1, 1);
    assert!(matches!(
        client.sync(&[secret]).await,
        Err(MemoryError::SecretRejected)
    ));

    let mut malformed = record(1, 1, "malformed snapshot record");
    malformed.key = MemoryKey::local(1, 1);
    malformed.updated_at_ms = -1;
    assert!(matches!(
        client.sync(&[malformed]).await,
        Err(MemoryError::InvalidMetadata)
    ));

    let mut first = record(1, 1, "first duplicate id");
    first.key = MemoryKey::local(1, 1);
    let mut second = record(1, 1, "second duplicate id");
    second.key = MemoryKey::local(1, 1);
    assert!(matches!(
        client.sync(&[first, second]).await,
        Err(MemoryError::InvalidMetadata)
    ));
    let mut duplicate_identity_one = record(1, 1, "Same normalized identity");
    duplicate_identity_one.key = MemoryKey::local(1, 1);
    let mut duplicate_identity_two = record(2, 1, "  same NORMALIZED identity  ");
    duplicate_identity_two.key = MemoryKey::local(2, 1);
    assert!(matches!(
        client
            .sync(&[duplicate_identity_one, duplicate_identity_two])
            .await,
        Err(MemoryError::Duplicate)
    ));

    assert_eq!(requests.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn client_rejects_oversized_scan_responses() {
    let app = Router::new().route(&format!("/{}", protocol::SCAN_PATH), post(oversized_scan));
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    let error = client.scan("candidate", 1).await.unwrap_err();
    let MemoryError::Backend { source } = error else {
        panic!("expected backend error, got {error:?}");
    };
    assert!(matches!(
        source.downcast_ref::<RemoteClientError>(),
        Some(RemoteClientError::InvalidResponse)
    ));
    task.abort();
}

#[tokio::test]
async fn client_rejects_ambiguous_versions_in_scan_responses() {
    let app = Router::new().route(
        &format!("/{}", protocol::SCAN_PATH),
        post(ambiguous_version_scan),
    );
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    let error = client.scan("version", 2).await.unwrap_err();
    let MemoryError::Backend { source } = error else {
        panic!("expected backend error, got {error:?}");
    };
    assert!(matches!(
        source.downcast_ref::<RemoteClientError>(),
        Some(RemoteClientError::InvalidResponse)
    ));
    task.abort();
}

#[tokio::test]
async fn client_rejects_scan_responses_out_of_rank_order() {
    let app = Router::new().route(
        &format!("/{}", protocol::SCAN_PATH),
        post(ascending_score_scan),
    );
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    let error = client.scan("candidate", 2).await.unwrap_err();
    let MemoryError::Backend { source } = error else {
        panic!("expected backend error, got {error:?}");
    };
    assert!(matches!(
        source.downcast_ref::<RemoteClientError>(),
        Some(RemoteClientError::InvalidResponse)
    ));
    task.abort();
}

#[tokio::test]
async fn client_enforces_the_requested_export_page_size() {
    let app = Router::new().route(
        &format!("/{}", protocol::EXPORT_PATH),
        post(oversized_export),
    );
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    let error = client.export_page(None, None, 1).await.unwrap_err();
    let MemoryError::Backend { source } = error else {
        panic!("expected backend error, got {error:?}");
    };
    assert!(matches!(
        source.downcast_ref::<RemoteClientError>(),
        Some(RemoteClientError::InvalidResponse)
    ));
    task.abort();
}

#[tokio::test]
async fn client_rejects_sync_reports_that_do_not_match_the_snapshot() {
    let app = Router::new().route(
        &format!("/{}", protocol::SYNC_PATH),
        post(impossible_sync_report),
    );
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    let error = client.sync(&[record(1, 1, "snapshot")]).await.unwrap_err();
    let MemoryError::Backend { source } = error else {
        panic!("expected backend error, got {error:?}");
    };
    assert!(matches!(
        source.downcast_ref::<RemoteClientError>(),
        Some(RemoteClientError::InvalidResponse)
    ));
    task.abort();
}

#[tokio::test]
async fn client_rejects_put_responses_unrelated_to_the_request() {
    let app = Router::new().route(&format!("/{}", protocol::PUT_PATH), post(unrelated_put));
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    for replacement in [None, Some(MemoryKey::remote("alice".to_owned(), 1, 1))] {
        let error = client
            .put("submitted content", replacement)
            .await
            .unwrap_err();
        let MemoryError::Backend { source } = error else {
            panic!("expected backend error, got {error:?}");
        };
        assert!(matches!(
            source.downcast_ref::<RemoteClientError>(),
            Some(RemoteClientError::InvalidResponse)
        ));
    }
    task.abort();
}

#[tokio::test]
async fn client_preserves_equivalent_content_from_distinct_namespaces() {
    let app = Router::new().route(
        &format!("/{}", protocol::READ_PATH),
        post(equivalent_content_read),
    );
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    let memories = client
        .read(
            &[],
            &[
                MemoryKey::remote("alice".to_owned(), 1, 1),
                MemoryKey::remote("bob".to_owned(), 1, 1),
            ],
        )
        .await
        .unwrap();
    assert_eq!(memories.len(), 2);
    task.abort();
}

#[tokio::test]
async fn client_rejects_ambiguous_versions_for_an_unversioned_id() {
    let app = Router::new().route(
        &format!("/{}", protocol::READ_PATH),
        post(ambiguous_version_read),
    );
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    let error = client.read(&[1], &[]).await.unwrap_err();
    let MemoryError::Backend { source } = error else {
        panic!("expected backend error, got {error:?}");
    };
    assert!(matches!(
        source.downcast_ref::<RemoteClientError>(),
        Some(RemoteClientError::InvalidResponse)
    ));
    task.abort();
}

#[tokio::test]
async fn client_ignores_namespace_less_remote_keys() {
    let client = RemoteMemoryClient::new(
        "http://127.0.0.1:1/",
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    assert!(
        client
            .read(&[], &[MemoryKey::local(1, 1)])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn client_rejects_ambiguous_versions_in_list_responses() {
    let app = Router::new().route(
        &format!("/{}", protocol::LIST_PATH),
        post(ambiguous_version_list),
    );
    let (endpoint, task) = live_server(app).await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".to_owned(),
        RemoteToken::new(ALICE_TOKEN.to_owned()).unwrap(),
    )
    .unwrap();

    let error = client.list().await.unwrap_err();
    let MemoryError::Backend { source } = error else {
        panic!("expected backend error, got {error:?}");
    };
    assert!(matches!(
        source.downcast_ref::<RemoteClientError>(),
        Some(RemoteClientError::InvalidResponse)
    ));
    task.abort();
}

async fn live_server(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{address}/"), task)
}

#[derive(Clone)]
struct AsyncStore {
    namespace: String,
    puts: Arc<Mutex<Vec<(String, String)>>>,
    gate: Option<Gate>,
}

#[derive(Clone)]
struct Gate {
    active: Arc<AtomicUsize>,
    maximum_active: Arc<AtomicUsize>,
    started: Arc<Notify>,
    release: Arc<Semaphore>,
}

impl MemoryStore for AsyncStore {
    fn scan(
        &self,
        _query: &str,
        _limit: usize,
    ) -> impl Future<Output = Result<MemoryScan, MemoryError>> + Send {
        async {
            Ok(MemoryScan {
                abstained: true,
                candidates: Vec::new(),
            })
        }
    }

    fn read(
        &self,
        _ids: &[i64],
        _keys: &[MemoryKey],
    ) -> impl Future<Output = Result<Vec<MemoryRecord>, MemoryError>> + Send {
        async { Ok(Vec::new()) }
    }

    async fn list(&self) -> Result<Vec<MemoryRecord>, MemoryError> {
        if let Some(gate) = &self.gate {
            let active = gate.active.fetch_add(1, Ordering::SeqCst) + 1;
            gate.maximum_active.fetch_max(active, Ordering::SeqCst);
            gate.started.notify_one();
            gate.release.acquire().await.unwrap().forget();
            gate.active.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(Vec::new())
    }

    fn put(
        &self,
        content: &str,
        _replacement: Option<MemoryKey>,
    ) -> impl Future<Output = Result<MemoryRecord, MemoryError>> + Send {
        let namespace = self.namespace.clone();
        let content = content.to_owned();
        let puts = self.puts.clone();
        async move {
            puts.lock()
                .unwrap()
                .push((namespace.clone(), content.clone()));
            Ok(MemoryRecord {
                metadata: Default::default(),
                key: MemoryKey::remote(namespace, 41, 1),
                content,
                created_at_ms: 1,
                updated_at_ms: 1,
                last_scanned_at_ms: None,
                scan_count: 0,
                last_used_at_ms: None,
                use_count: 0,
                probation_until_ms: None,
            })
        }
    }

    async fn delete(&self, _key: MemoryKey) -> Result<(), MemoryError> {
        Ok(())
    }

    fn sync(
        &self,
        _memories: &[MemoryRecord],
    ) -> impl Future<Output = Result<SyncReport, MemoryError>> + Send {
        async { Ok(SyncReport::default()) }
    }

    fn export_page(
        &self,
        _namespaces: Option<&[String]>,
        _cursor: Option<&ExportCursor>,
        _limit: usize,
    ) -> impl Future<Output = Result<(Vec<MemoryRecord>, Option<ExportCursor>), MemoryError>> + Send
    {
        async { Ok((Vec::new(), None)) }
    }
}

#[tokio::test]
async fn generic_async_store_implementors_plug_into_the_public_server() {
    let puts = Arc::new(Mutex::new(Vec::new()));
    let bindings = Arc::new(Mutex::new(Vec::new()));
    let factory_puts = puts.clone();
    let factory_bindings = bindings.clone();
    let app = MemoryServer::new(
        move |namespace| {
            factory_bindings.lock().unwrap().push(namespace.clone());
            AsyncStore {
                namespace,
                puts: factory_puts.clone(),
                gate: None,
            }
        },
        [credential("alice", RemoteRole::Writer, ALICE_TOKEN)],
    )
    .unwrap()
    .router();

    let mismatch = send(
        &app,
        protocol::PUT_PATH,
        ALICE_TOKEN,
        "bob",
        &PutRequest {
            metadata: Default::default(),
            content: "must not bind".to_owned(),
            replacement: None,
        },
    )
    .await;
    assert_error(
        mismatch,
        StatusCode::FORBIDDEN,
        RemoteErrorCode::NamespaceMismatch,
    )
    .await;
    assert!(bindings.lock().unwrap().is_empty());

    let memory = json::<PutResponse>(
        send(
            &app,
            protocol::PUT_PATH,
            ALICE_TOKEN,
            "alice",
            &PutRequest {
                metadata: Default::default(),
                content: "native async custom store".to_owned(),
                replacement: None,
            },
        )
        .await,
    )
    .await
    .memory;
    assert_eq!(memory.key, MemoryKey::remote("alice".to_owned(), 41, 1));
    assert_eq!(*bindings.lock().unwrap(), ["alice"]);
    assert_eq!(
        *puts.lock().unwrap(),
        [("alice".to_owned(), "native async custom store".to_owned())]
    );
}

#[tokio::test]
async fn router_limits_store_operations_to_64_in_flight() {
    let gate = Gate {
        active: Arc::new(AtomicUsize::new(0)),
        maximum_active: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(Notify::new()),
        release: Arc::new(Semaphore::new(0)),
    };
    let factory_gate = gate.clone();
    let app = MemoryServer::new(
        move |namespace| AsyncStore {
            namespace,
            puts: Arc::new(Mutex::new(Vec::new())),
            gate: Some(factory_gate.clone()),
        },
        [credential("alice", RemoteRole::Reader, ALICE_TOKEN)],
    )
    .unwrap()
    .router();

    let mut requests = tokio::task::JoinSet::new();
    for _ in 0..65 {
        let app = app.clone();
        requests.spawn(async move {
            app.oneshot(request::<()>(
                "POST",
                protocol::LIST_PATH,
                Some(ALICE_TOKEN),
                Some("alice"),
                Some(&()),
            ))
            .await
            .unwrap()
        });
    }

    tokio::time::timeout(Duration::from_secs(1), async {
        while gate.active.load(Ordering::SeqCst) < 64 {
            gate.started.notified().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(gate.maximum_active.load(Ordering::SeqCst), 64);

    gate.release.add_permits(65);
    while let Some(result) = requests.join_next().await {
        assert_eq!(result.unwrap().status(), StatusCode::OK);
    }
    assert_eq!(gate.maximum_active.load(Ordering::SeqCst), 64);
}

#[tokio::test]
async fn remote_metadata_scoped_scan_and_import_keep_original_provenance() {
    use crate::{
        MemoryKind, MemoryMetadata, MemoryOrigin, MemoryScope, SourceEvidence, TraceReference,
    };
    let (endpoint, task) = live_server(memory_app(vec![credential(
        "alice",
        RemoteRole::Writer,
        ALICE_TOKEN,
    )]))
    .await;
    let client = RemoteMemoryClient::new(
        &endpoint,
        "alice".into(),
        RemoteToken::new(ALICE_TOKEN.into()).unwrap(),
    )
    .unwrap();
    client.session().await.unwrap();
    let metadata = MemoryMetadata {
        scope: MemoryScope::Repository {
            identity: "repository-one".into(),
        },
        kind: MemoryKind::CodeClaim,
        origin: MemoryOrigin::Model,
        evidence: vec![SourceEvidence::Artifact {
            digest: "a".repeat(64),
            source: "test-artifact".into(),
        }],
        producing_traces: vec![TraceReference {
            session: "s".into(),
            request: "r".into(),
            task: "t".into(),
        }],
        ..Default::default()
    };
    let original = client
        .put_with_metadata("portable remote claim", &metadata, None)
        .await
        .unwrap();
    let mut metadata = metadata;
    assert!(original.metadata.ownership_id.is_some());
    metadata.ownership_id = original.metadata.ownership_id.clone();
    assert_eq!(original.metadata, metadata);
    assert_eq!(
        client
            .scan_scoped("portable", 5, Some("repository-one"))
            .await
            .unwrap()
            .candidates[0]
            .metadata,
        metadata
    );
    assert!(
        client
            .scan_scoped("portable", 5, Some("repository-two"))
            .await
            .unwrap()
            .abstained
    );
    let replacement = client
        .put_with_metadata(
            "corrected remote claim",
            &metadata,
            Some(original.key.clone()),
        )
        .await
        .unwrap();
    assert!(matches!(
        client
            .put_with_metadata("stale write", &metadata, Some(original.key))
            .await,
        Err(MemoryError::Conflict)
    ));
    let records = client.export_all(None).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let local = crate::LocalMemoryStore::new(dir.path().join("memory/v1.sqlite3"));
    local.merge_remote_export(records).await.unwrap();
    let imported = local.list().await.unwrap().remove(0);
    assert_eq!(imported.metadata.evidence, metadata.evidence);
    assert_eq!(imported.metadata.origin, metadata.origin);
    assert_eq!(
        imported.metadata.producing_traces,
        metadata.producing_traces
    );
    assert_eq!(imported.metadata.imported_from, vec![replacement.key]);
    task.abort();
}
#[tokio::test]
async fn t05_review_remote_lessons_merges_only_its_owned_namespace() {
    use crate::{
        MemoryKind, MemoryMetadata, MemoryScope, ProposalState, SourceEvidence, TraceReference,
    };
    let (endpoint, task) = live_server(memory_app(vec![
        credential("alice", RemoteRole::Writer, ALICE_TOKEN),
        credential("bob", RemoteRole::Writer, BOB_TOKEN),
    ]))
    .await;
    let alice = RemoteMemoryClient::new(
        &endpoint,
        "alice".into(),
        RemoteToken::new(ALICE_TOKEN.into()).unwrap(),
    )
    .unwrap();
    let bob = RemoteMemoryClient::new(
        &endpoint,
        "bob".into(),
        RemoteToken::new(BOB_TOKEN.into()).unwrap(),
    )
    .unwrap();
    let ev = SourceEvidence::Artifact {
        digest: "a".repeat(64),
        source: "behavior-test".into(),
    };
    let meta = MemoryMetadata {
        scope: MemoryScope::Global,
        kind: MemoryKind::LessonProposal {
            behavior_test: ev.clone(),
            state: ProposalState::Pending,
        },
        evidence: vec![ev],
        producing_traces: vec![TraceReference {
            session: "s".into(),
            request: "r".into(),
            task: "t".into(),
        }],
        ..Default::default()
    };
    crate::propose_lesson(&alice, "test before delivery", meta.clone())
        .await
        .unwrap();
    let result = crate::propose_lesson(&bob, "test before delivery", meta).await;
    println!("Bob repeats Alice's global lesson: {result:?}");
    assert_eq!(result.unwrap().key.namespace.as_deref(), Some("bob"));
    task.abort();
}
#[tokio::test]
async fn t05_review_remote_window_does_not_skip_pending_lessons() {
    use crate::{
        MemoryKind, MemoryMetadata, MemoryScope, ProposalState, SourceEvidence, TraceReference,
    };
    let database = TestMemoryDatabase::default();
    let alice = database.bind("alice".into());
    for i in 0..512 {
        alice
            .put(&format!("fixture filler {i}"), None)
            .await
            .unwrap();
    }
    let app = MemoryServer::new(
        move |namespace| database.bind(namespace),
        [credential("bob", RemoteRole::Writer, BOB_TOKEN)],
    )
    .unwrap()
    .router();
    let (endpoint, task) = live_server(app).await;
    let bob = RemoteMemoryClient::new(
        &endpoint,
        "bob".into(),
        RemoteToken::new(BOB_TOKEN.into()).unwrap(),
    )
    .unwrap();
    let ev = SourceEvidence::Artifact {
        digest: "a".repeat(64),
        source: "behavior-test".into(),
    };
    let trace = TraceReference {
        session: "s".into(),
        request: "r".into(),
        task: "t".into(),
    };
    let meta = MemoryMetadata {
        scope: MemoryScope::Global,
        kind: MemoryKind::LessonProposal {
            behavior_test: ev.clone(),
            state: ProposalState::Pending,
        },
        evidence: vec![ev],
        producing_traces: vec![trace.clone()],
        ..Default::default()
    };
    let pending = crate::propose_lesson(&bob, "test before delivery", meta)
        .await
        .unwrap();
    let finalized = crate::finalize_lessons(&bob, &trace).await.unwrap();
    let records = bob.read(&[pending.key.id], &[]).await.unwrap();
    println!(
        "Bob finalization after 512 Alice entries: finalized={finalized}, state={:?}",
        records[0].metadata.kind
    );
    assert_eq!(finalized, 1);
    assert!(matches!(
        records[0].metadata.kind,
        MemoryKind::LessonProposal {
            state: ProposalState::Proposed,
            ..
        }
    ));
    task.abort();
}

#[tokio::test]
async fn t05_owned_backlog_over_512_is_processed_in_bounded_http_pages() {
    use crate::{
        MemoryKind, MemoryMetadata, MemoryScope, ProposalState, SourceEvidence, TraceReference,
    };
    let database = TestMemoryDatabase::default();
    let trace = TraceReference {
        session: "session".into(),
        request: "run".into(),
        task: "task".into(),
    };
    let evidence = SourceEvidence::Artifact {
        source: "test".into(),
        digest: "a".repeat(64),
    };
    let metadata = MemoryMetadata {
        scope: MemoryScope::Global,
        kind: MemoryKind::LessonProposal {
            behavior_test: evidence.clone(),
            state: ProposalState::Pending,
        },
        evidence: vec![evidence],
        producing_traces: vec![trace.clone()],
        pending_run: Some(trace.clone()),
        ..Default::default()
    };
    // Seed a deployment with a larger historical namespace capacity. The query contract must not
    // silently inherit the current UI/transfer collector's 512-row cap.
    {
        let mut state = database.state.lock().unwrap();
        for namespace in ["alice", "bob"] {
            for id in 1..=600 {
                let mut record = record(id, 1, &format!("lesson {id}"));
                record.key.namespace = Some(namespace.into());
                record.metadata = metadata.clone();
                record.metadata.ownership_id = Some(format!(
                    "{:032x}",
                    id + if namespace == "bob" { 600 } else { 0 }
                ));
                state.records.insert((namespace.into(), id), record);
            }
            state.next_ids.insert(namespace.into(), 601);
        }
    }
    let backend = database.clone();
    let (endpoint, task) = live_server(
        MemoryServer::new(
            move |namespace| backend.bind(namespace),
            [
                credential("bob", RemoteRole::Writer, BOB_TOKEN),
                credential("reader", RemoteRole::Reader, READER_TOKEN),
            ],
        )
        .unwrap()
        .router(),
    )
    .await;
    let bob = RemoteMemoryClient::new(
        &endpoint,
        "bob".into(),
        RemoteToken::new(BOB_TOKEN.into()).unwrap(),
    )
    .unwrap();
    assert_eq!(bob.list().await.unwrap().len(), 512);
    let mut nomination = metadata.clone();
    nomination.pending_run = None;
    let repeated = crate::propose_lesson(&bob, "lesson 600", nomination)
        .await
        .unwrap();
    assert_eq!(repeated.key.id, 600);
    assert_eq!(repeated.key.version, 2);
    let page = bob
        .lesson_page(
            &crate::LessonQuery::Pending {
                trace: trace.clone(),
            },
            0,
        )
        .await
        .unwrap();
    assert_eq!(page.len(), protocol::MAX_EXPORT_PAGE_RECORDS);
    assert!(
        page.iter()
            .all(|record| record.key.namespace.as_deref() == Some("bob"))
    );
    assert_eq!(crate::finalize_lessons(&bob, &trace).await.unwrap(), 600);
    assert_eq!(crate::finalize_lessons(&bob, &trace).await.unwrap(), 0);
    {
        let state = database.state.lock().unwrap();
        assert!(
            state
                .records
                .values()
                .filter(|record| record.key.namespace.as_deref() == Some("alice"))
                .all(|record| matches!(
                    record.metadata.kind,
                    MemoryKind::LessonProposal {
                        state: ProposalState::Pending,
                        ..
                    }
                ))
        );
    }
    let reader = RemoteMemoryClient::new(
        &endpoint,
        "reader".into(),
        RemoteToken::new(READER_TOKEN.into()).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        reader
            .lesson_page(&crate::LessonQuery::Pending { trace }, 0)
            .await,
        Err(MemoryError::RemoteReadOnly)
    ));
    task.abort();
}

#[tokio::test]
async fn t05_http_scoped_read_does_not_clear_hidden_probation() {
    let database = TestMemoryDatabase::default();
    let alice = database.bind("alice".into());
    let record = alice
        .put_with_metadata(
            "foreign scope",
            &crate::MemoryMetadata {
                scope: crate::MemoryScope::Repository {
                    identity: "foreign".into(),
                },
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    let backend = database.clone();
    let (endpoint, task) = live_server(
        MemoryServer::new(
            move |namespace| backend.bind(namespace),
            [credential("bob", RemoteRole::Writer, BOB_TOKEN)],
        )
        .unwrap()
        .router(),
    )
    .await;
    let bob = RemoteMemoryClient::new(
        &endpoint,
        "bob".into(),
        RemoteToken::new(BOB_TOKEN.into()).unwrap(),
    )
    .unwrap();
    assert!(
        bob.read_scoped(&[], std::slice::from_ref(&record.key), Some("active"))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        database.state.lock().unwrap().records[&("alice".into(), record.key.id)],
        record
    );
    // Scope is not authorization: an ordinary shared read is still permitted.
    assert_eq!(bob.read(&[], &[record.key]).await.unwrap()[0].use_count, 1);
    task.abort();
}

#[tokio::test]
async fn t05_owned_query_rejects_namespace_spoofing() {
    let app = memory_app(vec![credential("bob", RemoteRole::Writer, BOB_TOKEN)]);
    let query = serde_json::json!({"query":{"type":"pending","trace":{"session":"s","request":"r","task":"t"}},"after":0});
    let mismatch = send(&app, protocol::LESSONS_PATH, BOB_TOKEN, "alice", &query).await;
    assert_error(
        mismatch,
        StatusCode::FORBIDDEN,
        RemoteErrorCode::NamespaceMismatch,
    )
    .await;
    let mut forged = query;
    forged["namespace"] = "alice".into();
    let invalid = send(&app, protocol::LESSONS_PATH, BOB_TOKEN, "bob", &forged).await;
    assert_error(
        invalid,
        StatusCode::BAD_REQUEST,
        RemoteErrorCode::BadRequest,
    )
    .await;
}
