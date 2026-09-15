use crate::{
    Digest,
    contract::{Contract, DeliveryKind, Limits},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(pub Uuid);

impl TaskId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Understand,
    Baseline,
    Implement,
    Challenge,
    Verify,
    Deliver,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Conversation,
    Task,
    Auxiliary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Complete,
    DeliveredWithExceptions,
    Blocked,
    BudgetExhausted,
    Cancelled,
    Failed,
    /// Ordinary native-host work ended from final prose or an explicit finish.
    /// The user's live workspace keeps every change; no certificate exists.
    FinishedUnverified,
}

impl Outcome {
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Complete | Self::FinishedUnverified => 0,
            Self::Failed => 1,
            Self::Blocked => 20,
            Self::BudgetExhausted => 21,
            Self::DeliveredWithExceptions => 22,
            Self::Cancelled => 130,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub source: Digest,
    pub environment: Digest,
    pub artifact: Digest,
    pub frozen: bool,
    pub provenance: Option<Digest>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
    Inconclusive,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EvidenceIdentity {
    pub contract: Digest,
    pub source: Digest,
    pub environment: Digest,
    pub artifact: Digest,
    pub check_definition: Digest,
    pub verifier: Digest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ControlObservation {
    pub kind: crate::contract::ControlRequirement,
    pub source: Digest,
    pub rejected: bool,
    pub intended_reason: bool,
    pub report: Digest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub status: CheckStatus,
    pub report: Digest,
    pub assertions: u64,
    pub discovered: Option<u64>,
    pub skipped: u64,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub control: Option<ControlObservation>,
    pub baseline_unchanged: bool,
    pub limitations: Vec<String>,
    #[serde(default)]
    pub unreconciled_jobs: Vec<Uuid>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub job_id: Uuid,
    pub generation: u64,
    pub check: String,
    pub identity: EvidenceIdentity,
    pub started_ms: u64,
    pub finished_ms: u64,
    pub observation: Observation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
    Fenced,
}

impl JobStatus {
    pub const fn unresolved(self) -> bool {
        matches!(self, Self::Running | Self::Unknown)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub generation: u64,
    pub status: JobStatus,
    pub mutates_candidate: bool,
    pub check: Option<String>,
    pub identity: Option<EvidenceIdentity>,
    pub started_ms: u64,
    pub deadline_ms: u64,
    pub invocation: Option<JobInvocation>,
    pub fence_receipt: Option<Digest>,
    pub execution_receipt: Option<Digest>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JobInvocation {
    pub session: crate::session::SessionId,
    pub request: Uuid,
    pub call_id: Option<String>,
    pub capability: String,
    pub input: Digest,
    pub environment: Digest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectStatus {
    Intended,
    Unknown,
    Succeeded,
    Failed,
    Reconciled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub operation_id: Uuid,
    pub description: String,
    pub status: EffectStatus,
    pub idempotent: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub description: String,
    pub blocking: bool,
    pub resolved: bool,
    pub resolution: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    pub kind: DeliveryKind,
    pub source: Digest,
    pub artifact: Digest,
    pub receipt: Digest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Certificate {
    pub task: TaskId,
    pub contract: Digest,
    pub source: Digest,
    pub artifact: Digest,
    pub environment: Digest,
    pub generation: u64,
    pub evaluated_revision: u64,
    pub evidence: BTreeMap<String, Uuid>,
    pub delivery_receipt: Digest,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub model_calls: u32,
    pub tokens: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCallStatus {
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelCallReceipt {
    pub status: ModelCallStatus,
    /// None means that the provider might have charged an unknown amount.
    pub tokens: Option<u64>,
    pub report: Digest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskState {
    pub workspace_origin: Option<Digest>,
    pub workspace_override: Option<Digest>,
    pub origin: Option<Digest>,
    pub id: TaskId,
    pub revision: u64,
    pub generation: u64,
    pub scope_revision: u64,
    pub admitted_scope_revision: u64,
    pub directive_scopes: BTreeMap<Uuid, u64>,
    pub directives: Vec<(Uuid, Digest)>,
    pub amendment_pending: bool,
    pub started_ms: u64,
    pub request: String,
    pub initial_limits: Limits,
    pub intake: Option<Digest>,
    pub input: Option<Digest>,
    pub contract: Option<Contract>,
    pub contract_admission: Option<Digest>,
    pub contract_history: Vec<(Digest, String)>,
    pub phase: Phase,
    pub outcome: Option<Outcome>,
    pub cancellation_requested: bool,
    pub disposition_reason: Option<String>,
    pub candidate: Option<Candidate>,
    pub baseline: Option<Candidate>,
    pub evidence: Vec<Evidence>,
    pub jobs: BTreeMap<Uuid, Job>,
    pub effects: BTreeMap<Uuid, Effect>,
    pub findings: BTreeMap<String, Finding>,
    pub usage: Usage,
    pub model_reservations: BTreeSet<Uuid>,
    pub model_receipts: BTreeMap<Uuid, ModelCallReceipt>,
    pub usage_receipts: BTreeMap<Uuid, Usage>,
    pub delivery: Option<Delivery>,
    pub certificates: Vec<Certificate>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum TaskEvent {
    ManualStarted {
        job: Job,
    },
    ManualWorkspace {
        candidate: Candidate,
        origin: Digest,
    },
    WorkspaceRestored {
        source: Digest,
    },
    OriginCaptured {
        source: Digest,
    },
    DirectiveReplaced {
        request: Uuid,
        input: Digest,
    },
    DirectiveReceived {
        request: Uuid,
        input: Digest,
    },
    AdditiveContractAccepted {
        contract: Contract,
        receipt: Digest,
        scope_revision: u64,
    },
    Requested {
        request: String,
        limits: Limits,
        intake: Digest,
        input: Option<Digest>,
        at_ms: u64,
    },
    ContractAdmitted {
        contract: Contract,
        basis: String,
        receipt: Digest,
    },
    Created {
        contract: Contract,
        at_ms: u64,
    },
    ContractAmended {
        contract: Contract,
        reason: String,
    },
    CandidateSelected(Candidate),
    WorkspaceChanged {
        reason: String,
    },
    EvidenceInvalidated {
        reason: String,
    },
    CancellationRequested,
    BaselineEstablished(Candidate),
    PhaseChanged(Phase),
    JobStarted(Job),
    JobSettled {
        id: Uuid,
        status: JobStatus,
        receipt: Option<Digest>,
    },
    JobFenced {
        id: Uuid,
        receipt: Digest,
    },
    Observed(Evidence),
    EffectRecorded(Effect),
    FindingRecorded(Finding),
    ModelCallReserved {
        operation: Uuid,
    },
    ModelCallRecorded {
        operation: Uuid,
        receipt: ModelCallReceipt,
    },
    UsageCharged {
        operation: Uuid,
        usage: Usage,
    },
    Delivered(Delivery),
    Completed(Certificate),
    Stopped {
        outcome: Outcome,
        reason: String,
    },
    Reopened {
        reason: String,
    },
    Interrupted {
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
#[error("task has no accepted executable contract")]
pub struct ContractPending;

impl TaskState {
    pub(crate) fn created(id: TaskId, contract: Contract, started_ms: u64) -> Self {
        let mut state = Self::requested(
            id,
            contract.request.clone(),
            contract.limits,
            started_ms,
            None,
            None,
        );
        state.contract = Some(contract);
        state
    }

    pub(crate) fn requested(
        id: TaskId,
        request: String,
        limits: Limits,
        started_ms: u64,
        intake: Option<Digest>,
        input: Option<Digest>,
    ) -> Self {
        Self {
            origin: None,
            workspace_origin: None,
            workspace_override: None,
            id,
            revision: 1,
            generation: 1,
            scope_revision: 0,
            admitted_scope_revision: 0,
            directive_scopes: BTreeMap::new(),
            directives: Vec::new(),
            amendment_pending: false,
            started_ms,
            request,
            initial_limits: limits,
            intake,
            input,
            contract: None,
            contract_admission: None,
            contract_history: Vec::new(),
            phase: Phase::Understand,
            outcome: None,
            cancellation_requested: false,
            disposition_reason: None,
            candidate: None,
            baseline: None,
            evidence: Vec::new(),
            jobs: BTreeMap::new(),
            effects: BTreeMap::new(),
            findings: BTreeMap::new(),
            usage: Usage::default(),
            model_reservations: BTreeSet::new(),
            model_receipts: BTreeMap::new(),
            usage_receipts: BTreeMap::new(),
            delivery: None,
            certificates: Vec::new(),
        }
    }

    pub fn accepted_contract(&self) -> Result<&Contract, ContractPending> {
        self.contract.as_ref().ok_or(ContractPending)
    }

    pub fn limits(&self) -> Limits {
        self.contract
            .as_ref()
            .map_or(self.initial_limits, |contract| contract.limits)
    }

    pub(crate) fn apply(&mut self, event: &TaskEvent) -> Result<(), serde_json::Error> {
        match event {
            TaskEvent::ManualStarted { job } => {
                self.invalidate();
                self.scope_revision += 1;
                self.outcome = None;
                self.cancellation_requested = false;
                self.jobs.insert(job.id, job.clone());
            }
            TaskEvent::ManualWorkspace { candidate, origin } => {
                self.workspace_override = Some(candidate.source);
                self.workspace_origin = Some(*origin);
                self.candidate = Some(candidate.clone());
            }
            TaskEvent::WorkspaceRestored { .. } => {
                self.workspace_override = None;
            }
            TaskEvent::OriginCaptured { source } => {
                self.origin = Some(*source);
                self.workspace_origin = Some(*source);
            }
            TaskEvent::DirectiveReplaced { request, input } => {
                if let Some(directive) = self.directives.iter_mut().find(|(id, _)| id == request) {
                    directive.1 = *input;
                }
                self.scope_revision += 1;
                self.directive_scopes.insert(*request, self.scope_revision);
                self.amendment_pending = true;
                self.invalidate();
                self.outcome = None;
                self.disposition_reason = None;
            }
            TaskEvent::DirectiveReceived { request, input } => {
                self.directives.push((*request, *input));
                self.scope_revision += 1;
                self.directive_scopes.insert(*request, self.scope_revision);
                self.amendment_pending = true;
                self.invalidate();
                self.outcome = None;
                self.disposition_reason = None;
            }
            TaskEvent::AdditiveContractAccepted {
                contract, receipt, ..
            } => {
                if let Some(previous) = &self.contract {
                    self.contract_history.push((
                        previous.digest()?,
                        format!("user follow-up admission {receipt}"),
                    ));
                }
                self.contract = Some(contract.clone());
                self.contract_admission = Some(*receipt);
                self.amendment_pending = false;
                self.admitted_scope_revision = self.scope_revision;
                self.invalidate();
            }
            TaskEvent::Requested { .. } | TaskEvent::Created { .. } => {
                unreachable!("creation is handled by the journal loader")
            }
            TaskEvent::ContractAdmitted {
                contract, receipt, ..
            } => {
                self.contract = Some(contract.clone());
                self.contract_admission = Some(*receipt);
                self.invalidate();
            }
            TaskEvent::ContractAmended { contract, reason } => {
                if let Some(previous) = &self.contract {
                    self.contract_history
                        .push((previous.digest()?, reason.clone()));
                }
                self.contract = Some(contract.clone());
                self.invalidate();
            }
            TaskEvent::CandidateSelected(candidate) => {
                self.invalidate();
                self.candidate = Some(candidate.clone());
            }
            TaskEvent::WorkspaceChanged { .. } => {
                self.invalidate();
                self.candidate = None;
            }
            TaskEvent::EvidenceInvalidated { reason } => {
                self.invalidate();
                self.outcome = Some(Outcome::Blocked);
                self.disposition_reason = Some(reason.clone());
            }
            TaskEvent::CancellationRequested => self.cancellation_requested = true,
            TaskEvent::BaselineEstablished(candidate) => self.baseline = Some(candidate.clone()),
            TaskEvent::PhaseChanged(phase) => self.phase = *phase,
            TaskEvent::JobStarted(job) => {
                self.jobs.insert(job.id, job.clone());
            }
            TaskEvent::JobSettled {
                id,
                status,
                receipt,
            } => {
                if let Some(job) = self.jobs.get_mut(id) {
                    job.status = *status;
                    job.execution_receipt = *receipt;
                }
            }
            TaskEvent::JobFenced { id, receipt } => {
                if let Some(job) = self.jobs.get_mut(id) {
                    job.status = JobStatus::Fenced;
                    job.fence_receipt = Some(*receipt);
                }
            }
            TaskEvent::Observed(evidence) => {
                if let Some(job) = self.jobs.get_mut(&evidence.job_id) {
                    job.status = if !evidence.observation.unreconciled_jobs.is_empty() {
                        JobStatus::Unknown
                    } else if evidence.observation.status == CheckStatus::Passed {
                        JobStatus::Succeeded
                    } else {
                        JobStatus::Failed
                    };
                }
                self.evidence.push(evidence.clone());
            }
            TaskEvent::EffectRecorded(effect) => {
                self.effects.insert(effect.operation_id, effect.clone());
            }
            TaskEvent::FindingRecorded(finding) => {
                self.findings.insert(finding.id.clone(), finding.clone());
            }
            TaskEvent::ModelCallReserved { operation } => {
                self.model_reservations.insert(*operation);
                self.usage.model_calls = self.usage.model_calls.saturating_add(1);
            }
            TaskEvent::ModelCallRecorded { operation, receipt } => {
                let previous = self
                    .model_receipts
                    .get(operation)
                    .and_then(|r| r.tokens)
                    .unwrap_or(0);
                self.usage.tokens = self
                    .usage
                    .tokens
                    .saturating_add(receipt.tokens.unwrap_or(0).saturating_sub(previous));
                self.model_receipts.insert(*operation, receipt.clone());
                if self.outcome == Some(Outcome::Complete)
                    && self.usage.tokens > self.limits().tokens
                {
                    self.invalidate();
                    self.outcome = Some(Outcome::BudgetExhausted);
                    self.disposition_reason =
                        Some("late provider accounting exceeded the task budget".into());
                }
            }
            TaskEvent::UsageCharged { operation, usage } => {
                self.usage_receipts.insert(*operation, *usage);
                self.usage.model_calls = self.usage.model_calls.saturating_add(usage.model_calls);
                self.usage.tokens = self.usage.tokens.saturating_add(usage.tokens);
                if self.outcome == Some(Outcome::Complete)
                    && (self.usage.model_calls > self.limits().model_calls
                        || self.usage.tokens > self.limits().tokens)
                {
                    self.invalidate();
                    self.outcome = Some(Outcome::BudgetExhausted);
                    self.disposition_reason = Some("late accounting exceeded the task budget; the earlier certificate remains historical".into());
                }
            }
            TaskEvent::Delivered(delivery) => self.delivery = Some(delivery.clone()),
            TaskEvent::Completed(certificate) => {
                self.certificates.push(certificate.clone());
                self.outcome = Some(Outcome::Complete);
            }
            TaskEvent::Stopped { outcome, reason } => {
                self.outcome = Some(*outcome);
                self.disposition_reason = Some(reason.clone());
            }
            TaskEvent::Reopened { .. } => {
                self.invalidate();
                self.outcome = None;
                self.disposition_reason = None;
                self.cancellation_requested = false;
            }
            TaskEvent::Interrupted { reason } => {
                self.invalidate();
                self.outcome = Some(if self.cancellation_requested {
                    Outcome::Cancelled
                } else {
                    Outcome::Blocked
                });
                self.disposition_reason = Some(reason.clone());
                for job in self
                    .jobs
                    .values_mut()
                    .filter(|job| job.status == JobStatus::Running)
                {
                    job.status = JobStatus::Unknown;
                }
                for effect in self
                    .effects
                    .values_mut()
                    .filter(|effect| effect.status == EffectStatus::Intended)
                {
                    effect.status = EffectStatus::Unknown;
                }
            }
        }
        self.revision += 1;
        Ok(())
    }

    fn invalidate(&mut self) {
        self.generation += 1;
        self.delivery = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Outcomes cross process, store and UI boundaries as serde tags; a
    /// renamed or reordered variant would silently strand persisted tasks.
    #[test]
    fn outcomes_roundtrip_through_their_serde_tags() {
        for outcome in [
            Outcome::Complete,
            Outcome::DeliveredWithExceptions,
            Outcome::Blocked,
            Outcome::BudgetExhausted,
            Outcome::Cancelled,
            Outcome::Failed,
            Outcome::FinishedUnverified,
        ] {
            let tag = serde_json::to_value(outcome).unwrap();
            assert_eq!(
                serde_json::from_value::<Outcome>(tag.clone()).unwrap(),
                outcome
            );
            assert_eq!(tag, serde_json::to_value(outcome).unwrap());
        }
        assert_eq!(
            serde_json::to_value(Outcome::FinishedUnverified).unwrap(),
            serde_json::json!("finished_unverified")
        );
    }
}
