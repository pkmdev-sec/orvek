use super::{MAX_EVENT_BYTES, Store, StoreError, aggregate_hash};
use crate::{
    Digest,
    import::PreparedImport,
    session::{
        ImportedSource, SessionCommand, SessionConfig, SessionEvent, SessionId, SessionState,
    },
};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use uuid::Uuid;

const MAX_LOOKUP_EVENTS: usize = 100_000;
const MAX_LOOKUP_BYTES: usize = 128 * 1024 * 1024;

impl Store {
    /// Scan a consistent verified journal view; SQL predicates are not trusted to
    /// hide a corrupted binding. A limit aborts instead of returning a false miss.
    pub fn lookup_legacy_import(
        &self,
        operation: Uuid,
        fingerprint: Digest,
    ) -> Result<Option<SessionState>, StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        let target = lookup(&transaction, operation, fingerprint)?;
        let state = target
            .map(|id| super::load_session_state(&transaction, id, None).map(|value| value.0))
            .transpose()?;
        transaction.commit()?;
        Ok(state)
    }
    pub fn commit_legacy_import(
        &mut self,
        operation: Uuid,
        fingerprint: Digest,
        config: SessionConfig,
        prepared: PreparedImport,
    ) -> Result<SessionState, StoreError> {
        if let Some(existing) = self.lookup_legacy_import(operation, fingerprint)? {
            return Ok(existing);
        }
        let target = Digest::of_value(&("tact.import.target.v1", prepared.import_id, &config))?;
        let id = SessionId(Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            target.to_string().as_bytes(),
        ));
        match self.load_session(id) {
            Ok(existing) => {
                if existing.parent.is_some()
                    || existing.initial_config != config
                    || !existing
                        .imported
                        .as_ref()
                        .is_some_and(|source| source.import_id == prepared.import_id)
                {
                    return Err(StoreError::Integrity(
                        "legacy content identity collides with unrelated progress",
                    ));
                }
                self.session_command(
                    id,
                    existing.revision,
                    operation,
                    SessionCommand::LegacyImportBound { fingerprint },
                )
            }
            Err(StoreError::MissingSession(_)) => {
                let source = ImportedSource {
                    import_id: prepared.import_id,
                    manifest: prepared.manifest,
                    source_snapshot: prepared.source_snapshot,
                    source_session: prepared.source_session,
                    title: prepared.title,
                    request_fingerprint: fingerprint,
                    first_operation: Some(operation),
                };
                self.create_imported_session(id, config, source, prepared.history)
            }
            Err(error) => Err(error),
        }
    }
}
fn lookup(
    connection: &Connection,
    operation: Uuid,
    fingerprint: Digest,
) -> Result<Option<SessionId>, StoreError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut statement=connection.prepare("SELECT CASE WHEN length(aggregate)<=64 THEN aggregate END,revision,length(event),CASE WHEN length(event)<=?1 THEN event END,CASE WHEN length(hash)<=64 THEN hash END FROM events WHERE kind='session' ORDER BY sequence")?;
    let mut rows = statement.query([MAX_EVENT_BYTES as i64])?;
    let mut heads = BTreeMap::<SessionId, (u64, Digest)>::new();
    let mut count = 0usize;
    let mut bytes_seen = 0usize;
    let mut found = None;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > MAX_LOOKUP_EVENTS || Instant::now() >= deadline {
            return Err(StoreError::Invalid(
                "legacy import lookup exceeds its event/time bound",
            ));
        }
        let id = row
            .get::<_, String>(0)?
            .parse::<SessionId>()
            .map_err(|_| StoreError::Integrity("invalid import journal session"))?;
        let revision = u64::try_from(row.get::<_, i64>(1)?)
            .map_err(|_| StoreError::Integrity("invalid import journal revision"))?;
        let length = usize::try_from(row.get::<_, i64>(2)?)
            .map_err(|_| StoreError::Integrity("invalid import event length"))?;
        bytes_seen = bytes_seen.saturating_add(length);
        if length > MAX_EVENT_BYTES || bytes_seen > MAX_LOOKUP_BYTES {
            return Err(StoreError::Invalid(
                "legacy import lookup exceeds its byte bound",
            ));
        }
        let bytes: Vec<u8> = row.get(3)?;
        let previous = heads.get(&id).copied();
        if revision != previous.map_or(1, |value| value.0 + 1) {
            return Err(StoreError::Integrity("import journal predecessor missing"));
        }
        let hash = aggregate_hash(
            "session",
            id.0,
            revision,
            previous.map(|value| value.1),
            &bytes,
        )?;
        if hash.to_string() != row.get::<_, String>(4)? {
            return Err(StoreError::Integrity("import journal hash mismatch"));
        }
        heads.insert(id, (revision, hash));
        let event = serde_json::from_slice::<SessionEvent>(&bytes)?;
        if !matches!(
            (revision, &event),
            (1, SessionEvent::Created { .. }) | (2.., SessionEvent::Command { .. })
        ) {
            return Err(StoreError::Integrity(
                "invalid import journal creation sequence",
            ));
        }
        let binding = match event {
            SessionEvent::Created {
                parent: None,
                imported: Some(source),
                ..
            } if source.first_operation == Some(operation) => Some(source.request_fingerprint),
            SessionEvent::Command {
                operation: recorded,
                command: SessionCommand::LegacyImportBound { fingerprint },
                ..
            } if recorded == operation => Some(fingerprint),
            SessionEvent::Command {
                operation: recorded,
                ..
            } if recorded == operation => {
                return Err(StoreError::Invalid(
                    "operation ID already belongs to a different command",
                ));
            }
            _ => None,
        };
        if let Some(bound) = binding {
            if bound != fingerprint {
                return Err(StoreError::Invalid(
                    "legacy import operation reused with different input",
                ));
            }
            if found.replace(id).is_some() {
                return Err(StoreError::Integrity(
                    "legacy import operation has duplicate bindings",
                ));
            }
        }
    }
    drop(rows);
    drop(statement);
    let mut statement = connection.prepare("SELECT id,revision,head FROM sessions")?;
    let mut rows = statement.query([])?;
    let mut sessions = 0usize;
    while let Some(row) = rows.next()? {
        sessions += 1;
        if sessions > MAX_LOOKUP_EVENTS || Instant::now() >= deadline {
            return Err(StoreError::Invalid(
                "legacy import lookup exceeds its session/time bound",
            ));
        }
        let id = row
            .get::<_, String>(0)?
            .parse::<SessionId>()
            .map_err(|_| StoreError::Integrity("invalid import session cache identity"))?;
        let revision = u64::try_from(row.get::<_, i64>(1)?)
            .map_err(|_| StoreError::Integrity("invalid import session cache revision"))?;
        let Some((observed, hash)) = heads.remove(&id) else {
            return Err(StoreError::Integrity("import session journal is missing"));
        };
        if revision != observed || hash.to_string() != row.get::<_, String>(2)? {
            return Err(StoreError::Integrity(
                "import session head differs from journal",
            ));
        }
    }
    if !heads.is_empty() {
        return Err(StoreError::Integrity("import session cache is missing"));
    }
    Ok(found)
}
