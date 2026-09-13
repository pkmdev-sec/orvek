//! Append-only, branch-scoped context archives.
//!
//! Sources, manifests, and pages are retained conservatively. Session removal must
//! account for these independent branch and manifest references before adding GC.
//! Application-owned payload buffers zeroize; SQLite, PNG decoder, and digest
//! internals are dependency-owned and outside this crate's zeroization guarantee.

use super::storage::SessionStorage;
use nanocodex::{
    agent::session::{SessionId, compaction::ContextCheckpoint},
    oai::responses::ResponseItemId,
};
use png::{Decoder, Limits};
use rusqlite::{Connection, ErrorCode, OptionalExtension, Row, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fmt, io::Cursor};
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PAGE_SET_BYTES: usize = 32 * 1024 * 1024;
const MAX_PAGES: usize = 256;
const MAX_BRANCH_DEPTH: usize = 256;
const MAX_RECORDS: usize = 100_000;
const MAX_VALIDATION_BYTES: usize = 256 * 1024 * 1024;
const MAX_ITEM_ID_BYTES: usize = 512;
const MAX_PAGE_EDGE: u32 = 1568;
const MAX_DECODED_PAGE_BYTES: usize = 1568 * 1568 * 4;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS context_resume_backups(
    session_id TEXT PRIMARY KEY,
    state_zstd BLOB NOT NULL,
    state_hash TEXT NOT NULL,
    replaced_state_zstd BLOB
) STRICT;
CREATE TABLE IF NOT EXISTS context_archive_branches(
    branch TEXT PRIMARY KEY,
    runtime_session TEXT NOT NULL,
    parent_branch TEXT REFERENCES context_archive_branches(branch),
    parent_sequence INTEGER CHECK(parent_sequence >= 0),
    base_generation INTEGER NOT NULL CHECK(base_generation >= 0),
    latest_sequence INTEGER NOT NULL DEFAULT 0 CHECK(latest_sequence >= 0),
    latest_generation INTEGER NOT NULL CHECK(latest_generation >= 0),
    CHECK((parent_branch IS NULL) = (parent_sequence IS NULL))
) STRICT;
CREATE TABLE IF NOT EXISTS context_archive_records(
    branch TEXT NOT NULL REFERENCES context_archive_branches(branch),
    sequence INTEGER NOT NULL CHECK(sequence > 0),
    model_generation INTEGER NOT NULL CHECK(model_generation >= 0),
    item_id TEXT NOT NULL,
    tool_success INTEGER CHECK(tool_success IN (0, 1)),
    original BLOB NOT NULL,
    original_hash TEXT NOT NULL,
    visible BLOB NOT NULL,
    visible_hash TEXT NOT NULL,
    record_hash TEXT NOT NULL,
    PRIMARY KEY(branch, sequence),
    UNIQUE(branch, item_id)
) STRICT;
CREATE TABLE IF NOT EXISTS context_archive_pages(
    hash TEXT PRIMARY KEY,
    width INTEGER NOT NULL CHECK(width > 0),
    height INTEGER NOT NULL CHECK(height > 0),
    png BLOB NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS context_archive_manifests(
    id TEXT PRIMARY KEY,
    branch TEXT NOT NULL REFERENCES context_archive_branches(branch),
    sequence INTEGER NOT NULL CHECK(sequence >= 0),
    model_generation INTEGER NOT NULL CHECK(model_generation >= 0),
    encoded BLOB NOT NULL,
    page_count INTEGER NOT NULL CHECK(page_count > 0)
) STRICT;
CREATE TABLE IF NOT EXISTS context_archive_manifest_pages(
    manifest_id TEXT NOT NULL REFERENCES context_archive_manifests(id),
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    page_hash TEXT NOT NULL REFERENCES context_archive_pages(hash),
    PRIMARY KEY(manifest_id, ordinal)
) STRICT;
";

#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct ArchiveRecord {
    #[zeroize(skip)]
    pub(crate) checkpoint: ContextCheckpoint,
    #[zeroize(skip)]
    pub(crate) item_id: ResponseItemId,
    #[zeroize(skip)]
    pub(crate) tool_success: Option<bool>,
    pub(crate) original: Zeroizing<Vec<u8>>,
    pub(crate) visible: Zeroizing<Vec<u8>>,
}

/// Retrieved records report their source branch, sequence, and model generation.
pub(crate) type ArchivedItem = ArchiveRecord;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum ArchiveValidation {
    Complete,
    NativeRecovery,
}

impl fmt::Debug for ArchiveRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ArchiveRecord([REDACTED])")
    }
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct ArchivePage {
    #[zeroize(skip)]
    pub(crate) hash: String,
    #[zeroize(skip)]
    pub(crate) width: u32,
    #[zeroize(skip)]
    pub(crate) height: u32,
    pub(crate) png: Zeroizing<Vec<u8>>,
}

impl fmt::Debug for ArchivePage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ArchivePage([REDACTED])")
    }
}

#[derive(Debug, Error)]
pub(crate) enum ArchiveError {
    #[error("context archive schema is unavailable")]
    Unavailable,
    #[error("context archive is missing {kind}")]
    Missing { kind: &'static str },
    #[error("context archive is invalid: {reason}")]
    Invalid { reason: &'static str },
    #[error("context archive conflicts with an existing {kind}")]
    Conflict { kind: &'static str },
    #[error("context archive exceeded the {resource} limit")]
    Limit { resource: &'static str },
    #[error("context archive database operation {operation} failed: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: ArchiveDatabaseError,
    },
}

/// SQLite messages can originate in database triggers. Preserve the useful error
/// code without retaining or exposing arbitrary database-provided message text.
#[derive(Debug, Error)]
#[error("SQLite error code {code:?}")]
pub(crate) struct ArchiveDatabaseError {
    code: Option<ErrorCode>,
}

fn database(operation: &'static str, error: rusqlite::Error) -> ArchiveError {
    ArchiveError::Database {
        operation,
        source: ArchiveDatabaseError {
            code: error.sqlite_error_code(),
        },
    }
}

fn invalid(reason: &'static str) -> ArchiveError {
    ArchiveError::Invalid { reason }
}

impl SessionStorage {
    pub(crate) fn archive_open(
        &mut self,
        runtime_session: SessionId,
        inherited: Option<&ContextCheckpoint>,
        validation: ArchiveValidation,
    ) -> Result<ContextCheckpoint, ArchiveError> {
        require_schema(&self.connection)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| database("begin branch", error))?;
        if let Some(checkpoint) = inherited {
            validate_archive_with(&transaction, checkpoint, validation)?;
        }
        let branch = SessionId::new();
        let generation = inherited.map_or(0, |checkpoint| checkpoint.model_generation);
        transaction.execute(
            "INSERT INTO context_archive_branches
             (branch, runtime_session, parent_branch, parent_sequence, base_generation, latest_generation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![branch.to_string(), runtime_session.to_string(),
                inherited.map(|checkpoint| checkpoint.branch.to_string()),
                inherited.map(|checkpoint| sql_number(checkpoint.sequence)).transpose()?,
                sql_number(generation)?],
        ).map_err(|error| database("create branch", error))?;
        transaction
            .commit()
            .map_err(|error| database("commit branch", error))?;
        Ok(ContextCheckpoint {
            branch,
            sequence: 0,
            model_generation: generation,
            manifest: inherited.and_then(|checkpoint| checkpoint.manifest.clone()),
        })
    }

    pub(crate) fn archive_record(&mut self, record: &ArchiveRecord) -> Result<(), ArchiveError> {
        require_schema(&self.connection)?;
        validate_record_input(record)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| database("begin source", error))?;
        let branch = load_branch(&transaction, record.checkpoint.branch)?;
        if record.checkpoint.sequence <= branch.latest_sequence {
            let existing = load_record_at(
                &transaction,
                record.checkpoint.branch,
                record.checkpoint.sequence,
            )?
            .ok_or(ArchiveError::Missing {
                kind: "source sequence",
            })?;
            if existing.item_id != record.item_id
                || existing.checkpoint.model_generation != record.checkpoint.model_generation
                || existing.tool_success != record.tool_success
                || existing.original != record.original
                || existing.visible != record.visible
            {
                return Err(ArchiveError::Conflict {
                    kind: "source sequence",
                });
            }
            return Ok(());
        }
        if record.checkpoint.sequence
            != branch
                .latest_sequence
                .checked_add(1)
                .ok_or(ArchiveError::Limit {
                    resource: "source sequence",
                })?
            || record.checkpoint.model_generation < branch.latest_generation
        {
            return Err(ArchiveError::Conflict {
                kind: "source sequence or generation",
            });
        }
        let previous = ContextCheckpoint {
            branch: record.checkpoint.branch,
            sequence: branch.latest_sequence,
            model_generation: record.checkpoint.model_generation,
            manifest: None,
        };
        let previous_scopes = scopes(&transaction, &previous)?;
        if previous_scopes
            .iter()
            .map(|scope| scope.sequence)
            .sum::<u64>()
            >= MAX_RECORDS as u64
        {
            return Err(ArchiveError::Limit {
                resource: "source count",
            });
        }
        let existing_bytes = previous_scopes.iter().map(|scope| scope.bytes).sum::<u64>();
        if existing_bytes + (record.original.len() + record.visible.len()) as u64
            > MAX_VALIDATION_BYTES as u64
        {
            return Err(ArchiveError::Limit {
                resource: "source validation bytes",
            });
        }
        for scope in previous_scopes {
            let exists: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM context_archive_records
                 WHERE branch = ?1 AND sequence <= ?2 AND item_id = ?3)",
                    params![
                        scope.branch.to_string(),
                        sql_number(scope.sequence)?,
                        record.item_id.as_str()
                    ],
                    |row| row.get(0),
                )
                .map_err(|error| database("check source identity", error))?;
            if exists {
                return Err(ArchiveError::Conflict {
                    kind: "source item identity",
                });
            }
        }
        transaction
            .execute(
                "INSERT INTO context_archive_records
             (branch, sequence, model_generation, item_id, tool_success,
              original, original_hash, visible, visible_hash, record_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    record.checkpoint.branch.to_string(),
                    sql_number(record.checkpoint.sequence)?,
                    sql_number(record.checkpoint.model_generation)?,
                    record.item_id.as_str(),
                    record.tool_success,
                    &*record.original,
                    hash(&record.original),
                    &*record.visible,
                    hash(&record.visible),
                    record_hash(record)
                ],
            )
            .map_err(|error| database("insert source", error))?;
        transaction
            .execute(
                "UPDATE context_archive_branches SET latest_sequence = ?2, latest_generation = ?3
             WHERE branch = ?1",
                params![
                    record.checkpoint.branch.to_string(),
                    sql_number(record.checkpoint.sequence)?,
                    sql_number(record.checkpoint.model_generation)?
                ],
            )
            .map_err(|error| database("advance source sequence", error))?;
        transaction
            .commit()
            .map_err(|error| database("commit source", error))
    }

    pub(crate) fn archive_successful_results(
        &self,
        checkpoint: &ContextCheckpoint,
    ) -> Result<Vec<ResponseItemId>, ArchiveError> {
        require_schema(&self.connection)?;
        let scope = scopes(&self.connection, checkpoint)?;
        let Some(cutoff) = checkpoint.model_generation.checked_sub(2) else {
            return Ok(Vec::new());
        };
        let mut result = Vec::new();
        let mut bytes = 0usize;
        for scope in scope.iter().rev() {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT branch, sequence, model_generation, item_id, tool_success,
                        original, original_hash, visible, visible_hash, record_hash
                 FROM context_archive_records WHERE branch = ?1 AND sequence <= ?2
                 AND tool_success = 1 AND model_generation <= ?3 ORDER BY sequence",
                )
                .map_err(|error| database("prepare successful sources", error))?;
            let mut rows = statement
                .query(params![
                    scope.branch.to_string(),
                    sql_number(scope.sequence)?,
                    sql_number(cutoff)?
                ])
                .map_err(|error| database("query successful sources", error))?;
            while let Some(row) = rows
                .next()
                .map_err(|error| database("read successful source", error))?
            {
                if result.len() == MAX_RECORDS {
                    return Err(ArchiveError::Limit {
                        resource: "source count",
                    });
                }
                let record = read_record(row)?;
                bytes += record.original.len() + record.visible.len();
                if bytes > MAX_VALIDATION_BYTES {
                    return Err(ArchiveError::Limit {
                        resource: "source validation bytes",
                    });
                }
                result.push(record.item_id.clone());
            }
        }
        Ok(result)
    }

    pub(crate) fn archive_read(
        &self,
        checkpoint: &ContextCheckpoint,
        item_id: &str,
    ) -> Result<ArchivedItem, ArchiveError> {
        require_schema(&self.connection)?;
        validate_item_id(item_id)?;
        for scope in scopes(&self.connection, checkpoint)? {
            let sequence = self
                .connection
                .query_row(
                    "SELECT sequence FROM context_archive_records
                 WHERE branch = ?1 AND sequence <= ?2 AND item_id = ?3",
                    params![
                        scope.branch.to_string(),
                        sql_number(scope.sequence)?,
                        item_id
                    ],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(|error| database("find source", error))?;
            if let Some(sequence) = sequence {
                return load_record_at(&self.connection, scope.branch, unsigned(sequence)?)?
                    .ok_or(ArchiveError::Missing { kind: "source" });
            }
        }
        Err(ArchiveError::Missing {
            kind: "scoped source",
        })
    }

    pub(crate) fn archive_save_manifest(
        &mut self,
        id: &str,
        checkpoint: &ContextCheckpoint,
        encoded: &[u8],
        pages: &[ArchivePage],
    ) -> Result<(), ArchiveError> {
        require_schema(&self.connection)?;
        validate_blob_hash(id, encoded, MAX_MANIFEST_BYTES, "manifest bytes")?;
        if pages.is_empty() || pages.len() > MAX_PAGES {
            return Err(ArchiveError::Limit {
                resource: "manifest page count",
            });
        }
        let mut total_bytes = 0usize;
        for page in pages {
            total_bytes = total_bytes
                .checked_add(page.png.len())
                .ok_or(ArchiveError::Limit {
                    resource: "manifest page bytes",
                })?;
            if total_bytes > MAX_PAGE_SET_BYTES {
                return Err(ArchiveError::Limit {
                    resource: "manifest page bytes",
                });
            }
            validate_page(page)?;
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| database("begin manifest", error))?;
        let scope = scopes(&transaction, checkpoint)?;
        validate_sources(&transaction, &scope)?;
        for page in pages {
            let exists: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM context_archive_pages WHERE hash = ?1)",
                    [&page.hash],
                    |row| row.get(0),
                )
                .map_err(|error| database("find page", error))?;
            if exists {
                let stored = load_page(&transaction, &page.hash)?;
                if stored.width != page.width
                    || stored.height != page.height
                    || stored.png != page.png
                {
                    return Err(ArchiveError::Conflict { kind: "page" });
                }
            } else {
                transaction.execute(
                    "INSERT INTO context_archive_pages(hash, width, height, png) VALUES (?1, ?2, ?3, ?4)",
                    params![page.hash, page.width, page.height, &*page.png],
                ).map_err(|error| database("insert page", error))?;
            }
        }
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM context_archive_manifests WHERE id = ?1)",
                [id],
                |row| row.get(0),
            )
            .map_err(|error| database("find manifest", error))?;
        if exists {
            let stored = load_manifest(&transaction, id)?;
            let references = manifest_pages(&transaction, id, stored.page_count)?;
            if stored.checkpoint.branch != checkpoint.branch
                || stored.checkpoint.sequence != checkpoint.sequence
                || stored.checkpoint.model_generation != checkpoint.model_generation
                || stored.encoded.as_slice() != encoded
                || references
                    .iter()
                    .map(String::as_str)
                    .ne(pages.iter().map(|page| page.hash.as_str()))
            {
                return Err(ArchiveError::Conflict { kind: "manifest" });
            }
        } else {
            transaction.execute(
                "INSERT INTO context_archive_manifests(id, branch, sequence, model_generation, encoded, page_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, checkpoint.branch.to_string(), sql_number(checkpoint.sequence)?,
                    sql_number(checkpoint.model_generation)?, encoded, sql_number(pages.len())?],
            ).map_err(|error| database("insert manifest", error))?;
            for (ordinal, page) in pages.iter().enumerate() {
                transaction.execute(
                    "INSERT INTO context_archive_manifest_pages(manifest_id, ordinal, page_hash) VALUES (?1, ?2, ?3)",
                    params![id, sql_number(ordinal)?, page.hash],
                ).map_err(|error| database("reference page", error))?;
            }
        }
        transaction
            .commit()
            .map_err(|error| database("commit manifest", error))
    }

    pub(crate) fn archive_load_manifest(
        &self,
        id: &str,
        validation: ArchiveValidation,
    ) -> Result<Zeroizing<Vec<u8>>, ArchiveError> {
        require_schema(&self.connection)?;
        let mut manifest = load_manifest(&self.connection, id)?;
        if validation == ArchiveValidation::Complete {
            validate_manifest_pages(&self.connection, id, manifest.page_count)?;
        }
        Ok(Zeroizing::new(std::mem::take(&mut *manifest.encoded)))
    }

    pub(crate) fn archive_load_page(&self, hash: &str) -> Result<Zeroizing<Vec<u8>>, ArchiveError> {
        require_schema(&self.connection)?;
        let mut page = load_page(&self.connection, hash)?;
        Ok(Zeroizing::new(std::mem::take(&mut *page.png)))
    }

    pub(crate) fn archive_validate(
        &self,
        checkpoint: &ContextCheckpoint,
    ) -> Result<(), ArchiveError> {
        require_schema(&self.connection)?;
        validate_archive(&self.connection, checkpoint)
    }

    pub(crate) fn archive_validate_native(
        &self,
        checkpoint: &ContextCheckpoint,
    ) -> Result<(), ArchiveError> {
        require_schema(&self.connection)?;
        validate_archive_with(
            &self.connection,
            checkpoint,
            ArchiveValidation::NativeRecovery,
        )
    }
}

struct Branch {
    parent: Option<SessionId>,
    parent_sequence: Option<u64>,
    base_generation: u64,
    latest_sequence: u64,
    latest_generation: u64,
}

struct Scope {
    branch: SessionId,
    sequence: u64,
    bytes: u64,
}

fn require_schema(connection: &Connection) -> Result<(), ArchiveError> {
    let version: i32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| database("read archive version", error))?;
    if version != 3 {
        return Err(ArchiveError::Unavailable);
    }
    Ok(())
}

fn load_branch(connection: &Connection, branch: SessionId) -> Result<Branch, ArchiveError> {
    let mut statement = connection.prepare(
        "SELECT parent_branch, parent_sequence, base_generation, latest_sequence, latest_generation
         FROM context_archive_branches WHERE branch = ?1",
    ).map_err(|error| database("prepare branch", error))?;
    let mut rows = statement
        .query([branch.to_string()])
        .map_err(|error| database("query branch", error))?;
    let row = rows
        .next()
        .map_err(|error| database("read branch", error))?
        .ok_or(ArchiveError::Missing { kind: "branch" })?;
    let parent = match row
        .get_ref(0)
        .map_err(|error| database("read parent branch", error))?
    {
        rusqlite::types::ValueRef::Null => None,
        _ => Some(read_session_id(row, 0)?),
    };
    let branch = Branch {
        parent,
        parent_sequence: row
            .get::<_, Option<i64>>(1)
            .map_err(|error| database("read parent cutoff", error))?
            .map(unsigned)
            .transpose()?,
        base_generation: read_number(row, 2)?,
        latest_sequence: read_number(row, 3)?,
        latest_generation: read_number(row, 4)?,
    };
    if branch.parent.is_some() != branch.parent_sequence.is_some()
        || branch.latest_generation < branch.base_generation
    {
        return Err(invalid("branch metadata"));
    }
    Ok(branch)
}

fn scopes(
    connection: &Connection,
    checkpoint: &ContextCheckpoint,
) -> Result<Vec<Scope>, ArchiveError> {
    sql_number(checkpoint.sequence)?;
    sql_number(checkpoint.model_generation)?;
    let mut result = Vec::new();
    let mut visited = HashSet::new();
    let mut source_count = 0u64;
    let mut source_bytes = 0u64;
    let mut current = Some((checkpoint.branch, checkpoint.sequence));
    while let Some((branch_id, sequence)) = current {
        if result.len() == MAX_BRANCH_DEPTH {
            return Err(ArchiveError::Limit {
                resource: "branch depth",
            });
        }
        if !visited.insert(branch_id) {
            return Err(invalid("branch lineage cycle"));
        }
        let branch = load_branch(connection, branch_id)?;
        if sequence > branch.latest_sequence || checkpoint.model_generation < branch.base_generation
        {
            return Err(invalid("checkpoint cutoff"));
        }
        source_count = source_count
            .checked_add(sequence)
            .ok_or(ArchiveError::Limit {
                resource: "source count",
            })?;
        if source_count > MAX_RECORDS as u64 {
            return Err(ArchiveError::Limit {
                resource: "source count",
            });
        }
        let (count, generation, bytes): (i64, i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(MAX(model_generation), 0),
                        COALESCE(SUM(length(original) + length(visible)), 0)
                 FROM context_archive_records
             WHERE branch = ?1 AND sequence <= ?2",
                params![branch_id.to_string(), sql_number(sequence)?],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|error| database("validate source cutoff", error))?;
        if unsigned(count)? != sequence || unsigned(generation)? > checkpoint.model_generation {
            return Err(invalid("missing source sequence or invalid generation"));
        }
        let bytes = unsigned(bytes)?;
        source_bytes = source_bytes.checked_add(bytes).ok_or(ArchiveError::Limit {
            resource: "source validation bytes",
        })?;
        if source_bytes > MAX_VALIDATION_BYTES as u64 {
            return Err(ArchiveError::Limit {
                resource: "source validation bytes",
            });
        }
        result.push(Scope {
            branch: branch_id,
            sequence,
            bytes,
        });
        current = branch.parent.zip(branch.parent_sequence);
    }
    Ok(result)
}

pub(super) fn validate_archive(
    connection: &Connection,
    checkpoint: &ContextCheckpoint,
) -> Result<(), ArchiveError> {
    validate_archive_with(connection, checkpoint, ArchiveValidation::Complete)
}

fn validate_archive_with(
    connection: &Connection,
    checkpoint: &ContextCheckpoint,
    validation: ArchiveValidation,
) -> Result<(), ArchiveError> {
    let scope = scopes(connection, checkpoint)?;
    validate_sources(connection, &scope)?;
    if let Some(id) = &checkpoint.manifest {
        let manifest = load_manifest(connection, id)?;
        if !scope.iter().any(|scope| {
            scope.branch == manifest.checkpoint.branch
                && manifest.checkpoint.sequence <= scope.sequence
        }) || manifest.checkpoint.model_generation > checkpoint.model_generation
        {
            return Err(invalid("manifest is outside the checkpoint scope"));
        }
        if validation == ArchiveValidation::Complete {
            validate_manifest_pages(connection, id, manifest.page_count)?;
        }
    }
    Ok(())
}

fn validate_sources(connection: &Connection, scopes: &[Scope]) -> Result<(), ArchiveError> {
    let mut count = 0usize;
    let mut bytes = 0usize;
    let mut items = HashSet::new();
    for scope in scopes {
        let mut statement = connection
            .prepare(
                "SELECT branch, sequence, model_generation, item_id, tool_success,
                    original, original_hash, visible, visible_hash, record_hash
             FROM context_archive_records WHERE branch = ?1 AND sequence <= ?2 ORDER BY sequence",
            )
            .map_err(|error| database("prepare source validation", error))?;
        let mut rows = statement
            .query(params![
                scope.branch.to_string(),
                sql_number(scope.sequence)?
            ])
            .map_err(|error| database("query source validation", error))?;
        while let Some(row) = rows
            .next()
            .map_err(|error| database("read source validation", error))?
        {
            count += 1;
            if count > MAX_RECORDS {
                return Err(ArchiveError::Limit {
                    resource: "source count",
                });
            }
            let record = read_record(row)?;
            bytes = bytes
                .checked_add(record.original.len() + record.visible.len())
                .ok_or(ArchiveError::Limit {
                    resource: "source validation bytes",
                })?;
            if bytes > MAX_VALIDATION_BYTES {
                return Err(ArchiveError::Limit {
                    resource: "source validation bytes",
                });
            }
            if !items.insert(record.item_id.clone()) {
                return Err(invalid("duplicate scoped item identity"));
            }
        }
    }
    Ok(())
}

fn load_record_at(
    connection: &Connection,
    branch: SessionId,
    sequence: u64,
) -> Result<Option<ArchivedItem>, ArchiveError> {
    let mut statement = connection
        .prepare(
            "SELECT branch, sequence, model_generation, item_id, tool_success,
                original, original_hash, visible, visible_hash, record_hash
         FROM context_archive_records WHERE branch = ?1 AND sequence = ?2",
        )
        .map_err(|error| database("prepare source", error))?;
    let mut rows = statement
        .query(params![branch.to_string(), sql_number(sequence)?])
        .map_err(|error| database("query source", error))?;
    rows.next()
        .map_err(|error| database("read source", error))?
        .map(read_record)
        .transpose()
}

fn read_record(row: &Row<'_>) -> Result<ArchivedItem, ArchiveError> {
    let original = read_blob(row, 5, MAX_SOURCE_BYTES, "original source bytes")?;
    let visible = read_blob(row, 7, MAX_SOURCE_BYTES, "visible source bytes")?;
    validate_blob_hash(
        read_text(row, 6, 64)?,
        &original,
        MAX_SOURCE_BYTES,
        "original source bytes",
    )?;
    validate_blob_hash(
        read_text(row, 8, 64)?,
        &visible,
        MAX_SOURCE_BYTES,
        "visible source bytes",
    )?;
    let success: Option<i64> = row
        .get(4)
        .map_err(|error| database("read source outcome", error))?;
    let record = ArchiveRecord {
        checkpoint: ContextCheckpoint {
            branch: read_session_id(row, 0)?,
            sequence: read_number(row, 1)?,
            model_generation: read_number(row, 2)?,
            manifest: None,
        },
        item_id: ResponseItemId::from_server(read_text(row, 3, MAX_ITEM_ID_BYTES)?),
        tool_success: match success {
            None => None,
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => return Err(invalid("source outcome")),
        },
        original,
        visible,
    };
    validate_record_input(&record)?;
    if read_text(row, 9, 64)? != record_hash(&record) {
        return Err(invalid("source metadata hash"));
    }
    Ok(record)
}

fn validate_record_input(record: &ArchiveRecord) -> Result<(), ArchiveError> {
    validate_item_id(record.item_id.as_str())?;
    if record.checkpoint.sequence == 0 {
        return Err(invalid("zero source sequence"));
    }
    sql_number(record.checkpoint.sequence)?;
    sql_number(record.checkpoint.model_generation)?;
    if record.original.len() > MAX_SOURCE_BYTES || record.visible.len() > MAX_SOURCE_BYTES {
        return Err(ArchiveError::Limit {
            resource: "source bytes",
        });
    }
    Ok(())
}

fn validate_item_id(id: &str) -> Result<(), ArchiveError> {
    if id.is_empty() || id.len() > MAX_ITEM_ID_BYTES {
        return Err(ArchiveError::Limit {
            resource: "item identity",
        });
    }
    Ok(())
}

fn record_hash(record: &ArchiveRecord) -> String {
    let mut digest = Sha256::new();
    digest.update(b"tact-context-source-v1\0");
    digest.update(record.checkpoint.branch.as_uuid().as_bytes());
    digest.update(record.checkpoint.sequence.to_le_bytes());
    digest.update(record.checkpoint.model_generation.to_le_bytes());
    digest.update([match record.tool_success {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    }]);
    for bytes in [
        record.item_id.as_str().as_bytes(),
        record.original.as_slice(),
        record.visible.as_slice(),
    ] {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}

#[derive(Zeroize, ZeroizeOnDrop)]
struct StoredManifest {
    #[zeroize(skip)]
    checkpoint: ContextCheckpoint,
    #[zeroize(skip)]
    page_count: usize,
    encoded: Zeroizing<Vec<u8>>,
}

fn load_manifest(connection: &Connection, id: &str) -> Result<StoredManifest, ArchiveError> {
    validate_hash(id)?;
    let mut statement = connection
        .prepare(
            "SELECT branch, sequence, model_generation, encoded, page_count
         FROM context_archive_manifests WHERE id = ?1",
        )
        .map_err(|error| database("prepare manifest", error))?;
    let mut rows = statement
        .query([id])
        .map_err(|error| database("query manifest", error))?;
    let row = rows
        .next()
        .map_err(|error| database("read manifest", error))?
        .ok_or(ArchiveError::Missing { kind: "manifest" })?;
    let encoded = read_blob(row, 3, MAX_MANIFEST_BYTES, "manifest bytes")?;
    validate_blob_hash(id, &encoded, MAX_MANIFEST_BYTES, "manifest bytes")?;
    let page_count = read_size(row, 4)?;
    if page_count == 0 || page_count > MAX_PAGES {
        return Err(invalid("manifest page count"));
    }
    Ok(StoredManifest {
        checkpoint: ContextCheckpoint {
            branch: read_session_id(row, 0)?,
            sequence: read_number(row, 1)?,
            model_generation: read_number(row, 2)?,
            manifest: None,
        },
        page_count,
        encoded,
    })
}

fn manifest_pages(
    connection: &Connection,
    id: &str,
    count: usize,
) -> Result<Vec<String>, ArchiveError> {
    let mut statement = connection.prepare(
        "SELECT ordinal, page_hash FROM context_archive_manifest_pages WHERE manifest_id = ?1 ORDER BY ordinal",
    ).map_err(|error| database("prepare page references", error))?;
    let mut rows = statement
        .query([id])
        .map_err(|error| database("query page references", error))?;
    let mut pages = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| database("read page reference", error))?
    {
        let ordinal = read_size(row, 0)?;
        if ordinal != pages.len() || pages.len() >= count {
            return Err(invalid("manifest page references"));
        }
        let hash = read_text(row, 1, 64)?;
        validate_hash(hash)?;
        pages.push(hash.to_owned());
    }
    if pages.len() != count {
        return Err(ArchiveError::Missing {
            kind: "manifest page reference",
        });
    }
    Ok(pages)
}

fn validate_manifest_pages(
    connection: &Connection,
    id: &str,
    count: usize,
) -> Result<(), ArchiveError> {
    let mut total = 0usize;
    for hash in manifest_pages(connection, id, count)? {
        let page = load_page(connection, &hash)?;
        total += page.png.len();
        if total > MAX_PAGE_SET_BYTES {
            return Err(ArchiveError::Limit {
                resource: "manifest page bytes",
            });
        }
    }
    Ok(())
}

fn load_page(connection: &Connection, hash: &str) -> Result<ArchivePage, ArchiveError> {
    validate_hash(hash)?;
    let mut statement = connection
        .prepare("SELECT width, height, png FROM context_archive_pages WHERE hash = ?1")
        .map_err(|error| database("prepare page", error))?;
    let mut rows = statement
        .query([hash])
        .map_err(|error| database("query page", error))?;
    let row = rows
        .next()
        .map_err(|error| database("read page", error))?
        .ok_or(ArchiveError::Missing { kind: "page" })?;
    let page = ArchivePage {
        hash: hash.to_owned(),
        width: row
            .get(0)
            .map_err(|error| database("read page width", error))?,
        height: row
            .get(1)
            .map_err(|error| database("read page height", error))?,
        png: read_blob(row, 2, MAX_PAGE_BYTES, "page bytes")?,
    };
    validate_page(&page)?;
    Ok(page)
}

fn validate_page(page: &ArchivePage) -> Result<(), ArchiveError> {
    validate_blob_hash(&page.hash, &page.png, MAX_PAGE_BYTES, "page bytes")?;
    if page.width == 0
        || page.height == 0
        || page.width > MAX_PAGE_EDGE
        || page.height > MAX_PAGE_EDGE
    {
        return Err(invalid("page dimensions"));
    }
    let mut decoder = Decoder::new(Cursor::new(&*page.png));
    decoder.set_limits(Limits {
        bytes: MAX_DECODED_PAGE_BYTES,
    });
    let mut reader = decoder
        .read_info()
        .map_err(|_| invalid("page PNG header"))?;
    if reader.info().width != page.width
        || reader.info().height != page.height
        || reader.info().animation_control.is_some()
    {
        return Err(invalid("page PNG dimensions or animation"));
    }
    let length = reader
        .output_buffer_size()
        .filter(|length| *length <= MAX_DECODED_PAGE_BYTES)
        .ok_or(ArchiveError::Limit {
            resource: "decoded page bytes",
        })?;
    let mut pixels = Zeroizing::new(vec![0; length]);
    reader
        .next_frame(&mut pixels)
        .map_err(|_| invalid("page PNG data"))?;
    reader.finish().map_err(|_| invalid("page PNG ending"))?;
    Ok(())
}

fn read_blob(
    row: &Row<'_>,
    index: usize,
    limit: usize,
    resource: &'static str,
) -> Result<Zeroizing<Vec<u8>>, ArchiveError> {
    let value = row
        .get_ref(index)
        .map_err(|error| database("read payload", error))?;
    let bytes = value.as_blob().map_err(|_| invalid("payload type"))?;
    if bytes.len() > limit {
        return Err(ArchiveError::Limit { resource });
    }
    Ok(Zeroizing::new(bytes.to_vec()))
}

fn read_text<'a>(row: &'a Row<'_>, index: usize, limit: usize) -> Result<&'a str, ArchiveError> {
    let value = row
        .get_ref(index)
        .map_err(|error| database("read metadata", error))?;
    let text = value.as_str().map_err(|_| invalid("metadata type"))?;
    if text.len() > limit {
        return Err(ArchiveError::Limit {
            resource: "metadata bytes",
        });
    }
    Ok(text)
}

fn read_session_id(row: &Row<'_>, index: usize) -> Result<SessionId, ArchiveError> {
    read_text(row, index, 36)?
        .parse()
        .map_err(|_| invalid("branch identity"))
}

fn read_number(row: &Row<'_>, index: usize) -> Result<u64, ArchiveError> {
    let value = row
        .get::<_, i64>(index)
        .map_err(|error| database("read archive integer", error))?;
    unsigned(value)
}

fn read_size(row: &Row<'_>, index: usize) -> Result<usize, ArchiveError> {
    usize::try_from(read_number(row, index)?)
        .map_err(|_| invalid("archive size is outside the platform range"))
}

fn unsigned(value: i64) -> Result<u64, ArchiveError> {
    u64::try_from(value).map_err(|_| invalid("negative archive integer"))
}

fn sql_number(value: impl TryInto<i64>) -> Result<i64, ArchiveError> {
    value.try_into().map_err(|_| ArchiveError::Limit {
        resource: "SQLite integer",
    })
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_hash(hash: &str) -> Result<(), ArchiveError> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("SHA256 identity"));
    }
    Ok(())
}

fn validate_blob_hash(
    expected: &str,
    bytes: &[u8],
    limit: usize,
    resource: &'static str,
) -> Result<(), ArchiveError> {
    if bytes.len() > limit {
        return Err(ArchiveError::Limit { resource });
    }
    validate_hash(expected)?;
    if hash(bytes) != expected {
        return Err(invalid("payload hash"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ArchiveError, ArchivePage, ArchiveRecord, ArchiveValidation, MAX_ITEM_ID_BYTES,
        MAX_SOURCE_BYTES, hash,
    };
    use crate::sessions::storage::{SessionStorage, database_path};
    use nanocodex::{
        agent::session::{SessionId, compaction::ContextCheckpoint},
        oai::responses::ResponseItemId,
    };
    use png::{BitDepth, ColorType, Encoder};
    use rusqlite::{Connection, params};
    use std::path::Path;
    use tempfile::{TempDir, tempdir};
    use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

    fn storage() -> (TempDir, SessionStorage) {
        let directory = tempdir().unwrap();
        let storage = SessionStorage::open(&directory.path().join("config.toml")).unwrap();
        (directory, storage)
    }

    fn fresh(storage: &mut SessionStorage) -> ContextCheckpoint {
        storage
            .archive_open(SessionId::new(), None, ArchiveValidation::Complete)
            .unwrap()
    }

    fn append(
        storage: &mut SessionStorage,
        checkpoint: &mut ContextCheckpoint,
        id: &str,
        generation: u64,
        success: Option<bool>,
        original: &[u8],
        visible: &[u8],
    ) {
        checkpoint.sequence += 1;
        checkpoint.model_generation = generation;
        storage
            .archive_record(&ArchiveRecord {
                checkpoint: checkpoint.clone(),
                item_id: ResponseItemId::from_server(id),
                tool_success: success,
                original: Zeroizing::new(original.to_vec()),
                visible: Zeroizing::new(visible.to_vec()),
            })
            .unwrap();
    }

    fn page() -> ArchivePage {
        let mut png = Vec::new();
        {
            let mut encoder = Encoder::new(&mut png, 8, 16);
            encoder.set_color(ColorType::Grayscale);
            encoder.set_depth(BitDepth::One);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0xff; 16]).unwrap();
            writer.finish().unwrap();
        }
        ArchivePage {
            hash: hash(&png),
            width: 8,
            height: 16,
            png: Zeroizing::new(png),
        }
    }

    fn install_manifest(
        storage: &mut SessionStorage,
        checkpoint: &mut ContextCheckpoint,
    ) -> String {
        let encoded = format!(
            "{{\"branch\":\"{}\",\"sequence\":{}}}",
            checkpoint.branch, checkpoint.sequence
        );
        let id = hash(encoded.as_bytes());
        storage
            .archive_save_manifest(&id, checkpoint, encoded.as_bytes(), &[page()])
            .unwrap();
        checkpoint.manifest = Some(id.clone());
        id
    }

    #[test]
    fn exact_original_and_visible_bytes_round_trip_with_acceptance_metadata() {
        let (_directory, mut storage) = storage();
        let mut checkpoint = fresh(&mut storage);
        let original = "  fn main() {\r\n\tprintln!(\"日本語\");\n}\n\0".as_bytes();
        let visible = "  fn main() {\r\n\t[ordinary output limit]\n}".as_bytes();
        append(
            &mut storage,
            &mut checkpoint,
            "tool_result_1",
            1,
            Some(true),
            original,
            visible,
        );
        let mut restored = storage.archive_read(&checkpoint, "tool_result_1").unwrap();
        assert_eq!(&*restored.original, original);
        assert_eq!(&*restored.visible, visible);
        assert_eq!(restored.checkpoint, checkpoint);
        assert_eq!(restored.item_id.as_str(), "tool_result_1");
        assert_eq!(restored.tool_success, Some(true));
        storage.archive_validate(&checkpoint).unwrap();
        fn secret<T: Zeroize + ZeroizeOnDrop>() {}
        secret::<ArchiveRecord>();
        secret::<ArchivePage>();
        assert_eq!(format!("{restored:?}"), "ArchiveRecord([REDACTED])");
        restored.zeroize();
        assert!(restored.original.is_empty());
        assert!(restored.visible.is_empty());
    }

    #[test]
    fn authoritative_success_excludes_the_latest_two_generations() {
        let (_directory, mut storage) = storage();
        let mut checkpoint = fresh(&mut storage);
        for (id, generation, success) in [
            ("old_success", 1, Some(true)),
            ("failure", 2, Some(false)),
            ("unknown", 2, None),
            ("recent", 3, Some(true)),
            ("newest", 4, Some(true)),
        ] {
            append(
                &mut storage,
                &mut checkpoint,
                id,
                generation,
                success,
                b"source",
                b"visible",
            );
        }
        let ids = storage.archive_successful_results(&checkpoint).unwrap();
        assert_eq!(
            ids.iter().map(ResponseItemId::as_str).collect::<Vec<_>>(),
            ["old_success"]
        );
        let mut child = storage
            .archive_open(
                SessionId::new(),
                Some(&checkpoint),
                ArchiveValidation::Complete,
            )
            .unwrap();
        append(
            &mut storage,
            &mut child,
            "child_result",
            5,
            Some(true),
            b"child",
            b"child",
        );
        let ids = storage.archive_successful_results(&child).unwrap();
        assert_eq!(
            ids.iter().map(ResponseItemId::as_str).collect::<Vec<_>>(),
            ["old_success", "recent"]
        );
    }

    #[test]
    fn nested_forks_obey_all_cutoffs_and_cannot_read_siblings() {
        let (_directory, mut storage) = storage();
        let mut parent = fresh(&mut storage);
        append(
            &mut storage,
            &mut parent,
            "ancestor",
            1,
            Some(true),
            b"ancestor",
            b"ancestor",
        );
        let cutoff = parent.clone();
        append(
            &mut storage,
            &mut parent,
            "late_parent",
            2,
            Some(true),
            b"late",
            b"late",
        );
        let mut fork = storage
            .archive_open(SessionId::new(), Some(&cutoff), ArchiveValidation::Complete)
            .unwrap();
        let initial_fork = fork.clone();
        append(
            &mut storage,
            &mut fork,
            "fork_only",
            3,
            Some(true),
            b"fork",
            b"fork",
        );
        let sibling = storage
            .archive_open(SessionId::new(), Some(&parent), ArchiveValidation::Complete)
            .unwrap();
        let grandchild = storage
            .archive_open(
                SessionId::new(),
                Some(&initial_fork),
                ArchiveValidation::Complete,
            )
            .unwrap();
        assert_eq!(
            &*storage.archive_read(&fork, "ancestor").unwrap().original,
            b"ancestor"
        );
        for (scope, id) in [
            (&fork, "late_parent"),
            (&sibling, "fork_only"),
            (&grandchild, "fork_only"),
            (&grandchild, "late_parent"),
        ] {
            assert!(matches!(
                storage.archive_read(scope, id),
                Err(ArchiveError::Missing { .. })
            ));
        }
        storage.archive_validate(&grandchild).unwrap();
        let independent = fresh(&mut storage);
        assert!(matches!(
            storage.archive_read(&independent, "ancestor"),
            Err(ArchiveError::Missing { .. })
        ));
    }

    #[test]
    fn resuming_same_runtime_creates_a_new_branch_and_hides_unfinished_tail() {
        let (_directory, mut storage) = storage();
        let runtime = SessionId::new();
        let mut before = storage
            .archive_open(runtime, None, ArchiveValidation::Complete)
            .unwrap();
        append(
            &mut storage,
            &mut before,
            "completed",
            1,
            None,
            b"completed",
            b"completed",
        );
        let completed = before.clone();
        append(
            &mut storage,
            &mut before,
            "unfinished",
            2,
            None,
            b"unfinished",
            b"unfinished",
        );
        let mut resumed = storage
            .archive_open(runtime, Some(&completed), ArchiveValidation::Complete)
            .unwrap();
        assert_ne!(resumed.branch, before.branch);
        assert_ne!(resumed.branch, runtime);
        assert_eq!(resumed.sequence, 0);
        assert_eq!(resumed.model_generation, completed.model_generation);
        assert!(storage.archive_read(&resumed, "completed").is_ok());
        assert!(storage.archive_read(&resumed, "unfinished").is_err());
        append(
            &mut storage,
            &mut resumed,
            "continued",
            2,
            None,
            b"continued",
            b"continued",
        );
        assert!(storage.archive_read(&before, "continued").is_err());
        assert!(storage.archive_read(&before, "unfinished").is_ok());
    }

    #[test]
    fn repeated_identical_records_are_idempotent_and_conflicts_do_not_advance() {
        let (_directory, mut storage) = storage();
        let mut checkpoint = fresh(&mut storage);
        append(
            &mut storage,
            &mut checkpoint,
            "item",
            1,
            Some(true),
            b"source",
            b"visible",
        );
        let mut record = storage.archive_read(&checkpoint, "item").unwrap();
        storage.archive_record(&record).unwrap();
        *record.original = b"conflicting".to_vec();
        assert!(matches!(
            storage.archive_record(&record),
            Err(ArchiveError::Conflict { .. })
        ));
        *record.original = b"source".to_vec();
        record.checkpoint.sequence = 3;
        assert!(matches!(
            storage.archive_record(&record),
            Err(ArchiveError::Conflict { .. })
        ));
        let count: i64 = storage
            .connection
            .query_row("SELECT COUNT(*) FROM context_archive_records", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
        storage.archive_validate(&checkpoint).unwrap();
    }

    #[test]
    fn source_hashes_reject_payload_and_outcome_corruption() {
        for column in ["visible", "tool_success"] {
            let (_directory, mut storage) = storage();
            let mut checkpoint = fresh(&mut storage);
            append(
                &mut storage,
                &mut checkpoint,
                "item",
                1,
                Some(true),
                b"source",
                b"visible",
            );
            let sql = if column == "visible" {
                "UPDATE context_archive_records SET visible = X'00'"
            } else {
                "UPDATE context_archive_records SET tool_success = 0"
            };
            storage.connection.execute(sql, []).unwrap();
            assert!(matches!(
                storage.archive_read(&checkpoint, "item"),
                Err(ArchiveError::Invalid { .. })
            ));
            assert!(matches!(
                storage.archive_validate(&checkpoint),
                Err(ArchiveError::Invalid { .. })
            ));
        }
    }

    #[test]
    fn oversized_stored_source_is_rejected_at_the_blob_boundary() {
        let (_directory, mut storage) = storage();
        let mut checkpoint = fresh(&mut storage);
        append(
            &mut storage,
            &mut checkpoint,
            "item",
            1,
            None,
            b"source",
            b"visible",
        );
        storage
            .connection
            .execute(
                "UPDATE context_archive_records SET original = zeroblob(?1)",
                [i64::try_from(MAX_SOURCE_BYTES + 1).unwrap()],
            )
            .unwrap();
        assert!(matches!(
            storage.archive_read(&checkpoint, "item"),
            Err(ArchiveError::Limit {
                resource: "original source bytes"
            })
        ));
    }

    #[test]
    fn source_deletion_and_lineage_cycles_reject_saved_boundaries() {
        let (_directory, mut storage) = storage();
        let mut checkpoint = fresh(&mut storage);
        append(
            &mut storage,
            &mut checkpoint,
            "first",
            1,
            None,
            b"first",
            b"first",
        );
        append(
            &mut storage,
            &mut checkpoint,
            "last",
            2,
            None,
            b"last",
            b"last",
        );
        storage
            .connection
            .execute("DELETE FROM context_archive_records WHERE sequence = 1", [])
            .unwrap();
        assert!(storage.archive_validate(&checkpoint).is_err());
        let mut empty = fresh(&mut storage);
        storage.connection.execute("UPDATE context_archive_branches SET parent_branch = branch, parent_sequence = 0 WHERE branch = ?1", [empty.branch.to_string()]).unwrap();
        empty.sequence = 0;
        assert!(matches!(
            storage.archive_validate(&empty),
            Err(ArchiveError::Invalid {
                reason: "branch lineage cycle"
            })
        ));
    }

    #[test]
    fn manifests_and_pages_round_trip_idempotently_and_follow_ancestor_scope() {
        let (_directory, mut storage) = storage();
        let mut checkpoint = fresh(&mut storage);
        append(
            &mut storage,
            &mut checkpoint,
            "item",
            1,
            Some(true),
            b"source",
            b"visible",
        );
        let id = install_manifest(&mut storage, &mut checkpoint);
        let encoded = storage
            .archive_load_manifest(&id, ArchiveValidation::Complete)
            .unwrap();
        let png = page();
        assert_eq!(storage.archive_load_page(&png.hash).unwrap(), png.png);
        storage
            .archive_save_manifest(&id, &checkpoint, &encoded, &[page()])
            .unwrap();
        let restored = storage
            .archive_open(
                SessionId::new(),
                Some(&checkpoint),
                ArchiveValidation::Complete,
            )
            .unwrap();
        assert_eq!(restored.manifest.as_deref(), Some(id.as_str()));
        storage.archive_validate(&restored).unwrap();
        let mut unrelated = fresh(&mut storage);
        unrelated.manifest = Some(id);
        assert!(matches!(
            storage.archive_validate(&unrelated),
            Err(ArchiveError::Invalid { .. })
        ));
    }

    #[test]
    fn page_writes_roll_back_when_manifest_publication_fails() {
        let (_directory, mut storage) = storage();
        let checkpoint = fresh(&mut storage);
        storage.connection.execute_batch("CREATE TRIGGER reject_manifest BEFORE INSERT ON context_archive_manifests BEGIN SELECT RAISE(ABORT, 'injected publication failure'); END;").unwrap();
        let encoded = b"manifest";
        let error = storage
            .archive_save_manifest(&hash(encoded), &checkpoint, encoded, &[page()])
            .unwrap_err();
        assert!(matches!(error, ArchiveError::Database { .. }));
        assert!(!error.to_string().contains("injected publication failure"));
        for table in [
            "context_archive_pages",
            "context_archive_manifests",
            "context_archive_manifest_pages",
        ] {
            let count: i64 = storage
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0);
        }
        storage.archive_validate(&checkpoint).unwrap();
    }

    #[test]
    fn invalid_page_hash_dimensions_and_reused_corrupt_pages_are_rejected() {
        let (_directory, mut storage) = storage();
        let mut checkpoint = fresh(&mut storage);
        let encoded = b"candidate";
        let id = hash(encoded);
        let mut wrong = page();
        wrong.width += 1;
        assert!(
            storage
                .archive_save_manifest(&id, &checkpoint, encoded, &[wrong])
                .is_err()
        );
        let mut wrong = page();
        wrong.hash = "0".repeat(64);
        assert!(
            storage
                .archive_save_manifest(&id, &checkpoint, encoded, &[wrong])
                .is_err()
        );
        let mut truncated = page();
        let incomplete_length = truncated.png.len() - 12;
        truncated.png.truncate(incomplete_length);
        truncated.hash = hash(&truncated.png);
        assert!(
            storage
                .archive_save_manifest(&id, &checkpoint, encoded, &[truncated])
                .is_err()
        );
        install_manifest(&mut storage, &mut checkpoint);
        storage
            .connection
            .execute("UPDATE context_archive_pages SET png = X'00'", [])
            .unwrap();
        assert!(
            storage
                .archive_save_manifest(&id, &checkpoint, encoded, &[page()])
                .is_err()
        );
        assert!(storage.archive_validate(&checkpoint).is_err());
    }

    #[test]
    fn missing_pages_and_references_prevent_manifest_restore() {
        for table in ["context_archive_pages", "context_archive_manifest_pages"] {
            let (_directory, mut storage) = storage();
            let mut checkpoint = fresh(&mut storage);
            let id = install_manifest(&mut storage, &mut checkpoint);
            storage
                .connection
                .execute_batch("PRAGMA foreign_keys = OFF")
                .unwrap();
            storage
                .connection
                .execute(&format!("DELETE FROM {table}"), [])
                .unwrap();
            assert!(
                storage
                    .archive_load_manifest(&id, ArchiveValidation::Complete)
                    .is_err()
            );
            assert!(storage.archive_validate(&checkpoint).is_err());
        }
    }

    #[test]
    fn corrupt_manifest_and_out_of_scope_source_cutoff_are_rejected() {
        let (_directory, mut storage) = storage();
        let mut checkpoint = fresh(&mut storage);
        append(
            &mut storage,
            &mut checkpoint,
            "first",
            1,
            None,
            b"first",
            b"first",
        );
        let old = checkpoint.clone();
        append(
            &mut storage,
            &mut checkpoint,
            "second",
            2,
            None,
            b"second",
            b"second",
        );
        let id = install_manifest(&mut storage, &mut checkpoint);
        let mut child = storage
            .archive_open(SessionId::new(), Some(&old), ArchiveValidation::Complete)
            .unwrap();
        child.manifest = Some(id.clone());
        assert!(storage.archive_validate(&child).is_err());
        storage
            .connection
            .execute("UPDATE context_archive_manifests SET encoded = X'00'", [])
            .unwrap();
        assert!(
            storage
                .archive_load_manifest(&id, ArchiveValidation::Complete)
                .is_err()
        );
    }

    fn legacy_database(config: &Path) {
        let storage = SessionStorage::open(config).unwrap();
        storage.connection.execute_batch("
            DROP TABLE context_archive_manifest_pages;
            DROP TABLE context_archive_manifests;
            DROP TABLE context_archive_pages;
            DROP TABLE context_archive_records;
            DROP TABLE context_archive_branches;
            PRAGMA user_version = 2;
            INSERT INTO sessions VALUES ('legacy', NULL, '/workspace', 'model', 'medium', 'standard', 0, 'version', 1, 1, 'preview');
            INSERT INTO events(session_id, record_json) VALUES ('legacy', X'0009FF');
            INSERT INTO resume_states VALUES ('legacy', X'010AFF');
        ").unwrap();
    }

    #[test]
    fn readonly_v2_is_preserved_and_writable_open_migrates_exact_legacy_bytes() {
        let directory = tempdir().unwrap();
        let config = directory.path().join("config.toml");
        legacy_database(&config);
        let readonly = SessionStorage::open_read_only(&config).unwrap().unwrap();
        let checkpoint = ContextCheckpoint {
            branch: SessionId::new(),
            sequence: 0,
            model_generation: 0,
            manifest: None,
        };
        assert!(matches!(
            readonly.archive_validate(&checkpoint),
            Err(ArchiveError::Unavailable)
        ));
        let version: i64 = readonly
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);
        drop(readonly);
        let mut upgraded = SessionStorage::open(&config).unwrap();
        let version: i64 = upgraded
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 3);
        let record: Vec<u8> = upgraded
            .connection
            .query_row(
                "SELECT record_json FROM events WHERE session_id = 'legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let state: Vec<u8> = upgraded
            .connection
            .query_row(
                "SELECT state_zstd FROM resume_states WHERE session_id = 'legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(record, [0, 9, 255]);
        assert_eq!(state, [1, 10, 255]);
        fresh(&mut upgraded);
        assert!(database_path(&config).ends_with("sessions/v2.sqlite3"));
    }

    #[test]
    fn failed_migration_rolls_back_new_tables_and_keeps_version_two() {
        let directory = tempdir().unwrap();
        let config = directory.path().join("config.toml");
        legacy_database(&config);
        let connection = Connection::open(database_path(&config)).unwrap();
        connection
            .execute(
                "CREATE INDEX context_archive_manifests ON events(event_id)",
                [],
            )
            .unwrap();
        assert!(SessionStorage::open(&config).is_err());
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);
        let created: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'context_archive_branches')", [], |row| row.get(0)).unwrap();
        assert!(!created);
    }

    #[test]
    fn invalid_identifiers_and_integer_overflow_fail_before_persistence() {
        let (_directory, mut storage) = storage();
        let checkpoint = fresh(&mut storage);
        let mut record = ArchiveRecord {
            checkpoint: ContextCheckpoint {
                sequence: u64::MAX,
                ..checkpoint.clone()
            },
            item_id: ResponseItemId::from_server("item"),
            tool_success: None,
            original: Zeroizing::new(Vec::new()),
            visible: Zeroizing::new(Vec::new()),
        };
        assert!(matches!(
            storage.archive_record(&record),
            Err(ArchiveError::Limit { .. })
        ));
        record.checkpoint.sequence = 1;
        record.item_id = ResponseItemId::from_server("x".repeat(MAX_ITEM_ID_BYTES + 1));
        assert!(matches!(
            storage.archive_record(&record),
            Err(ArchiveError::Limit { .. })
        ));
        assert!(storage.archive_load_page("not a hash").is_err());
        let count: i64 = storage
            .connection
            .query_row(
                "SELECT COUNT(*) FROM context_archive_records",
                params![],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}
