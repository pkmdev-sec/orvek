//! Private versioned SQLite storage.

use super::{MemoryError, MemoryStore, current_time_ms};
use crate::{
    MemoryImportReport, MemoryKey, MemoryLimits, MemoryRecord, MemoryScan,
    model::{MemoryRecordScope, StoredMemory},
    secrets::contains_likely_secret,
    server::protocol::{self, ExportCursor, SyncReport},
};
use rusqlite::{
    Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior, params,
};
use std::{
    collections::{HashMap, HashSet},
    fs,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use thiserror::Error;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const DATABASE_PAGE_SIZE_BYTES: usize = 4 * 1024;
const SCHEMA_VERSION: i64 = 3;
const ALLOCATOR_ROW_ID: i64 = 1;
const INSTALL_ALLOCATOR_TRIGGER: &str = "CREATE TRIGGER IF NOT EXISTS memory_id_allocator
     AFTER INSERT ON memories
     BEGIN
        SELECT CASE
            WHEN NEW.id < (SELECT next_id FROM memory_metadata WHERE id = 1)
            THEN RAISE(ABORT, 'memory id was already allocated')
        END;
        UPDATE memory_metadata SET next_id = NEW.id + 1
        WHERE id = 1 AND next_id <= NEW.id;
     END;";

#[derive(Debug, Error)]
enum LocalStoreError {
    #[error("could not prepare the memory directory")]
    Directory(#[source] std::io::Error),
    #[error("memory storage task stopped unexpectedly")]
    Task(#[source] tokio::task::JoinError),
}

/// Concrete private SQLite memory store.
#[derive(Clone, Debug)]
pub struct LocalMemoryStore {
    pub(crate) path: Arc<PathBuf>,
    limits: MemoryLimits,
    #[cfg(test)]
    migration_barrier: Option<Arc<std::sync::Barrier>>,
}

impl LocalMemoryStore {
    /// Opens or creates a private local SQLite store at `path` on first use.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Arc::new(path.into()),
            limits: MemoryLimits::PRODUCTION,
            #[cfg(test)]
            migration_barrier: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_limits(path: impl Into<PathBuf>, limits: MemoryLimits) -> Self {
        Self {
            path: Arc::new(path.into()),
            limits,
            migration_barrier: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_migration_barrier(
        path: impl Into<PathBuf>,
        barrier: Arc<std::sync::Barrier>,
    ) -> Self {
        Self {
            path: Arc::new(path.into()),
            limits: MemoryLimits::PRODUCTION,
            migration_barrier: Some(barrier),
        }
    }

    /// Searches visible memories and records scan telemetry at `now_ms`.
    async fn scan(
        &self,
        query: &str,
        limit: usize,
        now_ms: i64,
    ) -> Result<MemoryScan, MemoryError> {
        let store = self.clone();
        let query = query.to_owned();
        let limit = limit.min(self.limits.scan_results);
        run_local(move || store.scan_local(&query, limit, now_ms)).await
    }

    pub(crate) fn scan_local(
        &self,
        query: &str,
        limit: usize,
        now_ms: i64,
    ) -> Result<MemoryScan, MemoryError> {
        self.scan_filtered(query, limit, now_ms, None)
    }

    fn scan_filtered(
        &self,
        query: &str,
        limit: usize,
        now_ms: i64,
        scope: Option<&protocol::ScanScope>,
    ) -> Result<MemoryScan, MemoryError> {
        if query.len() > self.limits.query_bytes {
            return Err(MemoryError::QueryTooLarge {
                maximum_bytes: self.limits.query_bytes,
            });
        }

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        prune_expired(&transaction, now_ms)?;

        let memories = load_all(&transaction, &self.limits)?
            .into_iter()
            .filter(|memory| {
                memory.metadata.reject_likely_secret().is_ok()
                    && !contains_likely_secret(&memory.content)
            })
            .filter(|memory| {
                scope.is_none_or(|scope| memory.metadata.visible_in(scope.repository.as_deref()))
            })
            .map(MemoryRecord::from)
            .collect::<Vec<_>>();
        let limit = limit.min(self.limits.scan_results);
        let scan = MemoryScan::rank(query, &memories, limit);

        for candidate in &scan.candidates {
            let changed = transaction
                .execute(
                    "UPDATE memories
                     SET last_scanned_at_ms = MAX(?1, updated_at_ms,
                                                   COALESCE(last_scanned_at_ms, ?1)),
                         scan_count = scan_count + 1
                     WHERE id = ?2 AND scan_count < ?3",
                    params![now_ms, candidate.key.id, i64::MAX],
                )
                .map_err(sqlite_error)?;
            if changed != 1 {
                return Err(MemoryError::InvalidMetadata);
            }
        }
        transaction.commit().map_err(sqlite_error)?;

        Ok(scan)
    }

    pub(crate) fn read_local(
        &self,
        references: &[(i64, Option<u64>)],
        now_ms: i64,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        self.read_filtered(references, now_ms, None)
    }

    fn read_filtered(
        &self,
        references: &[(i64, Option<u64>)],
        now_ms: i64,
        scope: Option<&protocol::ScanScope>,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        prune_expired(&transaction, now_ms)?;

        let mut seen = HashSet::new();
        let mut records = Vec::with_capacity(references.len());
        for &(id, version) in references {
            let memory = load_one(&transaction, id, &self.limits)?;
            let Some(mut memory) = memory else {
                continue;
            };
            if scope.is_some_and(|scope| !memory.metadata.visible_in(scope.repository.as_deref()))
                || version.is_some_and(|version| version != memory.version)
                || memory.metadata.reject_likely_secret().is_err()
                || contains_likely_secret(&memory.content)
                || !seen.insert(id)
            {
                continue;
            }

            let last_used_at_ms = now_ms
                .max(memory.updated_at_ms)
                .max(memory.last_used_at_ms.unwrap_or(i64::MIN));
            let changed = transaction
                .execute(
                    "UPDATE memories
                     SET last_used_at_ms = ?1, use_count = use_count + 1,
                         probation_until_ms = NULL
                     WHERE id = ?2 AND use_count < ?3",
                    params![last_used_at_ms, id, i64::MAX],
                )
                .map_err(sqlite_error)?;
            if changed != 1 {
                return Err(MemoryError::InvalidMetadata);
            }
            memory.last_used_at_ms = Some(last_used_at_ms);
            memory.use_count = memory
                .use_count
                .checked_add(1)
                .ok_or(MemoryError::InvalidMetadata)?;
            memory.probation_until_ms = None;
            records.push(memory.into());
        }
        transaction.commit().map_err(sqlite_error)?;
        Ok(records)
    }

    /// Reads records selected by unversioned IDs and versioned keys.
    ///
    /// IDs refer to the active local store or configured remote namespace. Keys retain their
    /// namespace and version semantics. Missing, stale, and duplicate records are omitted.
    /// Successful reads record use telemetry at `now_ms`.
    async fn read(
        &self,
        local_ids: &[i64],
        keys: &[MemoryKey],
        now_ms: i64,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let store = self.clone();
        let owned_keys = keys.to_vec();
        let read_ids = local_ids.to_vec();
        run_local(move || {
            let mut references = owned_keys
                .iter()
                .filter(|key| key.is_local())
                .map(|key| (key.id, Some(key.version)))
                .collect::<Vec<_>>();
            references.extend(distinct_ids(&read_ids).into_iter().map(|id| (id, None)));
            store.read_local(&references, now_ms)
        })
        .await
    }

    /// Inserts content or compare-and-swap replaces the record identified by `replacement`.
    async fn put(
        &self,
        content: &str,
        replacement: Option<MemoryKey>,
        now_ms: i64,
    ) -> Result<MemoryRecord, MemoryError> {
        if replacement.as_ref().is_some_and(|key| !key.is_local()) {
            return Err(MemoryError::RemoteReadOnly);
        }
        let store = self.clone();
        let content = content.to_owned();
        run_local(move || store.put_local(&content, replacement, now_ms)).await
    }

    pub(crate) fn put_local(
        &self,
        content: &str,
        replacement: Option<MemoryKey>,
        now_ms: i64,
    ) -> Result<MemoryRecord, MemoryError> {
        self.put_metadata_local(
            content,
            &crate::MemoryMetadata::default(),
            replacement,
            now_ms,
        )
    }

    fn put_metadata_local(
        &self,
        content: &str,
        metadata: &crate::MemoryMetadata,
        replacement: Option<MemoryKey>,
        now_ms: i64,
    ) -> Result<MemoryRecord, MemoryError> {
        crate::store::validate_authored_content(content, metadata, &self.limits)?;
        let mut metadata = metadata.clone();
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        prune_expired(&transaction, now_ms)?;

        metadata.ownership_id = match &replacement {
            Some(key) => match load_one(&transaction, key.id, &self.limits)?
                .and_then(|record| record.metadata.ownership_id)
            {
                Some(identity) => Some(identity),
                None => Some(new_ownership(&transaction)?),
            },
            None => Some(new_ownership(&transaction)?),
        };
        metadata.validate()?;
        let normalized_identity = metadata.identity(content);
        let result = match replacement {
            Some(key) => self.replace(&transaction, content, &normalized_identity, key, now_ms),
            None => self.insert(&transaction, content, &normalized_identity, now_ms),
        }?;
        save_metadata(&transaction, result.id, &metadata)?;
        let result =
            load_one(&transaction, result.id, &self.limits)?.ok_or(MemoryError::NotFound)?;
        transaction.commit().map_err(sqlite_error)?;
        Ok(result.into())
    }

    /// Compare-and-swap deletes `key` from its owning backend.
    async fn delete(&self, key: MemoryKey) -> Result<(), MemoryError> {
        if !key.is_local() {
            return Err(MemoryError::RemoteReadOnly);
        }
        let store = self.clone();
        run_local(move || store.delete_local(key)).await
    }

    pub(crate) fn delete_local(&self, key: MemoryKey) -> Result<(), MemoryError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let current_version = transaction
            .query_row(
                "SELECT version FROM memories WHERE id = ?1",
                [key.id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        let Some(current_version) = current_version else {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(());
        };
        if current_version as u64 != key.version {
            return Err(MemoryError::Conflict);
        }

        transaction
            .execute("DELETE FROM memories WHERE id = ?1", [key.id])
            .map_err(sqlite_error)?;
        transaction.commit().map_err(sqlite_error)?;
        Ok(())
    }

    /// Lists all visible records after pruning probation at `now_ms`.
    async fn list(&self, now_ms: i64) -> Result<Vec<MemoryRecord>, MemoryError> {
        let store = self.clone();
        run_local(move || store.list_local(now_ms)).await
    }

    /// Imports a validated remote snapshot into a local store as new probationary records.
    pub async fn merge_remote_export(
        &self,
        memories: Vec<MemoryRecord>,
    ) -> Result<MemoryImportReport, MemoryError> {
        if memories.is_empty() {
            return Ok(MemoryImportReport::default());
        }
        let store = self.clone();
        let now_ms = current_time_ms();
        run_local(move || store.merge_remote_export_local(memories, now_ms)).await
    }

    fn merge_remote_export_local(
        &self,
        memories: Vec<MemoryRecord>,
        now_ms: i64,
    ) -> Result<MemoryImportReport, MemoryError> {
        for memory in &memories {
            memory.validate(MemoryRecordScope::AnyRemote, &self.limits)?;
        }
        self.import_records_local(memories, now_ms)
    }

    /// Imports portable records atomically, retaining source owning keys in provenance.
    pub async fn import_records(
        &self,
        memories: Vec<MemoryRecord>,
    ) -> Result<MemoryImportReport, MemoryError> {
        let store = self.clone();
        run_local(move || store.import_records_local(memories, current_time_ms())).await
    }

    fn import_records_local(
        &self,
        memories: Vec<MemoryRecord>,
        now_ms: i64,
    ) -> Result<MemoryImportReport, MemoryError> {
        for memory in &memories {
            memory.validate(MemoryRecordScope::Portable, &self.limits)?;
            crate::store::validate_authored_content(
                &memory.content,
                &memory.metadata,
                &self.limits,
            )?;
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        prune_expired(&transaction, now_ms)?;
        let mut report = MemoryImportReport::default();
        let mut existing = load_all(&transaction, &self.limits)?;
        for memory in memories {
            let mut metadata = memory.metadata;
            // Old archives had no owning-store identity. Do not invent shared ownership from ID 1.
            // Only the recorded key, payload, and creation time are available for identity.
            let source_ownership = metadata.ownership_id.clone().unwrap_or_else(|| {
                crate::sources::digest(
                    serde_json::to_string(&(
                        memory.key.clone(),
                        &memory.content,
                        &metadata,
                        memory.created_at_ms,
                    ))
                    .expect("serializable record")
                    .as_bytes(),
                )[..32]
                    .to_owned()
            });
            metadata.ownership_id = Some(source_ownership.clone());
            let transfer_identity =
                metadata.transfer_identity(&memory.content, memory.key.namespace.as_deref());
            if !metadata.imported_from.contains(&memory.key) {
                metadata.imported_from.push(memory.key.clone());
            }
            let source = crate::OwnershipReference {
                ownership_id: source_ownership,
                key: memory.key.clone(),
            };
            if !metadata.transferred_from.contains(&source) {
                metadata.transferred_from.push(source);
            }
            if let Some(current) = existing.iter_mut().find(|record| {
                record.metadata.transfer_identity(&record.content, None) == transfer_identity
            }) {
                let mut merged = current.metadata.clone();
                for key in metadata.imported_from {
                    if !merged.imported_from.contains(&key) {
                        merged.imported_from.push(key);
                    }
                }
                for source in metadata.transferred_from {
                    if !merged.transferred_from.contains(&source) {
                        merged.transferred_from.push(source);
                    }
                }
                merged.validate()?;
                if merged != current.metadata {
                    // Provenance changes are CAS-visible; content and original evidence stay intact.
                    let version = current
                        .version
                        .checked_add(1)
                        .ok_or(MemoryError::Conflict)?;
                    transaction.execute("UPDATE memories SET version = ?1, normalized_identity = ?2 WHERE id = ?3", params![version as i64, merged.identity(&current.content), current.id]).map_err(sqlite_write_error)?;
                    save_metadata(&transaction, current.id, &merged)?;
                    current.version = version;
                    current.metadata = merged;
                }
                report.skipped += 1;
                continue;
            }
            metadata.ownership_id = Some(new_ownership(&transaction)?);
            metadata.validate()?;
            let identity = metadata.identity(&memory.content);
            let inserted = self.insert(&transaction, &memory.content, &identity, now_ms)?;
            // New local ownership is distinct, but the original version and timestamps survive.
            transaction.execute("UPDATE memories SET version = ?1, created_at_ms = ?2, updated_at_ms = ?3, last_scanned_at_ms = ?4, scan_count = ?5, last_used_at_ms = ?6, use_count = ?7, probation_until_ms = ?8 WHERE id = ?9",
                params![memory.key.version as i64, memory.created_at_ms, memory.updated_at_ms, memory.last_scanned_at_ms, memory.scan_count as i64, memory.last_used_at_ms, memory.use_count as i64, memory.probation_until_ms, inserted.id]).map_err(sqlite_write_error)?;
            save_metadata(&transaction, inserted.id, &metadata)?;
            existing.push(
                load_one(&transaction, inserted.id, &self.limits)?.ok_or(MemoryError::NotFound)?,
            );
            report.inserted += 1;
        }
        transaction.commit().map_err(sqlite_write_error)?;
        Ok(report)
    }

    pub(crate) fn list_local(&self, now_ms: i64) -> Result<Vec<MemoryRecord>, MemoryError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        prune_expired(&transaction, now_ms)?;
        let records = load_all(&transaction, &self.limits)?
            .into_iter()
            .filter(|memory| {
                memory.metadata.reject_likely_secret().is_ok()
                    && !contains_likely_secret(&memory.content)
            })
            .map(MemoryRecord::from)
            .collect();
        transaction.commit().map_err(sqlite_error)?;
        Ok(records)
    }

    fn insert(
        &self,
        transaction: &Transaction<'_>,
        content: &str,
        normalized_identity: &str,
        now_ms: i64,
    ) -> Result<StoredMemory, MemoryError> {
        if identity_exists(transaction, normalized_identity, None)? {
            return Err(MemoryError::Duplicate);
        }
        let totals = totals(transaction)?;
        if totals.records >= self.limits.records as u64 {
            return Err(MemoryError::RecordCapacity {
                maximum: self.limits.records,
            });
        }
        self.check_content_capacity(totals.content_bytes, 0, content.len())?;

        let probation_until_ms = now_ms.saturating_add(self.limits.probation_duration_ms);
        let id = allocate_id(transaction)?;
        transaction
            .execute(
                "INSERT INTO memories (
                    id, content, normalized_identity, created_at_ms, updated_at_ms,
                    last_scanned_at_ms, scan_count, last_used_at_ms, use_count,
                    probation_until_ms, version
                 ) VALUES (?1, ?2, ?3, ?4, ?4, NULL, 0, NULL, 0, ?5, 1)",
                params![id, content, normalized_identity, now_ms, probation_until_ms],
            )
            .map_err(sqlite_write_error)?;
        load_one(transaction, id, &self.limits)?.ok_or(MemoryError::NotFound)
    }

    fn replace(
        &self,
        transaction: &Transaction<'_>,
        content: &str,
        normalized_identity: &str,
        key: MemoryKey,
        now_ms: i64,
    ) -> Result<StoredMemory, MemoryError> {
        let current = transaction
            .query_row(
                "SELECT version, length(CAST(content AS BLOB)) FROM memories WHERE id = ?1",
                [key.id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(sqlite_error)?;
        let Some((current_version, previous_content_bytes)) = current else {
            return Err(MemoryError::NotFound);
        };
        if current_version as u64 != key.version {
            return Err(MemoryError::Conflict);
        }
        if identity_exists(transaction, normalized_identity, Some(key.id))? {
            return Err(MemoryError::Duplicate);
        }

        let totals = totals(transaction)?;
        self.check_content_capacity(
            totals.content_bytes,
            previous_content_bytes as usize,
            content.len(),
        )?;
        let next_version = current_version
            .checked_add(1)
            .ok_or(MemoryError::Conflict)?;
        let probation_until_ms = now_ms.saturating_add(self.limits.probation_duration_ms);
        transaction
            .execute(
                "UPDATE memories
                 SET content = ?1, normalized_identity = ?2, updated_at_ms = ?3,
                     last_scanned_at_ms = NULL, scan_count = 0,
                     last_used_at_ms = NULL, use_count = 0,
                     probation_until_ms = ?4, version = ?5
                 WHERE id = ?6 AND version = ?7",
                params![
                    content,
                    normalized_identity,
                    now_ms,
                    probation_until_ms,
                    next_version,
                    key.id,
                    current_version,
                ],
            )
            .map_err(sqlite_write_error)?;
        load_one(transaction, key.id, &self.limits)?.ok_or(MemoryError::NotFound)
    }

    fn check_content_capacity(
        &self,
        current_bytes: u64,
        replaced_bytes: usize,
        new_bytes: usize,
    ) -> Result<(), MemoryError> {
        let resulting_bytes = current_bytes
            .saturating_sub(replaced_bytes as u64)
            .saturating_add(new_bytes as u64);
        if resulting_bytes > self.limits.total_content_bytes as u64 {
            return Err(MemoryError::ContentCapacity {
                maximum_bytes: self.limits.total_content_bytes,
            });
        }
        Ok(())
    }

    pub(crate) fn open(&self) -> Result<Connection, MemoryError> {
        let database_path = prepare_private_path(&self.path)?;
        let mut connection = open_private_database(&database_path)?;
        connection
            .busy_timeout(BUSY_TIMEOUT)
            .map_err(sqlite_error)?;
        let schema_version = connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .map_err(sqlite_error)?;
        if !(0..=SCHEMA_VERSION).contains(&schema_version) {
            return Err(MemoryError::UnsupportedSchemaVersion {
                found: schema_version,
                supported: SCHEMA_VERSION,
            });
        }
        let journal_mode = connection
            .query_row("PRAGMA journal_mode = DELETE", [], |row| {
                row.get::<_, String>(0)
            })
            .map_err(sqlite_error)?;
        if !journal_mode.eq_ignore_ascii_case("delete") {
            return Err(MemoryError::InvalidMetadata);
        }
        #[cfg(test)]
        if let Some(barrier) = &self.migration_barrier {
            barrier.wait();
            barrier.wait();
        }
        // The allocator table is a backward-compatible schema-v1 extension. Older builds ignore
        // it; current builds retain identity history even when every memory row is deleted.
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let schema_version = transaction
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .map_err(sqlite_error)?;
        if !(0..=SCHEMA_VERSION).contains(&schema_version) {
            return Err(MemoryError::UnsupportedSchemaVersion {
                found: schema_version,
                supported: SCHEMA_VERSION,
            });
        }
        if schema_version == 0 {
            transaction
                .pragma_update(None, "page_size", DATABASE_PAGE_SIZE_BYTES as i64)
                .map_err(sqlite_error)?;
        }
        let page_size = transaction
            .query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0))
            .map_err(sqlite_error)? as usize;
        let maximum_pages = self.limits.database_bytes.div_ceil(page_size).max(1);
        transaction
            .pragma_update(None, "max_page_count", maximum_pages as i64)
            .map_err(sqlite_error)?;
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS memories (
                    id INTEGER PRIMARY KEY,
                    content TEXT NOT NULL,
                    normalized_identity TEXT NOT NULL UNIQUE,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    last_scanned_at_ms INTEGER,
                    scan_count INTEGER NOT NULL DEFAULT 0 CHECK (scan_count >= 0),
                    last_used_at_ms INTEGER,
                    use_count INTEGER NOT NULL DEFAULT 0 CHECK (use_count >= 0),
                    probation_until_ms INTEGER,
                    version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0)
                 );
                 CREATE TABLE IF NOT EXISTS memory_metadata (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    next_id INTEGER NOT NULL CHECK (next_id > 0)
                 );",
            )
            .map_err(sqlite_write_error)?;
        if schema_version < 2 {
            transaction
                .execute_batch(
                    "ALTER TABLE memories ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}';",
                )
                .map_err(sqlite_write_error)?;
        }
        let maximum_id = transaction
            .query_row("SELECT COALESCE(MAX(id), 0) FROM memories", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(sqlite_error)?;
        let next_id = maximum_id
            .checked_add(1)
            .ok_or(MemoryError::StorageCapacity)?;
        transaction
            .execute(
                "INSERT INTO memory_metadata (id, next_id) VALUES (?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET next_id = excluded.next_id
                 WHERE memory_metadata.next_id < excluded.next_id",
                params![ALLOCATOR_ROW_ID, next_id],
            )
            .map_err(sqlite_write_error)?;
        transaction
            .execute_batch(INSTALL_ALLOCATOR_TRIGGER)
            .map_err(sqlite_write_error)?;
        if schema_version < 3 {
            transaction.execute("UPDATE memories SET metadata = json_set(metadata, '$.ownership_id', lower(hex(randomblob(16)))) WHERE json_extract(metadata, '$.ownership_id') IS NULL", []).map_err(sqlite_write_error)?;
        }
        if schema_version < SCHEMA_VERSION {
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(sqlite_write_error)?;
        }
        transaction.commit().map_err(sqlite_write_error)?;
        Ok(connection)
    }

    async fn sync_local_snapshot(
        &self,
        memories: Vec<MemoryRecord>,
        now_ms: i64,
    ) -> Result<SyncReport, MemoryError> {
        let store = self.clone();
        run_local(move || store.sync_local_snapshot_blocking(&memories, now_ms)).await
    }

    fn sync_local_snapshot_blocking(
        &self,
        memories: &[MemoryRecord],
        now_ms: i64,
    ) -> Result<SyncReport, MemoryError> {
        crate::store::validate_authored_snapshot(memories, &self.limits)?;

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        prune_expired(&transaction, now_ms)?;
        let existing = load_all(&transaction, &self.limits)?;
        let previous = existing
            .iter()
            .cloned()
            .map(|memory| (memory.id, MemoryRecord::from(memory)))
            .collect::<HashMap<_, _>>();
        let incoming_ids = memories
            .iter()
            .map(|memory| memory.key.id)
            .collect::<HashSet<_>>();
        transaction
            .execute_batch("DROP TRIGGER memory_id_allocator")
            .map_err(sqlite_write_error)?;
        transaction
            .execute("DELETE FROM memories", [])
            .map_err(sqlite_write_error)?;
        let mut report = SyncReport {
            deleted: existing
                .iter()
                .filter(|memory| !incoming_ids.contains(&memory.id))
                .count(),
            ..SyncReport::default()
        };
        for memory in memories {
            observe_id(&transaction, memory.key.id)?;
            match previous.get(&memory.key.id) {
                Some(previous) if previous == memory => report.unchanged += 1,
                Some(_) => report.replaced += 1,
                None => report.inserted += 1,
            }
            transaction.execute(
                "INSERT INTO memories (id, content, normalized_identity, created_at_ms, updated_at_ms, last_scanned_at_ms, scan_count, last_used_at_ms, use_count, probation_until_ms, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![memory.key.id, memory.content, memory.metadata.identity(&memory.content), memory.created_at_ms, memory.updated_at_ms, memory.last_scanned_at_ms, memory.scan_count as i64, memory.last_used_at_ms, memory.use_count as i64, memory.probation_until_ms, memory.key.version as i64],
            ).map_err(sqlite_write_error)?;
            let mut metadata = memory.metadata.clone();
            if metadata.ownership_id.is_none() {
                metadata.ownership_id = Some(new_ownership(&transaction)?);
            }
            save_metadata(&transaction, memory.key.id, &metadata)?;
        }
        transaction
            .execute_batch(INSTALL_ALLOCATOR_TRIGGER)
            .map_err(sqlite_write_error)?;
        transaction.commit().map_err(sqlite_write_error)?;
        Ok(report)
    }

    async fn export_local_page(
        &self,
        cursor: Option<ExportCursor>,
        limit: usize,
        now_ms: i64,
    ) -> Result<(Vec<MemoryRecord>, Option<ExportCursor>), MemoryError> {
        let mut records = self.list(now_ms).await?;
        let after = cursor.map_or(0, |cursor| cursor.id);
        records.retain(|record| record.key.id > after);
        let limit = limit.clamp(1, protocol::MAX_EXPORT_PAGE_RECORDS);
        let has_more = records.len() > limit;
        records.truncate(limit);
        let next = has_more.then(|| ExportCursor {
            namespace: String::new(),
            id: records.last().expect("non-empty limited page").key.id,
        });
        Ok((records, next))
    }
}

impl MemoryStore for LocalMemoryStore {
    async fn read_scoped(
        &self,
        ids: &[i64],
        keys: &[MemoryKey],
        repository: Option<&str>,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let references = keys
            .iter()
            .filter(|key| key.is_local())
            .map(|key| (key.id, Some(key.version)))
            .chain(ids.iter().map(|id| (*id, None)))
            .collect::<Vec<_>>();
        let scope = protocol::ScanScope {
            repository: repository.map(str::to_owned),
        };
        let store = self.clone();
        run_local(move || store.read_filtered(&references, current_time_ms(), Some(&scope))).await
    }
    async fn lesson_page(
        &self,
        query: &crate::LessonQuery,
        after: i64,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let query = query.clone();
        let store = self.clone();
        run_local(move || {
            // The local corpus is itself bounded; this query does not use the shared UI contract.
            Ok(store
                .list_local(current_time_ms())?
                .into_iter()
                .filter(|record| record.key.id > after && query.matches(record))
                .take(protocol::MAX_EXPORT_PAGE_RECORDS)
                .collect())
        })
        .await
    }

    fn scan(
        &self,
        query: &str,
        limit: usize,
    ) -> impl Future<Output = Result<MemoryScan, MemoryError>> + Send {
        LocalMemoryStore::scan(self, query, limit, current_time_ms())
    }
    async fn scan_scoped(
        &self,
        query: &str,
        limit: usize,
        repository: Option<&str>,
    ) -> Result<MemoryScan, MemoryError> {
        let store = self.clone();
        let query = query.to_owned();
        let repository = repository.map(str::to_owned);
        run_local(move || {
            store.scan_filtered(
                &query,
                limit,
                current_time_ms(),
                Some(&protocol::ScanScope { repository }),
            )
        })
        .await
    }
    fn read(
        &self,
        ids: &[i64],
        keys: &[MemoryKey],
    ) -> impl Future<Output = Result<Vec<MemoryRecord>, MemoryError>> + Send {
        LocalMemoryStore::read(self, ids, keys, current_time_ms())
    }
    fn list(&self) -> impl Future<Output = Result<Vec<MemoryRecord>, MemoryError>> + Send {
        LocalMemoryStore::list(self, current_time_ms())
    }
    fn put(
        &self,
        content: &str,
        replacement: Option<MemoryKey>,
    ) -> impl Future<Output = Result<MemoryRecord, MemoryError>> + Send {
        LocalMemoryStore::put(self, content, replacement, current_time_ms())
    }
    async fn put_with_metadata(
        &self,
        content: &str,
        metadata: &crate::MemoryMetadata,
        replacement: Option<MemoryKey>,
    ) -> Result<MemoryRecord, MemoryError> {
        if replacement.as_ref().is_some_and(|key| !key.is_local()) {
            return Err(MemoryError::RemoteReadOnly);
        }
        let store = self.clone();
        let content = content.to_owned();
        let metadata = metadata.clone();
        run_local(move || {
            store.put_metadata_local(&content, &metadata, replacement, current_time_ms())
        })
        .await
    }
    fn delete(&self, key: MemoryKey) -> impl Future<Output = Result<(), MemoryError>> + Send {
        LocalMemoryStore::delete(self, key)
    }
    fn sync(
        &self,
        memories: &[MemoryRecord],
    ) -> impl Future<Output = Result<SyncReport, MemoryError>> + Send {
        let store = self.clone();
        let memories = memories.to_vec();
        let now_ms = current_time_ms();
        async move { store.sync_local_snapshot(memories, now_ms).await }
    }
    fn export_page(
        &self,
        _namespaces: Option<&[String]>,
        cursor: Option<&ExportCursor>,
        limit: usize,
    ) -> impl Future<Output = Result<(Vec<MemoryRecord>, Option<ExportCursor>), MemoryError>> + Send
    {
        let store = self.clone();
        let cursor = cursor.cloned();
        let now_ms = current_time_ms();
        async move { store.export_local_page(cursor, limit, now_ms).await }
    }
}

async fn run_local<T>(
    operation: impl FnOnce() -> Result<T, MemoryError> + Send + 'static,
) -> Result<T, MemoryError>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|source| MemoryError::backend(LocalStoreError::Task(source)))?
}

struct Totals {
    records: u64,
    content_bytes: u64,
}

fn totals(transaction: &Transaction<'_>) -> Result<Totals, MemoryError> {
    transaction
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(length(CAST(content AS BLOB))), 0) FROM memories",
            [],
            |row| {
                Ok(Totals {
                    records: row.get::<_, i64>(0)? as u64,
                    content_bytes: row.get::<_, i64>(1)? as u64,
                })
            },
        )
        .map_err(sqlite_error)
}

fn identity_exists(
    transaction: &Transaction<'_>,
    normalized_identity: &str,
    excluded_id: Option<i64>,
) -> Result<bool, MemoryError> {
    transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM memories
                WHERE normalized_identity = ?1 AND (?2 IS NULL OR id != ?2)
             )",
            params![normalized_identity, excluded_id],
            |row| row.get(0),
        )
        .map_err(sqlite_error)
}

fn prune_expired(transaction: &Transaction<'_>, now_ms: i64) -> Result<(), MemoryError> {
    transaction
        .execute(
            "DELETE FROM memories
             WHERE probation_until_ms IS NOT NULL
               AND probation_until_ms <= ?1
               AND use_count = 0",
            [now_ms],
        )
        .map_err(sqlite_error)?;
    Ok(())
}

fn allocate_id(transaction: &Transaction<'_>) -> Result<i64, MemoryError> {
    let recorded = transaction
        .query_row(
            "SELECT next_id FROM memory_metadata WHERE id = ?1",
            [ALLOCATOR_ROW_ID],
            |row| row.get::<_, i64>(0),
        )
        .map_err(sqlite_error)?;
    let maximum_id = transaction
        .query_row("SELECT COALESCE(MAX(id), 0) FROM memories", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(sqlite_error)?;
    let id = recorded.max(
        maximum_id
            .checked_add(1)
            .ok_or(MemoryError::StorageCapacity)?,
    );
    id.checked_add(1).ok_or(MemoryError::StorageCapacity)?;
    Ok(id)
}

fn observe_id(transaction: &Transaction<'_>, id: i64) -> Result<(), MemoryError> {
    let next_id = id.checked_add(1).ok_or(MemoryError::StorageCapacity)?;
    transaction
        .execute(
            "UPDATE memory_metadata SET next_id = MAX(next_id, ?1) WHERE id = ?2",
            params![next_id, ALLOCATOR_ROW_ID],
        )
        .map_err(sqlite_write_error)?;
    Ok(())
}

fn new_ownership(transaction: &Transaction<'_>) -> Result<String, MemoryError> {
    transaction
        .query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))
        .map_err(sqlite_error)
}

fn save_metadata(
    transaction: &Transaction<'_>,
    id: i64,
    metadata: &crate::MemoryMetadata,
) -> Result<(), MemoryError> {
    transaction
        .execute(
            "UPDATE memories SET metadata = ?1 WHERE id = ?2",
            params![
                serde_json::to_string(metadata).map_err(MemoryError::backend)?,
                id
            ],
        )
        .map_err(sqlite_write_error)?;
    Ok(())
}

fn load_all(
    transaction: &Transaction<'_>,
    limits: &MemoryLimits,
) -> Result<Vec<StoredMemory>, MemoryError> {
    let mut statement = transaction
        .prepare(
            "SELECT id, content, created_at_ms, updated_at_ms,
                    last_scanned_at_ms, scan_count, last_used_at_ms, use_count,
                    probation_until_ms, version, metadata
             FROM memories
             ORDER BY id",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map([], row_to_memory)
        .map_err(sqlite_error)?;
    rows.map(|row| validate_loaded(row.map_err(sqlite_error)?, limits))
        .collect()
}

fn load_one(
    transaction: &Transaction<'_>,
    id: i64,
    limits: &MemoryLimits,
) -> Result<Option<StoredMemory>, MemoryError> {
    transaction
        .query_row(
            "SELECT id, content, created_at_ms, updated_at_ms,
                    last_scanned_at_ms, scan_count, last_used_at_ms, use_count,
                    probation_until_ms, version, metadata
             FROM memories
             WHERE id = ?1",
            [id],
            row_to_memory,
        )
        .optional()
        .map_err(sqlite_error)?
        .map(|memory| validate_loaded(memory, limits))
        .transpose()
}

fn validate_loaded(
    memory: StoredMemory,
    limits: &MemoryLimits,
) -> Result<StoredMemory, MemoryError> {
    MemoryRecord::from(memory.clone()).validate(MemoryRecordScope::Local, limits)?;
    Ok(memory)
}

fn row_to_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMemory> {
    Ok(StoredMemory {
        namespace: None,
        metadata: serde_json::from_str(&row.get::<_, String>(10)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                10,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        id: row.get(0)?,
        content: row.get(1)?,
        created_at_ms: row.get(2)?,
        updated_at_ms: row.get(3)?,
        last_scanned_at_ms: row.get(4)?,
        scan_count: row.get::<_, i64>(5)? as u64,
        last_used_at_ms: row.get(6)?,
        use_count: row.get::<_, i64>(7)? as u64,
        probation_until_ms: row.get(8)?,
        version: row.get::<_, i64>(9)? as u64,
    })
}

fn distinct_ids(ids: &[i64]) -> Vec<i64> {
    let mut seen = HashSet::new();
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

fn sqlite_error(source: rusqlite::Error) -> MemoryError {
    let retryable = matches!(
        &source,
        rusqlite::Error::SqliteFailure(error, _)
            if matches!(error.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    );
    if retryable {
        MemoryError::unavailable(source)
    } else {
        MemoryError::backend(source)
    }
}

fn sqlite_write_error(source: rusqlite::Error) -> MemoryError {
    match &source {
        rusqlite::Error::SqliteFailure(error, _) if error.code == ErrorCode::DiskFull => {
            MemoryError::StorageCapacity
        }
        _ => sqlite_error(source),
    }
}

#[cfg(unix)]
fn prepare_private_path(path: &Path) -> Result<PathBuf, MemoryError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(MemoryError::InvalidMetadata)?;
    let filename = path.file_name().ok_or(MemoryError::InvalidMetadata)?;
    match fs::symlink_metadata(parent) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            match builder.create(parent) {
                Ok(()) => {}
                Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    return Err(MemoryError::backend(LocalStoreError::Directory(source)));
                }
            }
        }
        Err(error) => return Err(MemoryError::backend(error)),
    }
    let owner = rustix::process::geteuid().as_raw();
    let parent_metadata = fs::symlink_metadata(parent).map_err(MemoryError::backend)?;
    if !parent_metadata.is_dir()
        || parent_metadata.file_type().is_symlink()
        || parent_metadata.uid() != owner
    {
        return Err(MemoryError::InvalidMetadata);
    }
    if parent_metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(MemoryError::InvalidMetadata);
    }
    let canonical_path = fs::canonicalize(parent)
        .map_err(MemoryError::backend)?
        .join(filename);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&canonical_path)
        .map_err(MemoryError::backend)?;
    let metadata = file.metadata().map_err(MemoryError::backend)?;
    if !metadata.is_file() || metadata.uid() != owner {
        return Err(MemoryError::InvalidMetadata);
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(MemoryError::backend)?;
    Ok(canonical_path)
}

#[cfg(not(unix))]
fn prepare_private_path(_path: &Path) -> Result<PathBuf, MemoryError> {
    Err(MemoryError::InvalidMetadata)
}

#[cfg(unix)]
fn open_private_database(path: &Path) -> Result<Connection, MemoryError> {
    use rusqlite::OpenFlags;

    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(sqlite_error)
}

#[cfg(not(unix))]
fn open_private_database(_path: &Path) -> Result<Connection, MemoryError> {
    Err(MemoryError::InvalidMetadata)
}

#[cfg(test)]
mod allocator_tests {
    use super::*;

    #[test]
    fn allocation_reconciles_rows_inserted_by_a_legacy_writer() {
        let directory = tempfile::tempdir().unwrap();
        let store = LocalMemoryStore::new(directory.path().join("memory/v1.sqlite3"));
        let mut connection = store.open().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        transaction
            .execute(
                "INSERT INTO memories (
                    id, content, normalized_identity, created_at_ms, updated_at_ms, version
                 ) VALUES (1, 'legacy', 'legacy', 1, 1, 1)",
                [],
            )
            .unwrap();

        assert_eq!(allocate_id(&transaction).unwrap(), 2);
    }

    #[test]
    fn legacy_writers_cannot_reuse_retired_ids() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("memory/v1.sqlite3");
        let store = LocalMemoryStore::new(&path);
        let memory = store.put_local("retired", None, 1).unwrap();
        store.delete_local(memory.key).unwrap();

        let legacy = Connection::open(path).unwrap();
        assert!(
            legacy
                .execute(
                    "INSERT INTO memories (
                        content, normalized_identity, created_at_ms, updated_at_ms, version
                     ) VALUES ('legacy', 'legacy', 2, 2, 1)",
                    [],
                )
                .is_err()
        );
    }
}
