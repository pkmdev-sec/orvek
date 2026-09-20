use crate::{
    Digest,
    contract::{BaselinePolicy, ControlRequirement, FlakePolicy},
    state::*,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Rejection {
    pub obligation: Option<String>,
    pub reason: String,
}

pub fn evaluate(state: &TaskState, at_ms: u64) -> Result<Certificate, Vec<Rejection>> {
    let contract = state.accepted_contract().map_err(|error| {
        vec![Rejection {
            obligation: None,
            reason: error.to_string(),
        }]
    })?;
    let mut reasons = Vec::new();
    let mut reject = |obligation: Option<String>, reason: &str| {
        reasons.push(Rejection {
            obligation,
            reason: reason.to_owned(),
        })
    };
    if contract.validate().is_err() {
        reject(None, "invalid contract");
    }
    if state.outcome.is_some() {
        reject(None, "task already has a terminal outcome");
    }
    if state.cancellation_requested {
        reject(None, "cancellation has been requested");
    }
    if state.amendment_pending {
        reject(
            None,
            "user follow-up has not been admitted into the task contract",
        );
    }
    if !contract.open_questions.is_empty() {
        reject(None, "required product decisions remain open");
    }
    for finding in state
        .findings
        .values()
        .filter(|f| f.blocking && !f.resolved)
    {
        reject(Some(finding.id.clone()), &finding.description);
    }
    if state.jobs.values().any(|job| job.status.unresolved()) {
        reject(None, "jobs remain running or have unknown outcomes");
    }
    if state
        .jobs
        .values()
        .any(|job| job.status == JobStatus::Fenced && job.fence_receipt.is_none())
    {
        reject(None, "a fenced job has no termination receipt");
    }
    if state.model_reservations.iter().any(|operation| {
        !state.model_receipts.get(operation).is_some_and(|receipt| {
            receipt.tokens.is_some() && receipt.status != ModelCallStatus::Unknown
        })
    }) {
        reject(None, "provider attempts have unknown outcomes or billing");
    }
    if state.effects.values().any(|effect| {
        matches!(
            effect.status,
            EffectStatus::Intended | EffectStatus::Unknown
        )
    }) {
        reject(None, "external effects require reconciliation");
    }
    if state.usage.tokens > contract.limits.tokens
        || state.usage.model_calls > contract.limits.model_calls
        || at_ms.saturating_sub(state.started_ms) >= contract.limits.elapsed_ms
    {
        reject(None, "execution budget exceeded");
    }
    let Some(candidate) = &state.candidate else {
        reject(None, "no frozen candidate");
        return Err(reasons);
    };
    if !candidate.frozen {
        reject(None, "candidate is still writable");
    }
    let contract_digest = contract.digest().map_err(|error| {
        vec![Rejection {
            obligation: None,
            reason: error.to_string(),
        }]
    })?;
    let mut accepted = BTreeMap::new();
    for requirement in &contract.requirements {
        for check_id in &requirement.checks {
            if accepted.contains_key(check_id) {
                continue;
            }
            let Some(check) = contract.checks.get(check_id) else {
                reject(
                    Some(requirement.id.clone()),
                    "acceptance definition is missing",
                );
                continue;
            };
            let definition_digest = Digest::of_value(check).map_err(|error| {
                vec![Rejection {
                    obligation: None,
                    reason: error.to_string(),
                }]
            })?;
            let identity = EvidenceIdentity {
                contract: contract_digest,
                source: candidate.source,
                environment: candidate.environment,
                artifact: candidate.artifact,
                check_definition: definition_digest,
                verifier: check.verifier,
            };
            let attempts = state
                .evidence
                .iter()
                .filter(|e| e.check == *check_id && e.identity == identity)
                .collect::<Vec<_>>();
            let Some(last) = attempts.last().filter(|e| e.generation == state.generation) else {
                reject(
                    Some(requirement.id.clone()),
                    &format!("check {check_id} has no current evidence"),
                );
                continue;
            };
            let result = &last.observation;
            if !result.limitations.is_empty() || !result.unreconciled_jobs.is_empty() {
                reject(
                    Some(requirement.id.clone()),
                    &format!("check {check_id} has unresolved evidence limitations"),
                );
                continue;
            }
            let passed = result.status == CheckStatus::Passed
                && result.exit_code == Some(0)
                && result.signal.is_none();
            let baseline = matches!(check.baseline, BaselinePolicy::NoNewFailure { .. })
                && result.status == CheckStatus::Failed
                && result.baseline_unchanged
                && result.signal.is_none();
            if !passed && !baseline {
                reject(
                    Some(requirement.id.clone()),
                    &format!("check {check_id} did not satisfy its acceptance policy"),
                );
                continue;
            }
            if result.assertions < check.minimum_assertions
                || result.discovered == Some(0)
                || result.discovered.is_some_and(|n| n <= result.skipped)
            {
                reject(
                    Some(requirement.id.clone()),
                    &format!("check {check_id} did not exercise its required assertions"),
                );
                continue;
            }
            if check.control != ControlRequirement::None
                && !result.control.as_ref().is_some_and(|control| {
                    let expected_source = match check.control {
                        ControlRequirement::BaselineFailure => {
                            state.baseline.as_ref().map(|base| base.source)
                        }
                        ControlRequirement::NegativeControl => check.control_source,
                        ControlRequirement::None => None,
                    };
                    control.rejected
                        && control.intended_reason
                        && control.kind == check.control
                        && Some(control.source) == expected_source
                })
            {
                reject(
                    Some(requirement.id.clone()),
                    &format!("check {check_id} has no valid negative control"),
                );
                continue;
            }
            let failed_before = attempts
                .iter()
                .take(attempts.len().saturating_sub(1))
                .any(|e| e.observation.status == CheckStatus::Failed);
            if (check.flake == FlakePolicy::RejectAnyFailure && failed_before)
                || matches!(check.flake, FlakePolicy::Retry { max_attempts } if attempts.len() > max_attempts as usize)
            {
                reject(
                    Some(requirement.id.clone()),
                    &format!("check {check_id} violates its declared retry policy"),
                );
                continue;
            }
            accepted.insert(check_id.clone(), last.job_id);
        }
    }
    let Some(delivery) = &state.delivery else {
        reject(None, "requested delivery has not been verified");
        return Err(reasons);
    };
    if delivery.kind != contract.delivery
        || delivery.source != candidate.source
        || delivery.artifact != candidate.artifact
    {
        reject(
            None,
            "delivery does not match the candidate and requested stage",
        );
    }
    if !reasons.is_empty() {
        return Err(reasons);
    }
    Ok(Certificate {
        task: state.id,
        contract: contract_digest,
        source: candidate.source,
        artifact: candidate.artifact,
        environment: candidate.environment,
        generation: state.generation,
        evaluated_revision: state.revision,
        evidence: accepted,
        delivery_receipt: delivery.receipt,
    })
}
