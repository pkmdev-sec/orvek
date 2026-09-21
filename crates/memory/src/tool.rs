//! Explicit agent access to the global memory store.

use super::{MemoryAccess, MemoryError, MemoryKey, MemoryRecord, MemoryStore, SelectedMemoryStore};
use crate::{
    EvidenceState, LineRange, MemoryKind, MemoryMetadata, MemoryOrigin, MemoryScope, ProposalState,
    TraceReference, WorkspaceSources,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use zeroize::Zeroizing;

const DEFAULT_SCAN_LIMIT: usize = 5;

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum MemoryOperation {
    Scan {
        query: String,
        #[serde(default)]
        limit: Option<usize>,
    },
    Read {
        keys: Vec<MemoryKey>,
    },
    Put {
        content: MemoryContent,
        #[serde(default)]
        replace: Option<MemoryKey>,
        #[serde(default)]
        metadata: Option<MemoryDraft>,
    },
    ProposeLesson {
        content: MemoryContent,
        metadata: MemoryDraft,
        behavior_test: SourceRequest,
    },
    Delete {
        key: MemoryKey,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRequest {
    path: String,
    #[serde(default)]
    range: Option<LineRange>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DraftKind {
    Preference,
    Procedure,
    CodeClaim,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DraftScope {
    Global,
    Repository,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryDraft {
    scope: DraftScope,
    kind: DraftKind,
    #[serde(default)]
    sources: Vec<SourceRequest>,
}

/// Zeroizes Orvek's typed copy even when object deserialization later rejects the call.
///
/// Host/provider adapters retain raw JSON arguments and conversation records outside this wrapper.
/// Those copies do not provide a zeroization guarantee.
struct MemoryContent(Zeroizing<String>);

impl<'de> Deserialize<'de> for MemoryContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)
            .map(Zeroizing::new)
            .map(Self)
    }
}

#[derive(Serialize)]
struct ScanOutput {
    operation: &'static str,
    backend: MemoryAccess,
    abstained: bool,
    candidates: Vec<ToolCandidate>,
}

#[derive(Serialize)]
struct ToolCandidate {
    key: MemoryKey,
    preview: String,
    score: f64,
    metadata: MemoryMetadata,
    freshness: EvidenceState,
}

#[derive(Serialize)]
struct ReadOutput {
    operation: &'static str,
    backend: MemoryAccess,
    memories: Vec<Value>,
}

#[derive(Serialize)]
struct PutOutput {
    operation: &'static str,
    backend: MemoryAccess,
    memory: MemoryRecord,
    replaced: bool,
}

#[derive(Serialize)]
struct DeleteOutput {
    operation: &'static str,
    backend: MemoryAccess,
    key: MemoryKey,
}

/// Host-supplied permission; remote credentials can further restrict writes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryPermission {
    /// Scan and read only.
    ReadOnly,
    /// Scan, read, put and delete.
    ReadWrite,
}

/// A memory operation rejected at the protocol or storage boundary.
#[derive(Debug, thiserror::Error)]
pub enum MemoryOperationError {
    /// Arguments or run-local preconditions were not satisfied.
    #[error("{0}")]
    Invalid(&'static str),
    /// The selected backend rejected or could not complete the operation.
    #[error(transparent)]
    Store(#[from] MemoryError),
    /// The bounded output could not be encoded.
    #[error("memory output encoding failed")]
    Encoding(#[from] serde_json::Error),
}

/// Transport-independent memory operations for one admitted primary or read-only run.
pub struct MemorySession {
    store: SelectedMemoryStore,
    searched: AtomicBool,
    sources: Option<WorkspaceSources>,
    trace: Option<TraceReference>,
}

impl MemorySession {
    /// Applies the session's retrieval scope, not an authorization decision.
    pub fn visible(&self, metadata: &MemoryMetadata) -> bool {
        metadata.visible_in(self.sources.as_ref().map(WorkspaceSources::repository))
    }

    /// Creates run-local scan-before-put state over the selected backend.
    pub const fn new(store: SelectedMemoryStore) -> Self {
        Self {
            store,
            searched: AtomicBool::new(false),
            sources: None,
            trace: None,
        }
    }

    /// Installs the host workspace source boundary; non-repositories remain explicit unknowns.
    pub fn with_workspace(mut self, workspace: &std::path::Path) -> Self {
        self.sources = WorkspaceSources::open(workspace).ok();
        self
    }

    /// Producing run identity comes from the host, not model arguments.
    pub fn bind_trace(&mut self, trace: TraceReference) {
        self.trace = Some(trace);
    }

    fn freshness(&self, metadata: &MemoryMetadata) -> EvidenceState {
        if metadata.evidence.is_empty()
            && !matches!(metadata.kind, MemoryKind::LessonProposal { .. })
        {
            return EvidenceState::Unverified;
        }
        self.sources.as_ref().map_or_else(
            || EvidenceState::Unavailable {
                reason: "repository is not mounted".into(),
            },
            |sources| sources.assess(metadata),
        )
    }

    fn metadata(&self, draft: Option<MemoryDraft>) -> Result<MemoryMetadata, MemoryOperationError> {
        let mut metadata = MemoryMetadata {
            origin: MemoryOrigin::Model,
            ..MemoryMetadata::default()
        };
        if let Some(trace) = &self.trace {
            metadata.producing_traces.push(trace.clone());
        }
        if let Some(draft) = draft {
            metadata.scope = match draft.scope {
                DraftScope::Global => MemoryScope::Global,
                DraftScope::Repository => MemoryScope::Repository {
                    identity: self
                        .sources
                        .as_ref()
                        .ok_or(MemoryOperationError::Invalid(
                            "repository identity is unavailable",
                        ))?
                        .repository()
                        .into(),
                },
            };
            metadata.kind = match draft.kind {
                DraftKind::Preference => MemoryKind::Preference,
                DraftKind::Procedure => MemoryKind::Procedure,
                DraftKind::CodeClaim => MemoryKind::CodeClaim,
            };
            if draft.sources.len() > 16 {
                return Err(MemoryOperationError::Invalid(
                    "at most 16 sources are accepted",
                ));
            }
            for request in draft.sources {
                metadata.evidence.push(self.capture(request)?);
            }
        }
        metadata.validate()?;
        Ok(metadata)
    }

    fn capture(
        &self,
        request: SourceRequest,
    ) -> Result<crate::SourceEvidence, MemoryOperationError> {
        self.sources
            .as_ref()
            .ok_or(MemoryOperationError::Invalid(
                "repository sources are unavailable",
            ))?
            .capture(&request.path, request.range)
            .map_err(|_| MemoryOperationError::Invalid("evidence source is unavailable or invalid"))
    }

    /// Executes a closed operation shape. Permission comes from the host, never arguments.
    pub async fn execute(
        &self,
        arguments: Value,
        permission: MemoryPermission,
    ) -> Result<Value, MemoryOperationError> {
        let operation: MemoryOperation = serde_json::from_value(arguments)
            .map_err(|_| MemoryOperationError::Invalid("invalid memory operation arguments"))?;
        if matches!(
            operation,
            MemoryOperation::Put { .. }
                | MemoryOperation::ProposeLesson { .. }
                | MemoryOperation::Delete { .. }
        ) && permission == MemoryPermission::ReadOnly
        {
            return Err(MemoryOperationError::Invalid(
                "memory mutation is only available to primary tasks",
            ));
        }
        match operation {
            MemoryOperation::Scan { query, limit } => self.scan(query, limit).await,
            MemoryOperation::Read { keys } => self.read(keys).await,
            MemoryOperation::Put {
                content,
                replace,
                metadata,
            } => self.put(content, replace, metadata).await,
            MemoryOperation::ProposeLesson {
                content,
                metadata,
                behavior_test,
            } => self.propose(content, metadata, behavior_test).await,
            MemoryOperation::Delete { key } => self.delete(key).await,
        }
    }

    /// Provider-neutral schema restricted to the admitted permission.
    pub fn parameters(permission: MemoryPermission) -> Value {
        let mut schema = memory_input_schema();
        schema["type"] = json!("object");
        if permission == MemoryPermission::ReadOnly {
            schema["oneOf"]
                .as_array_mut()
                .expect("closed memory schema")
                .truncate(2);
        }
        schema
    }

    async fn scan(
        &self,
        query: String,
        limit: Option<usize>,
    ) -> Result<Value, MemoryOperationError> {
        if query.trim().is_empty() {
            return Err(MemoryOperationError::Invalid("memory scan query is empty"));
        }
        let limit = limit.unwrap_or(DEFAULT_SCAN_LIMIT);
        if !(1..=DEFAULT_SCAN_LIMIT).contains(&limit) {
            return Err(MemoryOperationError::Invalid(
                "memory scan limit must be between 1 and 5",
            ));
        }
        let backend = self.store.access().await?;
        let repository = self.sources.as_ref().map(WorkspaceSources::repository);
        let scan = self.store.scan_scoped(&query, limit, repository).await?;
        self.searched.store(true, Ordering::Release);
        json_output(&ScanOutput {
            operation: "scan",
            backend,
            abstained: scan.abstained,
            candidates: scan
                .candidates
                .into_iter()
                .map(|candidate| ToolCandidate {
                    key: candidate.key,
                    preview: candidate.preview,
                    score: candidate.score,
                    freshness: self.freshness(&candidate.metadata),
                    metadata: candidate.metadata,
                })
                .collect(),
        })
    }

    async fn read(&self, keys: Vec<MemoryKey>) -> Result<Value, MemoryOperationError> {
        if keys.is_empty() {
            return Err(MemoryOperationError::Invalid(
                "memory read requires at least one key",
            ));
        }
        let backend = self.store.access().await?;
        let repository = self.sources.as_ref().map(WorkspaceSources::repository);
        let memories = self
            .store
            .read_scoped(&[], &keys, repository)
            .await?
            .into_iter()
            .filter(|record| record.metadata.visible_in(repository))
            .map(|record| {
                let freshness = self.freshness(&record.metadata);
                let mut value = serde_json::to_value(record)?;
                value["freshness"] = serde_json::to_value(freshness)?;
                Ok(value)
            })
            .collect::<Result<Vec<_>, serde_json::Error>>()?;
        json_output(&ReadOutput {
            operation: "read",
            backend,
            memories,
        })
    }

    async fn put(
        &self,
        content: MemoryContent,
        replace: Option<MemoryKey>,
        draft: Option<MemoryDraft>,
    ) -> Result<Value, MemoryOperationError> {
        let content = content.0;
        if !self.searched.swap(false, Ordering::AcqRel) {
            return Err(MemoryOperationError::Invalid(
                "scan memory before storing a conclusion",
            ));
        }
        let backend = self.store.access().await?;
        let replaced = replace.is_some();
        let mut metadata = self.metadata(draft)?;
        if let Some(key) = &replace
            && let Some(previous) = self
                .store
                .read_scoped(
                    &[],
                    std::slice::from_ref(key),
                    self.sources.as_ref().map(WorkspaceSources::repository),
                )
                .await?
                .first()
        {
            metadata.imported_from = previous.metadata.imported_from.clone();
            metadata.transferred_from = previous.metadata.transferred_from.clone();
            metadata.ownership_id = previous.metadata.ownership_id.clone();
        }
        let memory = self
            .store
            .put_with_metadata(content.as_str(), &metadata, replace)
            .await?;
        json_output(&PutOutput {
            operation: "put",
            backend,
            memory,
            replaced,
        })
    }

    async fn propose(
        &self,
        content: MemoryContent,
        draft: MemoryDraft,
        behavior_test: SourceRequest,
    ) -> Result<Value, MemoryOperationError> {
        if self.trace.is_none() {
            return Err(MemoryOperationError::Invalid(
                "lesson proposals require a host-bound producing run",
            ));
        }
        if !self.searched.swap(false, Ordering::AcqRel) {
            return Err(MemoryOperationError::Invalid(
                "scan memory before proposing a lesson",
            ));
        }
        let mut metadata = self.metadata(Some(draft))?;
        if metadata.evidence.is_empty() {
            return Err(MemoryOperationError::Invalid(
                "lesson proposals require evidence",
            ));
        }
        metadata.kind = MemoryKind::LessonProposal {
            behavior_test: self.capture(behavior_test)?,
            state: ProposalState::Pending,
        };
        let record = crate::propose_lesson(&self.store, &content.0, metadata).await?;
        Ok(
            json!({"operation":"propose_lesson", "memory":record, "authority":"reference_data", "behavior_test_status":"cited_not_executed"}),
        )
    }

    async fn delete(&self, key: MemoryKey) -> Result<Value, MemoryOperationError> {
        let backend = self.store.access().await?;
        self.store.delete(key.clone()).await?;
        json_output(&DeleteOutput {
            operation: "delete",
            backend,
            key,
        })
    }
}

fn json_output(value: &impl Serialize) -> Result<Value, MemoryOperationError> {
    Ok(serde_json::to_value(value)?)
}

fn memory_input_schema() -> Value {
    let mut schema = json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "operation": { "type": "string", "const": "scan" },
                    "query": { "type": "string", "minLength": 1, "maxLength": 512 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 5, "default": 5 }
                },
                "required": ["operation", "query"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "operation": { "type": "string", "const": "read" },
                    "keys": {
                        "type": "array",
                        "items": memory_key_schema(),
                        "minItems": 1,
                        "description": "Exact candidate keys returned by scan. Preserve each id, version, and namespace unchanged."
                    }
                },
                "required": ["operation", "keys"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "operation": { "type": "string", "const": "put" },
                    "content": { "type": "string", "minLength": 1, "maxLength": 1024 },
                    "replace": memory_key_schema(),
                    "metadata": draft_schema()
                },
                "required": ["operation", "content"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "operation": { "type": "string", "const": "delete" },
                    "key": memory_key_schema()
                },
                "required": ["operation", "key"],
                "additionalProperties": false
            }
        ]
    });
    schema["oneOf"].as_array_mut().expect("closed schema").push(json!({
        "type":"object", "properties": {"operation":{"const":"propose_lesson","type":"string"}, "content":{"type":"string","maxLength":1024}, "metadata":draft_schema(), "behavior_test":source_schema()},
        "required":["operation","content","metadata","behavior_test"], "additionalProperties":false
    }));
    schema
}

fn source_schema() -> Value {
    json!({"type":"object","properties":{"path":{"type":"string"},"range":{"type":"object","properties":{"start":{"type":"integer","minimum":1},"end":{"type":"integer","minimum":1}},"required":["start","end"],"additionalProperties":false}},"required":["path"],"additionalProperties":false})
}
fn draft_schema() -> Value {
    json!({"type":"object","properties":{"scope":{"type":"string","enum":["global","repository"]},"kind":{"type":"string","enum":["preference","procedure","code_claim"]},"sources":{"type":"array","maxItems":16,"items":source_schema()}},"required":["scope","kind"],"additionalProperties":false})
}

fn memory_key_schema() -> Value {
    json!({
        "type": "object",
        "description": "An exact memory key returned by scan, read, list, or put. Preserve every field unchanged.",
        "properties": {
            "id": { "type": "integer", "minimum": 1 },
            "version": { "type": "integer", "minimum": 1 }
            ,"namespace": { "type": "string", "minLength": 1 }
        },
        "required": ["id", "version"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::{MemoryOperation, MemoryPermission, MemorySession};
    use crate::{MemoryStore, SelectedMemoryStore};
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn permission_specific_schema_is_closed_and_uses_exact_keys() {
        let writable = MemorySession::parameters(MemoryPermission::ReadWrite);
        let readonly = MemorySession::parameters(MemoryPermission::ReadOnly);
        let operations = writable["oneOf"].as_array().unwrap();

        assert_eq!(operations.len(), 5);
        assert_eq!(readonly["oneOf"].as_array().unwrap().len(), 2);
        assert!(
            operations
                .iter()
                .all(|operation| operation["additionalProperties"] == json!(false))
        );
        let operation = |name| {
            operations
                .iter()
                .find(|operation| operation["properties"]["operation"]["const"] == name)
                .unwrap()
        };
        assert_eq!(operation("read")["required"], json!(["operation", "keys"]));
        assert_eq!(operation("delete")["required"], json!(["operation", "key"]));
    }

    #[test]
    fn operation_arguments_cannot_supply_authority() {
        assert!(
            serde_json::from_value::<MemoryOperation>(json!({
                "operation": "delete",
                "key": {"id": 1, "version": 1},
                "is_root": true
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn session_enforces_permission_exact_keys_and_scan_before_put() {
        let directory = tempdir().unwrap();
        let store = SelectedMemoryStore::local(directory.path().join("memory.sqlite3"));
        let session = MemorySession::new(store.clone());
        let put = || json!({"operation": "put", "content": "Use one exact memory key shape."});

        assert_eq!(
            session
                .execute(put(), MemoryPermission::ReadWrite)
                .await
                .unwrap_err()
                .to_string(),
            "scan memory before storing a conclusion"
        );
        session
            .execute(
                json!({"operation": "scan", "query": "key contract"}),
                MemoryPermission::ReadWrite,
            )
            .await
            .unwrap();
        session
            .execute(put(), MemoryPermission::ReadWrite)
            .await
            .unwrap();
        let key = store.list().await.unwrap().remove(0).key;

        let read = session
            .execute(
                json!({"operation": "read", "keys": [key.clone()]}),
                MemoryPermission::ReadOnly,
            )
            .await
            .unwrap();
        assert_eq!(read["memories"].as_array().unwrap().len(), 1);
        assert_eq!(
            session
                .execute(
                    json!({"operation": "delete", "key": key.clone()}),
                    MemoryPermission::ReadOnly,
                )
                .await
                .unwrap_err()
                .to_string(),
            "memory mutation is only available to primary tasks"
        );
        session
            .execute(
                json!({"operation": "delete", "key": key}),
                MemoryPermission::ReadWrite,
            )
            .await
            .unwrap();
        assert!(store.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn selected_store_rejects_secret_content_before_storage() {
        let directory = tempdir().unwrap();
        let store = SelectedMemoryStore::local(directory.path().join("memory.sqlite3"));
        let session = MemorySession::new(store.clone());
        session
            .execute(
                json!({"operation": "scan", "query": "credentials"}),
                MemoryPermission::ReadWrite,
            )
            .await
            .unwrap();

        assert_eq!(
            session
                .execute(
                    json!({"operation": "put", "content": "password=hunter2"}),
                    MemoryPermission::ReadWrite,
                )
                .await
                .unwrap_err()
                .to_string(),
            "memory content was rejected as a likely secret"
        );
        assert!(store.list().await.unwrap().is_empty());
    }
}
