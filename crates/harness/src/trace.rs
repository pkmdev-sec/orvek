//! Local forensic bundles. Replay reduces receipts; it never runs a provider or a tool.
use crate::{
    Digest, Store, StoreError,
    inference::{InferenceRequest, Transport, UsdCost},
    session::{SessionCommand, SessionCreation, SessionEvent, SessionId, SessionState},
    state::{TaskEvent, TaskId, TaskState},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::{Read, Write},
    path::Path,
};
use uuid::Uuid;

mod receipts;
pub use receipts::{CallReplay, CausalGap, CausalReplay, RecordedStatus, ToolReplay};

const VERSION: u32 = 1;
const FILE_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("trace I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("trace encoding: {0}")]
    Json(#[from] serde_json::Error),
    #[error("trace journal: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("trace store: {0}")]
    Store(#[from] StoreError),
    #[error("invalid trace: {0}")]
    Invalid(String),
}
fn invalid(message: impl Into<String>) -> TraceError {
    TraceError::Invalid(message.into())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct TraceLimits {
    pub records: usize,
    pub artifacts: usize,
    /// Bounds decoded storage and, separately, receipt decoding plus serialized spans.
    pub bytes: u64,
    pub depth: usize,
}
impl Default for TraceLimits {
    fn default() -> Self {
        Self {
            records: 100_000,
            artifacts: 10_000,
            bytes: 64 * 1024 * 1024,
            depth: 16,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraceRecord {
    pub sequence: u64,
    pub aggregate: Uuid,
    pub kind: String,
    pub revision: u64,
    /// Original bytes, not a reserialization of a JSON map.
    pub event_base64: String,
    pub hash: Digest,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
pub enum Payload {
    Present(String),
    Missing,
    Omitted,
    Bounded,
    Identity,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReplayIdentity {
    pub states: BTreeMap<String, Digest>,
    pub contexts: BTreeMap<String, Digest>,
    pub outcomes: BTreeMap<String, Digest>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraceBundle {
    pub version: u32,
    pub after: u64,
    pub through: u64,
    pub exporter_revision: Option<String>,
    pub limits: TraceLimits,
    pub records: Vec<TraceRecord>,
    pub artifacts: BTreeMap<Digest, Payload>,
    pub expected: ReplayIdentity,
    pub exact: bool,
    pub unresolved: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    digest: Digest,
    bundle: TraceBundle,
}
#[derive(Debug, Serialize)]
pub struct ReplayReport {
    /// Causal gaps are separate from journal and artifact closure consistency.
    pub causality: CausalReplay,
    pub exact: bool,
    pub identity: ReplayIdentity,
    pub sessions: BTreeMap<SessionId, SessionState>,
    pub tasks: BTreeMap<TaskId, TaskState>,
    pub spans: Vec<Value>,
    pub cost: CostConfidence,
    pub unresolved: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct CostConfidence {
    /// Sum of attributable cost receipts, not an exact total unless complete.
    pub recorded_usd: UsdCost,
    pub complete: bool,
    pub calls: usize,
    pub unknown_calls: usize,
    /// Unknown when any usage cannot be attributed and deduplicated by call.
    pub total_tokens: Option<u64>,
}
#[derive(Debug, Serialize)]
pub struct PrefixFixture {
    pub version: u32,
    pub before_sequence: u64,
    pub decision: Value,
    pub prefix: TraceBundle,
}

impl TraceBundle {
    /// Reads the entire host prefix. This is intentionally not a public/redacted export.
    /// SQLite pins the range; no owner lock, migration, or recovery is performed.
    pub fn export(
        root: &Path,
        through: Option<u64>,
        limits: TraceLimits,
        omitted: &BTreeSet<Digest>,
        exporter_revision: Option<String>,
    ) -> Result<Self, TraceError> {
        validate_limits(limits)?;
        let mut connection =
            Connection::open_with_flags(root.join("v1.sqlite3"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let transaction = connection.transaction()?;
        let head: i64 =
            transaction.query_row("SELECT COALESCE(MAX(sequence),0) FROM events", [], |row| {
                row.get(0)
            })?;
        let head = u64::try_from(head).map_err(|_| invalid("negative journal head"))?;
        let through = through.unwrap_or(head);
        if through > head {
            return Err(invalid("cursor is beyond journal head"));
        }
        let mut statement = transaction.prepare("SELECT sequence,aggregate,kind,revision,event,hash FROM events WHERE sequence<=?1 ORDER BY sequence")?;
        let mut rows = statement
            .query([i64::try_from(through).map_err(|_| invalid("cursor exceeds SQLite range"))?])?;
        let mut records = Vec::new();
        let mut used = 0u64;
        while let Some(row) = rows.next()? {
            let bytes: Vec<u8> = row.get(4)?;
            used = used.saturating_add(bytes.len() as u64);
            if records.len() >= limits.records || used > limits.bytes {
                return Err(invalid(
                    "journal exceeds export bounds; choose an earlier --through cursor",
                ));
            }
            let aggregate: String = row.get(1)?;
            let hash: String = row.get(5)?;
            records.push(TraceRecord {
                sequence: u64::try_from(row.get::<_, i64>(0)?)
                    .map_err(|_| invalid("negative cursor"))?,
                aggregate: Uuid::parse_str(&aggregate).map_err(|_| invalid("aggregate UUID"))?,
                kind: row.get(2)?,
                revision: u64::try_from(row.get::<_, i64>(3)?)
                    .map_err(|_| invalid("negative revision"))?,
                event_base64: STANDARD.encode(bytes),
                hash: hash.parse().map_err(invalid)?,
            });
        }
        let mut bundle = Self {
            version: VERSION,
            after: 0,
            through,
            exporter_revision,
            limits,
            records,
            artifacts: BTreeMap::new(),
            expected: ReplayIdentity::default(),
            exact: false,
            unresolved: vec![],
        };
        let reduced = bundle.reduce()?;
        // At the current head also compare the authoritative cached projections.
        if through == head {
            for (kind, states) in [
                (
                    "sessions",
                    reduced
                        .sessions
                        .iter()
                        .map(|(id, s)| Ok((id.to_string(), serde_json::to_value(s)?)))
                        .collect::<Result<Vec<_>, serde_json::Error>>()?,
                ),
                (
                    "tasks",
                    reduced
                        .tasks
                        .iter()
                        .map(|(id, s)| Ok((id.to_string(), serde_json::to_value(s)?)))
                        .collect::<Result<Vec<_>, serde_json::Error>>()?,
                ),
            ] {
                for (id, state) in states {
                    let cached: Vec<u8> = transaction.query_row(
                        &format!("SELECT state FROM {kind} WHERE id=?1"),
                        [id],
                        |row| row.get(0),
                    )?;
                    if serde_json::from_slice::<Value>(&cached)? != state {
                        return Err(invalid("journal disagrees with stored state"));
                    }
                }
            }
        }
        bundle.expected = reduced.identity;
        let mut queue = VecDeque::new();
        for record in &bundle.records {
            references(&decode_json(&record.event_base64)?, 0, &mut queue);
        }
        while let Some((digest, depth, identity)) = queue.pop_front() {
            if let Some(payload) = bundle.artifacts.get_mut(&digest) {
                if matches!(payload, Payload::Identity) && !identity {
                    *payload = Payload::Missing;
                }
                continue;
            }
            if bundle.artifacts.len() >= limits.artifacts {
                bundle.unresolved.push(
                    "artifact count bound reached; remaining closure was not traversed".into(),
                );
                break;
            }
            let payload = if omitted.contains(&digest) {
                Payload::Omitted
            } else if depth > limits.depth {
                Payload::Bounded
            } else {
                let path = root.join("artifacts").join(digest.to_string());
                match fs::symlink_metadata(&path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        if identity {
                            Payload::Identity
                        } else {
                            Payload::Missing
                        }
                    }
                    Err(error) => return Err(error.into()),
                    Ok(meta) => {
                        if !meta.file_type().is_file() {
                            return Err(invalid(format!(
                                "artifact is not a regular file: {digest}"
                            )));
                        }
                        if used.saturating_add(meta.len()) > limits.bytes {
                            Payload::Bounded
                        } else {
                            let bytes = read_bounded(&path, limits.bytes.saturating_sub(used))?;
                            if Digest::of(&bytes) != digest {
                                return Err(invalid(format!("artifact hash mismatch: {digest}")));
                            }
                            used += bytes.len() as u64;
                            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                                references(&value, depth + 1, &mut queue);
                            }
                            Payload::Present(STANDARD.encode(bytes))
                        }
                    }
                }
            };
            bundle.artifacts.insert(digest, payload);
        }
        bundle.exact = bundle.unresolved.is_empty()
            && bundle
                .artifacts
                .values()
                .all(|p| matches!(p, Payload::Present(_) | Payload::Identity));
        bundle.replay()?;
        Ok(bundle)
    }

    pub fn write(&self, path: &Path) -> Result<(), TraceError> {
        self.replay()?;
        let bytes = serde_json::to_vec(&Envelope {
            digest: Digest::of_value(self)?,
            bundle: self.clone(),
        })?;
        if bytes.len() as u64 > FILE_LIMIT {
            return Err(invalid("encoded bundle exceeds file bound"));
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(())
    }
    pub fn read(path: &Path) -> Result<Self, TraceError> {
        let envelope: Envelope = serde_json::from_slice(&read_bounded(path, FILE_LIMIT)?)?;
        if Digest::of_value(&envelope.bundle)? != envelope.digest {
            return Err(invalid("bundle hash mismatch"));
        }
        envelope.bundle.replay()?;
        Ok(envelope.bundle)
    }
    pub fn replay(&self) -> Result<ReplayReport, TraceError> {
        validate_limits(self.limits)?;
        if self.records.len() > self.limits.records {
            return Err(invalid("record count exceeds bound"));
        }
        if self.artifacts.len() > self.limits.artifacts {
            return Err(invalid("artifact count exceeds bound"));
        }
        let mut unresolved = Vec::new();
        let mut references_queue = VecDeque::new();
        let mut used = 0u64;
        for record in &self.records {
            used = used.saturating_add(decoded_len(&record.event_base64)?);
            if used > self.limits.bytes {
                return Err(invalid("decoded bundle exceeds byte bound"));
            }
            let bytes = decode(&record.event_base64)?;
            references(
                &serde_json::from_slice::<Value>(&bytes)?,
                0,
                &mut references_queue,
            );
        }
        for (digest, payload) in &self.artifacts {
            match payload {
                Payload::Present(encoded) => {
                    used = used.saturating_add(decoded_len(encoded)?);
                    if used > self.limits.bytes {
                        return Err(invalid("decoded bundle exceeds byte bound"));
                    }
                    let bytes = decode(encoded)?;
                    if Digest::of(&bytes) != *digest {
                        return Err(invalid(format!("artifact hash mismatch: {digest}")));
                    }
                }
                Payload::Identity => {}
                other => unresolved.push(format!("artifact {digest}: {other:?}")),
            }
        }
        let mut report = self.reduce()?;
        if report.identity != self.expected {
            return Err(invalid(
                "reconstructed state/context/outcome differs from manifest",
            ));
        }
        report.unresolved = unresolved;
        let mut traversed = BTreeSet::new();
        while let Some((digest, depth, identity)) = references_queue.pop_front() {
            if matches!(self.artifacts.get(&digest), Some(Payload::Identity)) && !identity {
                report
                    .unresolved
                    .push(format!("payload mislabeled identity {digest}"));
            }
            if !traversed.insert(digest) {
                continue;
            }
            if depth > self.limits.depth {
                report
                    .unresolved
                    .push(format!("artifact hop bound exceeded: {digest}"));
                continue;
            }
            match self.artifacts.get(&digest) {
                None => report
                    .unresolved
                    .push(format!("untraversed reference {digest}")),
                Some(Payload::Present(encoded)) => {
                    if let Ok(value) = decode_json(encoded) {
                        references(&value, depth + 1, &mut references_queue);
                    }
                }
                _ => {}
            }
        }
        report.unresolved.extend(self.unresolved.clone());
        report.exact = report.unresolved.is_empty();
        if self.exact && !report.exact {
            return Err(invalid(
                "manifest claims exact replay with missing or omitted data",
            ));
        }
        report.exact &= self.exact;
        Ok(report)
    }

    fn reduce(&self) -> Result<ReplayReport, TraceError> {
        if self.version != VERSION || self.after != 0 {
            return Err(invalid("unsupported bundle version or nonzero baseline"));
        }
        if self.records.len() > self.limits.records {
            return Err(invalid("record count exceeds bound"));
        }
        let mut sessions = BTreeMap::<SessionId, SessionState>::new();
        let mut tasks = BTreeMap::<TaskId, TaskState>::new();
        let mut heads = BTreeMap::<(String, Uuid), (u64, Digest)>::new();
        let mut spans = Vec::new();
        let mut materialization = MaterializationBudget(self.limits.bytes);
        materialization.consume(2)?; // The spans array delimiters.
        let mut unlinked_usage = false;
        let mut costs = BTreeMap::new();
        let mut tokens = BTreeMap::new();
        let mut calls = BTreeSet::new();
        let mut cost_uncertain = false;
        for (index, record) in self.records.iter().enumerate() {
            if record.sequence != index as u64 + 1 {
                return Err(invalid("global journal cursor gap"));
            }
            let key = (record.kind.clone(), record.aggregate);
            let previous = heads.get(&key);
            if record.revision != previous.map_or(1, |(revision, _)| revision + 1) {
                return Err(invalid("aggregate revision gap"));
            }
            let bytes = decode(&record.event_base64)?;
            let hash = crate::store::aggregate_hash(
                &record.kind,
                record.aggregate,
                record.revision,
                previous.map(|(_, hash)| *hash),
                &bytes,
            )?;
            if hash != record.hash {
                return Err(invalid("journal hash mismatch"));
            }
            heads.insert(key, (record.revision, hash));
            match record.kind.as_str() {
                "task" => {
                    let id = TaskId(record.aggregate);
                    let event: TaskEvent = serde_json::from_slice(&bytes)?;
                    match (&mut tasks.get_mut(&id), &event) {
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
                            tasks.insert(
                                id,
                                TaskState::requested(
                                    id,
                                    request.clone(),
                                    *limits,
                                    *at_ms,
                                    Some(*intake),
                                    *input,
                                ),
                            );
                        }
                        (None, TaskEvent::Created { contract, at_ms }) => {
                            tasks.insert(id, TaskState::created(id, contract.clone(), *at_ms));
                        }
                        (Some(_), TaskEvent::Requested { .. } | TaskEvent::Created { .. })
                        | (None, _) => return Err(invalid("task creation sequence")),
                        (Some(state), _) => state.apply(&event)?,
                    }
                    if let TaskEvent::ModelCallRecorded { operation, receipt } = &event {
                        tokens.insert(*operation, receipt.tokens);
                        let report = match self.artifacts.get(&receipt.report) {
                            Some(Payload::Present(encoded)) => {
                                materialization.consume(decoded_len(encoded)?)?;
                                decode_json(encoded).ok()
                            }
                            _ => None,
                        };
                        materialization.push(&mut spans, json!({"sequence":record.sequence,"task":id,"span":{"kind":"model_response","call":operation,"receipt":receipt,"report":report}}))?;
                    }
                    let value = serde_json::to_value(&event)?;
                    if matches!(
                        event,
                        TaskEvent::ModelCallReserved { .. }
                            | TaskEvent::ModelCallRecorded { .. }
                            | TaskEvent::JobStarted(_)
                            | TaskEvent::JobSettled { .. }
                            | TaskEvent::Observed(_)
                            | TaskEvent::Completed(_)
                            | TaskEvent::Stopped { .. }
                    ) {
                        materialization.push(
                            &mut spans,
                            json!({"sequence":record.sequence,"task":id,"event":value}),
                        )?;
                    }
                    match event {
                        TaskEvent::ModelCallReserved { operation } => {
                            calls.insert(operation);
                        }
                        TaskEvent::UsageCharged { .. } => unlinked_usage = true,
                        _ => {}
                    }
                }
                "session" => {
                    let id = SessionId(record.aggregate);
                    let event: SessionEvent = serde_json::from_slice(&bytes)?;
                    match (sessions.get_mut(&id), event) {
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
                            sessions.insert(
                                id,
                                SessionState::create(
                                    id,
                                    SessionCreation {
                                        branch,
                                        config,
                                        admission: admission.map(|p| *p),
                                        parent,
                                        history,
                                        started_ms: at_ms,
                                        imported: imported.map(|p| *p),
                                    },
                                ),
                            );
                        }
                        (
                            Some(state),
                            SessionEvent::Command {
                                operation, command, ..
                            },
                        ) => {
                            if let SessionCommand::ContextProjected {
                                view: Some(view),
                                source_revision,
                                ..
                            } = &command
                                && (*source_revision != state.revision || !view.valid_for(state))
                            {
                                return Err(invalid("selected context does not match source"));
                            }
                            match &command {
                                SessionCommand::ProviderCost { call, cost_usd, .. } => {
                                    calls.insert(*call);
                                    if costs
                                        .insert(*call, *cost_usd)
                                        .is_some_and(|old| old != *cost_usd)
                                    {
                                        cost_uncertain = true;
                                    }
                                }
                                SessionCommand::Feedback { message } => materialization.push(&mut spans, json!({"sequence":record.sequence,"session":id,"span":{"kind":"host_feedback","message":message}}))?,
                                SessionCommand::ProviderUsage { call: None, .. } => {
                                    unlinked_usage = true
                                }
                                SessionCommand::ProviderUsage {
                                    call: Some(call), ..
                                } => {
                                    calls.insert(*call);
                                }
                                SessionCommand::TraceRecorded {
                                    record: receipt, ..
                                } => {
                                    if let Some(Payload::Present(encoded)) =
                                        self.artifacts.get(receipt)
                                    {
                                        materialization.consume(decoded_len(encoded)?)?;
                                        let span = decode_json(encoded)?;
                                        if let Some(call) = span
                                            .get("call")
                                            .and_then(Value::as_str)
                                            .and_then(|v| Uuid::parse_str(v).ok())
                                        {
                                            calls.insert(call);
                                            if span["kind"] == "model_response" && let Ok(outcome) = serde_json::from_value::<crate::inference::CallOutcome>(span["outcome"].clone()) {
                                                tokens.insert(call, outcome.accounted_tokens());
                                            }
                                        }
                                        materialization.push(&mut spans, json!({"sequence":record.sequence,"session":id,"receipt":receipt,"span":span}))?;
                                    }
                                }
                                _ => {}
                            }
                            state.apply(operation, &command)?;
                        }
                        _ => return Err(invalid("session creation sequence")),
                    }
                }
                _ => return Err(invalid("unsupported journal aggregate")),
            }
        }
        if self.through != self.records.last().map_or(0, |r| r.sequence) {
            return Err(invalid("pinned range is incomplete"));
        }
        let mut identity = ReplayIdentity::default();
        for (id, state) in &sessions {
            identity
                .states
                .insert(id.to_string(), Digest::of_value(state)?);
            identity.contexts.insert(
                id.to_string(),
                Digest::of_value(&(&state.history, &state.context_view))?,
            );
            identity.outcomes.insert(
                id.to_string(),
                Digest::of_value(&(&state.outcome, &state.error))?,
            );
        }
        for (id, state) in &tasks {
            identity
                .states
                .insert(id.to_string(), Digest::of_value(state)?);
            identity.outcomes.insert(
                id.to_string(),
                Digest::of_value(&(
                    &state.outcome,
                    &state.candidate,
                    &state.evidence,
                    &state.certificates,
                ))?,
            );
        }
        let mut recorded_usd = UsdCost::ZERO;
        let mut unknown_calls = 0;
        for call in &calls {
            match costs
                .get(call)
                .copied()
                .flatten()
                .and_then(|cost| recorded_usd.checked_add(cost))
            {
                Some(total) => recorded_usd = total,
                None => unknown_calls += 1,
            }
        }
        let mut report = ReplayReport {
            causality: CausalReplay::default(),
            exact: false,
            identity,
            sessions,
            tasks,
            spans,
            cost: CostConfidence {
                recorded_usd,
                complete: unknown_calls == 0 && !cost_uncertain && !unlinked_usage,
                calls: calls.len(),
                unknown_calls,
                total_tokens: if unlinked_usage {
                    None
                } else {
                    calls.iter().try_fold(0u64, |total, call| {
                        total.checked_add(tokens.get(call).copied().flatten()?)
                    })
                },
            },
            unresolved: vec![],
        };
        report.causality = receipts::replay(self, &report, &mut materialization)?;
        Ok(report)
    }

    /// Each fixture ends immediately before a recorded model dispatch (not its answer).
    pub fn prefixes(
        &self,
    ) -> Result<impl Iterator<Item = Result<PrefixFixture, TraceError>> + '_, TraceError> {
        self.replay()?;
        Ok(self
            .records
            .iter()
            .enumerate()
            .filter_map(|(index, record)| match self.prefix_at(index, record) {
                Ok(None) => None,
                Ok(Some(fixture)) => Some(Ok(fixture)),
                Err(error) => Some(Err(error)),
            }))
    }

    fn prefix_at(
        &self,
        index: usize,
        record: &TraceRecord,
    ) -> Result<Option<PrefixFixture>, TraceError> {
        let event = decode_json(&record.event_base64)?;
        let Some(digest) = event
            .pointer("/data/command/data/record")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<Digest>().ok())
        else {
            return Ok(None);
        };
        let Some(Payload::Present(encoded)) = self.artifacts.get(&digest) else {
            return Ok(None);
        };
        let mut decision = decode_json(encoded)?;
        if decision["kind"] != "model_dispatch" {
            return Ok(None);
        }
        if let Some(payload) = decision
            .get("payload")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<Digest>().ok())
        {
            // Historical dispatches also used the logical HTTP template, never
            // the effective auth/route body. Do not relabel them as wire captures.
            decision["payload_kind"] = json!("logical_http_template");
            decision["wire"] = json!({"status":"unavailable","reason":"outcome_not_recorded"});
            decision["logical_request_payload"] = match self.artifacts.get(&payload) {
                Some(Payload::Present(encoded)) => decode_json(encoded)?,
                _ => Value::Null,
            };
        }
        let mut prefix = self.clone();
        prefix.records.truncate(index);
        prefix.through = record.sequence - 1;
        // Do not leak future receipts/answers into the fixture.
        let mut needed = VecDeque::new();
        for r in &prefix.records {
            references(&decode_json(&r.event_base64)?, 0, &mut needed);
        }
        let mut retained = BTreeMap::new();
        while let Some((digest, depth, _)) = needed.pop_front() {
            if retained.contains_key(&digest) {
                continue;
            }
            if let Some(payload) = self.artifacts.get(&digest) {
                if let Payload::Present(encoded) = payload
                    && let Ok(value) = decode_json(encoded)
                {
                    references(&value, depth + 1, &mut needed);
                }
                retained.insert(digest, payload.clone());
            }
        }
        prefix.artifacts = retained;
        prefix.expected = prefix.reduce()?.identity;
        prefix.replay()?;
        Ok(Some(PrefixFixture {
            version: VERSION,
            before_sequence: record.sequence,
            decision,
            prefix,
        }))
    }

    /// Re-execution starts from user intent, never from an old tool dispatch.
    /// Uncertain effects must be reconciled separately by the original host.
    pub fn reexecution_intent(&self, task: TaskId) -> Result<String, TraceError> {
        let report = self.replay()?;
        let state = report
            .tasks
            .get(&task)
            .ok_or_else(|| invalid("task is absent from trace"))?;
        if state.jobs.values().any(|job| job.status.unresolved())
            || state.effects.values().any(|effect| {
                matches!(
                    effect.status,
                    crate::state::EffectStatus::Intended | crate::state::EffectStatus::Unknown
                )
            })
        {
            return Err(invalid(
                "uncertain effects or jobs cannot be re-executed; reconcile the original run first",
            ));
        }
        Ok(state.request.clone())
    }

    pub fn review(&self) -> Result<Value, TraceError> {
        let report = self.replay()?;
        let tasks=report.tasks.values().map(|task|json!({"task":task.id,"intent":task.request,"contract":task.contract,"candidate":task.candidate,"delivery":task.delivery,"patch":task.delivery.as_ref().filter(|delivery|delivery.kind == crate::contract::DeliveryKind::Patch).map(|delivery|json!({"digest":delivery.artifact,"payload":self.artifacts.get(&delivery.artifact)})),"verification":task.evidence,"certificates":task.certificates,"outcome":task.outcome})).collect::<Vec<_>>();
        Ok(
            json!({"version":VERSION,"range":{"after":self.after,"through":self.through},"exporter_revision":self.exporter_revision,"exact":report.exact,"causality":report.causality,"tasks":tasks,"cost":report.cost,"provenance":report.sessions.values().map(|s|json!({"session":s.id,"settings":s.model(),"admission":s.admission(),"context":s.context_view})).collect::<Vec<_>>(),"spans":report.spans,"unresolved":report.unresolved,"limitations":["Receipt replay is not fresh verification.","Native finished_unverified is not a completion certificate.","Provider-hidden reasoning and unrecorded external state are unavailable.","Historical missing call/child links are not inferred.","Dispatch payloads are logical HTTP templates, not effective provider bodies. Effective body/transport/dialect are available only in captured outcomes; missing outcomes (including crashes) leave wire provenance unavailable.","Prepared bodies and dispatched attempts do not prove remote delivery. Headers, credentials, endpoints and network framing are not captured.","Hashes detect corruption, not a malicious wholesale rewrite."]}),
        )
    }
}

// Charge each receipt before decoding, even when its digest was already seen. Count
// serialized spans without allocating another output buffer. This is a byte budget,
// not an allocator/RSS limit; one span and JSON container overhead are also live.
struct MaterializationBudget(u64);

impl MaterializationBudget {
    fn charge(&mut self, value: &impl Serialize) -> Result<(), TraceError> {
        serde_json::to_writer(self, value)?;
        Ok(())
    }

    fn consume(&mut self, bytes: u64) -> Result<(), TraceError> {
        self.0 = self
            .0
            .checked_sub(bytes)
            .ok_or_else(|| invalid("replay materialization exceeds byte bound"))?;
        Ok(())
    }

    fn push(&mut self, spans: &mut Vec<Value>, span: Value) -> Result<(), TraceError> {
        if !spans.is_empty() {
            self.consume(1)?;
        }
        serde_json::to_writer(&mut *self, &span)?;
        spans.push(span);
        Ok(())
    }
}

impl Write for MaterializationBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.consume(bytes.len() as u64)
            .map_err(std::io::Error::other)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// STANDARD requires padded, four-byte groups. Check size before allocating the
// decoded buffer; decode() still validates the alphabet and padding bits.
fn decoded_len(encoded: &str) -> Result<u64, TraceError> {
    if !encoded.len().is_multiple_of(4) {
        return Err(invalid("invalid base64 payload"));
    }
    let padding = if encoded.ends_with("==") {
        2
    } else if encoded.ends_with('=') {
        1
    } else {
        0
    };
    Ok((encoded.len() / 4 * 3 - padding) as u64)
}

fn validate_limits(limits: TraceLimits) -> Result<(), TraceError> {
    let max = TraceLimits::default();
    if limits.records == 0
        || limits.records > max.records
        || limits.artifacts == 0
        || limits.artifacts > max.artifacts
        || limits.bytes == 0
        || limits.bytes > max.bytes
        || limits.depth > max.depth
    {
        return Err(invalid("limits exceed supported bounds"));
    }
    Ok(())
}
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, TraceError> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(invalid("input is not a regular file"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("file exceeds byte bound"));
    }
    Ok(bytes)
}
fn decode(text: &str) -> Result<Vec<u8>, TraceError> {
    STANDARD
        .decode(text)
        .map_err(|_| invalid("invalid base64 payload"))
}
fn decode_json(text: &str) -> Result<Value, TraceError> {
    Ok(serde_json::from_slice(&decode(text)?)?)
}

// Unknown digest-shaped references are deliberately conservative: absent data is a gap,
// not a synthesized artifact. These fields identify values rather than stored payloads.
fn references(value: &Value, depth: usize, out: &mut VecDeque<(Digest, usize, bool)>) {
    fn walk(value: &Value, key: &str, depth: usize, out: &mut VecDeque<(Digest, usize, bool)>) {
        match value {
            Value::String(text) => {
                if matches!(key, "output" | "arguments")
                    && let Ok(nested) = serde_json::from_str::<Value>(text)
                {
                    walk(&nested, "", depth + 1, out);
                }
                if let Ok(digest) = text.parse() {
                    out.push_back((
                        digest,
                        depth,
                        matches!(
                            key,
                            "contract"
                                | "check_definition"
                                | "renderer"
                                | "controls"
                                | "source_history"
                                | "source_digest"
                                | "helper_digest"
                                | "original_history"
                                | "sent_input"
                                | "lineage"
                                | "routing"
                                | "stable_segments"
                                | "behavior"
                                | "envelope"
                                | "policy"
                                | "protocol"
                                | "model"
                                | "task_profile"
                                | "authority"
                                | "request_digest"
                                | "legacy_config_digest"
                                | "host_config"
                                | "host_build"
                                | "fingerprint"
                                | "request_fingerprint"
                                | "harness_revision"
                                | "revision"
                        ),
                    ));
                }
            }
            Value::Array(values) => {
                for value in values {
                    walk(value, key, depth, out);
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    let digest = value.as_str().and_then(|text| text.parse::<Digest>().ok());
                    let identity = (key == "identity"
                        && values.contains_key("backend")
                        && values.contains_key("window_limit")
                        && values.contains_key("keys"))
                        || (key == "digest"
                            && values.contains_key("path")
                            && values
                                .get("content")
                                .and_then(Value::as_str)
                                .is_some_and(|body| digest == Some(Digest::of(body.as_bytes()))))
                        || (key == "input"
                            && (values.contains_key("segments")
                                || values.contains_key("input_range")))
                        || (key == "environment" && values.contains_key("protocol"))
                        || (key == "executable_digest"
                            && values.contains_key("executable")
                            && values.contains_key("version"))
                        || (matches!(key.as_str(), "instructions" | "tools")
                            && values.contains_key("lineage"))
                        || (key == "digest"
                            && ((values.contains_key("bytes") && values.contains_key("mode"))
                                || (values.contains_key("path")
                                    && (values.contains_key("size_bytes")
                                        || values.contains_key("written_bytes")))
                                || values.get("kind").is_some_and(|kind| kind == "digest")));
                    if identity && let Some(digest) = digest {
                        out.push_back((digest, depth, true));
                    } else {
                        walk(value, key, depth, out);
                    }
                }
            }
            _ => {}
        }
    }
    walk(value, "", depth, out);
}

/// Record a causal span. Child proposals retain their own call IDs without gaining
/// authority in the parent's pending-call map.
pub(crate) fn record_span(
    store: &mut Store,
    session: SessionId,
    request: Uuid,
    span: Value,
) -> Result<Digest, StoreError> {
    let record = store.artifacts().put(&serde_json::to_vec(&span)?)?;
    let state = store.load_session(session)?;
    store.session_command(
        session,
        state.revision,
        Uuid::new_v4(),
        SessionCommand::TraceRecorded { request, record },
    )?;
    Ok(record)
}
/// Durable logical intent before dispatch. Effective bodies belong to provider outcomes.
pub(crate) fn record_dispatch(
    store: &mut Store,
    session: SessionId,
    request: Uuid,
    task: TaskId,
    child: Option<Uuid>,
    call: Uuid,
    inference: &InferenceRequest,
) -> Result<(), StoreError> {
    let wire = inference.wire(Transport::Http);
    let input = store
        .artifacts()
        .put(&serde_json::to_vec(&wire["input"])?)?;
    let tools = store
        .artifacts()
        .put(&serde_json::to_vec(&wire["tools"])?)?;
    let instructions = store
        .artifacts()
        .put(wire["instructions"].as_str().unwrap_or_default().as_bytes())?;
    let payload = store.artifacts().put(&serde_json::to_vec(&wire)?)?;
    record_span(
        store,
        session,
        request,
        json!({"version":VERSION,"kind":"model_dispatch","source_revision":env!("ORVEK_TRACE_SOURCE_REVISION"),"source_dirty":env!("ORVEK_TRACE_SOURCE_DIRTY"),"session":session,"request":request,"task":task,"child":child,"call":call,"model":inference.settings(),"input":input,"tools":tools,"instructions":instructions,"payload":payload,"payload_kind":"logical_http_template","wire":{"status":"unavailable","reason":"outcome_not_recorded"},"cache":inference.cache_identity()}),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
