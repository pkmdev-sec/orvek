use crate::{
    Digest,
    admission_profile::{
        BaselineReason, Channel, EnvironmentIdentity, ModelIdentity, ProtocolIdentity,
        TargetProfile, TaskProfileIdentity,
    },
    artifacts::{ArtifactError, ArtifactStore},
    completion::{self, Rejection},
    contract::{Contract, ContractError},
    session::{
        JournalRecord, ProviderCostSummary, SessionAdmissionProfile, SessionAdmissionRequest,
        SessionCommand, SessionConfig, SessionCursor, SessionEvent, SessionId, SessionState,
    },
    state::*,
    submission::OrdinaryKind,
};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

const SCHEMA_VERSION: i32 = 12;
const MAX_EVENT_BYTES: usize = 512 * 1024;
const MAX_JOURNAL_PAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_HOST_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
mod auxiliary;
mod event_intake;
mod imports;
mod manual;
mod monitor;
mod submissions;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    ContractPending(#[from] ContractPending),
    #[error("state I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("state database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("invalid state encoding: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error(transparent)]
    Contract(#[from] ContractError),
    #[error("unsupported store schema {0}")]
    Schema(i32),
    #[error("task not found: {0}")]
    Missing(TaskId),
    #[error("session not found: {0}")]
    MissingSession(SessionId),
    #[error("task revision changed: expected {expected}, found {actual}")]
    Revision { expected: u64, actual: u64 },
    #[error("task already ended")]
    Terminal,
    #[error("task cancellation has been requested")]
    Cancelled,
    #[error("invalid command: {0}")]
    Invalid(&'static str),
    #[error("state integrity failure: {0}")]
    Integrity(&'static str),
    #[error("verification lease is invalid, stale or already used")]
    Lease,
    #[error("execution budget exhausted")]
    Budget,
    #[error("completion rejected: {0:?}")]
    Incomplete(Vec<Rejection>),
}

/// Held only by the trusted runner coordinator. It is neither cloneable nor serializable.
pub struct VerificationLease {
    task: TaskId,
    job: Uuid,
    token: Zeroizing<String>,
}

impl VerificationLease {
    pub fn job_id(&self) -> Uuid {
        self.job
    }
}

impl std::fmt::Debug for VerificationLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerificationLease")
            .field("job", &self.job)
            .finish_non_exhaustive()
    }
}

/// Single host-owned writer. Untrusted executors receive no reference or filesystem access.
pub struct Store {
    connection: Connection,
    artifacts: ArtifactStore,
    // Fields drop in declaration order: close SQLite before releasing the writer lease.
    _owner: OwnerLock,
}

struct OwnerLock(File);

impl Drop for OwnerLock {
    fn drop(&mut self) {
        // A concurrent fork can retain this file description until exec, even with CLOEXEC.
        // Release our lease explicitly rather than waiting for every inherited fd to close.
        let _ = FileExt::unlock(&self.0);
    }
}

impl Store {
    pub fn open(root: &Path) -> Result<Self, StoreError> {
        Self::open_with_artifact_limit(root, MAX_HOST_ARTIFACT_BYTES)
    }

    pub fn open_with_artifact_limit(
        root: &Path,
        max_artifact_bytes: u64,
    ) -> Result<Self, StoreError> {
        fs::create_dir_all(root)?;
        if fs::symlink_metadata(root)?.file_type().is_symlink() {
            return Err(StoreError::Invalid("state directory cannot be a symlink"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        }
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("owner.lock"))?;
        owner.try_lock_exclusive()?;
        let owner = OwnerLock(owner);
        let database = root.join("v1.sqlite3");
        let mut connection = Connection::open(&database)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let version: i32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => initialize_schema(&connection)?,
            1 => {
                migrate_v1_to_v8(&connection)?;
                migrate_v8_to_v9(&mut connection)?;
            }
            2..=6 => {
                retire_legacy_schema(&connection)?;
                migrate_v8_to_v9(&mut connection)?;
            }
            7 => {
                migrate_v7_to_v8(&connection)?;
                migrate_v8_to_v9(&mut connection)?;
            }
            8 => migrate_v8_to_v9(&mut connection)?,
            9 | 10 | 11 | SCHEMA_VERSION => {}
            unsupported => return Err(StoreError::Schema(unsupported)),
        }
        event_intake::initialize(&connection)?;
        monitor::initialize(&connection)?;
        connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        let artifacts = ArtifactStore::open(&root.join("artifacts"), max_artifact_bytes)?;
        Ok(Self {
            connection,
            artifacts,
            _owner: owner,
        })
    }

    pub(crate) fn artifacts(&self) -> &ArtifactStore {
        &self.artifacts
    }

    fn fixture_admission(
        &self,
        config: &SessionConfig,
        reason: BaselineReason,
    ) -> Result<SessionAdmissionProfile, StoreError> {
        let request = SessionAdmissionRequest::new(
            config.workspace.clone(),
            config.model,
            config.context_window_tokens,
            Channel::Stable,
        );
        let target = TargetProfile::new(
            ModelIdentity::from_digest(Digest::of_value(&config.model)?),
            ProtocolIdentity::from_digest(Digest::of(b"orvek:store-fixture-protocol:v1")),
            EnvironmentIdentity::from_digest(Digest::of(b"orvek:store-fixture-environment:v1")),
            TaskProfileIdentity::from_digest(Digest::of(b"orvek:store-fixture-tools:v1")),
            Channel::Stable,
        );
        SessionAdmissionProfile::compiled(
            request,
            target,
            Digest::of(b"orvek:store-fixture-authority:v1"),
            reason,
        )
        .map_err(StoreError::from)
    }

    pub fn public_artifacts(&self) -> &ArtifactStore {
        &self.artifacts
    }

    pub fn journal_head(&self) -> Result<u64, StoreError> {
        let sequence: i64 = self.connection.query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM events",
            [],
            |row| row.get(0),
        )?;
        sequence
            .try_into()
            .map_err(|_| StoreError::Integrity("negative journal sequence"))
    }

    pub fn recent_inputs(
        &self,
        limit: usize,
        before: Option<u64>,
        workspace: Option<&Path>,
    ) -> Result<Vec<crate::session::RecentInput>, StoreError> {
        if limit == 0 || limit > 100 {
            return Err(StoreError::Invalid("recent input limit must be 1..100"));
        }
        let before = i64::try_from(before.unwrap_or(i64::MAX as u64))
            .map_err(|_| StoreError::Invalid("recent input cursor exceeds its bound"))?;
        let workspace = workspace.map(|path| path.to_string_lossy().into_owned());
        let mut query = self.connection.prepare("SELECT e.aggregate,e.revision,e.sequence,e.event,e.hash,p.hash,c.event,c.hash FROM events e JOIN events p ON p.aggregate=e.aggregate AND p.kind='session' AND p.revision=e.revision-1 JOIN events c ON c.aggregate=e.aggregate AND c.kind='session' AND c.revision=1 WHERE e.kind='session' AND e.sequence<?1 AND json_extract(e.event,'$.type')='command' AND json_extract(e.event,'$.data.command.type')='input' AND json_extract(e.event,'$.data.command.data.kind') IN ('task','conversation') AND (?2 IS NULL OR COALESCE(json_extract(c.event,'$.data.admission.request.workspace'),json_extract(c.event,'$.data.config.workspace'))=?2) ORDER BY e.sequence DESC LIMIT ?3")?;
        let mut rows = query.query(params![before, workspace, limit as i64])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let session = id
                .parse::<SessionId>()
                .map_err(|_| StoreError::Integrity("invalid session identity"))?;
            let revision = u64::try_from(row.get::<_, i64>(1)?)
                .map_err(|_| StoreError::Integrity("invalid input revision"))?;
            let sequence = u64::try_from(row.get::<_, i64>(2)?)
                .map_err(|_| StoreError::Integrity("invalid input sequence"))?;
            let bytes: Vec<u8> = row.get(3)?;
            let previous: Digest = row
                .get::<_, String>(5)?
                .parse()
                .map_err(|_| StoreError::Integrity("invalid predecessor hash"))?;
            if aggregate_hash("session", session.0, revision, Some(previous), &bytes)?.to_string()
                != row.get::<_, String>(4)?
            {
                return Err(StoreError::Integrity("input event integrity"));
            }
            let created: Vec<u8> = row.get(6)?;
            if aggregate_hash("session", session.0, 1, None, &created)?.to_string()
                != row.get::<_, String>(7)?
            {
                return Err(StoreError::Integrity("session origin integrity"));
            }
            let SessionEvent::Created {
                config, admission, ..
            } = serde_json::from_slice(&created)?
            else {
                return Err(StoreError::Integrity("session origin is not a creation"));
            };
            let SessionEvent::Command {
                command: SessionCommand::Input { content, .. },
                at_ms,
                ..
            } = serde_json::from_slice(&bytes)?
            else {
                return Err(StoreError::Integrity(
                    "input index does not match its event",
                ));
            };
            let mut text = String::new();
            for item in &content {
                if let Some(part) = item["content"].as_str() {
                    text.push_str(part);
                }
                if let Some(parts) = item["content"].as_array() {
                    for part in parts {
                        if let Some(part) = part["text"].as_str() {
                            text.push_str(part);
                        }
                    }
                }
            }
            let truncated = text.chars().count() > 512;
            result.push(crate::session::RecentInput {
                session,
                revision,
                sequence,
                at_ms,
                workspace: admission
                    .as_ref()
                    .map_or(config.workspace, |profile| profile.workspace().clone()),
                text: text.chars().take(512).collect(),
                truncated,
            });
        }
        Ok(result)
    }

    pub fn create_session(
        &mut self,
        id: SessionId,
        config: SessionConfig,
        parent: Option<SessionCursor>,
    ) -> Result<SessionState, StoreError> {
        let profile = self.fixture_admission(&config, BaselineReason::StoreFixture)?;
        self.create_session_seeded(id, config, profile, parent, None, false)
    }

    pub(crate) fn create_bound_session(
        &mut self,
        id: SessionId,
        profile: SessionAdmissionProfile,
        parent: Option<SessionCursor>,
    ) -> Result<SessionState, StoreError> {
        profile.validate().map_err(StoreError::Invalid)?;
        let config = SessionConfig {
            workspace: profile.workspace().clone(),
            model: profile.model(),
            instructions: String::new(),
            context_window_tokens: profile.context_window_tokens(),
        };
        self.create_session_seeded(id, config, profile, parent, None, false)
    }

    pub fn create_handoff_session(
        &mut self,
        id: SessionId,
        parent: SessionCursor,
    ) -> Result<SessionState, StoreError> {
        let source = self.load_session_cursor(&parent)?;
        let profile = source
            .admission
            .clone()
            .ok_or(StoreError::Invalid("source session is not admission-bound"))?;
        self.create_session_seeded(id, source.config, profile, Some(parent), None, true)
    }

    pub fn create_imported_session(
        &mut self,
        id: SessionId,
        config: SessionConfig,
        source: crate::session::ImportedSource,
        history: Vec<serde_json::Value>,
    ) -> Result<SessionState, StoreError> {
        self.artifacts.read(source.manifest)?;
        self.artifacts.read(source.source_snapshot)?;
        let profile = self.fixture_admission(&config, BaselineReason::LegacyImport)?;
        self.create_session_seeded(id, config, profile, None, Some((source, history)), false)
    }

    pub(crate) fn create_bound_imported_session(
        &mut self,
        id: SessionId,
        profile: SessionAdmissionProfile,
        source: crate::session::ImportedSource,
        history: Vec<serde_json::Value>,
    ) -> Result<SessionState, StoreError> {
        profile.validate().map_err(StoreError::Invalid)?;
        self.artifacts.read(source.manifest)?;
        self.artifacts.read(source.source_snapshot)?;
        let config = SessionConfig {
            workspace: profile.workspace().clone(),
            model: profile.model(),
            instructions: String::new(),
            context_window_tokens: profile.context_window_tokens(),
        };
        self.create_session_seeded(id, config, profile, None, Some((source, history)), false)
    }

    fn create_session_seeded(
        &mut self,
        id: SessionId,
        config: SessionConfig,
        admission: SessionAdmissionProfile,
        parent: Option<SessionCursor>,
        seed: Option<(crate::session::ImportedSource, Vec<serde_json::Value>)>,
        fresh_context: bool,
    ) -> Result<SessionState, StoreError> {
        if !config.workspace.is_absolute() || !config.workspace.is_dir() {
            return Err(StoreError::Invalid(
                "session workspace must be an absolute directory",
            ));
        }
        match self.load_session(id) {
            Ok(existing) => {
                return if existing.initial_config == config
                    && existing.admission.as_ref() == Some(&admission)
                    && existing.branch.fresh_context == fresh_context
                    && existing.parent == parent
                    && seed.as_ref().map_or(
                        existing.imported.is_none() || parent.is_some(),
                        |(source, _)| existing.imported.as_ref() == Some(source),
                    ) {
                    Ok(existing)
                } else {
                    Err(StoreError::Invalid(
                        "session ID reused with different configuration or import",
                    ))
                };
            }
            Err(StoreError::MissingSession(_)) => {}
            Err(error) => return Err(error),
        }
        let (history, imported, workspace) = match (&parent, seed) {
            (None, Some((source, history))) => (history, Some(source), None),
            (Some(_), Some(_)) => {
                return Err(StoreError::Invalid(
                    "session cannot be both a native fork and a new legacy import",
                ));
            }
            (Some(cursor), None) => {
                if cursor.version != 1 {
                    return Err(StoreError::Invalid("unsupported session cursor"));
                }
                let parent =
                    load_session_state(&self.connection, cursor.session, Some(cursor.revision))?.0;
                if parent.active_request.is_some() {
                    return Err(StoreError::Invalid(
                        "fork cursor must select a settled turn",
                    ));
                }
                if parent.branch.pending_task.is_some() {
                    return Err(StoreError::Invalid(
                        "parent workspace has no settled source checkpoint",
                    ));
                }
                if parent.branch.pending_shell.is_some() {
                    return Err(StoreError::Invalid(
                        "parent has an unresolved shell operation",
                    ));
                }
                if parent.admission.as_ref() != Some(&admission) {
                    return Err(StoreError::Invalid(
                        "child session admission differs from its parent",
                    ));
                }
                (
                    if fresh_context {
                        Vec::new()
                    } else {
                        parent.history
                    },
                    parent.imported,
                    parent.branch.workspace,
                )
            }
            (None, None) => (Vec::new(), None, None),
        };
        let branch = crate::session::SessionBranch {
            workspace,
            fresh_context,
            pending_task: None,
            pending_shell: None,
        };
        if let Some(seed) = &branch.workspace {
            crate::workspace::Snapshot::load(seed.origin, &self.artifacts)
                .and_then(|snapshot| snapshot.verify_artifacts(&self.artifacts))
                .map_err(|_| StoreError::Integrity("workspace seed origin is unavailable"))?;
            crate::workspace::Snapshot::load(seed.source, &self.artifacts)
                .and_then(|snapshot| snapshot.verify_artifacts(&self.artifacts))
                .map_err(|_| StoreError::Integrity("workspace seed source is unavailable"))?;
        }
        let at_ms = now_ms();
        let event = SessionEvent::Created {
            branch: branch.clone(),
            config: config.clone(),
            admission: Some(Box::new(admission.clone())),
            parent: parent.clone(),
            history: history.clone(),
            at_ms,
            imported: imported.clone().map(Box::new),
        };
        let bytes = serde_json::to_vec(&event)?;
        if bytes.len() > MAX_EVENT_BYTES {
            return Err(StoreError::Invalid(
                "initial session context exceeds journal limit",
            ));
        }
        let state = SessionState::create(
            id,
            crate::session::SessionCreation {
                branch,
                config,
                admission: Some(admission),
                parent,
                history,
                started_ms: at_ms,
                imported,
            },
        );
        let hash = aggregate_hash("session", id.0, 1, None, &bytes)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO sessions VALUES (?1,1,?2,?3)",
            params![
                id.to_string(),
                serde_json::to_vec(&state)?,
                hash.to_string()
            ],
        )?;
        transaction.execute(
            "INSERT INTO events(aggregate,kind,revision,event,hash) VALUES (?1,'session',1,?2,?3)",
            params![id.to_string(), bytes, hash.to_string()],
        )?;
        store_checkpoint(&transaction, "session", id.0, 1, &state, hash)?;
        transaction.commit()?;
        Ok(state)
    }

    pub fn load_session(&self, id: SessionId) -> Result<SessionState, StoreError> {
        load_session_state(&self.connection, id, None).map(|(state, _)| state)
    }

    /// Returns the root session identity used to route exact-prefix cache hits
    /// across a branch without merging session or thread identity.
    pub fn prompt_cache_lineage(&self, id: SessionId) -> Result<SessionId, StoreError> {
        let mut session = self.load_session(id)?;
        let mut lineage = id;
        let mut visited = std::collections::BTreeSet::from([id]);
        while let Some(parent) = session.parent {
            if !visited.insert(parent.session) {
                return Err(StoreError::Integrity("cyclic session ancestry"));
            }
            lineage = parent.session;
            session = self.load_session(parent.session)?;
        }
        Ok(lineage)
    }

    pub(crate) fn unbound_session_ids(&self) -> Result<Vec<SessionId>, StoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM sessions ORDER BY id")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                let id = id
                    .parse::<SessionId>()
                    .map_err(|_| StoreError::Integrity("invalid session identity"))?;
                self.load_session(id)
                    .map(|state| state.admission.is_none().then_some(id))
            })
            .filter_map(|result| match result {
                Ok(Some(id)) => Some(Ok(id)),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    pub(crate) fn pin_session_admission(
        &mut self,
        id: SessionId,
        profile: SessionAdmissionProfile,
    ) -> Result<SessionState, StoreError> {
        profile.validate().map_err(StoreError::Invalid)?;
        let state = self.load_session(id)?;
        if let Some(existing) = &state.admission {
            return if existing == &profile {
                Ok(state)
            } else {
                Err(StoreError::Integrity(
                    "session admission is already pinned differently",
                ))
            };
        }
        if state.config.workspace != *profile.workspace()
            || state.config.model != profile.model()
            || state.config.context_window_tokens != profile.context_window_tokens()
        {
            return Err(StoreError::Integrity(
                "legacy session configuration differs from its admission",
            ));
        }
        let legacy_config_digest = Digest::of_value(&state.config)?;
        self.session_command(
            id,
            state.revision,
            Uuid::new_v5(&id.0, b"orvek-admission-pin-v1"),
            SessionCommand::AdmissionPinned {
                profile: Box::new(profile),
                legacy_config_digest,
            },
        )
    }

    pub fn load_session_cursor(&self, cursor: &SessionCursor) -> Result<SessionState, StoreError> {
        if cursor.version != 1 {
            return Err(StoreError::Invalid("unsupported session cursor"));
        }
        let state = load_session_state(&self.connection, cursor.session, Some(cursor.revision))?.0;
        if state.admission.is_none() {
            return Err(StoreError::Invalid(
                "session cursor predates trusted admission",
            ));
        }
        Ok(state)
    }

    pub fn scoped_history(
        &self,
        actor: SessionId,
        source: SessionId,
        revision: Option<u64>,
    ) -> Result<SessionState, StoreError> {
        let mut scope = self.load_session(actor)?;
        for _ in 0..128 {
            if scope.id == source {
                let revision = revision.unwrap_or(scope.revision);
                if revision > scope.revision {
                    return Err(StoreError::Invalid("history lies beyond the branch cutoff"));
                }
                return self.load_session_cursor(&SessionCursor {
                    version: 1,
                    session: source,
                    revision,
                });
            }
            let parent = scope
                .parent
                .ok_or(StoreError::Invalid("history source is outside this branch"))?;
            scope = self.load_session_cursor(&parent)?;
        }
        Err(StoreError::Invalid(
            "history lineage exceeds its traversal bound",
        ))
    }

    pub fn start_task(
        &mut self,
        session_id: SessionId,
        operation: Uuid,
        contract: Contract,
    ) -> Result<(SessionState, TaskState, bool), StoreError> {
        contract.validate()?;
        self.start_record(
            session_id,
            operation,
            contract.request.clone(),
            contract.limits,
            Some(contract),
            None,
            None,
        )
    }

    pub fn start_request(
        &mut self,
        session_id: SessionId,
        operation: Uuid,
        request: String,
        limits: crate::contract::Limits,
        intake: Digest,
    ) -> Result<(SessionState, TaskState, bool), StoreError> {
        if request.trim().is_empty() {
            return Err(StoreError::Invalid("empty task request"));
        }
        limits.validate()?;
        let policy: crate::admission::RequestPolicy =
            serde_json::from_slice(&self.artifacts.read(intake)?)?;
        policy.validate()?;
        self.start_record(
            session_id,
            operation,
            request,
            limits,
            None,
            Some(intake),
            None,
        )
    }

    pub fn start_prepared_request(
        &mut self,
        session_id: SessionId,
        operation: Uuid,
        input: crate::input::PreparedInput,
        limits: crate::contract::Limits,
        intake: Digest,
    ) -> Result<(SessionState, TaskState, bool), StoreError> {
        if input.text.trim().is_empty() {
            return Err(StoreError::Invalid(
                "a new coding task requires a textual instruction with its images",
            ));
        }
        limits.validate()?;
        let policy: crate::admission::RequestPolicy =
            serde_json::from_slice(&self.artifacts.read(intake)?)?;
        policy.validate()?;
        let encoded = self.artifacts.read(input.artifact)?;
        if serde_json::from_slice::<Vec<serde_json::Value>>(&encoded)? != input.messages {
            return Err(StoreError::Integrity(
                "input payload differs from its artifact",
            ));
        }
        self.start_record(
            session_id,
            operation,
            input.text.clone(),
            limits,
            None,
            Some(intake),
            Some(input),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_record(
        &mut self,
        session_id: SessionId,
        operation: Uuid,
        request: String,
        limits: crate::contract::Limits,
        contract: Option<Contract>,
        intake: Option<Digest>,
        prepared: Option<crate::input::PreparedInput>,
    ) -> Result<(SessionState, TaskState, bool), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut session, mut session_head) = load_session_state(&transaction, session_id, None)?;
        if let Some(id) = session.tasks_by_request.get(&operation) {
            let (task, _) = load_state(&transaction, *id)?;
            let first: Vec<u8> = transaction.query_row(
                "SELECT event FROM events WHERE aggregate=?1 AND kind='task' AND revision=1",
                [id.to_string()],
                |row| row.get(0),
            )?;
            let same = match (serde_json::from_slice::<TaskEvent>(&first)?, &contract) {
                (
                    TaskEvent::Created {
                        contract: initial, ..
                    },
                    Some(contract),
                ) => initial == *contract,
                (
                    TaskEvent::Requested {
                        request: initial,
                        limits: initial_limits,
                        intake: initial_intake,
                        input: initial_input,
                        ..
                    },
                    None,
                ) => {
                    initial == request
                        && initial_limits == limits
                        && Some(initial_intake) == intake
                        && initial_input == prepared.as_ref().map(|input| input.artifact)
                }
                _ => false,
            };
            if !same {
                return Err(StoreError::Invalid(
                    "request ID reused with different task input",
                ));
            }
            return Ok((session, task, false));
        }
        // An ordinary request legitimately passes through here twice: once to
        // claim classification and again to adopt the resulting task, so a
        // recorded operation ID is not by itself a conflict. The check below
        // still requires that an already-active request be an ordinary one.
        if session
            .active_request
            .is_some_and(|active| active != operation)
            || (session.active_request == Some(operation)
                && !session
                    .submissions
                    .get(&operation)
                    .is_some_and(|submission| {
                        matches!(
                            submission.intent,
                            crate::submission::WorkIntent::Ordinary { .. }
                        )
                    }))
        {
            return Err(StoreError::Invalid("session request is already active"));
        }
        if let Some(submission) = session.submissions.get(&operation)
            && matches!(
                submission.intent,
                crate::submission::WorkIntent::Ordinary { .. }
            )
            && session
                .operations
                .contains_key(&Uuid::new_v5(&operation, b"classification-intended"))
        {
            // Classification is pre-dispatch accounting for this still-unadmitted input.
        } else if session.branch.pending_shell.is_some() {
            return Err(StoreError::Invalid(
                "reconcile the pending shell before starting another task",
            ));
        }
        if let Some(submission) = session.submissions.get(&operation)
            && (submission.status != crate::submission::SubmissionStatus::Running
                || prepared.as_ref().map(|input| input.artifact) != Some(submission.input)
                || !matches!(&submission.intent, crate::submission::WorkIntent::NewTask { limits: accepted_limits, policy } | crate::submission::WorkIntent::Ordinary { limits: accepted_limits, policy, .. } if *accepted_limits == limits && Some(*policy) == intake && contract.is_none()))
        {
            return Err(StoreError::Invalid(
                "execution differs from its admitted submission",
            ));
        }
        let id = TaskId::new();
        let at_ms = now_ms();
        let input = SessionCommand::Input {
            kind: RequestKind::Task,
            content: prepared
                .as_ref()
                .map(|input| input.messages.clone())
                .unwrap_or_else(|| vec![serde_json::json!({"role":"user","content":request})]),
        };
        let (event, task) = match contract {
            Some(contract) => (
                TaskEvent::Created {
                    contract: contract.clone(),
                    at_ms,
                },
                TaskState::created(id, contract, at_ms),
            ),
            None => (
                TaskEvent::Requested {
                    request: request.clone(),
                    limits,
                    intake: intake
                        .ok_or(StoreError::Invalid("request requires its intake policy"))?,
                    input: prepared.as_ref().map(|input| input.artifact),
                    at_ms,
                },
                TaskState::requested(
                    id,
                    request,
                    limits,
                    at_ms,
                    intake,
                    prepared.as_ref().map(|input| input.artifact),
                ),
            ),
        };
        check_artifact_budget(
            &transaction,
            id,
            &event,
            &self.artifacts,
            task.limits().artifact_bytes,
        )?;
        let bytes = event_bytes(&event)?;
        let hash = event_hash(id, 1, None, &bytes)?;
        transaction.execute(
            "INSERT INTO tasks VALUES (?1,1,?2,?3)",
            params![id.to_string(), serde_json::to_vec(&task)?, hash.to_string()],
        )?;
        transaction.execute(
            "INSERT INTO events(aggregate,kind,revision,event,hash) VALUES (?1,'task',1,?2,?3)",
            params![id.to_string(), bytes, hash.to_string()],
        )?;
        store_checkpoint(&transaction, "task", id.0, 1, &task, hash)?;
        append_session_command(
            &transaction,
            &mut session,
            &mut session_head,
            operation,
            input,
        )?;
        append_session_command(
            &transaction,
            &mut session,
            &mut session_head,
            Uuid::new_v5(&operation, b"task-link"),
            SessionCommand::TaskLinked {
                request: operation,
                task: id,
            },
        )?;
        transaction.commit()?;
        Ok((session, task, true))
    }

    pub fn sessions(&self, offset: usize, limit: usize) -> Result<Vec<SessionState>, StoreError> {
        if limit == 0 || limit > 64 {
            return Err(StoreError::Invalid("session page limit must be 1..64"));
        }
        let offset = i64::try_from(offset)
            .map_err(|_| StoreError::Invalid("session offset exceeds its bound"))?;
        let mut statement = self
            .connection
            .prepare("SELECT id FROM sessions ORDER BY rowid DESC LIMIT ?1 OFFSET ?2")?;
        let ids = statement
            .query_map(params![limit as i64, offset], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                self.load_session(
                    id.parse()
                        .map_err(|_| StoreError::Integrity("invalid session ID"))?,
                )
            })
            .collect()
    }

    pub fn resume_task(
        &mut self,
        session_id: SessionId,
        operation: Uuid,
        id: TaskId,
        revision: u64,
        reason: String,
    ) -> Result<(SessionState, TaskState, bool), StoreError> {
        self.resume_task_record(session_id, operation, id, revision, reason, true)
    }

    pub(crate) fn resume_task_without_budget_limit(
        &mut self,
        session_id: SessionId,
        operation: Uuid,
        id: TaskId,
        revision: u64,
        reason: String,
    ) -> Result<(SessionState, TaskState, bool), StoreError> {
        self.resume_task_record(session_id, operation, id, revision, reason, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn resume_task_record(
        &mut self,
        session_id: SessionId,
        operation: Uuid,
        id: TaskId,
        revision: u64,
        reason: String,
        enforce_budget: bool,
    ) -> Result<(SessionState, TaskState, bool), StoreError> {
        if reason.trim().is_empty() {
            return Err(StoreError::Invalid("resuming requires a user basis"));
        }
        let input = SessionCommand::Input {
            kind: RequestKind::Task,
            content: vec![serde_json::json!({"role":"user","content":reason})],
        };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut session, mut head) = load_session_state(&transaction, session_id, None)?;
        let (mut task, task_head) = load_state(&transaction, id)?;
        if let Some(previous) = session.tasks_by_request.get(&operation) {
            if *previous != id
                || session.operations.get(&operation) != Some(&Digest::of_value(&input)?)
            {
                return Err(StoreError::Invalid(
                    "resume request ID reused with different input",
                ));
            }
            return Ok((session, task, false));
        }
        if session.active_request.is_some() || session.operations.contains_key(&operation) {
            return Err(StoreError::Invalid("session already has an active request"));
        }
        if !session.tasks_by_request.values().any(|task| *task == id) {
            return Err(StoreError::Invalid("task does not belong to this session"));
        }
        if task.revision != revision {
            return Err(StoreError::Revision {
                expected: revision,
                actual: task.revision,
            });
        }
        if task.outcome.is_none() || task.outcome == Some(Outcome::Complete) {
            return Err(StoreError::Invalid(
                "resume requires a settled incomplete task",
            ));
        }
        if task.jobs.values().any(|job| job.status.unresolved())
            || task.effects.values().any(|effect| {
                matches!(
                    effect.status,
                    EffectStatus::Intended | EffectStatus::Unknown
                )
            })
            || task.model_reservations.iter().any(|operation| {
                !task.model_receipts.get(operation).is_some_and(|receipt| {
                    receipt.tokens.is_some() && receipt.status != ModelCallStatus::Unknown
                })
            })
        {
            return Err(StoreError::Invalid(
                "reconcile unfinished jobs, effects and provider attempts before resuming",
            ));
        }
        task.cancellation_requested = false;
        if enforce_budget {
            check_budget(&task)?;
        }
        // No in-memory reset is committed independently of the reopening event.
        append_task_event(
            &transaction,
            &mut task,
            task_head,
            TaskEvent::Reopened { reason },
            &self.artifacts,
        )?;
        append_session_command(&transaction, &mut session, &mut head, operation, input)?;
        append_session_command(
            &transaction,
            &mut session,
            &mut head,
            Uuid::new_v5(&operation, b"task-link"),
            SessionCommand::TaskLinked {
                request: operation,
                task: id,
            },
        )?;
        transaction.commit()?;
        Ok((session, task, true))
    }

    pub fn session_command(
        &mut self,
        id: SessionId,
        revision: u64,
        operation: Uuid,
        command: SessionCommand,
    ) -> Result<SessionState, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut state, head) = load_session_state(&transaction, id, None)?;
        let fingerprint = Digest::of_value(&command)?;
        if let Some(previous) = state.operations.get(&operation) {
            return if *previous == fingerprint {
                Ok(state)
            } else {
                Err(StoreError::Invalid(
                    "command ID reused with different input",
                ))
            };
        }
        if state.revision != revision {
            return Err(StoreError::Revision {
                expected: revision,
                actual: state.revision,
            });
        }
        match &command {
            SessionCommand::Response { request, .. }
            | SessionCommand::Interpreter { request, .. }
            | SessionCommand::ProviderUsage { request, .. }
            | SessionCommand::WorkspaceSaved { request, .. }
            | SessionCommand::ToolResult { request, .. }
            | SessionCommand::ToolResultPart { request, .. }
            | SessionCommand::ToolResultEnd { request, .. }
            | SessionCommand::TaskLinked { request, .. }
            | SessionCommand::TurnSettled { request, .. }
                if state.active_request != Some(*request) =>
            {
                return Err(StoreError::Invalid(
                    "late result belongs to a superseded request",
                ));
            }
            _ => {}
        }
        match &command {
            SessionCommand::Interpreter { request, event } => {
                use crate::interpreter::InterpreterEvent;
                match event {
                    InterpreterEvent::Started {
                        cell,
                        task,
                        outer_call,
                        source,
                        environment,
                        ..
                    } => {
                        if state.interpreter.cells.contains_key(cell)
                            || state.tasks_by_request.get(request) != Some(task)
                            || !state.tool_calls.get(outer_call).is_some_and(|call| {
                                call.request == *request && call.output.is_none()
                            })
                        {
                            return Err(StoreError::Invalid(
                                "interpreter cell has no pending outer call",
                            ));
                        }
                        self.artifacts.read(*source)?;
                        self.artifacts.read(*environment)?;
                    }
                    InterpreterEvent::CallStarted {
                        cell,
                        ordinal,
                        call_id,
                        arguments,
                        ..
                    } => {
                        if *ordinal == 0
                            || *call_id != format!("{cell}/{ordinal}")
                            || state.interpreter.pending_calls.contains_key(call_id)
                            || !state.interpreter.cells.get(cell).is_some_and(|cell| {
                                cell.request == *request && cell.status.pending()
                            })
                        {
                            return Err(StoreError::Invalid(
                                "interpreter call has no active owning cell",
                            ));
                        }
                        self.artifacts.read(*arguments)?;
                    }
                    InterpreterEvent::CallSettled {
                        cell,
                        ordinal,
                        result,
                    } => {
                        if state
                            .interpreter
                            .pending_calls
                            .get(&format!("{cell}/{ordinal}"))
                            != Some(cell)
                            || !state.interpreter.cells.get(cell).is_some_and(|cell| {
                                cell.request == *request && cell.status.pending()
                            })
                        {
                            return Err(StoreError::Invalid(
                                "interpreter result has no pending call",
                            ));
                        }
                        self.artifacts.read(*result)?;
                    }
                    InterpreterEvent::Settled {
                        cell,
                        result,
                        checkpoint,
                        ..
                    } => {
                        if !state
                            .interpreter
                            .cells
                            .get(cell)
                            .is_some_and(|cell| cell.request == *request && cell.status.pending())
                            || state
                                .interpreter
                                .pending_calls
                                .values()
                                .any(|owner| owner == cell)
                        {
                            return Err(StoreError::Invalid(
                                "interpreter cell is not ready to settle",
                            ));
                        }
                        self.artifacts.read(*result)?;
                        if let Some(checkpoint) = checkpoint {
                            self.artifacts.read(checkpoint.artifact)?;
                        }
                    }
                }
            }
            SessionCommand::AdmissionPinned { profile, .. } => {
                profile.validate().map_err(StoreError::Invalid)?
            }
            SessionCommand::WorkspaceSaved { request, seed } => {
                let id = state
                    .tasks_by_request
                    .get(request)
                    .ok_or(StoreError::Invalid(
                        "workspace publication has no task owner",
                    ))?;
                let task = load_state(&transaction, *id)?.0;
                if seed.task != Some(*id)
                    || task.workspace_origin.or(task.origin) != Some(seed.origin)
                    || !task.candidate.as_ref().is_some_and(|candidate| {
                        candidate.frozen && candidate.source == seed.source
                    })
                    || task
                        .jobs
                        .values()
                        .any(|job| job.mutates_candidate && job.status.unresolved())
                {
                    return Err(StoreError::Invalid(
                        "workspace publication differs from its frozen task source",
                    ));
                }
                for digest in [seed.origin, seed.source] {
                    crate::workspace::Snapshot::load(digest, &self.artifacts)
                        .and_then(|snapshot| snapshot.verify_artifacts(&self.artifacts))
                        .map_err(|_| StoreError::Integrity("published workspace is unavailable"))?;
                }
            }
            SessionCommand::Response { items, .. } => {
                if items.len() > 256
                    || items
                        .iter()
                        .filter(|item| item["type"] == "function_call")
                        .count()
                        > 64
                {
                    return Err(StoreError::Invalid(
                        "provider response exceeds item or tool proposal limits",
                    ));
                }
                let mut seen = state
                    .tool_calls
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>();
                for item in items.iter().filter(|item| item["type"] == "function_call") {
                    let id = item["call_id"]
                        .as_str()
                        .filter(|id| !id.is_empty() && id.len() <= 256)
                        .ok_or(StoreError::Invalid("tool proposal requires an ID"))?;
                    if !seen.insert(id)
                        || state
                            .history
                            .iter()
                            .any(|item| item["type"] == "function_call" && item["call_id"] == id)
                    {
                        return Err(StoreError::Invalid(
                            "provider reused a tool call ID; execution was not repeated",
                        ));
                    }
                    if !item["name"]
                        .as_str()
                        .is_some_and(|name| !name.is_empty() && name.len() <= 128)
                        || !item["arguments"].is_string()
                    {
                        return Err(StoreError::Invalid("malformed tool proposal"));
                    }
                }
            }
            SessionCommand::ToolResult {
                request, call_id, ..
            }
            | SessionCommand::ToolResultPart {
                request, call_id, ..
            }
            | SessionCommand::ToolResultEnd {
                request, call_id, ..
            } => {
                if !state
                    .tool_calls
                    .get(call_id)
                    .is_some_and(|call| call.request == *request && call.output.is_none())
                {
                    return Err(StoreError::Invalid(
                        "tool result has no pending call in this request",
                    ));
                }
            }
            SessionCommand::Input { content, .. } => {
                if state.active_request.is_some() {
                    return Err(StoreError::Invalid("session already has an active request"));
                }
                if content.is_empty() {
                    return Err(StoreError::Invalid("empty input"));
                }
            }
            SessionCommand::TaskLinked { task, .. } => {
                load_state(&transaction, *task)?;
            }
            SessionCommand::TurnSettled {
                request,
                outcome: Some(outcome),
                ..
            } => {
                let task = state
                    .tasks_by_request
                    .get(request)
                    .copied()
                    .ok_or(StoreError::Invalid("task outcome without a task"))?;
                if load_state(&transaction, task)?.0.outcome != Some(*outcome) {
                    return Err(StoreError::Invalid(
                        "session cannot manufacture task outcomes",
                    ));
                }
            }
            SessionCommand::ContextProjected {
                source_revision, ..
            } if *source_revision != state.revision => {
                return Err(StoreError::Invalid("stale context projection"));
            }
            SessionCommand::ContextProjected {
                view: Some(view),
                projection,
                ..
            } => {
                if !projection.is_empty() || !view.valid_for(&state) {
                    return Err(StoreError::Invalid(
                        "context view does not match its source",
                    ));
                }
            }
            SessionCommand::ContextProjected { view: None, .. } => {
                return Err(StoreError::Invalid("context view manifest is required"));
            }
            _ => {}
        }
        let mut head = head;
        append_session_command(&transaction, &mut state, &mut head, operation, command)?;
        transaction.commit()?;
        Ok(state)
    }

    pub fn session_cost(&self, session: SessionId) -> Result<ProviderCostSummary, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT event FROM events WHERE aggregate=?1 AND kind='session' ORDER BY revision",
        )?;
        let mut rows = statement.query(params![session.to_string()])?;
        let mut usage_calls = BTreeSet::new();
        let mut receipts = BTreeMap::new();
        let mut uncertain = false;
        while let Some(row) = rows.next()? {
            let bytes: Vec<u8> = row.get(0)?;
            let event: SessionEvent = serde_json::from_slice(&bytes)?;
            let SessionEvent::Command { command, .. } = event else {
                continue;
            };
            match command {
                SessionCommand::ProviderUsage {
                    call: Some(call), ..
                } => {
                    usage_calls.insert(call);
                }
                SessionCommand::ProviderUsage { call: None, .. } => uncertain = true,
                SessionCommand::ProviderCost { call, cost_usd, .. } => {
                    if receipts
                        .get(&call)
                        .is_some_and(|previous| *previous != cost_usd)
                    {
                        uncertain = true;
                    } else {
                        receipts.entry(call).or_insert(cost_usd);
                    }
                }
                _ => {}
            }
        }
        if usage_calls.iter().any(|call| !receipts.contains_key(call)) {
            uncertain = true;
        }
        let mut total = crate::inference::UsdCost::ZERO;
        for cost in receipts.values() {
            match cost.and_then(|cost| total.checked_add(cost)) {
                Some(next) => total = next,
                None => uncertain = true,
            }
        }
        Ok(ProviderCostSummary { total, uncertain })
    }

    pub fn journal_page(&self, after: u64, limit: u32) -> Result<Vec<JournalRecord>, StoreError> {
        if limit == 0 || limit > 256 {
            return Err(StoreError::Invalid("journal page limit must be 1..256"));
        }
        let after = i64::try_from(after)
            .map_err(|_| StoreError::Invalid("cursor exceeds journal range"))?;
        let mut statement = self.connection.prepare("SELECT sequence,aggregate,kind,revision,event,hash FROM events WHERE sequence>?1 ORDER BY sequence LIMIT ?2")?;
        let mut rows = statement.query(params![after, limit])?;
        let mut records = Vec::new();
        let mut encoded_bytes = 0usize;
        while let Some(row) = rows.next()? {
            let aggregate: String = row.get(1)?;
            let kind: String = row.get(2)?;
            let revision = u64::try_from(row.get::<_, i64>(3)?)
                .map_err(|_| StoreError::Integrity("invalid journal revision"))?;
            let bytes: Vec<u8> = row.get(4)?;
            let previous: Option<String> = if revision > 1 {
                self.connection
                    .query_row(
                        "SELECT hash FROM events WHERE aggregate=?1 AND kind=?2 AND revision=?3",
                        params![aggregate, kind, (revision - 1) as i64],
                        |row| row.get(0),
                    )
                    .optional()?
            } else {
                None
            };
            if revision > 1 && previous.is_none() {
                return Err(StoreError::Integrity("journal predecessor missing"));
            }
            let previous = previous
                .map(|s| {
                    s.parse()
                        .map_err(|_| StoreError::Integrity("invalid predecessor hash"))
                })
                .transpose()?;
            let identity = Uuid::parse_str(&aggregate)
                .map_err(|_| StoreError::Integrity("invalid aggregate identity"))?;
            if aggregate_hash(&kind, identity, revision, previous, &bytes)?.to_string()
                != row.get::<_, String>(5)?
            {
                return Err(StoreError::Integrity("journal event integrity"));
            }
            let record = JournalRecord {
                sequence: row.get::<_, i64>(0)? as u64,
                aggregate,
                kind,
                revision,
                event: serde_json::from_slice(&bytes)?,
            };
            let record_bytes = serde_json::to_vec(&record)?.len();
            let separator = usize::from(!records.is_empty());
            if encoded_bytes
                .checked_add(separator)
                .and_then(|bytes| bytes.checked_add(record_bytes))
                .is_none_or(|bytes| bytes > MAX_JOURNAL_PAGE_BYTES)
            {
                break;
            }
            encoded_bytes += separator + record_bytes;
            records.push(record);
        }
        Ok(records)
    }

    pub fn create(&mut self, contract: Contract) -> Result<TaskState, StoreError> {
        contract.validate()?;
        let id = TaskId::new();
        let at_ms = now_ms();
        let event = TaskEvent::Created {
            contract: contract.clone(),
            at_ms,
        };
        let state = TaskState::created(id, contract, at_ms);
        let bytes = event_bytes(&event)?;
        let hash = event_hash(id, 1, None, &bytes)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_artifact_budget(
            &transaction,
            id,
            &event,
            &self.artifacts,
            state.limits().artifact_bytes,
        )?;
        transaction.execute(
            "INSERT INTO tasks VALUES (?1,1,?2,?3)",
            params![
                id.to_string(),
                serde_json::to_vec(&state)?,
                hash.to_string()
            ],
        )?;
        transaction.execute(
            "INSERT INTO events(aggregate,kind,revision,event,hash) VALUES (?1,'task',1,?2,?3)",
            params![id.to_string(), bytes, hash.to_string()],
        )?;
        store_checkpoint(&transaction, "task", id.0, 1, &state, hash)?;
        transaction.commit()?;
        Ok(state)
    }

    pub fn load(&self, id: TaskId) -> Result<TaskState, StoreError> {
        load_state(&self.connection, id).map(|(state, _)| state)
    }

    pub fn audit_evidence(&mut self, id: TaskId) -> Result<TaskState, StoreError> {
        let state = self.load(id)?;
        if state.outcome != Some(Outcome::Complete) {
            return Ok(state);
        }
        let verification = (|| -> Result<(), StoreError> {
            let certificate = state
                .certificates
                .last()
                .ok_or(StoreError::Integrity("completed task has no certificate"))?;
            let mut referenced = BTreeSet::from([
                (certificate.source, BlobKind::Snapshot),
                (certificate.artifact, BlobKind::Opaque),
                (certificate.environment, BlobKind::Opaque),
                (certificate.delivery_receipt, BlobKind::Opaque),
            ]);
            if let Some(proof) = state
                .candidate
                .as_ref()
                .and_then(|candidate| candidate.provenance)
            {
                referenced.insert((proof, BlobKind::PatchReceipt));
            }
            for evidence in state.evidence.iter().filter(|evidence| {
                certificate
                    .evidence
                    .values()
                    .any(|id| *id == evidence.job_id)
            }) {
                referenced.extend([
                    (evidence.observation.report, BlobKind::Opaque),
                    (evidence.identity.verifier, BlobKind::Opaque),
                ]);
                if let Some(control) = &evidence.observation.control {
                    referenced.extend([
                        (control.source, BlobKind::Snapshot),
                        (control.report, BlobKind::Opaque),
                    ]);
                }
            }
            verify_artifact_graph(referenced, &self.artifacts, state.limits().artifact_bytes)?;
            Ok(())
        })();
        match verification {
            Ok(()) => Ok(state),
            Err(error) => self.change(id, Some(state.revision), true, |_, _, _| {
                Ok(TaskEvent::EvidenceInvalidated {
                    reason: format!("completion evidence unavailable or corrupt: {error}"),
                })
            }),
        }
    }

    pub fn list(&self) -> Result<Vec<TaskId>, StoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM tasks ORDER BY rowid DESC")?;
        let values = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        values
            .into_iter()
            .map(|id| {
                Uuid::parse_str(&id)
                    .map(TaskId)
                    .map_err(|_| StoreError::Integrity("invalid task ID"))
            })
            .collect()
    }

    pub fn select_candidate(
        &mut self,
        id: TaskId,
        revision: u64,
        candidate: Candidate,
    ) -> Result<TaskState, StoreError> {
        self.change(id, Some(revision), false, |state, _, artifacts| {
            for digest in [candidate.source, candidate.environment, candidate.artifact] {
                artifacts.read(digest)?;
            }
            if candidate.artifact != candidate.source {
                let proof = candidate.provenance.ok_or(StoreError::Invalid(
                    "derived artifact requires reproduction evidence",
                ))?;
                let receipt: crate::delivery::PatchValidationReceipt =
                    serde_json::from_slice(&artifacts.read(proof)?)?;
                if receipt.patch != candidate.artifact
                    || receipt.candidate != candidate.source
                    || receipt.applied_snapshot != candidate.source
                    || !receipt
                        .commands
                        .iter()
                        .all(|command| command.process_group_quiescent)
                {
                    return Err(StoreError::Invalid(
                        "artifact reproduction evidence does not match the candidate",
                    ));
                }
            }
            if state
                .jobs
                .values()
                .any(|job| job.mutates_candidate && job.status.unresolved())
            {
                return Err(StoreError::Invalid("candidate has active writers"));
            }
            Ok(TaskEvent::CandidateSelected(candidate))
        })
    }

    pub fn establish_workspace(
        &mut self,
        id: TaskId,
        revision: u64,
        origin: Digest,
        baseline: Candidate,
    ) -> Result<TaskState, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut task, head) = load_state(&transaction, id)?;
        if task.revision != revision {
            return Err(StoreError::Revision {
                expected: revision,
                actual: task.revision,
            });
        }
        if task.outcome.is_some()
            || task.origin.is_some()
            || task.baseline.is_some()
            || !baseline.frozen
        {
            return Err(StoreError::Invalid(
                "workspace must be admitted exactly once",
            ));
        }
        for digest in [origin, baseline.source] {
            crate::workspace::Snapshot::load(digest, &self.artifacts)
                .and_then(|snapshot| snapshot.verify_artifacts(&self.artifacts))
                .map_err(|_| StoreError::Integrity("workspace admission has unavailable source"))?;
        }
        self.artifacts.read(baseline.environment)?;
        self.artifacts.read(baseline.artifact)?;
        append_task_event(
            &transaction,
            &mut task,
            head,
            TaskEvent::OriginCaptured { source: origin },
            &self.artifacts,
        )?;
        let head = load_state(&transaction, id)?.1;
        append_task_event(
            &transaction,
            &mut task,
            head,
            TaskEvent::BaselineEstablished(baseline),
            &self.artifacts,
        )?;
        transaction.commit()?;
        Ok(task)
    }

    pub fn save_task_workspace(
        &mut self,
        session: SessionId,
        request: Uuid,
        task: TaskId,
    ) -> Result<SessionState, StoreError> {
        let state = self.load_session(session)?;
        let task = self.load(task)?;
        let (Some(origin), Some(candidate)) =
            (task.workspace_origin.or(task.origin), task.candidate)
        else {
            return Ok(state);
        };
        if !candidate.frozen
            || task
                .jobs
                .values()
                .any(|job| job.mutates_candidate && job.status.unresolved())
        {
            return Ok(state);
        }
        self.session_command(
            session,
            state.revision,
            Uuid::new_v5(&request, b"workspace-saved"),
            SessionCommand::WorkspaceSaved {
                request,
                seed: crate::session::WorkspaceSeed {
                    origin,
                    source: candidate.source,
                    task: Some(task.id),
                },
            },
        )
    }

    pub fn establish_baseline(
        &mut self,
        id: TaskId,
        revision: u64,
        baseline: Candidate,
    ) -> Result<TaskState, StoreError> {
        self.change(id, Some(revision), false, |state, _, artifacts| {
            if state.baseline.is_some() || !baseline.frozen {
                return Err(StoreError::Invalid(
                    "baseline must be immutable and established once",
                ));
            }
            for digest in [baseline.source, baseline.environment, baseline.artifact] {
                artifacts.read(digest)?;
            }
            Ok(TaskEvent::BaselineEstablished(baseline))
        })
    }

    pub fn invalidate_candidate(
        &mut self,
        id: TaskId,
        revision: u64,
        reason: String,
    ) -> Result<TaskState, StoreError> {
        self.change(id, Some(revision), false, |_, _, _| {
            Ok(TaskEvent::WorkspaceChanged { reason })
        })
    }

    pub fn request_cancellation(&mut self, id: TaskId) -> Result<TaskState, StoreError> {
        let state = self.load(id)?;
        if state.cancellation_requested || state.outcome.is_some() {
            return Ok(state);
        }
        self.change(id, Some(state.revision), false, |_, _, _| {
            Ok(TaskEvent::CancellationRequested)
        })
    }

    /// Only the authenticated user/application amendment path may call this method.
    pub fn amend_contract(
        &mut self,
        id: TaskId,
        revision: u64,
        contract: Contract,
        user_basis: String,
    ) -> Result<TaskState, StoreError> {
        contract.validate()?;
        if user_basis.trim().is_empty() {
            return Err(StoreError::Invalid(
                "contract amendment requires a recorded user basis",
            ));
        }
        self.change(id, Some(revision), false, |state, _, _| {
            state.accepted_contract()?;
            if state.request != contract.request {
                return Err(StoreError::Invalid(
                    "amendments must preserve the original request",
                ));
            }
            Ok(TaskEvent::ContractAmended {
                contract,
                reason: user_basis,
            })
        })
    }

    pub fn admit_contract(
        &mut self,
        id: TaskId,
        revision: u64,
        contract: Contract,
        basis: String,
        receipt: Digest,
    ) -> Result<TaskState, StoreError> {
        contract.validate()?;
        if basis.trim().is_empty() {
            return Err(StoreError::Invalid(
                "contract admission requires a recorded basis",
            ));
        }
        self.change(id, Some(revision), false, |state, _, artifacts| {
            if state.contract.is_some() {
                return Err(StoreError::Invalid("contract is already admitted"));
            }
            if contract.request != state.request || contract.limits != state.initial_limits {
                return Err(StoreError::Invalid(
                    "contract admission cannot rewrite the request or its authorized limits",
                ));
            }
            artifacts.read(receipt)?;
            Ok(TaskEvent::ContractAdmitted {
                contract,
                basis,
                receipt,
            })
        })
    }

    pub fn set_phase(
        &mut self,
        id: TaskId,
        revision: u64,
        phase: Phase,
    ) -> Result<TaskState, StoreError> {
        self.change(id, Some(revision), false, |state, _, _| {
            if !matches!(phase, Phase::Understand | Phase::Baseline) {
                if state.amendment_pending {
                    return Err(StoreError::Invalid(
                        "user follow-up awaits contract admission",
                    ));
                }
                state.accepted_contract()?;
            }
            Ok(TaskEvent::PhaseChanged(phase))
        })
    }

    pub fn begin_check(
        &mut self,
        id: TaskId,
        revision: u64,
        check: &str,
    ) -> Result<VerificationLease, StoreError> {
        let token = Zeroizing::new(Uuid::new_v4().to_string());
        let job_id = Uuid::new_v4();
        self.change(id, Some(revision), false, |state, transaction, _| {
            if state.amendment_pending {
                return Err(StoreError::Invalid(
                    "user follow-up awaits contract admission",
                ));
            }
            admit_job(state, true)?;
            let candidate =
                state
                    .candidate
                    .as_ref()
                    .filter(|c| c.frozen)
                    .ok_or(StoreError::Invalid(
                        "verification requires a frozen candidate",
                    ))?;
            let definition = state
                .accepted_contract()?
                .checks
                .get(check)
                .ok_or(StoreError::Invalid("unknown acceptance check"))?;
            let identity = EvidenceIdentity {
                contract: state.accepted_contract()?.digest()?,
                source: candidate.source,
                environment: candidate.environment,
                artifact: candidate.artifact,
                check_definition: Digest::of_value(definition)?,
                verifier: definition.verifier,
            };
            let started_ms = now_ms();
            let job = Job {
                execution_receipt: None,
                invocation: None,
                fence_receipt: None,
                id: job_id,
                generation: state.generation,
                status: JobStatus::Running,
                mutates_candidate: false,
                check: Some(check.to_owned()),
                identity: Some(identity),
                started_ms,
                deadline_ms: started_ms.saturating_add(definition.timeout_ms),
            };
            transaction.execute(
                "INSERT INTO leases VALUES (?1,?2,?3)",
                params![
                    id.to_string(),
                    job_id.to_string(),
                    Digest::of(token.as_bytes()).to_string()
                ],
            )?;
            Ok(TaskEvent::JobStarted(job))
        })?;
        Ok(VerificationLease {
            task: id,
            job: job_id,
            token,
        })
    }

    pub fn finish_check(
        &mut self,
        lease: VerificationLease,
        observation: Observation,
    ) -> Result<TaskState, StoreError> {
        self.change(lease.task, None, false, |state, transaction, artifacts| {
            let expected: Option<String> = transaction
                .query_row(
                    "SELECT token_digest FROM leases WHERE task=?1 AND job=?2",
                    params![lease.task.to_string(), lease.job.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            if expected.as_deref() != Some(Digest::of(lease.token.as_bytes()).to_string().as_str())
            {
                return Err(StoreError::Lease);
            }
            let job = state
                .jobs
                .get(&lease.job)
                .filter(|job| {
                    job.status == JobStatus::Running && job.generation == state.generation
                })
                .ok_or(StoreError::Lease)?;
            let finished_ms = now_ms();
            if finished_ms > job.deadline_ms && observation.status == CheckStatus::Passed {
                return Err(StoreError::Lease);
            }
            artifacts.read(observation.report)?;
            if let Some(control) = &observation.control {
                artifacts.read(control.report)?;
                artifacts.read(control.source)?;
            }
            let evidence = Evidence {
                job_id: job.id,
                generation: job.generation,
                check: job.check.clone().ok_or(StoreError::Lease)?,
                identity: job.identity.clone().ok_or(StoreError::Lease)?,
                started_ms: job.started_ms,
                finished_ms,
                observation,
            };
            transaction.execute(
                "DELETE FROM leases WHERE task=?1 AND job=?2",
                params![lease.task.to_string(), lease.job.to_string()],
            )?;
            Ok(TaskEvent::Observed(evidence))
        })
    }

    pub fn start_job(
        &mut self,
        id: TaskId,
        revision: u64,
        mutates_candidate: bool,
        timeout_ms: u64,
    ) -> Result<(TaskState, Uuid), StoreError> {
        self.start_job_record(id, revision, mutates_candidate, timeout_ms, None, true)
    }

    pub fn start_execution_job(
        &mut self,
        id: TaskId,
        revision: u64,
        mutates_candidate: bool,
        timeout_ms: u64,
        invocation: JobInvocation,
    ) -> Result<(TaskState, Uuid), StoreError> {
        self.start_job_record(
            id,
            revision,
            mutates_candidate,
            timeout_ms,
            Some(invocation),
            true,
        )
    }

    pub(crate) fn start_execution_job_without_budget_limit(
        &mut self,
        id: TaskId,
        revision: u64,
        mutates_candidate: bool,
        timeout_ms: u64,
        invocation: JobInvocation,
    ) -> Result<(TaskState, Uuid), StoreError> {
        self.start_job_record(
            id,
            revision,
            mutates_candidate,
            timeout_ms,
            Some(invocation),
            false,
        )
    }

    fn start_job_record(
        &mut self,
        id: TaskId,
        revision: u64,
        mutates_candidate: bool,
        timeout_ms: u64,
        invocation: Option<JobInvocation>,
        enforce_budget: bool,
    ) -> Result<(TaskState, Uuid), StoreError> {
        let job_id = Uuid::new_v4();
        let state = self.change(
            id,
            Some(revision),
            false,
            |state, transaction, artifacts| {
                admit_job(state, enforce_budget)?;
                // Request-owned workspace execution does not require contract admission.
                if invocation.is_none() && mutates_candidate && state.amendment_pending {
                    return Err(StoreError::Invalid(
                        "user follow-up awaits contract admission",
                    ));
                }
                if let Some(invocation) = &invocation {
                    artifacts.read(invocation.input)?;
                    artifacts.read(invocation.environment)?;
                    let (session, _) = load_session_state(transaction, invocation.session, None)?;
                    if session.tasks_by_request.get(&invocation.request) != Some(&id)
                        || session.active_request != Some(invocation.request)
                    {
                        return Err(StoreError::Invalid(
                            "execution actor does not own this active request",
                        ));
                    }
                    if let Some(call) = &invocation.call_id
                        && !session.tool_calls.get(call).is_some_and(|call| {
                            call.request == invocation.request && call.output.is_none()
                        })
                        && !session.interpreter.admits(call, invocation.request, id)
                    {
                        return Err(StoreError::Invalid(
                            "execution has no pending admitted tool call",
                        ));
                    }
                }
                if invocation.is_none()
                    && mutates_candidate
                    && !state.accepted_contract()?.open_questions.is_empty()
                {
                    return Err(StoreError::Invalid(
                        "material product decisions remain unresolved",
                    ));
                }
                if timeout_ms == 0 {
                    return Err(StoreError::Invalid("job deadline must be positive"));
                }
                if mutates_candidate && state.candidate.as_ref().is_some_and(|c| c.frozen) {
                    return Err(StoreError::Invalid("frozen candidate cannot be mutated"));
                }
                let started_ms = now_ms();
                Ok(TaskEvent::JobStarted(Job {
                    execution_receipt: None,
                    invocation,
                    fence_receipt: None,
                    id: job_id,
                    generation: state.generation,
                    status: JobStatus::Running,
                    mutates_candidate,
                    check: None,
                    identity: None,
                    started_ms,
                    deadline_ms: started_ms.saturating_add(timeout_ms),
                }))
            },
        )?;
        Ok((state, job_id))
    }

    pub fn settle_job(
        &mut self,
        id: TaskId,
        job_id: Uuid,
        status: JobStatus,
    ) -> Result<TaskState, StoreError> {
        self.settle_job_record(id, job_id, status, None)
    }

    pub fn settle_execution_job(
        &mut self,
        id: TaskId,
        job_id: Uuid,
        status: JobStatus,
        receipt: Digest,
    ) -> Result<TaskState, StoreError> {
        self.settle_job_record(id, job_id, status, Some(receipt))
    }

    fn settle_job_record(
        &mut self,
        id: TaskId,
        job_id: Uuid,
        status: JobStatus,
        receipt: Option<Digest>,
    ) -> Result<TaskState, StoreError> {
        if status == JobStatus::Running {
            return Err(StoreError::Invalid("settlement cannot restart a job"));
        }
        if status == JobStatus::Fenced {
            return Err(StoreError::Invalid(
                "fencing requires a termination receipt",
            ));
        }
        self.change(id, None, true, |state, transaction, artifacts| {
            let job = state
                .jobs
                .get(&job_id)
                .ok_or(StoreError::Invalid("unknown job"))?;
            if job.invocation.is_some() && receipt.is_none() {
                return Err(StoreError::Invalid(
                    "execution settlement requires a host receipt",
                ));
            }
            if let Some(receipt) = receipt {
                artifacts.read(receipt)?;
            }
            if !job.status.unresolved() {
                return Err(StoreError::Invalid("job already settled"));
            }
            if status == JobStatus::Succeeded && job.check.is_some() {
                return Err(StoreError::Invalid(
                    "checks settle through the protected evidence channel",
                ));
            }
            transaction.execute(
                "DELETE FROM leases WHERE task=?1 AND job=?2",
                params![id.to_string(), job_id.to_string()],
            )?;
            Ok(TaskEvent::JobSettled {
                id: job_id,
                status,
                receipt,
            })
        })
    }

    pub fn fence_job(
        &mut self,
        id: TaskId,
        job_id: Uuid,
        receipt: Digest,
    ) -> Result<TaskState, StoreError> {
        let state = self.load(id)?;
        if state.jobs.get(&job_id).is_some_and(|job| {
            job.status == JobStatus::Fenced && job.fence_receipt == Some(receipt)
        }) {
            return Ok(state);
        }
        self.change(id, None, true, |state, transaction, artifacts| {
            if !state
                .jobs
                .get(&job_id)
                .is_some_and(|job| job.status.unresolved())
            {
                return Err(StoreError::Invalid("only unresolved jobs can be fenced"));
            }
            artifacts.read(receipt)?;
            transaction.execute(
                "DELETE FROM leases WHERE task=?1 AND job=?2",
                params![id.to_string(), job_id.to_string()],
            )?;
            Ok(TaskEvent::JobFenced {
                id: job_id,
                receipt,
            })
        })
    }

    pub fn record_finding(
        &mut self,
        id: TaskId,
        revision: u64,
        finding: Finding,
    ) -> Result<TaskState, StoreError> {
        if finding.id.trim().is_empty()
            || finding.description.trim().is_empty()
            || (finding.resolved
                && !finding
                    .resolution
                    .as_ref()
                    .is_some_and(|s| !s.trim().is_empty()))
        {
            return Err(StoreError::Invalid(
                "findings require identity, description and explicit resolution",
            ));
        }
        self.change(id, Some(revision), false, |_, _, _| {
            Ok(TaskEvent::FindingRecorded(finding))
        })
    }

    pub fn record_effect(
        &mut self,
        id: TaskId,
        revision: u64,
        effect: Effect,
    ) -> Result<TaskState, StoreError> {
        self.change(id, Some(revision), true, |state, _, _| {
            if let Some(previous) = state.effects.get(&effect.operation_id) {
                if previous.description != effect.description
                    || previous.idempotent != effect.idempotent
                {
                    return Err(StoreError::Invalid("effect identity cannot change"));
                }
                if !matches!(
                    previous.status,
                    EffectStatus::Intended | EffectStatus::Unknown
                ) {
                    return Err(StoreError::Invalid("effect already settled"));
                }
            } else if effect.status != EffectStatus::Intended || state.outcome.is_some() {
                return Err(StoreError::Invalid(
                    "effect must have a durable intent before dispatch",
                ));
            }
            Ok(TaskEvent::EffectRecorded(effect))
        })
    }

    pub fn charge_usage(
        &mut self,
        id: TaskId,
        operation: Uuid,
        usage: Usage,
    ) -> Result<TaskState, StoreError> {
        self.account_usage(id, operation, Some(usage), true)
    }

    pub fn reserve_model_call(
        &mut self,
        id: TaskId,
        operation: Uuid,
    ) -> Result<TaskState, StoreError> {
        self.account_usage(id, operation, None, true)
    }

    pub(crate) fn reserve_model_call_without_budget_limit(
        &mut self,
        id: TaskId,
        operation: Uuid,
    ) -> Result<TaskState, StoreError> {
        self.account_usage(id, operation, None, false)
    }

    /// Records every admitted attempt, including failures and unknown billing.
    /// A later protected reconciliation can fill uncertainty, never erase spend.
    pub fn record_model_call(
        &mut self,
        id: TaskId,
        operation: Uuid,
        receipt: ModelCallReceipt,
    ) -> Result<TaskState, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (state, head) = load_state(&transaction, id)?;
        if !state.model_reservations.contains(&operation) {
            return Err(StoreError::Invalid(
                "provider result without an admitted call",
            ));
        }
        if let Some(previous) = state.model_receipts.get(&operation) {
            if previous == &receipt {
                return Ok(state);
            }
            if (previous.tokens.is_some() && previous.tokens != receipt.tokens)
                || (previous.status != ModelCallStatus::Unknown
                    && previous.status != receipt.status)
                || (previous.tokens.is_some() && previous.status != ModelCallStatus::Unknown)
            {
                return Err(StoreError::Invalid(
                    "provider reconciliation cannot rewrite settled facts",
                ));
            }
        }
        self.artifacts.read(receipt.report)?;
        commit_event(
            transaction,
            state,
            head,
            TaskEvent::ModelCallRecorded { operation, receipt },
            &self.artifacts,
        )
    }

    /// Called only when a new host has acquired the exclusive writer lock.
    /// It fences authority durably; it does not assert that old processes died.
    pub fn recover_interrupted(&mut self) -> Result<Vec<TaskId>, StoreError> {
        let mut recovered = Vec::new();
        for id in self.list()? {
            let state = self.load(id)?;
            if state.outcome.is_none() {
                self.change(id, Some(state.revision), false, |_, transaction, _| {
                    transaction.execute("DELETE FROM leases WHERE task=?1", [id.to_string()])?;
                    Ok(TaskEvent::Interrupted { reason: "Host restarted before task settlement; reconcile old jobs and provider accounting before resuming".into() })
                })?;
                recovered.push(id);
            }
        }
        let mut offset = 0;
        loop {
            let sessions = self.sessions(offset, 64)?;
            if sessions.is_empty() {
                break;
            }
            offset += sessions.len();
            for mut session in sessions {
                if let Some(request) = session.active_request {
                    let outcome = session
                        .tasks_by_request
                        .get(&request)
                        .copied()
                        .map(|id| self.load(id))
                        .transpose()?
                        .and_then(|task| task.outcome);
                    session = self.session_command(session.id, session.revision, Uuid::new_v5(&request, b"host-recovered"), SessionCommand::TurnSettled {
                    request,
                    outcome,
                    error: Some("Host interrupted this request; persisted work is available for recovery".into()),
                })?;
                    debug_assert!(session.active_request.is_none());
                }
            }
        }
        Ok(recovered)
    }

    fn account_usage(
        &mut self,
        id: TaskId,
        operation: Uuid,
        usage: Option<Usage>,
        enforce_budget: bool,
    ) -> Result<TaskState, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (state, head) = load_state(&transaction, id)?;
        if let Some(previous) = state.usage_receipts.get(&operation) {
            return if usage == Some(*previous) {
                Ok(state)
            } else {
                Err(StoreError::Invalid(
                    "accounting operation ID reused with different input",
                ))
            };
        }
        if state.model_reservations.contains(&operation) {
            return if usage.is_none() {
                Ok(state)
            } else {
                Err(StoreError::Invalid(
                    "reservation ID reused for usage receipt",
                ))
            };
        }
        let event = match usage {
            Some(usage) => TaskEvent::UsageCharged { operation, usage },
            None => {
                if state.outcome.is_some() {
                    return Err(StoreError::Terminal);
                }
                if enforce_budget {
                    check_budget(&state)?;
                } else if state.cancellation_requested {
                    return Err(StoreError::Cancelled);
                }
                TaskEvent::ModelCallReserved { operation }
            }
        };
        commit_event(transaction, state, head, event, &self.artifacts)
    }

    pub fn record_delivery(
        &mut self,
        id: TaskId,
        revision: u64,
        delivery: Delivery,
    ) -> Result<TaskState, StoreError> {
        self.change(id, Some(revision), false, |state, _, artifacts| {
            let candidate = state
                .candidate
                .as_ref()
                .filter(|c| c.frozen)
                .ok_or(StoreError::Invalid("delivery requires a frozen candidate"))?;
            if candidate.source != delivery.source
                || candidate.artifact != delivery.artifact
                || state.accepted_contract()?.delivery != delivery.kind
            {
                return Err(StoreError::Invalid("delivery identity mismatch"));
            }
            artifacts.read(delivery.receipt)?;
            Ok(TaskEvent::Delivered(delivery))
        })
    }

    pub fn complete(&mut self, id: TaskId, revision: u64) -> Result<TaskState, StoreError> {
        self.change(id, Some(revision), false, |state, _, artifacts| {
            let certificate =
                completion::evaluate(state, now_ms()).map_err(StoreError::Incomplete)?;
            for digest in [
                certificate.source,
                certificate.artifact,
                certificate.environment,
                certificate.delivery_receipt,
            ] {
                artifacts.read(digest)?;
            }
            for (check_id, job_id) in &certificate.evidence {
                let evidence = state
                    .evidence
                    .iter()
                    .find(|e| e.job_id == *job_id)
                    .ok_or(StoreError::Integrity("accepted evidence disappeared"))?;
                artifacts.read(evidence.observation.report)?;
                if let Some(control) = &evidence.observation.control {
                    artifacts.read(control.report)?;
                }
                if let crate::contract::BaselinePolicy::NoNewFailure {
                    baseline_report, ..
                } = state.accepted_contract()?.checks[check_id].baseline
                {
                    artifacts.read(baseline_report)?;
                }
                artifacts.read(evidence.identity.verifier)?;
            }
            Ok(TaskEvent::Completed(certificate))
        })
    }

    pub fn stop(
        &mut self,
        id: TaskId,
        revision: u64,
        outcome: Outcome,
        reason: String,
    ) -> Result<TaskState, StoreError> {
        if outcome == Outcome::Complete {
            return Err(StoreError::Invalid("completion requires the evaluator"));
        }
        if reason.trim().is_empty() {
            return Err(StoreError::Invalid("incomplete outcome requires a reason"));
        }
        self.change(id, Some(revision), false, |_, _, _| {
            Ok(TaskEvent::Stopped { outcome, reason })
        })
    }

    pub fn reopen(
        &mut self,
        id: TaskId,
        revision: u64,
        reason: String,
    ) -> Result<TaskState, StoreError> {
        if reason.trim().is_empty() {
            return Err(StoreError::Invalid("reopening requires a user basis"));
        }
        self.change(id, Some(revision), true, |state, _, _| {
            if state.outcome.is_none() {
                return Err(StoreError::Invalid("task is already active"));
            }
            Ok(TaskEvent::Reopened { reason })
        })
    }

    fn change<F>(
        &mut self,
        id: TaskId,
        revision: Option<u64>,
        allow_terminal: bool,
        operation: F,
    ) -> Result<TaskState, StoreError>
    where
        F: FnOnce(&TaskState, &Transaction<'_>, &ArtifactStore) -> Result<TaskEvent, StoreError>,
    {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (state, head) = load_state(&transaction, id)?;
        if let Some(expected) = revision
            && expected != state.revision
        {
            return Err(StoreError::Revision {
                expected,
                actual: state.revision,
            });
        }
        if !allow_terminal && state.outcome.is_some() {
            return Err(StoreError::Terminal);
        }
        let event = operation(&state, &transaction, &self.artifacts)?;
        commit_event(transaction, state, head, event, &self.artifacts)
    }
}

fn initialize_schema(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE tasks(id TEXT PRIMARY KEY, revision INTEGER NOT NULL, state BLOB NOT NULL, head TEXT NOT NULL) STRICT;
         CREATE TABLE events(sequence INTEGER PRIMARY KEY AUTOINCREMENT, aggregate TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('task','session')), revision INTEGER NOT NULL, event BLOB NOT NULL, hash TEXT NOT NULL, UNIQUE(aggregate,kind,revision)) STRICT;
         CREATE TABLE sessions(id TEXT PRIMARY KEY, revision INTEGER NOT NULL, state BLOB NOT NULL, head TEXT NOT NULL) STRICT;
         CREATE TABLE checkpoints(aggregate TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('task','session')), revision INTEGER NOT NULL, state BLOB NOT NULL, head TEXT NOT NULL, PRIMARY KEY(aggregate,kind,revision)) STRICT;
         CREATE TABLE leases(task TEXT NOT NULL REFERENCES tasks(id), job TEXT NOT NULL, token_digest TEXT NOT NULL, PRIMARY KEY(task,job)) STRICT;
         PRAGMA user_version=9;
         COMMIT;",
    )?;
    Ok(())
}

fn migrate_v1_to_v8(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         ALTER TABLE events RENAME TO events_v1;
         CREATE TABLE events(sequence INTEGER PRIMARY KEY AUTOINCREMENT, aggregate TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('task','session')), revision INTEGER NOT NULL, event BLOB NOT NULL, hash TEXT NOT NULL, UNIQUE(aggregate,kind,revision)) STRICT;
         INSERT INTO events(sequence,aggregate,kind,revision,event,hash)
           SELECT sequence,aggregate,kind,revision,event,hash FROM events_v1 ORDER BY sequence;
         DROP TABLE events_v1;
         CREATE TABLE checkpoints(aggregate TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('task','session')), revision INTEGER NOT NULL, state BLOB NOT NULL, head TEXT NOT NULL, PRIMARY KEY(aggregate,kind,revision)) STRICT;
         PRAGMA user_version=8;
         COMMIT;",
    )?;
    Ok(())
}

fn migrate_v7_to_v8(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE checkpoints(aggregate TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('task','session')), revision INTEGER NOT NULL, state BLOB NOT NULL, head TEXT NOT NULL, PRIMARY KEY(aggregate,kind,revision)) STRICT;
         INSERT INTO checkpoints(aggregate,kind,revision,state,head)
           SELECT id,'task',revision,state,head FROM tasks;
         INSERT INTO checkpoints(aggregate,kind,revision,state,head)
           SELECT id,'session',revision,state,head FROM sessions;
         PRAGMA user_version=8;
         COMMIT;",
    )?;
    Ok(())
}

/// Versions 2-6 may contain prototype tables. They are left intact so opening a
/// historical store never destroys data, but current code neither reads nor writes them.
fn retire_legacy_schema(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE IF NOT EXISTS checkpoints(aggregate TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('task','session')), revision INTEGER NOT NULL, state BLOB NOT NULL, head TEXT NOT NULL, PRIMARY KEY(aggregate,kind,revision)) STRICT;
         PRAGMA user_version=8;
         COMMIT;",
    )?;
    Ok(())
}

fn migrate_v8_to_v9(connection: &mut Connection) -> Result<(), StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let has_sessions: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='sessions')",
        [],
        |row| row.get(0),
    )?;
    if !has_sessions {
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        transaction.commit()?;
        return Ok(());
    }
    let session_ids = {
        let mut statement = transaction.prepare("SELECT id FROM sessions ORDER BY id")?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut rebuilt = Vec::with_capacity(session_ids.len());
    let mut checkpoints = Vec::new();
    for encoded_id in session_ids {
        let id = SessionId(
            encoded_id
                .parse()
                .map_err(|_| StoreError::Integrity("invalid session ID"))?,
        );
        let (stored_revision, stored_head): (i64, String) = transaction.query_row(
            "SELECT revision,head FROM sessions WHERE id=?1",
            [&encoded_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let stored_revision = u64::try_from(stored_revision)
            .map_err(|_| StoreError::Integrity("negative session revision"))?;
        let mut state = None;
        let mut head = None;
        let mut sequence = 0_u64;
        let mut statement = transaction.prepare(
            "SELECT revision,event,hash FROM events WHERE aggregate=?1 AND kind='session' ORDER BY revision",
        )?;
        let mut rows = statement.query([&encoded_id])?;
        while let Some(row) = rows.next()? {
            sequence += 1;
            let event_revision = u64::try_from(row.get::<_, i64>(0)?)
                .map_err(|_| StoreError::Integrity("negative session event revision"))?;
            let bytes: Vec<u8> = row.get(1)?;
            if event_revision != sequence || bytes.len() > MAX_EVENT_BYTES {
                return Err(StoreError::Integrity("session journal ordering or size"));
            }
            let hash = aggregate_hash("session", id.0, event_revision, head, &bytes)?;
            if hash.to_string() != row.get::<_, String>(2)? {
                return Err(StoreError::Integrity("session journal hash"));
            }
            match (&mut state, serde_json::from_slice::<SessionEvent>(&bytes)?) {
                (
                    None,
                    SessionEvent::Created {
                        branch,
                        config,
                        admission,
                        parent,
                        history,
                        at_ms,
                        imported,
                    },
                ) => {
                    state = Some(SessionState::create(
                        id,
                        crate::session::SessionCreation {
                            branch,
                            config,
                            admission: admission.map(|profile| *profile),
                            parent,
                            history,
                            started_ms: at_ms,
                            imported: imported.map(|source| *source),
                        },
                    ));
                }
                (
                    Some(state),
                    SessionEvent::Command {
                        operation, command, ..
                    },
                ) => state.apply(operation, &command)?,
                _ => return Err(StoreError::Integrity("session creation sequence")),
            }
            head = Some(hash);
            if event_revision == 1 || event_revision.is_multiple_of(128) {
                checkpoints.push((
                    encoded_id.clone(),
                    event_revision,
                    serde_json::to_vec(state.as_ref().unwrap())?,
                    hash,
                ));
            }
        }
        let state = state.ok_or(StoreError::Integrity("session creation missing"))?;
        let head = head.ok_or(StoreError::Integrity("session head missing"))?;
        if state.revision != stored_revision || head.to_string() != stored_head {
            return Err(StoreError::Integrity(
                "session projection differs from journal",
            ));
        }
        rebuilt.push((encoded_id, state, head));
    }
    transaction.execute("DELETE FROM checkpoints WHERE kind='session'", [])?;
    for (id, revision, state, head) in checkpoints {
        transaction.execute(
            "INSERT INTO checkpoints(aggregate,kind,revision,state,head) VALUES (?1,'session',?2,?3,?4)",
            params![id, i64::try_from(revision).map_err(|_| StoreError::Invalid("checkpoint revision exhausted"))?, state, head.to_string()],
        )?;
    }
    for (id, state, head) in rebuilt {
        transaction.execute(
            "UPDATE sessions SET state=?2,head=?3 WHERE id=?1",
            params![id, serde_json::to_vec(&state)?, head.to_string()],
        )?;
    }
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

fn append_session_command(
    transaction: &Transaction<'_>,
    state: &mut SessionState,
    head: &mut Digest,
    operation: Uuid,
    command: SessionCommand,
) -> Result<(), StoreError> {
    let event = SessionEvent::Command {
        operation,
        command: command.clone(),
        at_ms: now_ms(),
    };
    let bytes = serde_json::to_vec(&event)?;
    if bytes.len() > MAX_EVENT_BYTES {
        if let SessionCommand::ToolResult {
            request,
            call_id,
            output,
        } = command
        {
            let mut offset = 0;
            while offset < output.len() {
                // Six-byte JSON escaping still leaves room for the event envelope.
                let end = output.floor_char_boundary((offset + 64 * 1024).min(output.len()));
                append_session_command(
                    transaction,
                    state,
                    head,
                    Uuid::new_v5(&operation, &(offset as u64).to_le_bytes()),
                    SessionCommand::ToolResultPart {
                        request,
                        call_id: call_id.clone(),
                        offset,
                        output: output[offset..end].to_owned(),
                    },
                )?;
                offset = end;
            }
            return append_session_command(
                transaction,
                state,
                head,
                operation,
                SessionCommand::ToolResultEnd {
                    request,
                    call_id,
                    digest: Digest::of(output.as_bytes()),
                },
            );
        }
        return Err(StoreError::Invalid("session command exceeds journal limit"));
    }
    state.apply(operation, &command)?;
    *head = aggregate_hash("session", state.id.0, state.revision, Some(*head), &bytes)?;
    let revision = i64::try_from(state.revision)
        .map_err(|_| StoreError::Invalid("session revision exhausted"))?;
    transaction.execute(
        "INSERT INTO events(aggregate,kind,revision,event,hash) VALUES (?1,'session',?2,?3,?4)",
        params![state.id.to_string(), revision, bytes, head.to_string()],
    )?;
    transaction.execute(
        "UPDATE sessions SET revision=?2,state=?3,head=?4 WHERE id=?1",
        params![
            state.id.to_string(),
            revision,
            serde_json::to_vec(state)?,
            head.to_string()
        ],
    )?;
    if state.revision.is_multiple_of(128) {
        store_checkpoint(
            transaction,
            "session",
            state.id.0,
            state.revision,
            state,
            *head,
        )?;
    }
    Ok(())
}

fn commit_event(
    transaction: Transaction<'_>,
    mut state: TaskState,
    head: Digest,
    event: TaskEvent,
    artifacts: &ArtifactStore,
) -> Result<TaskState, StoreError> {
    append_task_event(&transaction, &mut state, head, event, artifacts)?;
    transaction.commit()?;
    Ok(state)
}

fn append_task_event(
    transaction: &Transaction<'_>,
    state: &mut TaskState,
    head: Digest,
    event: TaskEvent,
    artifacts: &ArtifactStore,
) -> Result<(), StoreError> {
    let id = state.id;
    let limit = match &event {
        TaskEvent::ContractAmended { contract, .. }
        | TaskEvent::ContractAdmitted { contract, .. }
        | TaskEvent::AdditiveContractAccepted { contract, .. } => contract.limits.artifact_bytes,
        _ => state.limits().artifact_bytes,
    };
    if matches!(
        event,
        TaskEvent::ContractAmended { .. }
            | TaskEvent::ManualStarted { .. }
            | TaskEvent::ManualWorkspace { .. }
            | TaskEvent::OriginCaptured { .. }
            | TaskEvent::DirectiveReceived { .. }
            | TaskEvent::DirectiveReplaced { .. }
            | TaskEvent::AdditiveContractAccepted { .. }
            | TaskEvent::ContractAdmitted { .. }
            | TaskEvent::JobStarted(_)
            | TaskEvent::CandidateSelected(_)
            | TaskEvent::BaselineEstablished(_)
            | TaskEvent::Observed(_)
            | TaskEvent::Delivered(_)
            | TaskEvent::Completed(_)
    ) {
        check_artifact_budget(transaction, id, &event, artifacts, limit)?;
    }
    let bytes = event_bytes(&event)?;
    state.apply(&event)?;
    let hash = event_hash(id, state.revision, Some(head), &bytes)?;
    let sql_revision = i64::try_from(state.revision)
        .map_err(|_| StoreError::Invalid("journal revision exhausted"))?;
    transaction.execute(
        "INSERT INTO events(aggregate,kind,revision,event,hash) VALUES (?1,'task',?2,?3,?4)",
        params![id.to_string(), sql_revision, bytes, hash.to_string()],
    )?;
    transaction.execute(
        "UPDATE tasks SET revision=?2,state=?3,head=?4 WHERE id=?1",
        params![
            id.to_string(),
            sql_revision,
            serde_json::to_vec(&state)?,
            hash.to_string()
        ],
    )?;
    if state.revision.is_multiple_of(128) {
        store_checkpoint(transaction, "task", id.0, state.revision, state, hash)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum BlobKind {
    Opaque,
    Snapshot,
    PatchReceipt,
    UserInput,
}

fn check_artifact_budget(
    connection: &Connection,
    task: TaskId,
    event: &TaskEvent,
    artifacts: &ArtifactStore,
    limit: u64,
) -> Result<(), StoreError> {
    let mut referenced = BTreeSet::new();
    let mut statement =
        connection.prepare("SELECT event FROM events WHERE aggregate=?1 AND kind='task'")?;
    let mut rows = statement.query([task.to_string()])?;
    while let Some(row) = rows.next()? {
        let bytes: Vec<u8> = row.get(0)?;
        collect_artifacts(&serde_json::from_slice(&bytes)?, &mut referenced);
    }
    collect_artifacts(event, &mut referenced);
    verify_artifact_graph(referenced, artifacts, limit)
}

fn verify_artifact_graph(
    referenced: BTreeSet<(Digest, BlobKind)>,
    artifacts: &ArtifactStore,
    limit: u64,
) -> Result<(), StoreError> {
    let mut pending = referenced.into_iter().collect::<Vec<_>>();
    let mut visited = BTreeSet::new();
    let mut counted = BTreeSet::new();
    let mut total = 0u64;
    while let Some((digest, kind)) = pending.pop() {
        if !visited.insert((digest, kind)) {
            continue;
        }
        let bytes = artifacts.read(digest)?;
        if counted.insert(digest) {
            total = total
                .checked_add(bytes.len() as u64)
                .ok_or(StoreError::Budget)?;
        }
        if total > limit {
            return Err(StoreError::Budget);
        }
        match kind {
            BlobKind::UserInput => {
                pending.extend(
                    crate::input::media_references(&bytes)?
                        .into_iter()
                        .map(|digest| (digest, BlobKind::Opaque)),
                );
            }
            BlobKind::Opaque => {}
            BlobKind::Snapshot => {
                if let Ok(snapshot) = serde_json::from_slice::<crate::workspace::Snapshot>(&bytes) {
                    for entry in snapshot.entries.values() {
                        if let crate::workspace::Entry::File { content, .. } = entry {
                            pending.push((*content, BlobKind::Opaque));
                        }
                    }
                }
            }
            BlobKind::PatchReceipt => {
                let receipt: crate::delivery::PatchValidationReceipt =
                    serde_json::from_slice(&bytes)?;
                pending.extend([
                    (receipt.baseline, BlobKind::Snapshot),
                    (receipt.candidate, BlobKind::Snapshot),
                    (receipt.applied_snapshot, BlobKind::Snapshot),
                    (receipt.observed_snapshot, BlobKind::Snapshot),
                    (receipt.patch, BlobKind::Opaque),
                ]);
                for command in receipt.commands {
                    pending.extend([
                        (command.stdin_digest, BlobKind::Opaque),
                        (command.stdout_digest, BlobKind::Opaque),
                        (command.stderr_digest, BlobKind::Opaque),
                    ]);
                }
            }
        }
    }
    Ok(())
}

fn collect_artifacts(event: &TaskEvent, referenced: &mut BTreeSet<(Digest, BlobKind)>) {
    if let TaskEvent::ContractAdmitted { receipt, .. }
    | TaskEvent::AdditiveContractAccepted { receipt, .. } = event
    {
        referenced.insert((*receipt, BlobKind::Opaque));
    }
    match event {
        TaskEvent::ManualWorkspace { candidate, origin } => {
            referenced.extend([
                (candidate.source, BlobKind::Snapshot),
                (candidate.environment, BlobKind::Opaque),
                (*origin, BlobKind::Snapshot),
            ]);
        }
        TaskEvent::OriginCaptured { source } => {
            referenced.insert((*source, BlobKind::Snapshot));
        }
        TaskEvent::DirectiveReceived { input, .. } | TaskEvent::DirectiveReplaced { input, .. } => {
            referenced.insert((*input, BlobKind::UserInput));
        }
        TaskEvent::JobStarted(job) | TaskEvent::ManualStarted { job } => {
            if let Some(invocation) = &job.invocation {
                referenced.extend([
                    (invocation.input, BlobKind::Opaque),
                    (invocation.environment, BlobKind::Opaque),
                ]);
            }
        }
        TaskEvent::JobFenced { receipt, .. } => {
            referenced.insert((*receipt, BlobKind::Opaque));
        }
        TaskEvent::JobSettled {
            receipt: Some(receipt),
            ..
        } => {
            referenced.insert((*receipt, BlobKind::Opaque));
        }
        TaskEvent::Requested { intake, input, .. } => {
            referenced.insert((*intake, BlobKind::Opaque));
            if let Some(input) = input {
                referenced.insert((*input, BlobKind::UserInput));
            }
        }
        TaskEvent::Created { contract, .. }
        | TaskEvent::ContractAmended { contract, .. }
        | TaskEvent::AdditiveContractAccepted { contract, .. }
        | TaskEvent::ContractAdmitted { contract, .. } => {
            for check in contract.checks.values() {
                referenced.insert((check.verifier, BlobKind::Opaque));
                if let Some(source) = check.control_source {
                    referenced.insert((source, BlobKind::Snapshot));
                }
                if let crate::contract::BaselinePolicy::NoNewFailure {
                    baseline_report, ..
                } = check.baseline
                {
                    referenced.insert((baseline_report, BlobKind::Opaque));
                }
            }
        }
        TaskEvent::CandidateSelected(candidate) | TaskEvent::BaselineEstablished(candidate) => {
            referenced.extend([
                (candidate.source, BlobKind::Snapshot),
                (candidate.environment, BlobKind::Opaque),
                (candidate.artifact, BlobKind::Opaque),
            ]);
            if let Some(proof) = candidate.provenance {
                referenced.insert((proof, BlobKind::PatchReceipt));
            }
        }
        TaskEvent::Observed(evidence) => {
            referenced.insert((evidence.observation.report, BlobKind::Opaque));
            if let Some(control) = &evidence.observation.control {
                referenced.extend([
                    (control.report, BlobKind::Opaque),
                    (control.source, BlobKind::Snapshot),
                ]);
            }
        }
        TaskEvent::Delivered(delivery) => {
            referenced.insert((delivery.receipt, BlobKind::Opaque));
        }
        TaskEvent::ModelCallRecorded { receipt, .. } => {
            referenced.insert((receipt.report, BlobKind::Opaque));
        }
        _ => {}
    }
}

fn check_budget(state: &TaskState) -> Result<(), StoreError> {
    if state.cancellation_requested {
        return Err(StoreError::Cancelled);
    }
    let limit = state.limits();
    if state.usage.model_calls >= limit.model_calls
        || state.usage.tokens >= limit.tokens
        || now_ms().saturating_sub(state.started_ms) >= limit.elapsed_ms
    {
        return Err(StoreError::Budget);
    }
    Ok(())
}

fn admit_job(state: &TaskState, enforce_budget: bool) -> Result<(), StoreError> {
    if state.cancellation_requested {
        return Err(StoreError::Cancelled);
    }
    if enforce_budget && now_ms().saturating_sub(state.started_ms) >= state.limits().elapsed_ms {
        return Err(StoreError::Budget);
    }
    if state
        .jobs
        .values()
        .filter(|job| job.status.unresolved())
        .count()
        >= state.limits().concurrent_jobs as usize
    {
        return Err(StoreError::Invalid("job capacity exhausted"));
    }
    Ok(())
}

fn store_checkpoint<T: serde::Serialize>(
    connection: &Connection,
    kind: &str,
    aggregate: Uuid,
    revision: u64,
    state: &T,
    head: Digest,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT OR REPLACE INTO checkpoints(aggregate,kind,revision,state,head) VALUES (?1,?2,?3,?4,?5)",
        params![
            aggregate.to_string(),
            kind,
            i64::try_from(revision).map_err(|_| StoreError::Invalid("checkpoint revision exhausted"))?,
            serde_json::to_vec(state)?,
            head.to_string(),
        ],
    )?;
    Ok(())
}

fn load_state(connection: &Connection, id: TaskId) -> Result<(TaskState, Digest), StoreError> {
    let (revision, cached, stored_head): (i64, Vec<u8>, String) = connection
        .query_row(
            "SELECT revision,state,head FROM tasks WHERE id=?1",
            [id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or(StoreError::Missing(id))?;
    let revision =
        u64::try_from(revision).map_err(|_| StoreError::Integrity("negative task revision"))?;
    let checkpoint: Option<(i64, Vec<u8>, String)> = connection
        .query_row(
            "SELECT revision,state,head FROM checkpoints WHERE aggregate=?1 AND kind='task' AND revision<=?2 ORDER BY revision DESC LIMIT 1",
            params![id.to_string(), revision as i64],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (mut state, mut head, mut sequence) = if let Some((revision, bytes, head)) = checkpoint {
        let checkpoint_revision = u64::try_from(revision)
            .map_err(|_| StoreError::Integrity("negative task checkpoint revision"))?;
        let checkpoint_state: TaskState = serde_json::from_slice(&bytes)?;
        if checkpoint_state.id != id || checkpoint_state.revision != checkpoint_revision {
            return Err(StoreError::Integrity("invalid task checkpoint"));
        }
        (
            Some(checkpoint_state),
            Some(
                head.parse()
                    .map_err(|_| StoreError::Integrity("task checkpoint hash"))?,
            ),
            checkpoint_revision,
        )
    } else {
        (None, None, 0)
    };
    let mut statement = connection.prepare(
        "SELECT revision,event,hash FROM events WHERE aggregate=?1 AND kind='task' AND revision>?2 ORDER BY revision",
    )?;
    let mut rows = statement.query(params![id.to_string(), sequence as i64])?;
    while let Some(row) = rows.next()? {
        let event_revision = u64::try_from(row.get::<_, i64>(0)?)
            .map_err(|_| StoreError::Integrity("negative event revision"))?;
        let bytes: Vec<u8> = row.get(1)?;
        let saved_hash: String = row.get(2)?;
        sequence += 1;
        if event_revision != sequence || bytes.len() > MAX_EVENT_BYTES {
            return Err(StoreError::Integrity("journal ordering or size"));
        }
        let hash = event_hash(id, event_revision, head, &bytes)?;
        if saved_hash != hash.to_string() {
            return Err(StoreError::Integrity("journal hash"));
        }
        let event: TaskEvent = serde_json::from_slice(&bytes)?;
        match (&mut state, event) {
            (
                None,
                TaskEvent::Requested {
                    request,
                    limits,
                    intake,
                    input,
                    at_ms,
                },
            ) => {
                limits.validate()?;
                if request.trim().is_empty() {
                    return Err(StoreError::Integrity("empty original request"));
                }
                state = Some(TaskState::requested(
                    id,
                    request,
                    limits,
                    at_ms,
                    Some(intake),
                    input,
                ));
            }
            (None, TaskEvent::Created { contract, at_ms }) => {
                contract.validate()?;
                state = Some(TaskState::created(id, contract, at_ms));
            }
            (Some(_), TaskEvent::Created { .. } | TaskEvent::Requested { .. }) | (None, _) => {
                return Err(StoreError::Integrity("invalid task creation sequence"));
            }
            (Some(state), event) => state.apply(&event)?,
        }
        head = Some(hash);
    }
    let state = state.ok_or(StoreError::Integrity("missing creation event"))?;
    let head = head.ok_or(StoreError::Integrity("missing journal head"))?;
    if sequence != revision
        || head.to_string() != stored_head
        || serde_json::to_vec(&state)? != cached
    {
        return Err(StoreError::Integrity(
            "materialized state differs from journal",
        ));
    }
    Ok((state, head))
}

fn event_bytes(event: &TaskEvent) -> Result<Vec<u8>, StoreError> {
    let bytes = serde_json::to_vec(event)?;
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(StoreError::Invalid("event payload exceeds limit"));
    }
    Ok(bytes)
}

fn event_hash(
    task: TaskId,
    revision: u64,
    previous: Option<Digest>,
    bytes: &[u8],
) -> Result<Digest, StoreError> {
    aggregate_hash("task", task.0, revision, previous, bytes)
}

pub(crate) fn aggregate_hash(
    kind: &str,
    id: Uuid,
    revision: u64,
    previous: Option<Digest>,
    bytes: &[u8],
) -> Result<Digest, StoreError> {
    Ok(Digest::of_value(&(
        kind,
        id,
        revision,
        previous,
        Digest::of(bytes),
    ))?)
}

fn legacy_context_command(bytes: &[u8]) -> Result<Option<(Uuid, Digest)>, StoreError> {
    #[derive(serde::Deserialize)]
    struct RawEvent<'a> {
        #[serde(rename = "type")]
        kind: &'a str,
        #[serde(borrow)]
        data: &'a serde_json::value::RawValue,
    }

    #[derive(serde::Deserialize)]
    struct RawCommand<'a> {
        operation: Uuid,
        #[serde(borrow)]
        command: &'a serde_json::value::RawValue,
    }

    let event = serde_json::from_slice::<RawEvent<'_>>(bytes)?;
    if event.kind != "command" {
        return Ok(None);
    }
    let command = serde_json::from_str::<RawCommand<'_>>(event.data.get())?;
    let value = serde_json::from_str::<serde_json::Value>(command.command.get())?;
    if value["type"] != "context_projected" || !value["data"]["view"]["input"].is_array() {
        return Ok(None);
    }
    Ok(Some((
        command.operation,
        Digest::of(command.command.get().as_bytes()),
    )))
}

fn session_cache_matches(state: &SessionState, cached: &[u8]) -> Result<bool, StoreError> {
    let canonical = serde_json::to_vec(state)?;
    if canonical == cached {
        return Ok(true);
    }

    let mut legacy = serde_json::from_slice::<serde_json::Value>(cached)?;
    let manifest = {
        let Some(wrapper) = legacy
            .get_mut("context_view")
            .and_then(serde_json::Value::as_object_mut)
        else {
            return Ok(false);
        };
        if wrapper.len() != 2
            || !wrapper
                .get("input")
                .is_some_and(serde_json::Value::is_array)
        {
            return Ok(false);
        }
        let Some(manifest) = wrapper.remove("manifest") else {
            return Ok(false);
        };
        manifest
    };
    legacy["context_view"] = manifest;

    Ok(legacy == serde_json::from_slice::<serde_json::Value>(&canonical)?)
}

fn load_session_state(
    connection: &Connection,
    id: SessionId,
    through: Option<u64>,
) -> Result<(SessionState, Digest), StoreError> {
    let (revision, cached, stored_head): (i64, Vec<u8>, String) = connection
        .query_row(
            "SELECT revision,state,head FROM sessions WHERE id=?1",
            [id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or(StoreError::MissingSession(id))?;
    let current_revision =
        u64::try_from(revision).map_err(|_| StoreError::Integrity("negative session revision"))?;
    let last = through.unwrap_or(current_revision);
    if last == 0 || last > current_revision {
        return Err(StoreError::Invalid(
            "session cursor is outside persisted history",
        ));
    }
    let checkpoint: Option<(i64, Vec<u8>, String)> = connection
        .query_row(
            "SELECT revision,state,head FROM checkpoints WHERE aggregate=?1 AND kind='session' AND revision<=?2 ORDER BY revision DESC LIMIT 1",
            params![id.to_string(), last as i64],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (mut state, mut head, mut sequence) = if let Some((revision, bytes, head)) = checkpoint {
        let checkpoint_revision = u64::try_from(revision)
            .map_err(|_| StoreError::Integrity("negative session checkpoint revision"))?;
        let checkpoint_state: SessionState = serde_json::from_slice(&bytes)?;
        if checkpoint_state.id != id || checkpoint_state.revision != checkpoint_revision {
            return Err(StoreError::Integrity("invalid session checkpoint"));
        }
        (
            Some(checkpoint_state),
            Some(
                head.parse()
                    .map_err(|_| StoreError::Integrity("session checkpoint hash"))?,
            ),
            checkpoint_revision,
        )
    } else {
        (None, None, 0)
    };
    let mut statement = connection.prepare(
        "SELECT revision,event,hash FROM events WHERE aggregate=?1 AND kind='session' AND revision>?2 AND revision<=?3 ORDER BY revision",
    )?;
    let mut rows = statement.query(params![id.to_string(), sequence as i64, last as i64])?;
    while let Some(row) = rows.next()? {
        sequence += 1;
        let event_revision = u64::try_from(row.get::<_, i64>(0)?)
            .map_err(|_| StoreError::Integrity("negative session event revision"))?;
        let bytes: Vec<u8> = row.get(1)?;
        if event_revision != sequence || bytes.len() > MAX_EVENT_BYTES {
            return Err(StoreError::Integrity("session journal ordering or size"));
        }
        let hash = aggregate_hash("session", id.0, event_revision, head, &bytes)?;
        if hash.to_string() != row.get::<_, String>(2)? {
            return Err(StoreError::Integrity("session journal hash"));
        }
        match (&mut state, serde_json::from_slice::<SessionEvent>(&bytes)?) {
            (
                None,
                SessionEvent::Created {
                    branch,
                    config,
                    admission,
                    parent,
                    history,
                    at_ms,
                    imported,
                },
            ) => {
                state = Some(SessionState::create(
                    id,
                    crate::session::SessionCreation {
                        branch,
                        config,
                        admission: admission.map(|profile| *profile),
                        parent,
                        history,
                        started_ms: at_ms,
                        imported: imported.map(|imported| *imported),
                    },
                ))
            }
            (
                Some(state),
                SessionEvent::Command {
                    operation, command, ..
                },
            ) => {
                let legacy = if matches!(command, SessionCommand::ContextProjected { .. }) {
                    legacy_context_command(&bytes)?
                } else {
                    None
                };
                state.apply(operation, &command)?;
                if let Some((journaled_operation, command_digest)) = legacy {
                    if journaled_operation != operation {
                        return Err(StoreError::Integrity("session command operation"));
                    }
                    state.operations.insert(operation, command_digest);
                }
            }
            _ => return Err(StoreError::Integrity("session creation sequence")),
        }
        head = Some(hash);
    }
    let state = state.ok_or(StoreError::Integrity("session creation missing"))?;
    let head = head.ok_or(StoreError::Integrity("session head missing"))?;
    if state.revision != last
        || (last == current_revision
            && (head.to_string() != stored_head || !session_cache_matches(&state, &cached)?))
    {
        return Err(StoreError::Integrity(
            "session projection differs from journal",
        ));
    }
    Ok((state, head))
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod admission_store_tests {
    use super::*;
    use crate::{
        inference::ModelSettings,
        session::{SessionBranch, SessionCreation},
    };

    fn config(workspace: &Path) -> SessionConfig {
        SessionConfig {
            workspace: workspace.to_owned(),
            model: ModelSettings::default(),
            instructions: "legacy forensic instructions".into(),
            context_window_tokens: crate::context::DEFAULT_WINDOW_TOKENS,
        }
    }

    #[test]
    fn schema_v8_projection_migration_recovers_authoritative_history() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let id = SessionId::new();
        let original = serde_json::json!({"role":"user","content":"authoritative source"});
        {
            let mut store = Store::open(root.path()).unwrap();
            let state = store
                .create_session(id, config(workspace.path()), None)
                .unwrap();
            let mut state = store
                .session_command(
                    id,
                    state.revision,
                    Uuid::new_v4(),
                    SessionCommand::Input {
                        kind: RequestKind::Conversation,
                        content: vec![original.clone()],
                    },
                )
                .unwrap();
            let projection = vec![serde_json::json!({
                "role":"developer",
                "content":"legacy bounded view"
            })];
            let command_value = serde_json::json!({
                "type":"context_projected",
                "data": {
                    "source_revision": state.revision,
                    "projection": projection.clone()
                }
            });
            let command: SessionCommand = serde_json::from_value(command_value).unwrap();
            let operation = Uuid::new_v4();
            let event = SessionEvent::Command {
                operation,
                command: command.clone(),
                at_ms: now_ms(),
            };
            let bytes = serde_json::to_vec(&event).unwrap();
            let previous = store
                .connection
                .query_row(
                    "SELECT head FROM sessions WHERE id=?1",
                    [id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap()
                .parse::<Digest>()
                .unwrap();
            state.apply(operation, &command).unwrap();
            state.history = projection;
            let head =
                aggregate_hash("session", id.0, state.revision, Some(previous), &bytes).unwrap();
            store
                .connection
                .execute(
                    "INSERT INTO events(aggregate,kind,revision,event,hash) VALUES (?1,'session',?2,?3,?4)",
                    params![id.to_string(), state.revision as i64, bytes, head.to_string()],
                )
                .unwrap();
            store
                .connection
                .execute(
                    "UPDATE sessions SET revision=?2,state=?3,head=?4 WHERE id=?1",
                    params![
                        id.to_string(),
                        state.revision as i64,
                        serde_json::to_vec(&state).unwrap(),
                        head.to_string()
                    ],
                )
                .unwrap();
            store
                .connection
                .pragma_update(None, "user_version", 8)
                .unwrap();
        }

        let store = Store::open(root.path()).unwrap();
        let migrated = store.load_session(id).unwrap();
        assert_eq!(migrated.history, vec![original]);
        assert!(migrated.context_view.is_none());
        let version: i32 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn legacy_provider_usage_replays_without_changing_the_session_projection() {
        #[derive(serde::Serialize)]
        struct LegacyUsage {
            input_tokens: Option<u64>,
            output_tokens: Option<u64>,
            total_tokens: Option<u64>,
            cached_input_tokens: Option<u64>,
            reasoning_tokens: Option<u64>,
        }

        #[derive(serde::Serialize)]
        #[serde(tag = "type", content = "data", rename_all = "snake_case")]
        enum LegacyCommand {
            ProviderUsage { request: Uuid, usage: LegacyUsage },
        }

        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path()).unwrap();
        let id = SessionId::new();
        let mut state = store
            .create_session(id, config(workspace.path()), None)
            .unwrap();
        let operation = Uuid::new_v4();
        let legacy = LegacyCommand::ProviderUsage {
            request: Uuid::new_v4(),
            usage: LegacyUsage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                total_tokens: Some(15),
                cached_input_tokens: Some(2),
                reasoning_tokens: Some(1),
            },
        };
        let command_bytes = serde_json::to_vec(&legacy).unwrap();
        let command: serde_json::Value = serde_json::from_slice(&command_bytes).unwrap();
        let typed: SessionCommand = serde_json::from_slice(&command_bytes).unwrap();
        assert_eq!(serde_json::to_vec(&typed).unwrap(), command_bytes);
        state.apply(operation, &typed).unwrap();
        state
            .operations
            .insert(operation, Digest::of_value(&legacy).unwrap());
        let event = serde_json::json!({
            "type": "command",
            "data": {
                "operation": operation,
                "command": command,
                "at_ms": now_ms()
            }
        });
        let bytes = serde_json::to_vec(&event).unwrap();
        let previous = store
            .connection
            .query_row(
                "SELECT head FROM sessions WHERE id=?1",
                [id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
            .parse::<Digest>()
            .unwrap();
        let head = aggregate_hash("session", id.0, state.revision, Some(previous), &bytes).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO events(aggregate,kind,revision,event,hash) VALUES (?1,'session',?2,?3,?4)",
                params![id.to_string(), state.revision as i64, bytes, head.to_string()],
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE sessions SET revision=?2,state=?3,head=?4 WHERE id=?1",
                params![
                    id.to_string(),
                    state.revision as i64,
                    serde_json::to_vec(&state).unwrap(),
                    head.to_string()
                ],
            )
            .unwrap();

        assert_eq!(store.load_session(id).unwrap(), state);
    }

    #[test]
    fn session_cost_replays_exact_receipts_without_changing_session_projection() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path()).unwrap();
        let id = SessionId::new();
        let request = Uuid::new_v4();
        let call = Uuid::new_v4();
        let state = store
            .create_session(id, config(workspace.path()), None)
            .unwrap();
        let state = store
            .session_command(
                id,
                state.revision,
                request,
                SessionCommand::Input {
                    kind: RequestKind::Conversation,
                    content: vec![serde_json::json!({"role": "user", "content": "test"})],
                },
            )
            .unwrap();
        let usage = crate::inference::Usage {
            input_tokens: Some(10),
            output_tokens: Some(5),
            total_tokens: Some(15),
            cached_input_tokens: Some(0),
            reasoning_tokens: None,
            cost_usd: None,
        };
        let state = store
            .session_command(
                id,
                state.revision,
                Uuid::new_v4(),
                SessionCommand::ProviderUsage {
                    request,
                    call: Some(call),
                    usage,
                    representation: None,
                },
            )
            .unwrap();
        let cost = serde_json::from_str::<crate::inference::UsdCost>(r#""0.125""#).unwrap();
        let state = store
            .session_command(
                id,
                state.revision,
                Uuid::new_v4(),
                SessionCommand::ProviderCost {
                    request,
                    call,
                    cost_usd: Some(cost),
                },
            )
            .unwrap();

        assert_eq!(store.load_session(id).unwrap(), state);
        assert_eq!(
            store.session_cost(id).unwrap(),
            ProviderCostSummary {
                total: cost,
                uncertain: false,
            }
        );
    }

    #[test]
    fn session_cost_marks_legacy_unlinked_usage_uncertain() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path()).unwrap();
        let id = SessionId::new();
        let state = store
            .create_session(id, config(workspace.path()), None)
            .unwrap();
        let request = Uuid::new_v4();
        let state = store
            .session_command(
                id,
                state.revision,
                request,
                SessionCommand::Input {
                    kind: RequestKind::Conversation,
                    content: vec![serde_json::json!({"role": "user", "content": "test"})],
                },
            )
            .unwrap();
        store
            .session_command(
                id,
                state.revision,
                Uuid::new_v4(),
                SessionCommand::ProviderUsage {
                    request,
                    call: None,
                    usage: crate::inference::Usage::default(),
                    representation: None,
                },
            )
            .unwrap();

        assert!(store.session_cost(id).unwrap().uncertain);
    }

    #[test]
    fn legacy_pin_is_durable_and_pre_pin_cursors_stay_untrusted() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path()).unwrap();
        let id = SessionId::new();
        let config = config(workspace.path());
        let at_ms = now_ms();
        let event = SessionEvent::Created {
            branch: SessionBranch::default(),
            config: config.clone(),
            admission: None,
            parent: None,
            history: Vec::new(),
            at_ms,
            imported: None,
        };
        let bytes = serde_json::to_vec(&event).unwrap();
        let head = aggregate_hash("session", id.0, 1, None, &bytes).unwrap();
        let state = SessionState::create(
            id,
            SessionCreation {
                branch: SessionBranch::default(),
                config: config.clone(),
                admission: None,
                parent: None,
                history: Vec::new(),
                started_ms: at_ms,
                imported: None,
            },
        );
        store
            .connection
            .execute(
                "INSERT INTO events(aggregate,kind,revision,event,hash)
                 VALUES (?1,'session',1,?2,?3)",
                params![id.to_string(), bytes, head.to_string()],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO sessions(id,revision,state,head) VALUES (?1,1,?2,?3)",
                params![
                    id.to_string(),
                    serde_json::to_vec(&state).unwrap(),
                    head.to_string()
                ],
            )
            .unwrap();

        let before_pin = SessionCursor {
            version: 1,
            session: id,
            revision: 1,
        };
        assert!(matches!(
            store.load_session_cursor(&before_pin),
            Err(StoreError::Invalid(
                "session cursor predates trusted admission"
            ))
        ));

        let profile = store
            .fixture_admission(&config, BaselineReason::UnregisteredTarget)
            .unwrap();
        let pinned = store.pin_session_admission(id, profile.clone()).unwrap();
        assert_eq!(pinned.revision, 2);
        assert_eq!(pinned.admission(), Some(&profile));
        assert!(store.load_session_cursor(&before_pin).is_err());
        assert_eq!(
            store.load_session_cursor(&pinned.fork_cursor()).unwrap(),
            pinned
        );
        drop(store);

        let mut reopened = Store::open(root.path()).unwrap();
        assert_eq!(reopened.load_session(id).unwrap(), pinned);
        assert_eq!(reopened.pin_session_admission(id, profile).unwrap(), pinned);
        assert!(reopened.load_session_cursor(&before_pin).is_err());
    }

    #[test]
    fn fresh_store_does_not_create_retired_prototype_tables() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        let retired: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND (name LIKE 'evolution_%' OR name IN ('harness_revisions','harness_targets','evaluation_cohorts','campaigns','adaptive_score_reports','activation_certificates','activation_receipts','rollback_receipts'))",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(retired, 0);
    }

    #[test]
    fn version_one_store_migrates_without_prototype_tables() {
        let root = tempfile::tempdir().unwrap();
        {
            let connection = Connection::open(root.path().join("v1.sqlite3")).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE tasks(id TEXT PRIMARY KEY, revision INTEGER NOT NULL, state BLOB NOT NULL, head TEXT NOT NULL) STRICT;
                     CREATE TABLE events(sequence INTEGER PRIMARY KEY AUTOINCREMENT, aggregate TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('task','session')), revision INTEGER NOT NULL, event BLOB NOT NULL, hash TEXT NOT NULL, UNIQUE(aggregate,kind,revision)) STRICT;
                     CREATE TABLE sessions(id TEXT PRIMARY KEY, revision INTEGER NOT NULL, state BLOB NOT NULL, head TEXT NOT NULL) STRICT;
                     CREATE TABLE leases(task TEXT NOT NULL REFERENCES tasks(id), job TEXT NOT NULL, token_digest TEXT NOT NULL, PRIMARY KEY(task,job)) STRICT;
                     PRAGMA user_version=1;",
                )
                .unwrap();
        }

        let store = Store::open(root.path()).unwrap();
        let version: i32 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let prototype_tables: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name LIKE 'evolution_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(prototype_tables, 0);
    }
    #[test]
    fn long_session_history_replays_from_checkpoints_and_checks_the_tail() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path()).unwrap();
        let id = SessionId::new();
        let mut state = store
            .create_session(id, config(workspace.path()), None)
            .unwrap();
        for index in 0..300 {
            state = store
                .session_command(
                    id,
                    state.revision,
                    Uuid::new_v4(),
                    SessionCommand::Feedback {
                        message: format!("checkpoint event {index}"),
                    },
                )
                .unwrap();
        }

        let checkpoints = store
            .connection
            .prepare(
                "SELECT revision FROM checkpoints WHERE aggregate=?1 AND kind='session' ORDER BY revision",
            )
            .unwrap()
            .query_map([id.to_string()], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(checkpoints, vec![1, 128, 256]);
        assert_eq!(store.load_session(id).unwrap().revision, 301);

        let historical = store
            .load_session_cursor(&SessionCursor {
                version: 1,
                session: id,
                revision: 200,
            })
            .unwrap();
        assert_eq!(historical.revision, 200);
        assert_eq!(historical.history.len(), 199);

        store
            .connection
            .execute(
                "UPDATE events SET hash='corrupt' WHERE aggregate=?1 AND kind='session' AND revision=300",
                [id.to_string()],
            )
            .unwrap();
        assert!(matches!(
            store.load_session(id),
            Err(StoreError::Integrity("session journal hash"))
        ));
    }

    #[test]
    fn legacy_context_view_cache_matches_authoritative_manifest_projection() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path()).unwrap();
        let id = SessionId::new();
        let state = store
            .create_session(id, config(workspace.path()), None)
            .unwrap();
        let view = crate::context::project(&state, 4096).unwrap();
        let manifest = view.manifest.clone();
        let operation = Uuid::new_v4();
        let mut state = store
            .session_command(
                id,
                state.revision,
                operation,
                SessionCommand::ContextProjected {
                    source_revision: state.revision,
                    view: Some(view),
                    projection: vec![],
                },
            )
            .unwrap();
        let mut event = store
            .connection
            .query_row(
                "SELECT event FROM events WHERE aggregate=?1 AND kind='session' AND revision=?2",
                params![id.to_string(), state.revision as i64],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).unwrap())
            .unwrap();
        event["data"]["command"]["data"]["view"]["input"] =
            serde_json::json!([{"role": "user", "content": "legacy cached projection"}]);
        let event_bytes = serde_json::to_vec(&event).unwrap();
        let command_bytes = serde_json::to_vec(&event["data"]["command"]).unwrap();
        state
            .operations
            .insert(operation, Digest::of(&command_bytes));
        let previous = store
            .connection
            .query_row(
                "SELECT hash FROM events WHERE aggregate=?1 AND kind='session' AND revision=?2",
                params![id.to_string(), state.revision as i64 - 1],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
            .parse::<Digest>()
            .unwrap();
        let head = aggregate_hash(
            "session",
            id.0,
            state.revision,
            Some(previous),
            &event_bytes,
        )
        .unwrap();
        let mut legacy = serde_json::to_value(&state).unwrap();
        legacy["context_view"] = serde_json::json!({
            "manifest": manifest,
            "input": [{"role": "user", "content": "legacy cached projection"}]
        });
        let transaction = store.connection.transaction().unwrap();
        transaction
            .execute(
                "UPDATE events SET event=?3,hash=?4 WHERE aggregate=?1 AND kind='session' AND revision=?2",
                params![id.to_string(), state.revision as i64, event_bytes, head.to_string()],
            )
            .unwrap();
        transaction
            .execute(
                "UPDATE sessions SET state=?2,head=?3 WHERE id=?1",
                params![
                    id.to_string(),
                    serde_json::to_vec(&legacy).unwrap(),
                    head.to_string()
                ],
            )
            .unwrap();
        transaction.commit().unwrap();

        assert_eq!(store.load_session(id).unwrap(), state);
    }
}
