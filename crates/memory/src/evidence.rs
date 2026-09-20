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
        #[cfg(any(feature = "local", feature = "client"))]
        if crate::secrets::contains_likely_secret(
            &serde_json::to_string(self).map_err(MemoryError::backend)?,
        ) {
            return Err(MemoryError::SecretRejected);
        }
        if serde_json::to_vec(self)
            .map_err(MemoryError::backend)?
            .len()
            > 16 * 1024
            || self.evidence.len() > 16
            || self.producing_traces.len() > 32
            || self.imported_from.len() > 32
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
        for evidence in self.evidence.iter().chain(match &self.kind {
            MemoryKind::LessonProposal { behavior_test, .. } => Some(behavior_test),
            _ => None,
        }) {
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

    /// Backend duplicate key includes scope and imported ownership.
    pub fn identity(&self, content: &str) -> String {
        if let Some(source) = self.imported_from.first() {
            return format!(
                "import:{}:{}",
                serde_json::to_string(source).expect("serializable key"),
                normalize_identity(content)
            );
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

    pub fn visible_in(&self, repository: Option<&str>) -> bool {
        match &self.scope {
            MemoryScope::Repository { identity } => Some(identity.as_str()) == repository,
            MemoryScope::Global | MemoryScope::LegacyUnscoped => true,
        }
    }
}

pub(crate) fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
