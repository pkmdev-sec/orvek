//! Provenance is data. Matching source bytes never certifies a claim's truth.
use crate::{MemoryError, MemoryKey, normalize_identity};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryScope {
    #[default]
    LegacyUnscoped,
    Global,
    Repository {
        identity: String,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryKind {
    #[default]
    Unverified,
    Preference,
    Procedure,
    CodeClaim,
    LessonProposal {
        behavior_test: SourceEvidence,
        state: ProposalState,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalState {
    Pending,
    Proposed,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryOrigin {
    #[default]
    LegacyUnknown,
    User,
    Model,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TraceReference {
    pub session: String,
    pub request: String,
    pub task: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceEvidence {
    File {
        repository: String,
        path: String,
        range: Option<LineRange>,
        checked_revision: String,
        content_digest: String,
    },
    Artifact {
        digest: String,
        source: String,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryMetadata {
    pub scope: MemoryScope,
    pub kind: MemoryKind,
    pub origin: MemoryOrigin,
    pub evidence: Vec<SourceEvidence>,
    pub producing_traces: Vec<TraceReference>,
    /// Original owning keys, including namespace and version, through each import.
    pub imported_from: Vec<MemoryKey>,
    /// Backend-generated identity of this owning record, independent of numeric allocation.
    pub ownership_id: Option<String>,
    /// Ordered transfer history. Unlike a numeric key, ownership IDs distinguish separate stores.
    pub transferred_from: Vec<OwnershipReference>,
    /// The run that created the current pending version; history cannot authorize finalization.
    pub pending_run: Option<TraceReference>,
    /// Retained citations from earlier nominations, excluded from active freshness assessment.
    pub historical_evidence: Vec<SourceEvidence>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipReference {
    pub ownership_id: String,
    pub key: MemoryKey,
}

/// Selection within the backend's authenticated writer ownership, never the shared UI window.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LessonQuery {
    Matching {
        scope: MemoryScope,
        content_identity: String,
    },
    Pending {
        trace: TraceReference,
    },
}
impl LessonQuery {
    pub fn matches(&self, record: &crate::MemoryRecord) -> bool {
        if !matches!(record.metadata.kind, MemoryKind::LessonProposal { .. }) {
            return false;
        }
        match self {
            Self::Matching {
                scope,
                content_identity,
            } => {
                &record.metadata.scope == scope
                    && normalize_identity(&record.content) == *content_identity
            }
            Self::Pending { trace } => {
                record.metadata.pending_run.as_ref() == Some(trace)
                    && matches!(
                        record.metadata.kind,
                        MemoryKind::LessonProposal {
                            state: ProposalState::Pending,
                            ..
                        }
                    )
            }
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EvidenceState {
    #[default]
    Unverified,
    /// All cited bytes match. This is not factual or behavioral verification.
    Current,
    Stale,
    Unavailable {
        reason: String,
    },
}

impl MemoryMetadata {
    pub fn validate(&self) -> Result<(), MemoryError> {
        let invalid = || MemoryError::InvalidMetadata;
        if serde_json::to_vec(self)
            .map_err(MemoryError::backend)?
            .len()
            + if self.ownership_id.is_none() { 32 } else { 0 }
            > 16 * 1024
            || self.evidence.len() > 16
            || self.producing_traces.len() > 32
            || self.imported_from.len() > 32
            || self.transferred_from.len() > 32
            || self.historical_evidence.len() > 64
            || self
                .ownership_id
                .as_ref()
                .is_some_and(|id| !valid_ownership(id))
            || self.transferred_from.iter().any(|source| {
                !valid_ownership(&source.ownership_id)
                    || source.key.id <= 0
                    || source.key.version == 0
            })
            || self.pending_run.as_ref().is_some_and(|trace| {
                trace.session.is_empty()
                    || trace.request.is_empty()
                    || trace.task.is_empty()
                    || !self.producing_traces.contains(trace)
                    || !matches!(
                        self.kind,
                        MemoryKind::LessonProposal {
                            state: ProposalState::Pending,
                            ..
                        }
                    )
            })
        {
            return Err(invalid());
        }
        if matches!(&self.scope, MemoryScope::Repository { identity } if identity.is_empty())
            || matches!(
                self.kind,
                MemoryKind::CodeClaim | MemoryKind::LessonProposal { .. }
            ) && self.evidence.is_empty()
            || matches!(
                self.kind,
                MemoryKind::CodeClaim | MemoryKind::LessonProposal { .. }
            ) && self.origin == MemoryOrigin::Model
                && self.producing_traces.is_empty()
            || self.producing_traces.iter().any(|trace| {
                trace.session.is_empty() || trace.request.is_empty() || trace.task.is_empty()
            })
            || self.imported_from.iter().any(|key| {
                key.id <= 0
                    || key.version == 0
                    || key.namespace.as_ref().is_some_and(|namespace| {
                        !crate::server::protocol::is_valid_namespace(namespace)
                    })
            })
        {
            return Err(invalid());
        }
        for evidence in
            self.evidence
                .iter()
                .chain(&self.historical_evidence)
                .chain(match &self.kind {
                    MemoryKind::LessonProposal { behavior_test, .. } => Some(behavior_test),
                    _ => None,
                })
        {
            match evidence {
                SourceEvidence::File {
                    repository,
                    path,
                    range,
                    checked_revision,
                    content_digest,
                } => {
                    if repository.is_empty()
                        || checked_revision.is_empty()
                        || path.is_empty()
                        || std::path::Path::new(path).is_absolute()
                        || std::path::Path::new(path)
                            .components()
                            .any(|c| !matches!(c, std::path::Component::Normal(_)))
                        || range
                            .as_ref()
                            .is_some_and(|r| r.start == 0 || r.end < r.start)
                        || !valid_digest(content_digest)
                    {
                        return Err(invalid());
                    }
                }
                SourceEvidence::Artifact { digest, source } => {
                    if source.is_empty() || !valid_digest(digest) {
                        return Err(invalid());
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn reject_likely_secret(&self) -> Result<(), MemoryError> {
        #[cfg(any(feature = "local", feature = "client", feature = "server"))]
        if crate::secrets::contains_likely_secret(
            &serde_json::to_string(self).map_err(MemoryError::backend)?,
        ) {
            return Err(MemoryError::SecretRejected);
        }
        Ok(())
    }

    /// Backend duplicate key includes scope and imported ownership.
    pub fn identity(&self, content: &str) -> String {
        if !self.transferred_from.is_empty() || !self.imported_from.is_empty() {
            return format!("import:{}", self.transfer_identity(content, None));
        }
        if self.scope == MemoryScope::LegacyUnscoped {
            return normalize_identity(content);
        }
        format!(
            "{}:{}",
            serde_json::to_string(&self.scope).expect("serializable scope"),
            normalize_identity(content)
        )
    }

    /// Exact semantic snapshot identity. Transfer-only history is merged, never used as payload.
    pub fn transfer_identity(&self, content: &str, namespace: Option<&str>) -> String {
        let origin = self
            .transferred_from
            .first()
            .map(|source| &source.ownership_id)
            .or(self.ownership_id.as_ref());
        let namespace = self
            .transferred_from
            .first()
            .map_or(namespace, |source| source.key.namespace.as_deref());
        let mut payload = self.clone();
        payload.ownership_id = None;
        payload.transferred_from.clear();
        payload.imported_from.clear();
        serde_json::to_string(&(origin, namespace, content, payload))
            .expect("serializable metadata")
    }

    pub fn visible_in(&self, repository: Option<&str>) -> bool {
        match &self.scope {
            MemoryScope::Repository { identity } => Some(identity.as_str()) == repository,
            MemoryScope::Global | MemoryScope::LegacyUnscoped => true,
        }
    }
}

fn valid_ownership(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
