use crate::Digest;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub request: String,
    pub outcome: String,
    pub scope: String,
    pub requirements: Vec<Requirement>,
    pub checks: BTreeMap<String, CheckDefinition>,
    pub protected_behavior: Vec<String>,
    pub assumptions: Vec<String>,
    pub open_questions: Vec<String>,
    pub delivery: DeliveryKind,
    pub limits: Limits,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub id: String,
    pub behavior: String,
    pub origin: Origin,
    pub checks: Vec<String>,
    pub depends_on: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "basis", rename_all = "snake_case")]
pub enum Origin {
    User(String),
    Repository(String),
    Inferred(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckDefinition {
    pub purpose: String,
    pub kind: CheckKind,
    pub verifier: Digest,
    pub command: Vec<String>,
    pub timeout_ms: u64,
    pub minimum_assertions: u64,
    pub control: ControlRequirement,
    pub control_source: Option<Digest>,
    pub baseline: BaselinePolicy,
    pub flake: FlakePolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    Behavior,
    Build,
    Static,
    Integration,
    Interface,
    Migration,
    Performance,
    Review,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlRequirement {
    None,
    BaselineFailure,
    NegativeControl,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BaselinePolicy {
    MustPass,
    NoNewFailure {
        baseline_report: Digest,
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FlakePolicy {
    RejectAnyFailure,
    Retry { max_attempts: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    Patch,
    Source,
    Package,
    Deployment,
    Report,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub model_calls: u32,
    pub tokens: u64,
    pub elapsed_ms: u64,
    pub concurrent_jobs: u32,
    pub artifact_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            model_calls: 100,
            tokens: 1_000_000,
            elapsed_ms: 7_200_000,
            concurrent_jobs: 4,
            artifact_bytes: 256 * 1024 * 1024,
        }
    }
}

impl Limits {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.model_calls == 0
            || self.tokens == 0
            || self.elapsed_ms == 0
            || self.concurrent_jobs == 0
            || self.artifact_bytes == 0
        {
            return Err(ContractError::Limits);
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ContractError {
    #[error("contract has no {0}")]
    Empty(&'static str),
    #[error("invalid or duplicate requirement ID: {0}")]
    RequirementId(String),
    #[error("requirement {0} has no acceptance checks")]
    NoChecks(String),
    #[error("requirement {requirement} refers to missing {kind}: {id}")]
    Missing {
        requirement: String,
        kind: &'static str,
        id: String,
    },
    #[error("requirement dependencies contain a cycle")]
    Cycle,
    #[error("invalid check {id}: {reason}")]
    Check { id: String, reason: &'static str },
    #[error("all execution limits must be positive")]
    Limits,
}

impl Contract {
    pub fn validate(&self) -> Result<(), ContractError> {
        for (name, text) in [
            ("request", &self.request),
            ("outcome", &self.outcome),
            ("scope", &self.scope),
        ] {
            if text.trim().is_empty() {
                return Err(ContractError::Empty(name));
            }
        }
        if self.requirements.is_empty() {
            return Err(ContractError::Empty("requirements"));
        }
        let mut ids = BTreeSet::new();
        for requirement in &self.requirements {
            if !valid_id(&requirement.id) || !ids.insert(requirement.id.as_str()) {
                return Err(ContractError::RequirementId(requirement.id.clone()));
            }
            if requirement.behavior.trim().is_empty() {
                return Err(ContractError::Empty("observable behavior"));
            }
            let (Origin::User(basis) | Origin::Repository(basis) | Origin::Inferred(basis)) =
                &requirement.origin;
            if basis.trim().is_empty() {
                return Err(ContractError::Empty("requirement provenance"));
            }
            if requirement.checks.is_empty() {
                return Err(ContractError::NoChecks(requirement.id.clone()));
            }
        }
        for requirement in &self.requirements {
            for check in &requirement.checks {
                if !self.checks.contains_key(check) {
                    return Err(ContractError::Missing {
                        requirement: requirement.id.clone(),
                        kind: "check",
                        id: check.clone(),
                    });
                }
            }
            for dependency in &requirement.depends_on {
                if !ids.contains(dependency.as_str()) {
                    return Err(ContractError::Missing {
                        requirement: requirement.id.clone(),
                        kind: "requirement",
                        id: dependency.clone(),
                    });
                }
            }
        }
        let mut visited = BTreeSet::new();
        loop {
            let before = visited.len();
            for requirement in &self.requirements {
                if requirement
                    .depends_on
                    .iter()
                    .all(|id| visited.contains(id.as_str()))
                {
                    visited.insert(requirement.id.as_str());
                }
            }
            if visited.len() == ids.len() {
                break;
            }
            if visited.len() == before {
                return Err(ContractError::Cycle);
            }
        }
        for (id, check) in &self.checks {
            let reason = if !self.requirements.iter().any(|r| r.checks.contains(id)) {
                Some("check must belong to a required outcome")
            } else if !valid_id(id) || check.purpose.trim().is_empty() {
                Some("an ID and purpose are required")
            } else if check.command.is_empty()
                || check.command[0].is_empty()
                || check.timeout_ms == 0
            {
                Some("a command and positive timeout are required")
            } else if !matches!(check.kind, CheckKind::Build | CheckKind::Static)
                && check.minimum_assertions == 0
            {
                Some("behavioral checks require at least one assertion")
            } else if matches!(check.flake, FlakePolicy::Retry { max_attempts: 0 }) {
                Some("retry bound must be positive")
            } else if check.control == ControlRequirement::NegativeControl
                && check.control_source.is_none()
            {
                Some("negative control requires an immutable source identity")
            } else if let BaselinePolicy::NoNewFailure { reason, .. } = &check.baseline {
                reason
                    .trim()
                    .is_empty()
                    .then_some("baseline allowance requires a reason")
            } else {
                None
            };
            if let Some(reason) = reason {
                return Err(ContractError::Check {
                    id: id.clone(),
                    reason,
                });
            }
        }
        self.limits.validate()
    }

    pub fn digest(&self) -> Result<Digest, serde_json::Error> {
        Digest::of_value(self)
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
