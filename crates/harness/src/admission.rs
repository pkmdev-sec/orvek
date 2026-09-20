//! Compile model proposals against a protected repository profile and user limits.

use crate::{
    Digest, StoreError, artifacts::ArtifactStore, contract::*, state::TaskState,
    verification::CheckProgram, workspace::Snapshot,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedCheck {
    pub purpose: String,
    pub kind: CheckKind,
    pub program: CheckProgram,
    pub baseline_failure: bool,
    /// An omission is a disclosed judgment, never proof that a control was run.
    pub control_omission: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    pub outcome: String,
    pub scope: String,
    pub requirements: Vec<Requirement>,
    pub checks: BTreeMap<String, ProposedCheck>,
    pub protected_behavior: Vec<String>,
    pub assumptions: Vec<String>,
    pub open_questions: Vec<String>,
}

/// Supplied by the application from operator-controlled configuration. The
/// implementation model cannot alter this profile through its proposal.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryProfile {
    pub version: u32,
    pub name: String,
    pub checks: BTreeMap<String, ProposedCheck>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestPolicy {
    pub version: u32,
    pub profile: RepositoryProfile,
    pub delivery: DeliveryKind,
}

impl RequestPolicy {
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.version != 1 || !matches!(self.delivery, DeliveryKind::Source | DeliveryKind::Patch)
        {
            return Err(StoreError::Invalid(
                "unsupported intake policy or delivery adapter",
            ));
        }
        self.profile.validate()
    }
}

impl RepositoryProfile {
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.version != 1 || self.name.trim().is_empty() || self.checks.len() > 128 {
            return Err(StoreError::Invalid(
                "invalid repository profile identity or size",
            ));
        }
        for check in self.checks.values() {
            validate_check(check)?;
        }
        Ok(())
    }
}

pub struct CompiledContract {
    pub contract: Contract,
    pub receipt: Digest,
}

pub fn compile(
    task: &TaskState,
    proposal: Proposal,
    baseline: &Snapshot,
    artifacts: &ArtifactStore,
) -> Result<CompiledContract, StoreError> {
    let policy: RequestPolicy = serde_json::from_slice(
        &artifacts.read(
            task.intake
                .ok_or(StoreError::Invalid("missing durable intake policy"))?,
        )?,
    )?;
    policy.validate()?;
    let profile = &policy.profile;
    let delivery = policy.delivery;
    if task.contract.is_some() && !task.amendment_pending {
        return Err(StoreError::Invalid(
            "accepted contracts require the amendment path",
        ));
    }
    if !matches!(delivery, DeliveryKind::Source | DeliveryKind::Patch) {
        return Err(StoreError::Invalid("unsupported delivery adapter"));
    }
    profile.validate()?;
    let mut user_basis = task.request.clone();
    for (_, input) in &task.directives {
        user_basis.push('\n');
        user_basis.push_str(&crate::input::load(*input, artifacts)?.text);
    }
    if proposal.requirements.len() > 128 || proposal.checks.len() > 128 {
        return Err(StoreError::Invalid(
            "contract proposal exceeds requirement/check bounds",
        ));
    }
    for requirement in &proposal.requirements {
        match &requirement.origin {
            Origin::User(quote) if quote.trim().is_empty() || !user_basis.contains(quote) => {
                return Err(StoreError::Invalid(
                    "user-origin requirement must quote the original request",
                ));
            }
            Origin::Repository(path) if !baseline.entries.contains_key(path) => {
                return Err(StoreError::Invalid(
                    "repository-origin requirement must name an inspected baseline path",
                ));
            }
            _ => {}
        }
    }
    if !proposal
        .checks
        .values()
        .any(|check| !matches!(check.kind, CheckKind::Build | CheckKind::Static))
    {
        return Err(StoreError::Invalid(
            "a coding task requires behavior-specific acceptance beyond build/static checks",
        ));
    }
    let receipt_bytes = serde_json::to_vec(
        &serde_json::json!({"version":1,"request":task.request,"limits":task.initial_limits,"delivery":delivery,"profile":profile,"proposal":proposal,"baseline":Digest::of_value(baseline)?}),
    )?;
    let programs = proposal
        .checks
        .values()
        .chain(profile.checks.values())
        .map(|check| serde_json::to_vec(&check.program))
        .collect::<Result<Vec<_>, _>>()?;
    let mut requirements = proposal.requirements;
    let mut assumptions = proposal.assumptions;
    let mut checks = BTreeMap::new();
    for (id, check) in proposal.checks {
        if id.starts_with("profile-") {
            return Err(StoreError::Invalid("profile check IDs are reserved"));
        }
        let definition = definition(&id, &check, &mut assumptions)?;
        checks.insert(id, definition);
    }
    let profile_digest = Digest::of_value(profile)?;
    for (id, check) in &profile.checks {
        let id = format!("profile-{id}");
        checks.insert(id.clone(), definition(&id, check, &mut assumptions)?);
        requirements.push(Requirement {
            id: id.clone(),
            behavior: check.purpose.clone(),
            origin: Origin::Repository(format!(
                "protected profile {} @ {profile_digest}",
                profile.name
            )),
            checks: vec![id],
            depends_on: vec![],
        });
    }
    let mut contract = Contract {
        request: task.request.clone(),
        outcome: proposal.outcome,
        scope: proposal.scope,
        requirements,
        checks,
        protected_behavior: proposal.protected_behavior,
        assumptions,
        open_questions: proposal.open_questions,
        delivery,
        limits: task.initial_limits,
    };
    if let Some(old) = &task.contract {
        for requirement in &old.requirements {
            if let Some(proposed) = contract
                .requirements
                .iter()
                .find(|proposed| proposed.id == requirement.id)
            {
                if proposed != requirement {
                    return Err(StoreError::Invalid(
                        "follow-up cannot weaken or rewrite an accepted requirement",
                    ));
                }
            } else {
                contract.requirements.push(requirement.clone());
            }
        }
        for (id, check) in &old.checks {
            if let Some(proposed) = contract.checks.get(id) {
                if proposed != check {
                    return Err(StoreError::Invalid(
                        "follow-up cannot change a protected acceptance check",
                    ));
                }
            } else {
                contract.checks.insert(id.clone(), check.clone());
            }
        }
        for behavior in &old.protected_behavior {
            if !contract.protected_behavior.contains(behavior) {
                contract.protected_behavior.push(behavior.clone());
            }
        }
        for assumption in &old.assumptions {
            if !contract.assumptions.contains(assumption) {
                contract.assumptions.push(assumption.clone());
            }
        }
        if contract.outcome != old.outcome {
            contract.outcome = format!("{}\nFollow-up: {}", old.outcome, contract.outcome);
        }
        if contract.scope != old.scope {
            contract.scope = format!("{}\nFollow-up: {}", old.scope, contract.scope);
        }
        preserves_obligations(old, &contract)?;
    }
    contract.validate()?;
    for program in programs {
        artifacts.put(&program)?;
    }
    let receipt = artifacts.put(&receipt_bytes)?;
    Ok(CompiledContract { contract, receipt })
}

pub(crate) fn preserves_obligations(old: &Contract, new: &Contract) -> Result<(), StoreError> {
    if new.request != old.request
        || new.limits != old.limits
        || new.delivery != old.delivery
        || !new.outcome.starts_with(&old.outcome)
        || !new.scope.starts_with(&old.scope)
        || old
            .requirements
            .iter()
            .any(|requirement| !new.requirements.contains(requirement))
        || old
            .checks
            .iter()
            .any(|(id, check)| new.checks.get(id) != Some(check))
        || old
            .protected_behavior
            .iter()
            .any(|behavior| !new.protected_behavior.contains(behavior))
        || old
            .assumptions
            .iter()
            .any(|assumption| !new.assumptions.contains(assumption))
    {
        return Err(StoreError::Invalid(
            "follow-up must preserve all accepted obligations and limits",
        ));
    }
    Ok(())
}

fn validate_check(check: &ProposedCheck) -> Result<(), StoreError> {
    check.program.validate()?;
    if check.purpose.trim().is_empty() {
        return Err(StoreError::Invalid("acceptance check requires a purpose"));
    }
    if check.baseline_failure {
        if check.program.control_failure.is_none() || check.control_omission.is_some() {
            return Err(StoreError::Invalid(
                "baseline control requires its intended failure observation",
            ));
        }
    } else if !check
        .control_omission
        .as_ref()
        .is_some_and(|reason| !reason.trim().is_empty())
    {
        return Err(StoreError::Invalid(
            "omitting a negative control requires a visible reason",
        ));
    }
    Ok(())
}

fn definition(
    id: &str,
    check: &ProposedCheck,
    assumptions: &mut Vec<String>,
) -> Result<CheckDefinition, StoreError> {
    validate_check(check)?;
    if let Some(reason) = &check.control_omission {
        assumptions.push(format!("Check {id} omits a negative control: {reason}"));
    }
    Ok(CheckDefinition {
        purpose: check.purpose.clone(),
        kind: check.kind,
        verifier: Digest::of_value(&check.program)?,
        command: vec!["tact-protected-verifier".into(), id.into()],
        timeout_ms: 60_000,
        minimum_assertions: 1,
        control: if check.baseline_failure {
            ControlRequirement::BaselineFailure
        } else {
            ControlRequirement::None
        },
        control_source: None,
        baseline: BaselinePolicy::MustPass,
        flake: FlakePolicy::RejectAnyFailure,
    })
}
