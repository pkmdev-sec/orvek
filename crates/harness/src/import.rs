//! Read-only historical session export. Nothing here executes, resumes, certifies,
//! or grants authority to an old tool call, instruction, or completion event.
//!
//! A bounded SQLite backup captures committed WAL contents from one pinned read
//! transaction. Original record bytes, compressed resume bytes, and the complete
//! private database snapshot are retained. Optional archive tables are inventoried
//! and preserved in that snapshot, without interpreting their payloads.

mod publish;
use crate::Digest;
pub use publish::{
    ArchivedRecord, ImportCursor, ImportManifest, ImportPage, PageLimits, PreparedImport,
    PublicationLimits, PublishError, RecordIndex, RecordReference, ResumeArtifacts, prepare_import,
    read_import_page,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row,
    backup::{Backup, StepResult},
    limits::Limit,
    types::ValueRef,
};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::{
    collections::HashSet,
    io::Read,
    path::Path,
    time::{Duration, Instant},
};
use tempfile::NamedTempFile;

const EXPORT_VERSION: u32 = 1;
const SELECT_SESSION: &str = "SELECT session_id,parent_session_id,workspace,model,effort,reasoning_mode,fast_mode,application_version,started_at_ms,updated_at_ms,preview FROM sessions";

#[derive(Clone, Debug)]
pub struct ImportLimits {
    pub max_snapshot_bytes: usize,
    pub max_sessions: usize,
    pub max_records: usize,
    pub max_record_bytes: usize,
    pub max_selected_bytes: usize,
    pub max_compressed_resume_bytes: usize,
    pub max_decoded_resume_bytes: usize,
    pub max_metadata_bytes: usize,
    pub max_schema_objects: usize,
    pub max_archive_rows: usize,
    pub max_lineage_depth: usize,
    pub timeout: Duration,
}
impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_snapshot_bytes: 64 * 1024 * 1024,
            max_sessions: 10000,
            max_records: 100000,
            max_record_bytes: 1024 * 1024,
            max_selected_bytes: 32 * 1024 * 1024,
            max_compressed_resume_bytes: 8 * 1024 * 1024,
            max_decoded_resume_bytes: 32 * 1024 * 1024,
            max_metadata_bytes: 64 * 1024,
            max_schema_objects: 256,
            max_archive_rows: 10000,
            max_lineage_depth: 64,
            timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("invalid import limits")]
    InvalidLimits,
    #[error("historical database is unavailable or invalid")]
    Database,
    #[error("historical database schema version {0} is unsupported")]
    UnsupportedDatabaseVersion(u32),
    #[error("historical transcript version {0} is unsupported")]
    UnsupportedRecordVersion(u32),
    #[error("historical resume wrapper version {0} is unsupported")]
    UnsupportedResumeVersion(u32),
    #[error("historical provider snapshot version {0} is unsupported")]
    UnsupportedSnapshotVersion(u32),
    #[error("historical import exceeded its {0} bound")]
    Limit(&'static str),
    #[error("historical import timed out")]
    TimedOut,
    #[error("historical session was not found")]
    SessionNotFound,
    #[error("historical lineage references a missing ancestor")]
    MissingAncestor,
    #[error("historical lineage contains a cycle")]
    LineageCycle,
    #[error("historical session metadata or start record is inconsistent")]
    InvalidLineage,
    #[error("historical fork cutoff does not identify a persisted sequence")]
    InvalidForkCutoff,
    #[error("historical transcript record is corrupt")]
    CorruptRecord,
    #[error("historical resume data is corrupt or truncated")]
    CorruptResume,
}
impl From<rusqlite::Error> for ImportError {
    fn from(error: rusqlite::Error) -> Self {
        if error.sqlite_error_code() == Some(rusqlite::ffi::ErrorCode::OperationInterrupted) {
            Self::TimedOut
        } else {
            Self::Database
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LegacySetting {
    /// Exact SQL text, including the JSON quoting used by the original writer.
    pub raw: String,
    /// Decoded text when the legacy representation is a JSON string; no defaults.
    pub value: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LegacySessionMetadata {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub workspace: String,
    pub model: String,
    pub effort: LegacySetting,
    pub reasoning_mode: LegacySetting,
    pub fast_mode: bool,
    pub application_version: String,
    pub started_at_ms: u64,
    pub updated_at_ms: u64,
    pub preview: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct LegacyRecord {
    pub session_id: String,
    pub event_id: i64,
    pub schema_version: u32,
    pub sequence: u64,
    pub recorded_at_unix_ms: u64,
    pub source: String,
    pub kind: String,
    pub agent_protocol_version: Option<u32>,
    pub raw_digest: Digest,
    pub raw_json: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParentLink {
    pub session_id: String,
    pub through_sequence: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LegacyLineageSegment {
    pub metadata: LegacySessionMetadata,
    pub parent: Option<ParentLink>,
    /// Effective inclusive cutoff; zero means none of this ancestor's records.
    pub through_sequence: Option<u64>,
    pub first_record_index: usize,
    pub record_count: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct LegacyResume {
    pub wrapper_version: u32,
    pub snapshot_version: u32,
    pub compressed_digest: Digest,
    pub decoded_digest: Digest,
    /// Identity of the exact snapshot JSON substring inside the wrapper.
    pub snapshot_digest: Digest,
    pub compressed_zstd: Vec<u8>,
    pub decoded_json: Vec<u8>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SchemaObject {
    pub kind: String,
    pub name: String,
    pub table_name: String,
    pub sql: Option<String>,
    /// Extra tables, including compaction archives, are preserved without decoding.
    pub opaque: bool,
    pub opaque_row_count: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LegacySessionExport {
    pub export_version: u32,
    /// Content identity of this selected lineage and resume, independent of path
    /// and unrelated session writes. All identities are historical provenance.
    pub import_id: Digest,
    pub source_snapshot_id: Digest,
    pub database_version: u32,
    pub session_id: String,
    pub lineage: Vec<LegacyLineageSegment>,
    pub records: Vec<LegacyRecord>,
    pub resume: Option<LegacyResume>,
}

/// Immutable private snapshot plus bounded historical projections. Opening never
/// migrates or writes the supplied database; all later reads use the private copy.
pub struct LegacyArchive {
    connection: Connection,
    _snapshot_file: NamedTempFile,
    snapshot: Vec<u8>,
    snapshot_id: Digest,
    version: u32,
    schema: Vec<SchemaObject>,
    limits: ImportLimits,
}

impl LegacyArchive {
    pub fn open(path: &Path, limits: ImportLimits) -> Result<Self, ImportError> {
        validate_limits(&limits)?;
        let deadline = Instant::now() + limits.timeout;
        let source = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        configure(&source, &limits, deadline)?;
        source.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; BEGIN DEFERRED;")?;
        let version = database_version(&source)?;
        let pages: u64 = source.query_row("PRAGMA page_count", [], |row| {
            row.get::<_, u32>(0).map(u64::from)
        })?;
        let page_size: u64 = source.query_row("PRAGMA page_size", [], |row| {
            row.get::<_, u32>(0).map(u64::from)
        })?;
        if pages
            .checked_mul(page_size)
            .is_none_or(|size| size > limits.max_snapshot_bytes as u64)
        {
            return Err(ImportError::Limit("snapshot bytes"));
        }
        let schema = schema_objects(&source, &limits)?;
        validate_schema(&schema)?;
        let snapshot_file = NamedTempFile::new().map_err(|_| ImportError::Database)?;
        let mut destination = Connection::open(snapshot_file.path())?;
        {
            let backup = Backup::new(&source, &mut destination)?;
            loop {
                check_deadline(deadline)?;
                match backup.step(128)? {
                    StepResult::Done => break,
                    StepResult::More => {}
                    StepResult::Busy | StepResult::Locked => return Err(ImportError::Database),
                    _ => return Err(ImportError::Database),
                }
                if backup.progress().pagecount as u64 * page_size > limits.max_snapshot_bytes as u64
                {
                    return Err(ImportError::Limit("snapshot bytes"));
                }
            }
        }
        source.execute_batch("ROLLBACK;")?;
        // Normalize only the owned backup into a standalone file without WAL.
        destination.execute_batch("PRAGMA journal_mode=DELETE;")?;
        destination.close().map_err(|_| ImportError::Database)?;
        check_deadline(deadline)?;
        let metadata = snapshot_file
            .as_file()
            .metadata()
            .map_err(|_| ImportError::Database)?;
        if metadata.len() > limits.max_snapshot_bytes as u64 {
            return Err(ImportError::Limit("snapshot bytes"));
        }
        let mut snapshot = Vec::new();
        snapshot_file
            .reopen()
            .map_err(|_| ImportError::Database)?
            .take(limits.max_snapshot_bytes as u64 + 1)
            .read_to_end(&mut snapshot)
            .map_err(|_| ImportError::Database)?;
        if snapshot.len() > limits.max_snapshot_bytes {
            return Err(ImportError::Limit("snapshot bytes"));
        }
        let snapshot_id = Digest::of(&snapshot);
        let connection = Connection::open_with_flags(
            snapshot_file.path(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF;")?;
        configure(&connection, &limits, Instant::now() + limits.timeout)?;
        if database_version(&connection)? != version {
            return Err(ImportError::Database);
        }
        Ok(Self {
            connection,
            _snapshot_file: snapshot_file,
            snapshot,
            snapshot_id,
            version,
            schema,
            limits,
        })
    }

    pub fn snapshot_bytes(&self) -> &[u8] {
        &self.snapshot
    }
    pub fn snapshot_id(&self) -> Digest {
        self.snapshot_id
    }
    pub fn database_version(&self) -> u32 {
        self.version
    }
    pub fn schema(&self) -> &[SchemaObject] {
        &self.schema
    }

    pub fn sessions(&self) -> Result<Vec<LegacySessionMetadata>, ImportError> {
        let deadline = self.begin_operation()?;
        let sql = format!("{SELECT_SESSION} ORDER BY updated_at_ms DESC,session_id LIMIT ?1");
        let mut statement = self.connection.prepare(&sql)?;
        let mut rows = statement.query([self.limits.max_sessions as i64 + 1])?;
        let mut sessions = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = rows.next()? {
            check_deadline(deadline)?;
            if sessions.len() == self.limits.max_sessions {
                return Err(ImportError::Limit("session rows"));
            }
            let metadata = metadata(row, &self.limits)?;
            bytes = bytes.saturating_add(
                serde_json::to_vec(&metadata)
                    .map_err(|_| ImportError::Database)?
                    .len(),
            );
            if bytes > self.limits.max_selected_bytes {
                return Err(ImportError::Limit("selected bytes"));
            }
            sessions.push(metadata);
        }
        Ok(sessions)
    }

    pub fn export(&self, session_id: &str) -> Result<LegacySessionExport, ImportError> {
        if !valid_id(session_id) {
            return Err(ImportError::SessionNotFound);
        }
        let deadline = self.begin_operation()?;
        let mut visited = HashSet::new();
        let mut current = session_id.to_owned();
        let mut through = None;
        let mut reversed = Vec::new();
        let mut budget = SelectionBudget::default();
        loop {
            check_deadline(deadline)?;
            if !visited.insert(current.clone()) {
                return Err(ImportError::LineageCycle);
            }
            if visited.len() > self.limits.max_lineage_depth {
                return Err(ImportError::Limit("lineage depth"));
            }
            let session = self.session(&current)?.ok_or(if current == session_id {
                ImportError::SessionNotFound
            } else {
                ImportError::MissingAncestor
            })?;
            let (records, parent) = self.records(&session, through, &mut budget, deadline)?;
            budget.add_bytes(
                serde_json::to_vec(&session)
                    .map_err(|_| ImportError::Database)?
                    .len(),
                &self.limits,
            )?;
            let next = parent.as_ref().map(|parent| {
                (
                    parent.session_id.clone(),
                    if through == Some(0) {
                        0
                    } else {
                        parent.through_sequence
                    },
                )
            });
            reversed.push((session, parent, through, records));
            let Some((parent, cutoff)) = next else {
                break;
            };
            current = parent;
            through = Some(cutoff);
        }
        let mut records = Vec::new();
        let mut lineage = Vec::new();
        for (metadata, parent, through_sequence, local) in reversed.into_iter().rev() {
            lineage.push(LegacyLineageSegment {
                metadata,
                parent,
                through_sequence,
                first_record_index: records.len(),
                record_count: local.len(),
            });
            records.extend(local);
        }
        let resume = self.resume(session_id, &mut budget, deadline)?;
        let record_ids: Vec<_> = records
            .iter()
            .map(|record| (&record.session_id, record.event_id, record.raw_digest))
            .collect();
        let resume_ids = resume.as_ref().map(|resume| {
            (
                resume.wrapper_version,
                resume.snapshot_version,
                resume.compressed_digest,
                resume.decoded_digest,
                resume.snapshot_digest,
            )
        });
        let import_id = Digest::of_value(&(
            EXPORT_VERSION,
            self.version,
            session_id,
            &lineage,
            &record_ids,
            resume_ids,
        ))
        .map_err(|_| ImportError::Database)?;
        Ok(LegacySessionExport {
            export_version: EXPORT_VERSION,
            import_id,
            source_snapshot_id: self.snapshot_id,
            database_version: self.version,
            session_id: session_id.into(),
            lineage,
            records,
            resume,
        })
    }

    fn begin_operation(&self) -> Result<Instant, ImportError> {
        let deadline = Instant::now() + self.limits.timeout;
        configure(&self.connection, &self.limits, deadline)?;
        Ok(deadline)
    }

    fn session(&self, id: &str) -> Result<Option<LegacySessionMetadata>, ImportError> {
        let mut statement = self
            .connection
            .prepare(&format!("{SELECT_SESSION} WHERE session_id=?1"))?;
        let mut rows = statement.query([id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let result = metadata(row, &self.limits)?;
        if rows.next()?.is_some() {
            return Err(ImportError::InvalidLineage);
        }
        Ok(Some(result))
    }

    fn records(
        &self,
        session: &LegacySessionMetadata,
        through: Option<u64>,
        budget: &mut SelectionBudget,
        deadline: Instant,
    ) -> Result<(Vec<LegacyRecord>, Option<ParentLink>), ImportError> {
        let mut statement=self.connection.prepare("SELECT event_id,length(record_json),record_json FROM events WHERE session_id=?1 ORDER BY event_id")?;
        let mut rows = statement.query([&session.session_id])?;
        let mut records = Vec::new();
        let mut previous = 0;
        let mut parent = None;
        let mut first = true;
        let mut cutoff_found = through.is_none() || through == Some(0);
        while let Some(row) = rows.next()? {
            check_deadline(deadline)?;
            if budget.records >= self.limits.max_records {
                return Err(ImportError::Limit("record rows"));
            }
            let event_id: i64 = row.get(0)?;
            let length = row.get::<_, u32>(1)? as usize;
            if event_id <= 0 {
                return Err(ImportError::CorruptRecord);
            }
            if length > self.limits.max_record_bytes {
                return Err(ImportError::Limit("record bytes"));
            }
            let raw = match row.get_ref(2)? {
                ValueRef::Blob(raw) => raw,
                _ => return Err(ImportError::CorruptRecord),
            };
            let record: RecordEnvelope =
                serde_json::from_slice(raw).map_err(|_| ImportError::CorruptRecord)?;
            if record.schema_version != 2 {
                return Err(ImportError::UnsupportedRecordVersion(record.schema_version));
            }
            if record.sequence == 0
                || record.sequence <= previous
                || record.source.is_empty()
                || record.kind.is_empty()
            {
                return Err(ImportError::CorruptRecord);
            }
            if through.is_some_and(|cutoff| cutoff > 0 && record.sequence > cutoff) {
                return Err(ImportError::InvalidForkCutoff);
            }
            if record.source == "agent" && record.agent.is_none() {
                return Err(ImportError::CorruptRecord);
            }
            if let Some(agent) = &record.agent
                && (agent.request_id.is_empty() || agent.protocol_version == 0)
            {
                return Err(ImportError::CorruptRecord);
            }
            previous = record.sequence;
            if first {
                if record.source != "tact" || record.kind != "session.started" {
                    return Err(ImportError::InvalidLineage);
                }
                let started: Started = serde_json::from_str(record.payload.get())
                    .map_err(|_| ImportError::InvalidLineage)?;
                if started.session_id != session.session_id
                    || started.parent_session_id != session.parent_session_id
                {
                    return Err(ImportError::InvalidLineage);
                }
                parent = match (started.parent_session_id, started.parent_sequence) {
                    (None, None) => None,
                    (Some(id), Some(cutoff)) if valid_id(&id) => Some(ParentLink {
                        session_id: id,
                        through_sequence: cutoff,
                    }),
                    _ => return Err(ImportError::InvalidLineage),
                };
                first = false;
            } else if record.source == "tact" && record.kind == "session.started" {
                let started: Started = serde_json::from_str(record.payload.get())
                    .map_err(|_| ImportError::InvalidLineage)?;
                if started.session_id != session.session_id
                    || started.parent_session_id.is_some()
                    || started.parent_sequence.is_some()
                {
                    return Err(ImportError::InvalidLineage);
                }
            }
            budget.records += 1;
            budget.add_bytes(raw.len(), &self.limits)?;
            if through == Some(0) {
                break;
            }
            records.push(LegacyRecord {
                session_id: session.session_id.clone(),
                event_id,
                schema_version: record.schema_version,
                sequence: record.sequence,
                recorded_at_unix_ms: record.recorded_at_unix_ms,
                source: record.source,
                kind: record.kind,
                agent_protocol_version: record.agent.map(|agent| agent.protocol_version),
                raw_digest: Digest::of(raw),
                raw_json: raw.to_vec(),
            });
            if through == Some(record.sequence) {
                cutoff_found = true;
                break;
            }
        }
        if first {
            return Err(ImportError::InvalidLineage);
        }
        if !cutoff_found {
            return Err(ImportError::InvalidForkCutoff);
        }
        Ok((records, parent))
    }

    fn resume(
        &self,
        id: &str,
        budget: &mut SelectionBudget,
        deadline: Instant,
    ) -> Result<Option<LegacyResume>, ImportError> {
        let length: Option<i64> = self
            .connection
            .query_row(
                "SELECT length(state_zstd) FROM resume_states WHERE session_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(length) = length else {
            return Ok(None);
        };
        if length < 0 || length as u64 > self.limits.max_compressed_resume_bytes as u64 {
            return Err(ImportError::Limit("compressed resume bytes"));
        }
        let compressed: Vec<u8> = self.connection.query_row(
            "SELECT state_zstd FROM resume_states WHERE session_id=?1",
            [id],
            |row| row.get(0),
        )?;
        if compressed.len() > self.limits.max_compressed_resume_bytes {
            return Err(ImportError::Limit("compressed resume bytes"));
        }
        budget.add_bytes(compressed.len(), &self.limits)?;
        let mut decoder = zstd::stream::read::Decoder::new(compressed.as_slice())
            .map_err(|_| ImportError::CorruptResume)?;
        decoder
            .window_log_max(23)
            .map_err(|_| ImportError::CorruptResume)?;
        let mut decoded = Vec::new();
        let mut buffer = [0u8; 65536];
        loop {
            check_deadline(deadline)?;
            let allowance = (self.limits.max_decoded_resume_bytes - decoded.len())
                .saturating_add(1)
                .min(buffer.len());
            let count = decoder
                .read(&mut buffer[..allowance])
                .map_err(|_| ImportError::CorruptResume)?;
            if count == 0 {
                break;
            }
            if decoded.len().saturating_add(count) > self.limits.max_decoded_resume_bytes {
                return Err(ImportError::Limit("decoded resume bytes"));
            }
            budget.add_bytes(count, &self.limits)?;
            decoded.extend_from_slice(&buffer[..count]);
        }
        let wrapper: ResumeWrapper =
            serde_json::from_slice(&decoded).map_err(|_| ImportError::CorruptResume)?;
        if wrapper.format_version != 2 {
            return Err(ImportError::UnsupportedResumeVersion(
                wrapper.format_version,
            ));
        }
        let version: SnapshotVersion =
            serde_json::from_str(wrapper.snapshot.get()).map_err(|_| ImportError::CorruptResume)?;
        if !matches!(version.version, 1 | 2) {
            return Err(ImportError::UnsupportedSnapshotVersion(version.version));
        }
        let result = LegacyResume {
            wrapper_version: wrapper.format_version,
            snapshot_version: version.version,
            compressed_digest: Digest::of(&compressed),
            decoded_digest: Digest::of(&decoded),
            snapshot_digest: Digest::of(wrapper.snapshot.get().as_bytes()),
            compressed_zstd: compressed,
            decoded_json: decoded,
        };
        Ok(Some(result))
    }
}

#[derive(Deserialize)]
struct RecordEnvelope {
    schema_version: u32,
    sequence: u64,
    recorded_at_unix_ms: u64,
    source: String,
    #[serde(rename = "type")]
    kind: String,
    payload: Box<RawValue>,
    #[serde(default)]
    agent: Option<AgentMetadata>,
}
#[derive(Deserialize)]
struct AgentMetadata {
    protocol_version: u32,
    request_id: String,
    #[serde(rename = "sequence")]
    _sequence: u64,
}
#[derive(Deserialize)]
struct Started {
    session_id: String,
    #[serde(default)]
    parent_session_id: Option<String>,
    #[serde(default)]
    parent_sequence: Option<u64>,
    #[serde(rename = "model")]
    _model: String,
    #[serde(rename = "effort")]
    _effort: String,
    #[serde(rename = "reasoning_mode")]
    _reasoning_mode: String,
    #[serde(rename = "fast_mode")]
    _fast_mode: bool,
    #[serde(rename = "workspace")]
    _workspace: String,
    #[serde(rename = "application_version")]
    _application_version: String,
}
#[derive(Deserialize)]
struct ResumeWrapper {
    format_version: u32,
    snapshot: Box<RawValue>,
    #[serde(rename = "instructions")]
    _instructions: String,
    #[serde(rename = "skills_catalog_present")]
    _skills_catalog_present: bool,
}
#[derive(Deserialize)]
struct SnapshotVersion {
    version: u32,
}
#[derive(Default)]
struct SelectionBudget {
    records: usize,
    bytes: usize,
}
impl SelectionBudget {
    fn add_bytes(&mut self, count: usize, limits: &ImportLimits) -> Result<(), ImportError> {
        self.bytes = self.bytes.saturating_add(count);
        if self.bytes > limits.max_selected_bytes {
            return Err(ImportError::Limit("selected bytes"));
        }
        Ok(())
    }
}

fn metadata(row: &Row<'_>, limits: &ImportLimits) -> Result<LegacySessionMetadata, ImportError> {
    let mut raw_bytes = 0usize;
    for index in 0..row.as_ref().column_count() {
        if let ValueRef::Text(bytes) | ValueRef::Blob(bytes) = row.get_ref(index)? {
            raw_bytes = raw_bytes.saturating_add(bytes.len());
            if raw_bytes > limits.max_metadata_bytes {
                return Err(ImportError::Limit("metadata bytes"));
            }
        }
    }
    let session_id: String = row.get(0)?;
    let parent_session_id: Option<String> = row.get(1)?;
    if !valid_id(&session_id) || parent_session_id.as_deref().is_some_and(|id| !valid_id(id)) {
        return Err(ImportError::InvalidLineage);
    }
    let effort: String = row.get(4)?;
    let reasoning_mode: String = row.get(5)?;
    let fast_mode: i64 = row.get(6)?;
    if !matches!(fast_mode, 0 | 1) {
        return Err(ImportError::InvalidLineage);
    }
    let metadata = LegacySessionMetadata {
        session_id,
        parent_session_id,
        workspace: row.get(2)?,
        model: row.get(3)?,
        effort: LegacySetting {
            value: serde_json::from_str(&effort).ok(),
            raw: effort,
        },
        reasoning_mode: LegacySetting {
            value: serde_json::from_str(&reasoning_mode).ok(),
            raw: reasoning_mode,
        },
        fast_mode: fast_mode == 1,
        application_version: row.get(7)?,
        started_at_ms: row
            .get::<_, i64>(8)?
            .try_into()
            .map_err(|_| ImportError::InvalidLineage)?,
        updated_at_ms: row
            .get::<_, i64>(9)?
            .try_into()
            .map_err(|_| ImportError::InvalidLineage)?,
        preview: row.get(10)?,
    };
    if serde_json::to_vec(&metadata)
        .map_err(|_| ImportError::Database)?
        .len()
        > limits.max_metadata_bytes
    {
        return Err(ImportError::Limit("metadata bytes"));
    }
    Ok(metadata)
}
fn database_version(connection: &Connection) -> Result<u32, ImportError> {
    let version = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if !matches!(version, 2 | 3) {
        return Err(ImportError::UnsupportedDatabaseVersion(version));
    }
    Ok(version)
}
fn schema_objects(
    connection: &Connection,
    limits: &ImportLimits,
) -> Result<Vec<SchemaObject>, ImportError> {
    let mut statement = connection
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name LIMIT ?1")?;
    let mut rows = statement.query([limits.max_schema_objects as i64 + 1])?;
    let mut objects = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next()? {
        if objects.len() == limits.max_schema_objects {
            return Err(ImportError::Limit("schema objects"));
        }
        let kind: String = row.get(0)?;
        let name: String = row.get(1)?;
        let sql: Option<String> = row.get(3)?;
        bytes = bytes.saturating_add(sql.as_ref().map_or(0, String::len));
        if bytes > limits.max_metadata_bytes {
            return Err(ImportError::Limit("schema bytes"));
        }
        let opaque = kind == "table"
            && !matches!(
                name.as_str(),
                "sessions" | "events" | "resume_states" | "sqlite_sequence"
            );
        let opaque_row_count = if opaque {
            if !sql.as_deref().is_some_and(|sql| {
                sql.trim_start()
                    .to_ascii_uppercase()
                    .starts_with("CREATE TABLE")
            }) {
                return Err(ImportError::Database);
            }
            let quoted = format!("\"{}\"", name.replace('"', "\"\""));
            let count: u32 = connection.query_row(
                &format!("SELECT count(*) FROM (SELECT 1 FROM {quoted} LIMIT ?1)"),
                [limits.max_archive_rows as i64 + 1],
                |row| row.get(0),
            )?;
            if count as usize > limits.max_archive_rows {
                return Err(ImportError::Limit("opaque archive rows"));
            }
            Some(count as usize)
        } else {
            None
        };
        objects.push(SchemaObject {
            kind,
            name,
            table_name: row.get(2)?,
            sql,
            opaque,
            opaque_row_count,
        });
    }
    Ok(objects)
}
fn validate_schema(schema: &[SchemaObject]) -> Result<(), ImportError> {
    for required in ["sessions", "events", "resume_states"] {
        let object = schema
            .iter()
            .find(|o| o.name == required)
            .ok_or(ImportError::Database)?;
        if object.kind != "table"
            || !object.sql.as_deref().is_some_and(|sql| {
                sql.trim_start()
                    .to_ascii_uppercase()
                    .starts_with("CREATE TABLE")
            })
        {
            return Err(ImportError::Database);
        }
    }
    Ok(())
}
fn configure(
    connection: &Connection,
    limits: &ImportLimits,
    deadline: Instant,
) -> Result<(), ImportError> {
    connection.busy_timeout(limits.timeout.min(Duration::from_secs(1)))?;
    let maximum = limits
        .max_compressed_resume_bytes
        .max(limits.max_record_bytes)
        .max(limits.max_metadata_bytes);
    connection.set_limit(
        Limit::SQLITE_LIMIT_LENGTH,
        i32::try_from(maximum).map_err(|_| ImportError::InvalidLimits)?,
    )?;
    connection.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 128 * 1024)?;
    connection.set_limit(Limit::SQLITE_LIMIT_COLUMN, 128)?;
    connection.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
    Ok(())
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.contains('\0')
}
fn check_deadline(deadline: Instant) -> Result<(), ImportError> {
    if Instant::now() >= deadline {
        Err(ImportError::TimedOut)
    } else {
        Ok(())
    }
}
fn validate_limits(limits: &ImportLimits) -> Result<(), ImportError> {
    if limits.max_snapshot_bytes == 0
        || limits.max_snapshot_bytes > 512 * 1024 * 1024
        || limits.max_sessions == 0
        || limits.max_sessions > 100000
        || limits.max_records == 0
        || limits.max_records > 1000000
        || limits.max_record_bytes == 0
        || limits.max_record_bytes > 16 * 1024 * 1024
        || limits.max_selected_bytes == 0
        || limits.max_selected_bytes > 256 * 1024 * 1024
        || limits.max_compressed_resume_bytes == 0
        || limits.max_compressed_resume_bytes > 64 * 1024 * 1024
        || limits.max_decoded_resume_bytes == 0
        || limits.max_decoded_resume_bytes > 128 * 1024 * 1024
        || limits.max_metadata_bytes == 0
        || limits.max_metadata_bytes > 1024 * 1024
        || limits.max_schema_objects == 0
        || limits.max_schema_objects > 4096
        || limits.max_archive_rows == 0
        || limits.max_archive_rows > 1000000
        || limits.max_lineage_depth == 0
        || limits.max_lineage_depth > 256
        || limits.timeout.is_zero()
        || limits.timeout > Duration::from_secs(120)
    {
        Err(ImportError::InvalidLimits)
    } else {
        Ok(())
    }
}
