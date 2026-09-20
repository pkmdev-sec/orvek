//! Cloudflare D1 implementation of Orvek's shared memory storage contract.
//!
//! Each store instance owns one writer namespace while reads span the shared database. D1 batches
//! provide the transaction boundary for mutations and telemetry. Integer columns cross the
//! JavaScript boundary as decimal text so 64-bit IDs, versions, counters, and timestamps retain
//! their exact values.

mod row;

use orvek_memory::{
    MemoryError, MemoryKey, MemoryLimits, MemoryMetadata, MemoryRecord, MemoryScan, MemoryStore,
    server::protocol::{self, ExportCursor, SyncReport},
};
use row::{CapacityRow, CorpusRow, DecodeResult, ReplaceRow, SyncRow, VersionRow};
use serde::Serialize;
use std::{collections::HashSet, fmt::Display, future::Future, sync::Arc};
use thiserror::Error;
use worker::{
    D1DatabaseSession, Date,
    d1::{D1PreparedStatement, D1Result, D1Type},
    send::SendFuture,
};

const SYNC_MEMORY_SQL: &str = "INSERT INTO memories (namespace, metadata, id, version, content, identity, created_at_ms, updated_at_ms, last_scanned_at_ms, scan_count, last_used_at_ms, use_count, probation_until_ms) SELECT ?, json_set(value ->> '$.metadata', '$.ownership_id', COALESCE(json_extract(value ->> '$.metadata', '$.ownership_id'), lower(hex(randomblob(16))))), CAST(value ->> '$.id' AS INTEGER), CAST(value ->> '$.version' AS INTEGER), value ->> '$.content', value ->> '$.identity', CAST(value ->> '$.created_at_ms' AS INTEGER), CAST(value ->> '$.updated_at_ms' AS INTEGER), CAST(value ->> '$.last_scanned_at_ms' AS INTEGER), CAST(value ->> '$.scan_count' AS INTEGER), CAST(value ->> '$.last_used_at_ms' AS INTEGER), CAST(value ->> '$.use_count' AS INTEGER), CAST(value ->> '$.probation_until_ms' AS INTEGER) FROM json_each(?)";
const INSERT_MEMORY_SQL: &str = "INSERT INTO memories (namespace, id, version, metadata, content, identity, created_at_ms, updated_at_ms, probation_until_ms) SELECT namespace, next_id, 1, json_set(?, '$.ownership_id', lower(hex(randomblob(16)))), ?, ?, CAST(? AS INTEGER), CAST(? AS INTEGER), CAST(? AS INTEGER) FROM memory_namespaces WHERE namespace = ? AND (SELECT COUNT(*) FROM memories WHERE namespace = ?) < ? AND (SELECT COALESCE(SUM(length(CAST(content AS BLOB))), 0) FROM memories WHERE namespace = ?) + ? <= ? AND NOT EXISTS (SELECT 1 FROM memories WHERE namespace = ? AND identity = ?)";
const REPLACE_MEMORY_SQL: &str = "UPDATE memories SET version = CAST(? AS INTEGER), metadata = json_set(?, '$.ownership_id', json_extract(metadata, '$.ownership_id')), content = ?, identity = ?, updated_at_ms = CAST(? AS INTEGER), last_scanned_at_ms = NULL, scan_count = 0, last_used_at_ms = NULL, use_count = 0, probation_until_ms = CAST(? AS INTEGER) WHERE namespace = ? AND id = CAST(? AS INTEGER) AND version = CAST(? AS INTEGER) AND NOT EXISTS (SELECT 1 FROM memories other WHERE other.namespace = ? AND other.identity = ? AND other.id != CAST(? AS INTEGER)) AND (SELECT COALESCE(SUM(length(CAST(content AS BLOB))), 0) FROM memories WHERE namespace = ?) - length(CAST(memories.content AS BLOB)) + ? <= ?";

const OWNED_LESSON_FILTER: &str = "namespace = ? AND id > CAST(? AS INTEGER) AND json_extract(metadata, '$.kind.type') = 'lesson_proposal' AND (? = 'matching' OR (json_extract(metadata, '$.kind.state') = 'pending' AND json_extract(metadata, '$.pending_run.session') = ? AND json_extract(metadata, '$.pending_run.request') = ? AND json_extract(metadata, '$.pending_run.task') = ?))";
const READ_SCOPE_FILTER: &str = "(? = 'null' OR COALESCE(json_extract(metadata, '$.scope.type'), 'legacy_unscoped') != 'repository' OR json_extract(metadata, '$.scope.identity') = json_extract(?, '$.repository'))";

const RECORD_COLUMNS: &str = "memories.metadata AS metadata, memories.namespace AS namespace, CAST(memories.id AS TEXT) AS id, CAST(memories.version AS TEXT) AS version, memories.content AS content, CAST(memories.created_at_ms AS TEXT) AS created_at_ms, CAST(memories.updated_at_ms AS TEXT) AS updated_at_ms, CAST(memories.last_scanned_at_ms AS TEXT) AS last_scanned_at_ms, CAST(memories.scan_count AS TEXT) AS scan_count, CAST(memories.last_used_at_ms AS TEXT) AS last_used_at_ms, CAST(memories.use_count AS TEXT) AS use_count, CAST(memories.probation_until_ms AS TEXT) AS probation_until_ms";
const PRUNE_SQL: &str =
    "DELETE FROM memories WHERE probation_until_ms <= CAST(? AS INTEGER) AND use_count = 0";
const VISIBLE_RECORD_SQL: &str = "(memories.probation_until_ms IS NULL OR memories.probation_until_ms > CAST(? AS INTEGER) OR memories.use_count != 0)";

/// Deployment-selected bound for exact Worker-side BM25 retrieval.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScanBudget {
    pub(crate) records: usize,
    pub(crate) content_bytes: usize,
}

/// Namespace-bound shared memory backed by Cloudflare D1.
#[derive(Clone, Debug)]
pub(crate) struct CloudflareMemoryStore {
    session: Arc<D1DatabaseSession>,
    namespace: Arc<str>,
    scan_budget: ScanBudget,
}

impl CloudflareMemoryStore {
    fn scan_filtered(
        &self,
        query: &str,
        limit: usize,
        scope: Option<protocol::ScanScope>,
    ) -> impl Future<Output = Result<MemoryScan, MemoryError>> + Send {
        let store = self.clone();
        let query = query.to_owned();
        SendFuture::new(async move {
            let limits = MemoryLimits::PRODUCTION;
            if query.len() > limits.query_bytes {
                return Err(MemoryError::QueryTooLarge {
                    maximum_bytes: limits.query_bytes,
                });
            }
            let now = current_time_ms().to_string();
            let scan_budget = store.scan_budget;
            let corpus_size_sql = format!(
                "SELECT COUNT(*) AS record_count, COALESCE(SUM(length(CAST(content AS BLOB))), 0) AS content_bytes FROM memories WHERE {VISIBLE_RECORD_SQL}"
            );
            let corpus_sql = format!(
                "SELECT {RECORD_COLUMNS} FROM memories WHERE {VISIBLE_RECORD_SQL} AND (SELECT COUNT(*) FROM memories WHERE {VISIBLE_RECORD_SQL}) <= {} AND (SELECT COALESCE(SUM(length(CAST(content AS BLOB))), 0) FROM memories WHERE {VISIBLE_RECORD_SQL}) <= {} ORDER BY namespace, id",
                scan_budget.records, scan_budget.content_bytes
            );
            let statements = vec![
                store.statement(corpus_size_sql, &[D1Type::Text(&now)])?,
                store.statement(
                    corpus_sql,
                    &[D1Type::Text(&now), D1Type::Text(&now), D1Type::Text(&now)],
                )?,
            ];
            let results = store.batch(statements).await?;
            let corpus = results[0].one::<CorpusRow>()?;
            if corpus.record_count > scan_budget.records
                || corpus.content_bytes > scan_budget.content_bytes
            {
                return Err(MemoryError::StorageCapacity);
            }
            let mut records = results[1].records()?;
            if let Some(scope) = scope {
                records.retain(|record| record.metadata.visible_in(scope.repository.as_deref()));
            }
            let scan = MemoryScan::rank(&query, &records, limit.min(limits.scan_results));
            let mut updates = Vec::with_capacity(scan.candidates.len() + 1);
            updates.push(store.statement(PRUNE_SQL, &[D1Type::Text(&now)])?);
            for candidate in &scan.candidates {
                let namespace = candidate.key.namespace.as_deref().unwrap_or_default();
                updates.push(store.statement(
                    "UPDATE memories SET last_scanned_at_ms = CAST(? AS INTEGER), scan_count = CASE WHEN scan_count < 9223372036854775807 THEN scan_count + 1 ELSE scan_count END WHERE namespace = ? AND id = CAST(? AS INTEGER) AND version = CAST(? AS INTEGER)",
                    &[
                        D1Type::Text(&now),
                        D1Type::Text(namespace),
                        D1Type::Text(&candidate.key.id.to_string()),
                        D1Type::Text(&candidate.key.version.to_string()),
                    ],
                )?);
            }
            store.batch(updates).await?;
            Ok(scan)
        })
    }

    /// Binds a request-scoped D1 session to one writer namespace; reads remain shared.
    pub(crate) fn new(
        session: Arc<D1DatabaseSession>,
        namespace: String,
        scan_budget: ScanBudget,
    ) -> Self {
        Self {
            session,
            namespace: Arc::from(namespace),
            scan_budget,
        }
    }

    /// Prepares one statement and binds all parameters without exposing D1 errors to the protocol.
    fn statement(
        &self,
        sql: impl Into<String>,
        values: &[D1Type<'_>],
    ) -> Result<D1PreparedStatement, MemoryError> {
        self.session
            .prepare(sql)
            .bind_refs(values)
            .map_err(MessageError::backend)
    }

    /// Executes one atomic D1 batch.
    ///
    /// Transport and statement failures become transient unavailability because D1 rolls back a
    /// failed batch. Row decoding failures are backend contract violations.
    async fn batch(
        &self,
        statements: Vec<D1PreparedStatement>,
    ) -> Result<Vec<D1Result>, MemoryError> {
        let results = self
            .session
            .batch(statements)
            .await
            .map_err(|error| MemoryError::unavailable(MessageError(error.to_string())))?;
        if let Some(error) = results.iter().find_map(D1Result::error) {
            return Err(MemoryError::unavailable(MessageError(error)));
        }
        Ok(results)
    }
}

impl MemoryStore for CloudflareMemoryStore {
    fn scan(
        &self,
        query: &str,
        limit: usize,
    ) -> impl Future<Output = Result<MemoryScan, MemoryError>> + Send {
        self.scan_filtered(query, limit, None)
    }
    fn scan_scoped(
        &self,
        query: &str,
        limit: usize,
        repository: Option<&str>,
    ) -> impl Future<Output = Result<MemoryScan, MemoryError>> + Send {
        self.scan_filtered(
            query,
            limit,
            Some(protocol::ScanScope {
                repository: repository.map(str::to_owned),
            }),
        )
    }

    fn read(
        &self,
        ids: &[i64],
        keys: &[MemoryKey],
    ) -> impl Future<Output = Result<Vec<MemoryRecord>, MemoryError>> + Send {
        self.read_filtered(ids, keys, None)
    }
    fn read_scoped(
        &self,
        ids: &[i64],
        keys: &[MemoryKey],
        repository: Option<&str>,
    ) -> impl Future<Output = Result<Vec<MemoryRecord>, MemoryError>> + Send {
        self.read_filtered(
            ids,
            keys,
            Some(protocol::ScanScope {
                repository: repository.map(str::to_owned),
            }),
        )
    }
    fn lesson_page(
        &self,
        query: &orvek_memory::LessonQuery,
        after: i64,
    ) -> impl Future<Output = Result<Vec<MemoryRecord>, MemoryError>> + Send {
        let store = self.clone();
        let query = query.clone();
        SendFuture::new(async move {
            let now = current_time_ms().to_string();
            let (kind, session, request, task) = match &query {
                orvek_memory::LessonQuery::Matching { .. } => ("matching", "", "", ""),
                orvek_memory::LessonQuery::Pending { trace } => (
                    "pending",
                    trace.session.as_str(),
                    trace.request.as_str(),
                    trace.task.as_str(),
                ),
            };
            let sql = format!(
                "SELECT {RECORD_COLUMNS} FROM memories WHERE {VISIBLE_RECORD_SQL} AND {OWNED_LESSON_FILTER} ORDER BY id LIMIT {}",
                protocol::MAX_EXPORT_PAGE_RECORDS
            );
            let mut after = after;
            let mut matches = Vec::new();
            loop {
                let results = store
                    .batch(vec![store.statement(
                        sql.clone(),
                        &[
                            D1Type::Text(&now),
                            D1Type::Text(&store.namespace),
                            D1Type::Text(&after.to_string()),
                            D1Type::Text(kind),
                            D1Type::Text(session),
                            D1Type::Text(request),
                            D1Type::Text(task),
                        ],
                    )?])
                    .await?;
                let records = results[0].records()?;
                if records.is_empty() {
                    return Ok(matches);
                }
                for record in records {
                    after = record.key.id;
                    if query.matches(&record) {
                        matches.push(record);
                        if matches.len() == protocol::MAX_EXPORT_PAGE_RECORDS {
                            return Ok(matches);
                        }
                    }
                }
            }
        })
    }

    fn list(&self) -> impl Future<Output = Result<Vec<MemoryRecord>, MemoryError>> + Send {
        let store = self.clone();
        SendFuture::new(async move {
            let now = current_time_ms().to_string();
            let sql = format!(
                "SELECT {RECORD_COLUMNS} FROM memories WHERE {VISIBLE_RECORD_SQL} ORDER BY namespace, id LIMIT {}",
                MemoryLimits::PRODUCTION.records
            );
            let results = store
                .batch(vec![store.statement(sql, &[D1Type::Text(&now)])?])
                .await?;
            results[0].records()
        })
    }

    fn put(
        &self,
        content: &str,
        replacement: Option<MemoryKey>,
    ) -> impl Future<Output = Result<MemoryRecord, MemoryError>> + Send {
        async move {
            self.put_with_metadata(content, &MemoryMetadata::default(), replacement)
                .await
        }
    }

    fn put_with_metadata(
        &self,
        content: &str,
        metadata: &MemoryMetadata,
        replacement: Option<MemoryKey>,
    ) -> impl Future<Output = Result<MemoryRecord, MemoryError>> + Send {
        let metadata = metadata.clone();
        let store = self.clone();
        let content = content.to_owned();
        SendFuture::new(async move {
            validate_content(&content)?;
            metadata.validate()?;
            if replacement
                .as_ref()
                .is_some_and(|key| key.namespace.as_deref() != Some(store.namespace.as_ref()))
            {
                return Err(MemoryError::RemoteReadOnly);
            }
            match replacement {
                Some(key) => store.replace(content, metadata, key).await,
                None => store.insert(content, metadata).await,
            }
        })
    }

    fn delete(&self, key: MemoryKey) -> impl Future<Output = Result<(), MemoryError>> + Send {
        let store = self.clone();
        SendFuture::new(async move {
            if key.namespace.as_deref() != Some(store.namespace.as_ref()) {
                return Err(MemoryError::RemoteReadOnly);
            }
            let id = key.id.to_string();
            let version = key.version.to_string();
            let results = store.batch(vec![
                store.statement("SELECT CAST(version AS TEXT) AS version FROM memories WHERE namespace = ? AND id = CAST(? AS INTEGER)", &[D1Type::Text(&store.namespace), D1Type::Text(&id)])?,
                store.statement("DELETE FROM memories WHERE namespace = ? AND id = CAST(? AS INTEGER) AND version = CAST(? AS INTEGER)", &[D1Type::Text(&store.namespace), D1Type::Text(&id), D1Type::Text(&version)])?,
            ]).await?;
            let existing = results[0]
                .results::<VersionRow>()
                .map_err(MessageError::backend)?
                .into_iter()
                .next();
            if existing.is_some_and(|row| row.version != version) {
                return Err(MemoryError::Conflict);
            }
            Ok(())
        })
    }

    fn sync(
        &self,
        memories: &[MemoryRecord],
    ) -> impl Future<Output = Result<SyncReport, MemoryError>> + Send {
        let store = self.clone();
        let memories = memories.to_vec();
        SendFuture::new(async move {
            validate_snapshot(&memories)?;
            let payload = memories.iter().map(SyncRow::from).collect::<Vec<_>>();
            let json = serde_json::to_string(&payload).map_err(MessageError::backend)?;
            let now = current_time_ms().to_string();
            let namespace = store.namespace.as_ref();
            let select_sql =
                format!("SELECT {RECORD_COLUMNS} FROM memories WHERE namespace = ? ORDER BY id");
            // Replacing the namespace as a set permits identities to move between stable IDs.
            let delete_sql = "DELETE FROM memories WHERE namespace = ?";
            let insert_sql = SYNC_MEMORY_SQL;
            let next_sql = "UPDATE memory_namespaces SET next_id = MAX(next_id, COALESCE((SELECT MAX(CAST(value ->> '$.id' AS INTEGER)) + 1 FROM json_each(?)), 1)) WHERE namespace = ?";
            let results = store.batch(vec![
                store.statement(PRUNE_SQL, &[D1Type::Text(&now)])?,
                store.statement(select_sql, &[D1Type::Text(namespace)])?,
                store.statement("INSERT INTO memory_namespaces(namespace) VALUES (?) ON CONFLICT DO NOTHING", &[D1Type::Text(namespace)])?,
                store.statement(delete_sql, &[D1Type::Text(namespace)])?,
                store.statement(insert_sql, &[D1Type::Text(namespace), D1Type::Text(&json)])?,
                store.statement(next_sql, &[D1Type::Text(&json), D1Type::Text(namespace)])?,
            ]).await?;
            let previous = results[1].records()?;
            Ok(sync_report(&previous, &memories, namespace))
        })
    }

    fn export_page(
        &self,
        namespaces: Option<&[String]>,
        cursor: Option<&ExportCursor>,
        limit: usize,
    ) -> impl Future<Output = Result<(Vec<MemoryRecord>, Option<ExportCursor>), MemoryError>> + Send
    {
        let store = self.clone();
        let namespaces = namespaces.map(<[String]>::to_vec);
        let cursor = cursor.cloned();
        SendFuture::new(async move {
            let now = current_time_ms().to_string();
            let selected = serde_json::to_string(&namespaces).map_err(MessageError::backend)?;
            let has_cursor = if cursor.is_some() { "1" } else { "0" };
            let cursor_namespace = cursor.as_ref().map_or("", |value| value.namespace.as_str());
            let cursor_id = cursor
                .as_ref()
                .map_or("0".to_owned(), |value| value.id.to_string());
            let bounded = limit.clamp(1, protocol::MAX_EXPORT_PAGE_RECORDS);
            let sql = format!(
                "SELECT {RECORD_COLUMNS} FROM memories WHERE {VISIBLE_RECORD_SQL} AND (? = 'null' OR namespace IN (SELECT value FROM json_each(?))) AND (? = '0' OR namespace > ? OR (namespace = ? AND id > CAST(? AS INTEGER))) ORDER BY namespace, id LIMIT {}",
                bounded + 1
            );
            let results = store
                .batch(vec![store.statement(
                    sql,
                    &[
                        D1Type::Text(&now),
                        D1Type::Text(&selected),
                        D1Type::Text(&selected),
                        D1Type::Text(has_cursor),
                        D1Type::Text(cursor_namespace),
                        D1Type::Text(cursor_namespace),
                        D1Type::Text(&cursor_id),
                    ],
                )?])
                .await?;
            let mut records = results[0].records()?;
            let has_more = records.len() > bounded;
            records.truncate(bounded);
            let next = has_more.then(|| {
                let key = &records
                    .last()
                    .expect("a page with more rows is non-empty")
                    .key;
                ExportCursor {
                    namespace: key.namespace.clone().expect("D1 records are namespaced"),
                    id: key.id,
                }
            });
            Ok((records, next))
        })
    }
}

impl CloudflareMemoryStore {
    fn read_filtered(
        &self,
        ids: &[i64],
        keys: &[MemoryKey],
        scope: Option<protocol::ScanScope>,
    ) -> impl Future<Output = Result<Vec<MemoryRecord>, MemoryError>> + Send {
        let store = self.clone();
        let ids = ids.to_vec();
        let keys = keys.to_vec();
        SendFuture::new(async move {
            let now = current_time_ms().to_string();
            let requests = ReadRequest::collect(&store.namespace, ids, keys);
            let request_json = serde_json::to_string(&requests).map_err(MessageError::backend)?;
            let scope_json = serde_json::to_string(&scope).map_err(MessageError::backend)?;
            let update_sql = format!(
                "WITH requested AS (SELECT value ->> '$.namespace' AS namespace, CAST(value ->> '$.id' AS INTEGER) AS id, value ->> '$.version' AS version FROM json_each(?)) UPDATE memories SET last_used_at_ms = CAST(? AS INTEGER), use_count = CASE WHEN use_count < 9223372036854775807 THEN use_count + 1 ELSE use_count END, probation_until_ms = NULL WHERE EXISTS (SELECT 1 FROM requested WHERE requested.namespace = memories.namespace AND requested.id = memories.id AND (requested.version IS NULL OR CAST(requested.version AS INTEGER) = memories.version)) AND {READ_SCOPE_FILTER}"
            );
            let select_sql = format!(
                "WITH requested AS (SELECT CAST(key AS INTEGER) AS ordinal, value ->> '$.namespace' AS namespace, CAST(value ->> '$.id' AS INTEGER) AS id, value ->> '$.version' AS version FROM json_each(?)) SELECT {RECORD_COLUMNS} FROM requested JOIN memories USING (namespace, id) WHERE (requested.version IS NULL OR CAST(requested.version AS INTEGER) = memories.version) AND {READ_SCOPE_FILTER} ORDER BY requested.ordinal"
            );
            let results = store
                .batch(vec![
                    store.statement(PRUNE_SQL, &[D1Type::Text(&now)])?,
                    store.statement(
                        update_sql,
                        &[
                            D1Type::Text(&request_json),
                            D1Type::Text(&now),
                            D1Type::Text(&scope_json),
                            D1Type::Text(&scope_json),
                        ],
                    )?,
                    store.statement(
                        select_sql,
                        &[
                            D1Type::Text(&request_json),
                            D1Type::Text(&scope_json),
                            D1Type::Text(&scope_json),
                        ],
                    )?,
                ])
                .await?;
            results[2].records()
        })
    }

    /// Inserts one record while atomically enforcing namespace capacity and deduplication.
    async fn insert(
        &self,
        content: String,
        metadata: MemoryMetadata,
    ) -> Result<MemoryRecord, MemoryError> {
        let limits = MemoryLimits::PRODUCTION;
        let now_ms = current_time_ms();
        let now = now_ms.to_string();
        let identity = metadata.identity(&content);
        let metadata_json = serde_json::to_string(&metadata).map_err(MessageError::backend)?;
        let namespace = self.namespace.as_ref();
        let diagnostics = "SELECT EXISTS(SELECT 1 FROM memories WHERE namespace = ? AND identity = ?) AS duplicate, (SELECT COUNT(*) FROM memories WHERE namespace = ?) AS record_count, (SELECT COALESCE(SUM(length(CAST(content AS BLOB))), 0) FROM memories WHERE namespace = ?) AS content_bytes";
        let insert_sql = INSERT_MEMORY_SQL;
        let probation = now_ms
            .saturating_add(limits.probation_duration_ms)
            .to_string();
        let count_limit = limits.records.to_string();
        let content_len = content.len().to_string();
        let byte_limit = limits.total_content_bytes.to_string();
        let select_sql =
            format!("SELECT {RECORD_COLUMNS} FROM memories WHERE namespace = ? AND identity = ?");
        let statements = vec![
            self.statement(PRUNE_SQL, &[D1Type::Text(&now)])?,
            self.statement(
                "INSERT INTO memory_namespaces(namespace) VALUES (?) ON CONFLICT DO NOTHING",
                &[D1Type::Text(namespace)],
            )?,
            self.statement(
                diagnostics,
                &[
                    D1Type::Text(namespace),
                    D1Type::Text(&identity),
                    D1Type::Text(namespace),
                    D1Type::Text(namespace),
                ],
            )?,
            self.statement(
                insert_sql,
                &[
                    D1Type::Text(&metadata_json),
                    D1Type::Text(&content),
                    D1Type::Text(&identity),
                    D1Type::Text(&now),
                    D1Type::Text(&now),
                    D1Type::Text(&probation),
                    D1Type::Text(namespace),
                    D1Type::Text(namespace),
                    D1Type::Text(&count_limit),
                    D1Type::Text(namespace),
                    D1Type::Text(&content_len),
                    D1Type::Text(&byte_limit),
                    D1Type::Text(namespace),
                    D1Type::Text(&identity),
                ],
            )?,
            self.statement(
                "UPDATE memory_namespaces SET next_id = next_id + 1 WHERE namespace = ? AND changes() = 1",
                &[D1Type::Text(namespace)],
            )?,
            self.statement(
                select_sql,
                &[D1Type::Text(namespace), D1Type::Text(&identity)],
            )?,
        ];
        let results = self.batch(statements).await?;
        let diagnostic = results[2].one::<CapacityRow>()?;
        if diagnostic.duplicate != 0 {
            return Err(MemoryError::Duplicate);
        }
        if diagnostic.record_count >= limits.records {
            return Err(MemoryError::RecordCapacity {
                maximum: limits.records,
            });
        }
        if diagnostic.content_bytes.saturating_add(content.len()) > limits.total_content_bytes {
            return Err(MemoryError::ContentCapacity {
                maximum_bytes: limits.total_content_bytes,
            });
        }
        results[5].record()
    }

    /// Replaces one record with optimistic concurrency and namespace capacity checks.
    async fn replace(
        &self,
        content: String,
        metadata: MemoryMetadata,
        key: MemoryKey,
    ) -> Result<MemoryRecord, MemoryError> {
        let limits = MemoryLimits::PRODUCTION;
        if key.version >= i64::MAX as u64 {
            return Err(MemoryError::Conflict);
        }
        let now_ms = current_time_ms();
        let now = now_ms.to_string();
        let probation = now_ms
            .saturating_add(limits.probation_duration_ms)
            .to_string();
        let identity = metadata.identity(&content);
        let metadata_json = serde_json::to_string(&metadata).map_err(MessageError::backend)?;
        let namespace = self.namespace.as_ref();
        let id = key.id.to_string();
        let old_version = key.version.to_string();
        let new_version = key
            .version
            .checked_add(1)
            .ok_or(MemoryError::Conflict)?
            .to_string();
        let diagnostics = "SELECT EXISTS(SELECT 1 FROM memories WHERE namespace = ? AND identity = ? AND id != CAST(? AS INTEGER)) AS duplicate, CAST((SELECT version FROM memories WHERE namespace = ? AND id = CAST(? AS INTEGER)) AS TEXT) AS version, (SELECT COALESCE(SUM(length(CAST(content AS BLOB))), 0) FROM memories WHERE namespace = ?) AS content_bytes, (SELECT length(CAST(content AS BLOB)) FROM memories WHERE namespace = ? AND id = CAST(? AS INTEGER)) AS replaced_bytes";
        let update = REPLACE_MEMORY_SQL;
        let content_len = content.len().to_string();
        let byte_limit = limits.total_content_bytes.to_string();
        let select_sql = format!(
            "SELECT {RECORD_COLUMNS} FROM memories WHERE namespace = ? AND id = CAST(? AS INTEGER) AND version = CAST(? AS INTEGER)"
        );
        let results = self
            .batch(vec![
                self.statement(PRUNE_SQL, &[D1Type::Text(&now)])?,
                self.statement(
                    diagnostics,
                    &[
                        D1Type::Text(namespace),
                        D1Type::Text(&identity),
                        D1Type::Text(&id),
                        D1Type::Text(namespace),
                        D1Type::Text(&id),
                        D1Type::Text(namespace),
                        D1Type::Text(namespace),
                        D1Type::Text(&id),
                    ],
                )?,
                self.statement(
                    update,
                    &[
                        D1Type::Text(&new_version),
                        D1Type::Text(&metadata_json),
                        D1Type::Text(&content),
                        D1Type::Text(&identity),
                        D1Type::Text(&now),
                        D1Type::Text(&probation),
                        D1Type::Text(namespace),
                        D1Type::Text(&id),
                        D1Type::Text(&old_version),
                        D1Type::Text(namespace),
                        D1Type::Text(&identity),
                        D1Type::Text(&id),
                        D1Type::Text(namespace),
                        D1Type::Text(&content_len),
                        D1Type::Text(&byte_limit),
                    ],
                )?,
                self.statement(
                    select_sql,
                    &[
                        D1Type::Text(namespace),
                        D1Type::Text(&id),
                        D1Type::Text(&new_version),
                    ],
                )?,
            ])
            .await?;
        let diagnostic = results[1].one::<ReplaceRow>()?;
        if diagnostic.duplicate != 0 {
            return Err(MemoryError::Duplicate);
        }
        let Some(version) = diagnostic.version else {
            return Err(MemoryError::NotFound);
        };
        if version != old_version {
            return Err(MemoryError::Conflict);
        }
        if diagnostic
            .content_bytes
            .saturating_sub(diagnostic.replaced_bytes.unwrap_or(0))
            .saturating_add(content.len())
            > limits.total_content_bytes
        {
            return Err(MemoryError::ContentCapacity {
                maximum_bytes: limits.total_content_bytes,
            });
        }
        results[3].record()
    }
}

#[derive(Debug, Error)]
#[error("{0}")]
struct MessageError(String);

impl MessageError {
    /// Erases a backend-specific failure at the storage boundary.
    fn backend(error: impl Display) -> MemoryError {
        MemoryError::backend(Self(error.to_string()))
    }
}

/// One normalized read selector passed to SQLite's `json_each` table.
#[derive(Serialize)]
struct ReadRequest {
    namespace: String,
    id: String,
    version: Option<String>,
}

impl ReadRequest {
    /// Combines versioned remote keys and namespace-relative IDs in caller order.
    fn collect(namespace: &str, ids: Vec<i64>, keys: Vec<MemoryKey>) -> Vec<Self> {
        let mut seen = HashSet::new();
        let mut requests = Vec::new();
        for key in keys {
            let Some(owner) = key.namespace else {
                continue;
            };
            if seen.insert((owner.clone(), key.id)) {
                requests.push(Self {
                    namespace: owner,
                    id: key.id.to_string(),
                    version: Some(key.version.to_string()),
                });
            }
        }
        for id in ids {
            if seen.insert((namespace.to_owned(), id)) {
                requests.push(Self {
                    namespace: namespace.to_owned(),
                    id: id.to_string(),
                    version: None,
                });
            }
        }
        requests
    }
}

fn sync_report(
    previous: &[MemoryRecord],
    incoming: &[MemoryRecord],
    namespace: &str,
) -> SyncReport {
    let incoming_ids = incoming
        .iter()
        .map(|record| record.key.id)
        .collect::<HashSet<_>>();
    let mut report = SyncReport {
        deleted: previous
            .iter()
            .filter(|record| !incoming_ids.contains(&record.key.id))
            .count(),
        ..SyncReport::default()
    };
    for record in incoming {
        let mut normalized = record.clone();
        normalized.key.namespace = Some(namespace.to_owned());
        match previous.iter().find(|old| old.key.id == record.key.id) {
            Some(old) if old == &normalized => report.unchanged += 1,
            Some(_) => report.replaced += 1,
            None => report.inserted += 1,
        }
    }
    report
}

fn validate_content(content: &str) -> Result<(), MemoryError> {
    let limits = MemoryLimits::PRODUCTION;
    if content.trim().is_empty() {
        return Err(MemoryError::EmptyContent);
    }
    if content.len() > limits.content_bytes {
        return Err(MemoryError::ContentTooLarge {
            maximum_bytes: limits.content_bytes,
        });
    }
    Ok(())
}

fn validate_snapshot(memories: &[MemoryRecord]) -> Result<(), MemoryError> {
    let limits = MemoryLimits::PRODUCTION;
    if memories.len() > limits.records {
        return Err(MemoryError::RecordCapacity {
            maximum: limits.records,
        });
    }
    let mut ids = HashSet::new();
    let mut identities = HashSet::new();
    let mut bytes = 0usize;
    for memory in memories {
        if !memory.key.is_local()
            || memory.key.id <= 0
            || memory.key.version == 0
            || memory.key.version > i64::MAX as u64
            || memory.scan_count > i64::MAX as u64
            || memory.use_count > i64::MAX as u64
            || memory.created_at_ms < 0
            || memory.updated_at_ms < memory.created_at_ms
            || !ids.insert(memory.key.id)
        {
            return Err(MemoryError::Conflict);
        }
        validate_content(&memory.content)?;
        memory.metadata.validate()?;
        if !identities.insert(memory.metadata.identity(&memory.content)) {
            return Err(MemoryError::Duplicate);
        }
        bytes = bytes
            .checked_add(memory.content.len())
            .ok_or(MemoryError::ContentCapacity {
                maximum_bytes: limits.total_content_bytes,
            })?;
    }
    if bytes > limits.total_content_bytes {
        return Err(MemoryError::ContentCapacity {
            maximum_bytes: limits.total_content_bytes,
        });
    }
    Ok(())
}

fn current_time_ms() -> i64 {
    Date::now().as_millis().try_into().unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::RECORD_COLUMNS;
    use rusqlite::{Connection, params};

    #[test]
    fn read_projection_is_unambiguous() {
        let database = Connection::open_in_memory().unwrap();
        database
            .execute_batch(
                "CREATE TABLE memories (
                    namespace TEXT NOT NULL,
                    id INTEGER NOT NULL,
                    version INTEGER NOT NULL,
                    content TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    last_scanned_at_ms INTEGER,
                    scan_count INTEGER NOT NULL,
                    last_used_at_ms INTEGER,
                    use_count INTEGER NOT NULL,
                    probation_until_ms INTEGER,
                    metadata TEXT NOT NULL DEFAULT '{}'
                );
                INSERT INTO memories VALUES ('ben', 1, 1, 'fixture', 1, 1, NULL, 0, NULL, 0, NULL, '{}');",
            )
            .unwrap();
        let sql = format!(
            "WITH requested AS (SELECT CAST(key AS INTEGER) AS ordinal, value ->> '$.namespace' AS namespace, CAST(value ->> '$.id' AS INTEGER) AS id, value ->> '$.version' AS version FROM json_each(?)) SELECT {RECORD_COLUMNS} FROM requested JOIN memories USING (namespace, id) WHERE requested.version IS NULL OR CAST(requested.version AS INTEGER) = memories.version ORDER BY requested.ordinal"
        );
        let request = r#"[{"namespace":"ben","id":"1","version":"1"}]"#;

        let content = database
            .query_row(&sql, params![request], |row| {
                row.get::<_, String>("content")
            })
            .unwrap();

        assert_eq!(content, "fixture");
    }
    #[test]
    fn owned_queue_and_scoped_read_sql_filter_before_limits_and_telemetry() {
        use super::{OWNED_LESSON_FILTER, READ_SCOPE_FILTER, VISIBLE_RECORD_SQL};
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("../migrations/0001_initial.sql"))
            .unwrap();
        db.execute_batch(include_str!("../migrations/0002_evidence.sql"))
            .unwrap();
        for namespace in ["alice", "bob"] {
            db.execute(
                "INSERT INTO memory_namespaces(namespace) VALUES (?1)",
                [namespace],
            )
            .unwrap();
            for id in 1..=600 {
                let metadata = serde_json::json!({"kind":{"type":"lesson_proposal","state":"pending"}, "pending_run":{"session":"s","request":"r","task":"t"}, "scope":{"type":"repository","identity":"foreign"}});
                db.execute("INSERT INTO memories(namespace,id,version,content,identity,created_at_ms,updated_at_ms,probation_until_ms,metadata) VALUES (?1,?2,1,'lesson',?3,1,1,1000,?4)", params![namespace,id,id.to_string(),metadata.to_string()]).unwrap();
            }
        }
        db.execute_batch(include_str!("../migrations/0003_ownership.sql"))
            .unwrap();
        let owner: String = db.query_row("SELECT json_extract(metadata, '$.ownership_id') FROM memories WHERE namespace='alice' AND id=1", [], |row| row.get(0)).unwrap();
        assert_eq!(owner.len(), 32);
        db.execute_batch(include_str!("../migrations/0003_ownership.sql"))
            .unwrap();
        assert_eq!(db.query_row("SELECT json_extract(metadata, '$.ownership_id') FROM memories WHERE namespace='alice' AND id=1", [], |row| row.get::<_,String>(0)).unwrap(), owner);
        let sql = format!(
            "SELECT id FROM memories WHERE {VISIBLE_RECORD_SQL} AND {OWNED_LESSON_FILTER} ORDER BY id LIMIT 128"
        );
        let mut after = 0;
        let mut count = 0;
        loop {
            let ids = db
                .prepare(&sql)
                .unwrap()
                .query_map(
                    params!["2", "bob", after, "pending", "s", "r", "t"],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            if ids.is_empty() {
                break;
            }
            assert!(ids.len() <= 128);
            for id in ids {
                after = id;
                count += 1;
                db.execute("UPDATE memories SET metadata=json_set(metadata,'$.kind.state','proposed') WHERE namespace='bob' AND id=?1", [id]).unwrap();
            }
        }
        assert_eq!(count, 600);
        assert_eq!(db.query_row("SELECT count(*) FROM memories WHERE namespace='alice' AND json_extract(metadata,'$.kind.state')='pending'", [], |row| row.get::<_,i64>(0)).unwrap(), 600);
        let update = format!(
            "UPDATE memories SET use_count=use_count+1, probation_until_ms=NULL WHERE namespace='alice' AND id=1 AND {READ_SCOPE_FILTER}"
        );
        assert_eq!(
            db.execute(
                &update,
                params![r#"{"repository":"active"}"#, r#"{"repository":"active"}"#]
            )
            .unwrap(),
            0
        );
        assert_eq!(db.query_row("SELECT use_count,probation_until_ms FROM memories WHERE namespace='alice' AND id=1", [], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?))).unwrap(), (0,1000));
        assert_eq!(
            db.execute(
                &update,
                params![r#"{"repository":"foreign"}"#, r#"{"repository":"foreign"}"#]
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn metadata_migration_insert_replace_and_snapshot_use_the_same_sql_contract() {
        use super::{INSERT_MEMORY_SQL, REPLACE_MEMORY_SQL, SYNC_MEMORY_SQL};
        let database = Connection::open_in_memory().unwrap();
        database
            .execute_batch(include_str!("../migrations/0001_initial.sql"))
            .unwrap();
        database
            .execute_batch(include_str!("../migrations/0002_evidence.sql"))
            .unwrap();
        database
            .execute(
                "INSERT INTO memory_namespaces(namespace) VALUES ('alice')",
                [],
            )
            .unwrap();
        let metadata = r#"{"scope":{"type":"global"},"origin":{"type":"model"}}"#;
        database
            .execute(
                INSERT_MEMORY_SQL,
                params![
                    metadata,
                    "claim",
                    "global:claim",
                    "1",
                    "1",
                    "100000",
                    "alice",
                    "alice",
                    "512",
                    "alice",
                    "5",
                    "262144",
                    "alice",
                    "global:claim"
                ],
            )
            .unwrap();
        let read = || {
            database.query_row("SELECT metadata, content, version FROM memories WHERE namespace='alice' AND id=1",[],|row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?))).unwrap()
        };
        let ownership: String = database
            .query_row(
                "SELECT json_extract(metadata, '$.ownership_id') FROM memories",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(ownership.len(), 32);
        let with_owner = |value: &str| {
            let mut value: serde_json::Value = serde_json::from_str(value).unwrap();
            value["ownership_id"] = ownership.clone().into();
            value
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&read().0).unwrap(),
            with_owner(metadata)
        );
        assert_eq!((read().1, read().2), ("claim".into(), 1));
        let corrected = r#"{"scope":{"type":"global"},"origin":{"type":"user"}}"#;
        database
            .execute(
                REPLACE_MEMORY_SQL,
                params![
                    "2",
                    corrected,
                    "corrected claim",
                    "global:corrected claim",
                    "2",
                    "100000",
                    "alice",
                    "1",
                    "1",
                    "alice",
                    "global:corrected claim",
                    "1",
                    "alice",
                    "15",
                    "262144"
                ],
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&read().0).unwrap(),
            with_owner(corrected)
        );
        assert_eq!((read().1, read().2), ("corrected claim".into(), 2));
        assert_eq!(
            database
                .execute(
                    REPLACE_MEMORY_SQL,
                    params![
                        "2",
                        metadata,
                        "stale",
                        "global:stale",
                        "3",
                        "100000",
                        "alice",
                        "1",
                        "1",
                        "alice",
                        "global:stale",
                        "1",
                        "alice",
                        "5",
                        "262144"
                    ]
                )
                .unwrap(),
            0
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&read().0).unwrap(),
            with_owner(corrected)
        );
        assert_eq!((read().1, read().2), ("corrected claim".into(), 2));
        let snapshot = serde_json::json!([{"id":"1","version":"7","content":"transferred claim","identity":"global:transferred claim","metadata":metadata,"created_at_ms":"1","updated_at_ms":"3","last_scanned_at_ms":null,"scan_count":"0","last_used_at_ms":null,"use_count":"0","probation_until_ms":null}]);
        database.execute("DELETE FROM memories", []).unwrap();
        database
            .execute(SYNC_MEMORY_SQL, params!["alice", snapshot.to_string()])
            .unwrap();
        let mut transferred: serde_json::Value = serde_json::from_str(&read().0).unwrap();
        assert_eq!(
            transferred
                .as_object_mut()
                .unwrap()
                .remove("ownership_id")
                .unwrap()
                .as_str()
                .unwrap()
                .len(),
            32
        );
        assert_eq!(
            transferred,
            serde_json::from_str::<serde_json::Value>(metadata).unwrap()
        );
        assert_eq!((read().1, read().2), ("transferred claim".into(), 7));
    }
}
