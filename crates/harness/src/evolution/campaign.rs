use super::{
    CampaignId, CohortId, CompositePlan, CompositionFailure, CompositionFallback, PolicyIdentity,
};
use crate::Digest;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};
use thiserror::Error;

const MAX_NOTE_BYTES: usize = 512;
const MAX_ROUNDS: usize = 64;
const MAX_CANDIDATES_PER_ROUND: usize = 128;
const MAX_EFFECTS: u32 = 4_096;
const MAX_STORED_NUMBER: u64 = i64::MAX as u64;

macro_rules! digest_id {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Digest);

        impl $name {
            pub const fn from_digest(digest: Digest) -> Self {
                Self(digest)
            }

            pub const fn digest(self) -> Digest {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

digest_id!(RoundId);
digest_id!(CandidateId);
digest_id!(MiningResultId);
digest_id!(ProposalId);
digest_id!(TrialResultId);
digest_id!(ScoreResultId);
digest_id!(RoundVerdictId);
digest_id!(CompositionId);
digest_id!(AuditReportId);
digest_id!(ApprovalId);
digest_id!(ActivationReceiptId);
digest_id!(MonitoringReportId);
digest_id!(RollbackReceiptId);
digest_id!(EffectWorkId);
digest_id!(EffectOutputId);
digest_id!(EffectFailureId);
digest_id!(EffectUncertaintyId);
digest_id!(EffectReceiptId);
digest_id!(ReconciliationId);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EffectId(Digest);

impl EffectId {
    /// Binds an external effect to the event that first made it durable.
    pub fn derive(
        campaign: CampaignId,
        transition_revision: u64,
        kind: EffectKind,
        work: EffectWorkId,
    ) -> Self {
        let identity = format!(
            "orvek:campaign-effect:v1:{campaign}:{transition_revision}:{}:{work}",
            kind.as_str()
        );
        Self(Digest::of(identity.as_bytes()))
    }

    pub const fn digest(self) -> Digest {
        self.0
    }
}

impl fmt::Display for EffectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CampaignNote(String);

impl CampaignNote {
    pub fn new(value: impl Into<String>) -> Result<Self, CampaignValidationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(CampaignValidationError::EmptyText {
                field: "campaign note",
            });
        }
        if value.len() > MAX_NOTE_BYTES {
            return Err(CampaignValidationError::TextTooLarge {
                field: "campaign note",
                maximum: MAX_NOTE_BYTES,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(CampaignValidationError::ControlCharacter {
                field: "campaign note",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CampaignNote {
    type Error = CampaignValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<CampaignNote> for String {
    fn from(value: CampaignNote) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignPhase {
    Ready,
    Mining,
    Proposing,
    Trialing,
    Scoring,
    Composing,
    Auditing,
    AwaitingApproval,
    Activating,
    Monitoring,
    Paused,
    Terminal,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceUsage {
    pub queries: u64,
    pub error_nanos: u64,
    pub artifact_bytes: u64,
}

impl ResourceUsage {
    pub const fn new(queries: u64, error_nanos: u64, artifact_bytes: u64) -> Self {
        Self {
            queries,
            error_nanos,
            artifact_bytes,
        }
    }

    fn validate(self, field: &'static str) -> Result<(), CampaignValidationError> {
        if self.queries > MAX_STORED_NUMBER
            || self.error_nanos > MAX_STORED_NUMBER
            || self.artifact_bytes > MAX_STORED_NUMBER
        {
            return Err(CampaignValidationError::NumberOutOfRange { field });
        }
        Ok(())
    }

    fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            queries: self.queries.checked_add(other.queries)?,
            error_nanos: self.error_nanos.checked_add(other.error_nanos)?,
            artifact_bytes: self.artifact_bytes.checked_add(other.artifact_bytes)?,
        })
    }

    fn checked_sub(self, other: Self) -> Option<Self> {
        Some(Self {
            queries: self.queries.checked_sub(other.queries)?,
            error_nanos: self.error_nanos.checked_sub(other.error_nanos)?,
            artifact_bytes: self.artifact_bytes.checked_sub(other.artifact_bytes)?,
        })
    }

    fn fits_within(self, limit: Self) -> bool {
        self.queries <= limit.queries
            && self.error_nanos <= limit.error_nanos
            && self.artifact_bytes <= limit.artifact_bytes
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignBudget {
    pub max_effects: u32,
    pub resources: ResourceUsage,
}

impl CampaignBudget {
    pub fn new(
        max_effects: u32,
        resources: ResourceUsage,
    ) -> Result<Self, CampaignValidationError> {
        let budget = Self {
            max_effects,
            resources,
        };
        budget.validate()?;
        Ok(budget)
    }

    fn validate(self) -> Result<(), CampaignValidationError> {
        if self.max_effects == 0 || self.max_effects > MAX_EFFECTS {
            return Err(CampaignValidationError::NumberOutOfRange {
                field: "campaign max effects",
            });
        }
        self.resources.validate("campaign resource budget")
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignUsage {
    pub effects_intended: u32,
    pub used: ResourceUsage,
    pub reserved: ResourceUsage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectBudget {
    pub resources: ResourceUsage,
}

impl EffectBudget {
    pub fn new(resources: ResourceUsage) -> Result<Self, CampaignValidationError> {
        resources.validate("effect resource budget")?;
        Ok(Self { resources })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    Mine,
    Propose,
    Trial,
    Score,
    Compose,
    Audit,
    RequestApproval,
    Activate,
    Monitor,
    Rollback,
}

impl EffectKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mine => "mine",
            Self::Propose => "propose",
            Self::Trial => "trial",
            Self::Score => "score",
            Self::Compose => "compose",
            Self::Audit => "audit",
            Self::RequestApproval => "request_approval",
            Self::Activate => "activate",
            Self::Monitor => "monitor",
            Self::Rollback => "rollback",
        }
    }

    fn allowed_in(self, phase: CampaignPhase) -> bool {
        matches!(
            (self, phase),
            (Self::Mine, CampaignPhase::Mining)
                | (Self::Propose, CampaignPhase::Proposing)
                | (Self::Trial, CampaignPhase::Trialing)
                | (Self::Score, CampaignPhase::Scoring)
                | (Self::Compose, CampaignPhase::Composing)
                | (Self::Audit, CampaignPhase::Auditing)
                | (Self::RequestApproval, CampaignPhase::AwaitingApproval)
                | (Self::Activate, CampaignPhase::Activating)
                | (Self::Monitor | Self::Rollback, CampaignPhase::Monitoring)
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LeaseEpoch(u64);

impl LeaseEpoch {
    pub fn new(value: u64) -> Result<Self, CampaignValidationError> {
        if value == 0 || value > MAX_STORED_NUMBER {
            return Err(CampaignValidationError::NumberOutOfRange {
                field: "lease epoch",
            });
        }
        Ok(Self(value))
    }

    pub const fn initial() -> Self {
        Self(1)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    fn next(self) -> Result<Self, CampaignTransitionError> {
        let value = self
            .0
            .checked_add(1)
            .filter(|value| *value <= MAX_STORED_NUMBER)
            .ok_or(CampaignTransitionError::RevisionExhausted)?;
        Ok(Self(value))
    }

    fn validate(self) -> Result<(), CampaignValidationError> {
        Self::new(self.0).map(|_| ())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectIntent {
    id: EffectId,
    campaign: CampaignId,
    transition_revision: u64,
    kind: EffectKind,
    work: EffectWorkId,
    budget: EffectBudget,
}

impl EffectIntent {
    pub fn new(
        campaign: CampaignId,
        transition_revision: u64,
        kind: EffectKind,
        work: EffectWorkId,
        budget: EffectBudget,
    ) -> Result<Self, CampaignValidationError> {
        validate_revision(transition_revision, "effect transition revision")?;
        budget.resources.validate("effect resource budget")?;
        Ok(Self {
            id: EffectId::derive(campaign, transition_revision, kind, work),
            campaign,
            transition_revision,
            kind,
            work,
            budget,
        })
    }

    pub const fn id(&self) -> EffectId {
        self.id
    }

    pub const fn campaign(&self) -> CampaignId {
        self.campaign
    }

    pub const fn transition_revision(&self) -> u64 {
        self.transition_revision
    }

    pub const fn kind(&self) -> EffectKind {
        self.kind
    }

    pub const fn work(&self) -> EffectWorkId {
        self.work
    }

    pub const fn budget(&self) -> EffectBudget {
        self.budget
    }

    fn validate(&self) -> Result<(), CampaignValidationError> {
        validate_revision(self.transition_revision, "effect transition revision")?;
        self.budget.resources.validate("effect resource budget")?;
        if self.id
            != EffectId::derive(
                self.campaign,
                self.transition_revision,
                self.kind,
                self.work,
            )
        {
            return Err(CampaignValidationError::BadEffectIdentity);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum EffectOutcome {
    Succeeded { output: EffectOutputId },
    Failed { failure: EffectFailureId },
    InfrastructureUnknown { uncertainty: EffectUncertaintyId },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum EffectAccounting {
    Known(ResourceUsage),
    ReservationCharged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectReceipt {
    pub id: EffectReceiptId,
    pub outcome: EffectOutcome,
    pub accounting: EffectAccounting,
}

impl EffectReceipt {
    fn charged_usage(
        self,
        reservation: EffectBudget,
    ) -> Result<ResourceUsage, CampaignTransitionError> {
        match self.accounting {
            EffectAccounting::Known(usage) => {
                usage
                    .validate("effect receipt usage")
                    .map_err(CampaignTransitionError::Invalid)?;
                if matches!(self.outcome, EffectOutcome::InfrastructureUnknown { .. }) {
                    return Err(CampaignTransitionError::UnknownEffectMustChargeReservation);
                }
                if !usage.fits_within(reservation.resources) {
                    return Err(CampaignTransitionError::ReceiptExceedsReservation);
                }
                Ok(usage)
            }
            EffectAccounting::ReservationCharged => Ok(reservation.resources),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum EffectReconciliation {
    RetryAuthorized { evidence: ReconciliationId },
    Recovered { receipt: EffectReceipt },
    FencedInfrastructureUnknown { receipt: EffectReceipt },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum EffectLifecycle {
    Intended {
        lease_epoch: LeaseEpoch,
    },
    Leased {
        epoch: LeaseEpoch,
    },
    Reconciled {
        lease_epoch: LeaseEpoch,
        evidence: ReconciliationId,
    },
    Settled {
        epoch: LeaseEpoch,
        receipt: EffectReceipt,
    },
}

impl EffectLifecycle {
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Settled { .. })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectState {
    intent: EffectIntent,
    lifecycle: EffectLifecycle,
}

impl EffectState {
    pub const fn intent(&self) -> &EffectIntent {
        &self.intent
    }

    pub const fn lifecycle(&self) -> &EffectLifecycle {
        &self.lifecycle
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum CandidateParent {
    BaseRevision(Digest),
    Candidate(CandidateId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum CandidateStage {
    Proposed {
        proposal: ProposalId,
    },
    Trialed {
        proposal: ProposalId,
        trial: TrialResultId,
    },
    Scored {
        proposal: ProposalId,
        trial: TrialResultId,
        score: ScoreResultId,
    },
    Composed {
        proposal: ProposalId,
        trial: TrialResultId,
        score: ScoreResultId,
        composition: CompositionId,
        revision: Digest,
    },
    Audited {
        proposal: ProposalId,
        trial: TrialResultId,
        score: ScoreResultId,
        composition: CompositionId,
        revision: Digest,
        report: AuditReportId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum CompositeOutcome {
    Verified,
    NotVerified,
    Inconclusive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum CompositeStage {
    AwaitingTrial,
    Trialed {
        trial: TrialResultId,
    },
    Scored {
        trial: TrialResultId,
        score: ScoreResultId,
        outcome: CompositeOutcome,
    },
    Audited {
        trial: TrialResultId,
        score: ScoreResultId,
        report: AuditReportId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositeState {
    plan: CompositePlan,
    stage: CompositeStage,
}

impl CompositeState {
    pub const fn plan(&self) -> &CompositePlan {
        &self.plan
    }

    pub const fn stage(&self) -> CompositeStage {
        self.stage
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateState {
    id: CandidateId,
    parent: CandidateParent,
    stage: CandidateStage,
}

impl CandidateState {
    pub const fn id(&self) -> CandidateId {
        self.id
    }

    pub const fn parent(&self) -> CandidateParent {
        self.parent
    }

    pub const fn stage(&self) -> CandidateStage {
        self.stage
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum RoundVerdict {
    Continue {
        basis: RoundVerdictId,
    },
    Compose {
        candidate: CandidateId,
        basis: RoundVerdictId,
    },
    ComposeComposite {
        candidate: CandidateId,
        composition: CompositionId,
        basis: RoundVerdictId,
    },
    NoUpdate {
        basis: RoundVerdictId,
    },
    Inconclusive {
        basis: RoundVerdictId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoundState {
    id: RoundId,
    ordinal: u32,
    mining: Option<MiningResultId>,
    candidates: Vec<CandidateState>,
    verdict: Option<RoundVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    composite: Option<CompositeState>,
}

impl RoundState {
    pub const fn id(&self) -> RoundId {
        self.id
    }

    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    pub const fn mining(&self) -> Option<MiningResultId> {
        self.mining
    }

    pub fn candidates(&self) -> &[CandidateState] {
        &self.candidates
    }

    pub const fn verdict(&self) -> Option<RoundVerdict> {
        self.verdict
    }

    pub const fn composite(&self) -> Option<&CompositeState> {
        self.composite.as_ref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum FinalVerdict {
    Verified {
        candidate: CandidateId,
        revision: Digest,
        report: AuditReportId,
    },
    NotVerified {
        candidate: CandidateId,
        revision: Digest,
        report: AuditReportId,
    },
    Inconclusive {
        candidate: CandidateId,
        revision: Digest,
        report: AuditReportId,
    },
}

impl FinalVerdict {
    const fn candidate(self) -> CandidateId {
        match self {
            Self::Verified { candidate, .. }
            | Self::NotVerified { candidate, .. }
            | Self::Inconclusive { candidate, .. } => candidate,
        }
    }

    pub const fn revision(self) -> Digest {
        match self {
            Self::Verified { revision, .. }
            | Self::NotVerified { revision, .. }
            | Self::Inconclusive { revision, .. } => revision,
        }
    }

    pub const fn report(self) -> AuditReportId {
        match self {
            Self::Verified { report, .. }
            | Self::NotVerified { report, .. }
            | Self::Inconclusive { report, .. } => report,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved {
        id: ApprovalId,
        revision: Digest,
    },
    Rejected {
        id: ApprovalId,
        revision: Digest,
        reason: CampaignNote,
    },
}

impl ApprovalDecision {
    pub const fn revision(&self) -> Digest {
        match self {
            Self::Approved { revision, .. } | Self::Rejected { revision, .. } => *revision,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ApprovalState {
    Awaiting {
        revision: Digest,
    },
    Approved {
        id: ApprovalId,
        revision: Digest,
    },
    Rejected {
        id: ApprovalId,
        revision: Digest,
        reason: CampaignNote,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ActivationOutcome {
    Activated {
        revision: Digest,
        receipt: ActivationReceiptId,
    },
    Superseded {
        requested_revision: Digest,
        active_revision: Digest,
        receipt: ActivationReceiptId,
    },
}

impl ActivationOutcome {
    const fn requested_revision(self) -> Digest {
        match self {
            Self::Activated { revision, .. } => revision,
            Self::Superseded {
                requested_revision, ..
            } => requested_revision,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ActivationState {
    Awaiting {
        revision: Digest,
    },
    Activated {
        revision: Digest,
        receipt: ActivationReceiptId,
    },
    Superseded {
        requested_revision: Digest,
        active_revision: Digest,
        receipt: ActivationReceiptId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum MonitoringOutcome {
    Healthy {
        revision: Digest,
        report: MonitoringReportId,
    },
    RolledBack {
        from_revision: Digest,
        restored_revision: Digest,
        report: MonitoringReportId,
        receipt: RollbackReceiptId,
    },
    InfrastructureUnknown {
        revision: Digest,
        report: MonitoringReportId,
    },
}

impl MonitoringOutcome {
    const fn monitored_revision(self) -> Digest {
        match self {
            Self::Healthy { revision, .. } | Self::InfrastructureUnknown { revision, .. } => {
                revision
            }
            Self::RolledBack { from_revision, .. } => from_revision,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum MonitoringState {
    Active {
        revision: Digest,
        activation: ActivationReceiptId,
    },
    Completed(MonitoringOutcome),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum TerminalState {
    NoUpdate {
        round: RoundId,
        verdict: RoundVerdict,
    },
    RoundInconclusive {
        round: RoundId,
        verdict: RoundVerdict,
    },
    CompositionFallback {
        round: RoundId,
        failure: CompositionFailure,
    },
    CompositeFallback {
        round: RoundId,
        candidate: CandidateId,
        revision: Digest,
        score: ScoreResultId,
        outcome: CompositeOutcome,
        fallback: CompositionFallback,
    },
    NotVerified(FinalVerdict),
    AuditInconclusive(FinalVerdict),
    ApprovalRejected(ApprovalDecision),
    Superseded(ActivationOutcome),
    MonitoringCompleted(MonitoringOutcome),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PauseState {
    previous_phase: CampaignPhase,
    reason: CampaignNote,
}

impl PauseState {
    pub const fn previous_phase(&self) -> CampaignPhase {
        self.previous_phase
    }

    pub const fn reason(&self) -> &CampaignNote {
        &self.reason
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignState {
    id: CampaignId,
    cohort: CohortId,
    base_revision: Digest,
    policy: PolicyIdentity,
    revision: u64,
    phase: CampaignPhase,
    budget: CampaignBudget,
    usage: CampaignUsage,
    rounds: Vec<RoundState>,
    effects: BTreeMap<EffectId, EffectState>,
    final_verdict: Option<FinalVerdict>,
    approval: Option<ApprovalState>,
    activation: Option<ActivationState>,
    monitoring: Option<MonitoringState>,
    terminal: Option<TerminalState>,
    pause: Option<PauseState>,
}

impl CampaignState {
    pub const fn id(&self) -> CampaignId {
        self.id
    }

    pub const fn cohort(&self) -> CohortId {
        self.cohort
    }

    pub const fn base_revision(&self) -> Digest {
        self.base_revision
    }

    pub const fn policy(&self) -> PolicyIdentity {
        self.policy
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub const fn phase(&self) -> CampaignPhase {
        self.phase
    }

    pub const fn budget(&self) -> CampaignBudget {
        self.budget
    }

    pub const fn usage(&self) -> CampaignUsage {
        self.usage
    }

    pub fn rounds(&self) -> &[RoundState] {
        &self.rounds
    }

    pub const fn effects(&self) -> &BTreeMap<EffectId, EffectState> {
        &self.effects
    }

    pub const fn final_verdict(&self) -> Option<FinalVerdict> {
        self.final_verdict
    }

    pub const fn approval(&self) -> Option<&ApprovalState> {
        self.approval.as_ref()
    }

    pub const fn activation(&self) -> Option<ActivationState> {
        self.activation
    }

    pub const fn monitoring(&self) -> Option<MonitoringState> {
        self.monitoring
    }

    pub const fn terminal(&self) -> Option<&TerminalState> {
        self.terminal.as_ref()
    }

    pub const fn pause(&self) -> Option<&PauseState> {
        self.pause.as_ref()
    }

    pub fn apply(&self, event: &CampaignEvent) -> Result<Self, CampaignTransitionError> {
        apply_campaign(Some(self), event)
    }

    pub fn validate(&self) -> Result<(), CampaignValidationError> {
        validate_revision(self.revision, "campaign revision")?;
        self.budget.validate()?;
        if self.rounds.len() > MAX_ROUNDS {
            return Err(CampaignValidationError::TooManyEntries {
                field: "campaign rounds",
                maximum: MAX_ROUNDS,
            });
        }
        if self.effects.len() > MAX_EFFECTS as usize {
            return Err(CampaignValidationError::TooManyEntries {
                field: "campaign effects",
                maximum: MAX_EFFECTS as usize,
            });
        }
        if self.phase == CampaignPhase::Terminal && self.terminal.is_none()
            || self.phase != CampaignPhase::Terminal && self.terminal.is_some()
        {
            return Err(CampaignValidationError::InconsistentState(
                "terminal marker does not match campaign phase",
            ));
        }
        if self.phase == CampaignPhase::Paused && self.pause.is_none()
            || self.phase != CampaignPhase::Paused && self.pause.is_some()
        {
            return Err(CampaignValidationError::InconsistentState(
                "pause marker does not match campaign phase",
            ));
        }

        let mut rounds = BTreeMap::new();
        let mut candidates = BTreeMap::new();
        for (index, round) in self.rounds.iter().enumerate() {
            if rounds.insert(round.id, ()).is_some() {
                return Err(CampaignValidationError::DuplicateIdentity { field: "round" });
            }
            if round.ordinal as usize != index + 1 {
                return Err(CampaignValidationError::InconsistentState(
                    "round ordinals are not contiguous",
                ));
            }
            if round.candidates.len() > MAX_CANDIDATES_PER_ROUND {
                return Err(CampaignValidationError::TooManyEntries {
                    field: "round candidates",
                    maximum: MAX_CANDIDATES_PER_ROUND,
                });
            }
            for candidate in &round.candidates {
                if candidates.contains_key(&candidate.id) {
                    return Err(CampaignValidationError::DuplicateIdentity { field: "candidate" });
                }
                match candidate.parent {
                    CandidateParent::BaseRevision(revision) if revision != self.base_revision => {
                        return Err(CampaignValidationError::InconsistentState(
                            "candidate base revision differs from campaign base",
                        ));
                    }
                    CandidateParent::Candidate(parent) if !candidates.contains_key(&parent) => {
                        return Err(CampaignValidationError::InconsistentState(
                            "candidate parent does not precede its child",
                        ));
                    }
                    _ => {}
                }
                candidates.insert(candidate.id, candidate.parent);
            }
            if let Some(composite) = &round.composite {
                composite.plan.validate().map_err(|_| {
                    CampaignValidationError::InconsistentState("composite plan identity is invalid")
                })?;
                if composite.plan.parent() != self.base_revision {
                    return Err(CampaignValidationError::InconsistentState(
                        "composite parent differs from campaign base",
                    ));
                }
                if candidates.contains_key(&composite.plan.candidate()) {
                    return Err(CampaignValidationError::DuplicateIdentity { field: "candidate" });
                }
                for child in composite.plan.children() {
                    let child_state = round
                        .candidates
                        .iter()
                        .find(|candidate| candidate.id == child.candidate())
                        .ok_or(CampaignValidationError::InconsistentState(
                            "composite child is not in its round",
                        ))?;
                    if !matches!(
                        child_state.stage,
                        CandidateStage::Scored {
                            proposal,
                            score,
                            ..
                        } if proposal == child.proposal() && score == child.score()
                    ) {
                        return Err(CampaignValidationError::InconsistentState(
                            "composite child lineage differs from its scored candidate",
                        ));
                    }
                }
                candidates.insert(
                    composite.plan.candidate(),
                    CandidateParent::BaseRevision(composite.plan.parent()),
                );
            }
        }

        if let Some(TerminalState::CompositionFallback { round, failure }) = &self.terminal {
            failure.validate().map_err(|_| {
                CampaignValidationError::InconsistentState(
                    "composition failure identity is invalid",
                )
            })?;
            if failure.parent() != self.base_revision {
                return Err(CampaignValidationError::InconsistentState(
                    "composition failure parent differs from campaign base",
                ));
            }
            let round_state = self.rounds.iter().find(|state| state.id == *round).ok_or(
                CampaignValidationError::InconsistentState(
                    "composition failure round is not in the campaign",
                ),
            )?;
            if !matches!(
                round_state.verdict,
                Some(RoundVerdict::ComposeComposite {
                    candidate,
                    composition,
                    ..
                }) if candidate == failure.candidate() && composition == failure.id()
            ) {
                return Err(CampaignValidationError::InconsistentState(
                    "composition failure differs from the round verdict",
                ));
            }
            for child in failure.children() {
                let child_state = round_state
                    .candidates
                    .iter()
                    .find(|candidate| candidate.id == child.candidate())
                    .ok_or(CampaignValidationError::InconsistentState(
                        "composition failure child is not in its round",
                    ))?;
                if !matches!(
                    child_state.stage,
                    CandidateStage::Scored {
                        proposal,
                        score,
                        ..
                    } if proposal == child.proposal() && score == child.score()
                ) {
                    return Err(CampaignValidationError::InconsistentState(
                        "composition failure child differs from its scored candidate",
                    ));
                }
            }
        }

        let mut calculated = CampaignUsage::default();
        for (id, effect) in &self.effects {
            effect.intent.validate()?;
            if *id != effect.intent.id || effect.intent.campaign != self.id {
                return Err(CampaignValidationError::BadEffectIdentity);
            }
            if effect.intent.transition_revision > self.revision {
                return Err(CampaignValidationError::InconsistentState(
                    "effect intent is newer than campaign projection",
                ));
            }
            calculated.effects_intended = calculated.effects_intended.checked_add(1).ok_or(
                CampaignValidationError::NumberOutOfRange {
                    field: "campaign effect count",
                },
            )?;
            match &effect.lifecycle {
                EffectLifecycle::Intended { lease_epoch }
                | EffectLifecycle::Reconciled { lease_epoch, .. } => {
                    lease_epoch.validate()?;
                    calculated.reserved = calculated
                        .reserved
                        .checked_add(effect.intent.budget.resources)
                        .ok_or(CampaignValidationError::NumberOutOfRange {
                            field: "campaign reserved usage",
                        })?;
                }
                EffectLifecycle::Leased { epoch } => {
                    epoch.validate()?;
                    calculated.reserved = calculated
                        .reserved
                        .checked_add(effect.intent.budget.resources)
                        .ok_or(CampaignValidationError::NumberOutOfRange {
                            field: "campaign reserved usage",
                        })?;
                }
                EffectLifecycle::Settled { epoch, receipt } => {
                    epoch.validate()?;
                    let charged = match receipt.accounting {
                        EffectAccounting::Known(usage) => {
                            usage.validate("effect receipt usage")?;
                            if matches!(
                                receipt.outcome,
                                EffectOutcome::InfrastructureUnknown { .. }
                            ) || !usage.fits_within(effect.intent.budget.resources)
                            {
                                return Err(CampaignValidationError::InconsistentState(
                                    "effect receipt accounting is invalid",
                                ));
                            }
                            usage
                        }
                        EffectAccounting::ReservationCharged => effect.intent.budget.resources,
                    };
                    calculated.used = calculated.used.checked_add(charged).ok_or(
                        CampaignValidationError::NumberOutOfRange {
                            field: "campaign used resources",
                        },
                    )?;
                }
            }
        }
        if calculated != self.usage {
            return Err(CampaignValidationError::InconsistentState(
                "campaign usage does not match effect records",
            ));
        }
        if calculated.effects_intended > self.budget.max_effects
            || !calculated
                .used
                .checked_add(calculated.reserved)
                .is_some_and(|total| total.fits_within(self.budget.resources))
        {
            return Err(CampaignValidationError::InconsistentState(
                "campaign usage exceeds its budget",
            ));
        }
        Ok(())
    }

    fn current_round(&self, id: RoundId) -> Result<&RoundState, CampaignTransitionError> {
        self.rounds
            .last()
            .filter(|round| round.id == id)
            .ok_or(CampaignTransitionError::WrongRound { round: id })
    }

    fn current_round_mut(
        &mut self,
        id: RoundId,
    ) -> Result<&mut RoundState, CampaignTransitionError> {
        self.rounds
            .last_mut()
            .filter(|round| round.id == id)
            .ok_or(CampaignTransitionError::WrongRound { round: id })
    }

    fn candidate_exists(&self, id: CandidateId) -> bool {
        self.rounds.iter().any(|round| {
            round.candidates.iter().any(|candidate| candidate.id == id)
                || round
                    .composite
                    .as_ref()
                    .is_some_and(|composite| composite.plan.candidate() == id)
        })
    }

    fn require_no_open_effects(&self) -> Result<(), CampaignTransitionError> {
        if self
            .effects
            .values()
            .any(|effect| !effect.lifecycle.is_terminal())
        {
            return Err(CampaignTransitionError::OpenEffects);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum CampaignEvent {
    Started {
        campaign: CampaignId,
        cohort: CohortId,
        base_revision: Digest,
        policy: PolicyIdentity,
        budget: CampaignBudget,
    },
    RoundStarted {
        round: RoundId,
    },
    MiningCompleted {
        round: RoundId,
        result: MiningResultId,
    },
    CandidateProposed {
        round: RoundId,
        candidate: CandidateId,
        parent: CandidateParent,
        proposal: ProposalId,
    },
    ProposalsCompleted {
        round: RoundId,
    },
    CandidateTrialRecorded {
        round: RoundId,
        candidate: CandidateId,
        trial: TrialResultId,
    },
    TrialsCompleted {
        round: RoundId,
    },
    CandidateScoreRecorded {
        round: RoundId,
        candidate: CandidateId,
        score: ScoreResultId,
    },
    RoundVerdictRecorded {
        round: RoundId,
        verdict: RoundVerdict,
    },
    CompositionRecorded {
        round: RoundId,
        candidate: CandidateId,
        composition: CompositionId,
        revision: Digest,
    },
    CompositeCompositionRecorded {
        round: RoundId,
        plan: CompositePlan,
    },
    CompositeCompositionFailed {
        round: RoundId,
        failure: CompositionFailure,
    },
    CompositeScoreRecorded {
        round: RoundId,
        candidate: CandidateId,
        score: ScoreResultId,
        outcome: CompositeOutcome,
    },
    FinalVerdictRecorded {
        verdict: FinalVerdict,
    },
    ApprovalRecorded {
        decision: ApprovalDecision,
    },
    ActivationRecorded {
        outcome: ActivationOutcome,
    },
    MonitoringCompleted {
        outcome: MonitoringOutcome,
    },
    Paused {
        reason: CampaignNote,
    },
    Resumed,
    EffectIntended {
        intent: EffectIntent,
    },
    EffectLeased {
        effect: EffectId,
        epoch: LeaseEpoch,
    },
    EffectReconciled {
        effect: EffectId,
        leased_epoch: LeaseEpoch,
        next_epoch: LeaseEpoch,
        outcome: EffectReconciliation,
    },
    EffectSettled {
        effect: EffectId,
        epoch: LeaseEpoch,
        receipt: EffectReceipt,
    },
}

impl CampaignEvent {
    fn name(&self) -> &'static str {
        match self {
            Self::Started { .. } => "started",
            Self::RoundStarted { .. } => "round_started",
            Self::MiningCompleted { .. } => "mining_completed",
            Self::CandidateProposed { .. } => "candidate_proposed",
            Self::ProposalsCompleted { .. } => "proposals_completed",
            Self::CandidateTrialRecorded { .. } => "candidate_trial_recorded",
            Self::TrialsCompleted { .. } => "trials_completed",
            Self::CandidateScoreRecorded { .. } => "candidate_score_recorded",
            Self::RoundVerdictRecorded { .. } => "round_verdict_recorded",
            Self::CompositionRecorded { .. } => "composition_recorded",
            Self::CompositeCompositionRecorded { .. } => "composite_composition_recorded",
            Self::CompositeCompositionFailed { .. } => "composite_composition_failed",
            Self::CompositeScoreRecorded { .. } => "composite_score_recorded",
            Self::FinalVerdictRecorded { .. } => "final_verdict_recorded",
            Self::ApprovalRecorded { .. } => "approval_recorded",
            Self::ActivationRecorded { .. } => "activation_recorded",
            Self::MonitoringCompleted { .. } => "monitoring_completed",
            Self::Paused { .. } => "paused",
            Self::Resumed => "resumed",
            Self::EffectIntended { .. } => "effect_intended",
            Self::EffectLeased { .. } => "effect_leased",
            Self::EffectReconciled { .. } => "effect_reconciled",
            Self::EffectSettled { .. } => "effect_settled",
        }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum CampaignValidationError {
    #[error("{field} must not be empty")]
    EmptyText { field: &'static str },
    #[error("{field} exceeds the maximum of {maximum} bytes")]
    TextTooLarge { field: &'static str, maximum: usize },
    #[error("{field} contains a control character")]
    ControlCharacter { field: &'static str },
    #[error("{field} is outside durable storage bounds")]
    NumberOutOfRange { field: &'static str },
    #[error("{field} exceeds the maximum of {maximum} entries")]
    TooManyEntries { field: &'static str, maximum: usize },
    #[error("duplicate {field} identity")]
    DuplicateIdentity { field: &'static str },
    #[error("effect identity does not match its campaign transition")]
    BadEffectIdentity,
    #[error("invalid campaign projection: {0}")]
    InconsistentState(&'static str),
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum CampaignTransitionError {
    #[error(transparent)]
    Invalid(#[from] CampaignValidationError),
    #[error("campaign has not started")]
    NotStarted,
    #[error("campaign has already started")]
    AlreadyStarted,
    #[error("event {event} is illegal in campaign phase {phase:?}")]
    IllegalTransition {
        phase: CampaignPhase,
        event: &'static str,
    },
    #[error("campaign revision is exhausted")]
    RevisionExhausted,
    #[error("campaign terminal state cannot be mutated by {event}")]
    TerminalMutation { event: &'static str },
    #[error("round {round} is not the current campaign round")]
    WrongRound { round: RoundId },
    #[error("round {round} already exists")]
    DuplicateRound { round: RoundId },
    #[error("candidate {candidate} already exists")]
    DuplicateCandidate { candidate: CandidateId },
    #[error("candidate {candidate} does not exist")]
    UnknownCandidate { candidate: CandidateId },
    #[error("candidate {candidate} is in the wrong stage")]
    WrongCandidateStage { candidate: CandidateId },
    #[error("candidate {candidate} has an invalid parent")]
    InvalidCandidateParent { candidate: CandidateId },
    #[error("the current round has no candidates")]
    EmptyRound,
    #[error("not every candidate has reached the required stage")]
    IncompleteCandidates,
    #[error("verdict candidate {candidate} does not belong to the current round")]
    InvalidVerdictCandidate { candidate: CandidateId },
    #[error("event revision or revision identity does not match campaign state")]
    RevisionMismatch,
    #[error("campaign has unsettled external effects")]
    OpenEffects,
    #[error("campaign effect budget is exhausted")]
    BudgetExceeded,
    #[error("effect {effect} already exists")]
    DuplicateEffect { effect: EffectId },
    #[error("effect {effect} does not exist")]
    UnknownEffect { effect: EffectId },
    #[error("effect {effect} cannot be leased again without reconciliation")]
    ReconciliationRequired { effect: EffectId },
    #[error("effect {effect} has not been leased")]
    EffectNotLeased { effect: EffectId },
    #[error("effect {effect} is already terminal")]
    EffectAlreadySettled { effect: EffectId },
    #[error("stale lease epoch for effect {effect}: expected {expected}, got {actual}")]
    StaleLeaseEpoch {
        effect: EffectId,
        expected: u64,
        actual: u64,
    },
    #[error("invalid lease epoch for effect {effect}: expected {expected}, got {actual}")]
    InvalidLeaseEpoch {
        effect: EffectId,
        expected: u64,
        actual: u64,
    },
    #[error("effect receipt exceeds its durable reservation")]
    ReceiptExceedsReservation,
    #[error("an infrastructure-unknown receipt must conservatively charge its reservation")]
    UnknownEffectMustChargeReservation,
    #[error("a fenced reconciliation must produce an infrastructure-unknown receipt")]
    FenceRequiresInfrastructureUnknown,
    #[error("effect {effect} already has a different terminal receipt")]
    ConflictingTerminalReceipt { effect: EffectId },
}

/// Applies one journal event. A terminal receipt replay returns the state unchanged.
pub fn apply_campaign(
    state: Option<&CampaignState>,
    event: &CampaignEvent,
) -> Result<CampaignState, CampaignTransitionError> {
    let Some(state) = state else {
        return start_campaign(event);
    };
    state.validate()?;

    if matches!(event, CampaignEvent::Started { .. }) {
        return Err(CampaignTransitionError::AlreadyStarted);
    }
    if terminal_effect_replay(state, event)? {
        return Ok(state.clone());
    }
    if state.phase == CampaignPhase::Terminal {
        return Err(CampaignTransitionError::TerminalMutation {
            event: event.name(),
        });
    }

    let mut next = state.clone();
    apply_existing(&mut next, event)?;
    next.revision = next
        .revision
        .checked_add(1)
        .filter(|revision| *revision <= MAX_STORED_NUMBER)
        .ok_or(CampaignTransitionError::RevisionExhausted)?;
    next.validate()?;
    Ok(next)
}

fn start_campaign(event: &CampaignEvent) -> Result<CampaignState, CampaignTransitionError> {
    let CampaignEvent::Started {
        campaign,
        cohort,
        base_revision,
        policy,
        budget,
    } = event
    else {
        return Err(CampaignTransitionError::NotStarted);
    };
    budget.validate()?;
    let state = CampaignState {
        id: *campaign,
        cohort: *cohort,
        base_revision: *base_revision,
        policy: *policy,
        revision: 1,
        phase: CampaignPhase::Ready,
        budget: *budget,
        usage: CampaignUsage::default(),
        rounds: Vec::new(),
        effects: BTreeMap::new(),
        final_verdict: None,
        approval: None,
        activation: None,
        monitoring: None,
        terminal: None,
        pause: None,
    };
    state.validate()?;
    Ok(state)
}

fn apply_existing(
    state: &mut CampaignState,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    match event {
        CampaignEvent::Started { .. } => Err(CampaignTransitionError::AlreadyStarted),
        CampaignEvent::RoundStarted { round } => start_round(state, *round),
        CampaignEvent::MiningCompleted { round, result } => {
            require_phase(state, CampaignPhase::Mining, event)?;
            state.require_no_open_effects()?;
            let current = state.current_round_mut(*round)?;
            if current.mining.replace(*result).is_some() {
                return Err(CampaignTransitionError::IllegalTransition {
                    phase: state.phase,
                    event: event.name(),
                });
            }
            state.phase = CampaignPhase::Proposing;
            Ok(())
        }
        CampaignEvent::CandidateProposed {
            round,
            candidate,
            parent,
            proposal,
        } => propose_candidate(state, *round, *candidate, *parent, *proposal, event),
        CampaignEvent::ProposalsCompleted { round } => {
            require_phase(state, CampaignPhase::Proposing, event)?;
            state.require_no_open_effects()?;
            let current = state.current_round(*round)?;
            if current.candidates.is_empty() {
                return Err(CampaignTransitionError::EmptyRound);
            }
            state.phase = CampaignPhase::Trialing;
            Ok(())
        }
        CampaignEvent::CandidateTrialRecorded {
            round,
            candidate,
            trial,
        } => record_trial(state, *round, *candidate, *trial, event),
        CampaignEvent::TrialsCompleted { round } => {
            require_phase(state, CampaignPhase::Trialing, event)?;
            state.require_no_open_effects()?;
            let current = state.current_round(*round)?;
            let trials_complete = current.composite.as_ref().map_or_else(
                || {
                    current
                        .candidates
                        .iter()
                        .all(|candidate| matches!(candidate.stage, CandidateStage::Trialed { .. }))
                },
                |composite| matches!(composite.stage, CompositeStage::Trialed { .. }),
            );
            if !trials_complete {
                return Err(CampaignTransitionError::IncompleteCandidates);
            }
            state.phase = CampaignPhase::Scoring;
            Ok(())
        }
        CampaignEvent::CandidateScoreRecorded {
            round,
            candidate,
            score,
        } => record_score(state, *round, *candidate, *score, event),
        CampaignEvent::RoundVerdictRecorded { round, verdict } => {
            record_round_verdict(state, *round, *verdict, event)
        }
        CampaignEvent::CompositionRecorded {
            round,
            candidate,
            composition,
            revision,
        } => record_composition(state, *round, *candidate, *composition, *revision, event),
        CampaignEvent::CompositeCompositionRecorded { round, plan } => {
            record_composite_composition(state, *round, plan.clone(), event)
        }
        CampaignEvent::CompositeCompositionFailed { round, failure } => {
            record_composite_composition_failure(state, *round, failure.clone(), event)
        }
        CampaignEvent::CompositeScoreRecorded {
            round,
            candidate,
            score,
            outcome,
        } => record_composite_score(state, *round, *candidate, *score, *outcome, event),
        CampaignEvent::FinalVerdictRecorded { verdict } => {
            record_final_verdict(state, *verdict, event)
        }
        CampaignEvent::ApprovalRecorded { decision } => {
            record_approval(state, decision.clone(), event)
        }
        CampaignEvent::ActivationRecorded { outcome } => record_activation(state, *outcome, event),
        CampaignEvent::MonitoringCompleted { outcome } => record_monitoring(state, *outcome, event),
        CampaignEvent::Paused { reason } => pause(state, reason.clone(), event),
        CampaignEvent::Resumed => resume(state, event),
        CampaignEvent::EffectIntended { intent } => intend_effect(state, intent.clone(), event),
        CampaignEvent::EffectLeased { effect, epoch } => {
            lease_effect(state, *effect, *epoch, event)
        }
        CampaignEvent::EffectReconciled {
            effect,
            leased_epoch,
            next_epoch,
            outcome,
        } => reconcile_effect(state, *effect, *leased_epoch, *next_epoch, *outcome),
        CampaignEvent::EffectSettled {
            effect,
            epoch,
            receipt,
        } => settle_effect(state, *effect, *epoch, *receipt),
    }
}

fn require_phase(
    state: &CampaignState,
    expected: CampaignPhase,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    if state.phase != expected {
        return Err(CampaignTransitionError::IllegalTransition {
            phase: state.phase,
            event: event.name(),
        });
    }
    Ok(())
}

fn start_round(state: &mut CampaignState, round: RoundId) -> Result<(), CampaignTransitionError> {
    if state.phase != CampaignPhase::Ready {
        return Err(CampaignTransitionError::IllegalTransition {
            phase: state.phase,
            event: "round_started",
        });
    }
    if state.rounds.iter().any(|existing| existing.id == round) {
        return Err(CampaignTransitionError::DuplicateRound { round });
    }
    if state.rounds.len() == MAX_ROUNDS {
        return Err(CampaignValidationError::TooManyEntries {
            field: "campaign rounds",
            maximum: MAX_ROUNDS,
        }
        .into());
    }
    state.rounds.push(RoundState {
        id: round,
        ordinal: state.rounds.len() as u32 + 1,
        mining: None,
        candidates: Vec::new(),
        verdict: None,
        composite: None,
    });
    state.phase = CampaignPhase::Mining;
    Ok(())
}

fn propose_candidate(
    state: &mut CampaignState,
    round: RoundId,
    candidate: CandidateId,
    parent: CandidateParent,
    proposal: ProposalId,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    require_phase(state, CampaignPhase::Proposing, event)?;
    if state.candidate_exists(candidate) {
        return Err(CampaignTransitionError::DuplicateCandidate { candidate });
    }
    let parent_is_valid = match parent {
        CandidateParent::BaseRevision(revision) => revision == state.base_revision,
        CandidateParent::Candidate(parent) => state.candidate_exists(parent),
    };
    if !parent_is_valid {
        return Err(CampaignTransitionError::InvalidCandidateParent { candidate });
    }
    let current = state.current_round_mut(round)?;
    if current.candidates.len() == MAX_CANDIDATES_PER_ROUND {
        return Err(CampaignValidationError::TooManyEntries {
            field: "round candidates",
            maximum: MAX_CANDIDATES_PER_ROUND,
        }
        .into());
    }
    current.candidates.push(CandidateState {
        id: candidate,
        parent,
        stage: CandidateStage::Proposed { proposal },
    });
    Ok(())
}

fn record_trial(
    state: &mut CampaignState,
    round: RoundId,
    candidate: CandidateId,
    trial: TrialResultId,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    require_phase(state, CampaignPhase::Trialing, event)?;
    let current = state.current_round_mut(round)?;
    if let Some(composite) = current
        .composite
        .as_mut()
        .filter(|composite| composite.plan.candidate() == candidate)
    {
        if composite.stage != CompositeStage::AwaitingTrial {
            return Err(CampaignTransitionError::WrongCandidateStage { candidate });
        }
        composite.stage = CompositeStage::Trialed { trial };
        return Ok(());
    }
    let candidate_state = current
        .candidates
        .iter_mut()
        .find(|state| state.id == candidate)
        .ok_or(CampaignTransitionError::UnknownCandidate { candidate })?;
    let CandidateStage::Proposed { proposal } = candidate_state.stage else {
        return Err(CampaignTransitionError::WrongCandidateStage { candidate });
    };
    candidate_state.stage = CandidateStage::Trialed { proposal, trial };
    Ok(())
}

fn record_score(
    state: &mut CampaignState,
    round: RoundId,
    candidate: CandidateId,
    score: ScoreResultId,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    require_phase(state, CampaignPhase::Scoring, event)?;
    let current = state.current_round_mut(round)?;
    let candidate_state = current
        .candidates
        .iter_mut()
        .find(|state| state.id == candidate)
        .ok_or(CampaignTransitionError::UnknownCandidate { candidate })?;
    let CandidateStage::Trialed { proposal, trial } = candidate_state.stage else {
        return Err(CampaignTransitionError::WrongCandidateStage { candidate });
    };
    candidate_state.stage = CandidateStage::Scored {
        proposal,
        trial,
        score,
    };
    Ok(())
}

fn record_round_verdict(
    state: &mut CampaignState,
    round: RoundId,
    verdict: RoundVerdict,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    require_phase(state, CampaignPhase::Scoring, event)?;
    state.require_no_open_effects()?;
    {
        let current = state.current_round(round)?;
        if current.candidates.is_empty()
            || !current
                .candidates
                .iter()
                .all(|candidate| matches!(candidate.stage, CandidateStage::Scored { .. }))
        {
            return Err(CampaignTransitionError::IncompleteCandidates);
        }
        match verdict {
            RoundVerdict::Compose { candidate, .. }
                if !current.candidates.iter().any(|state| state.id == candidate) =>
            {
                return Err(CampaignTransitionError::InvalidVerdictCandidate { candidate });
            }
            RoundVerdict::ComposeComposite { candidate, .. }
                if state.candidate_exists(candidate) =>
            {
                return Err(CampaignTransitionError::DuplicateCandidate { candidate });
            }
            _ => {}
        }
    }
    state.current_round_mut(round)?.verdict = Some(verdict);
    match verdict {
        RoundVerdict::Continue { .. } => state.phase = CampaignPhase::Ready,
        RoundVerdict::Compose { .. } | RoundVerdict::ComposeComposite { .. } => {
            state.phase = CampaignPhase::Composing;
        }
        RoundVerdict::NoUpdate { .. } => {
            state.require_no_open_effects()?;
            state.phase = CampaignPhase::Terminal;
            state.terminal = Some(TerminalState::NoUpdate { round, verdict });
        }
        RoundVerdict::Inconclusive { .. } => {
            state.require_no_open_effects()?;
            state.phase = CampaignPhase::Terminal;
            state.terminal = Some(TerminalState::RoundInconclusive { round, verdict });
        }
    }
    Ok(())
}

fn record_composite_composition(
    state: &mut CampaignState,
    round: RoundId,
    plan: CompositePlan,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    require_phase(state, CampaignPhase::Composing, event)?;
    state.require_no_open_effects()?;
    plan.validate().map_err(|_| {
        CampaignValidationError::InconsistentState("composite plan identity is invalid")
    })?;
    if plan.parent() != state.base_revision {
        return Err(CampaignTransitionError::RevisionMismatch);
    }
    let current = state.current_round_mut(round)?;
    if !matches!(
        current.verdict,
        Some(RoundVerdict::ComposeComposite {
            candidate,
            composition,
            ..
        }) if candidate == plan.candidate() && composition == plan.id()
    ) {
        return Err(CampaignTransitionError::InvalidVerdictCandidate {
            candidate: plan.candidate(),
        });
    }
    if current.composite.is_some() {
        return Err(CampaignTransitionError::DuplicateCandidate {
            candidate: plan.candidate(),
        });
    }
    for child in plan.children() {
        let child_state = current
            .candidates
            .iter()
            .find(|candidate| candidate.id == child.candidate())
            .ok_or(CampaignTransitionError::UnknownCandidate {
                candidate: child.candidate(),
            })?;
        if !matches!(
            child_state.stage,
            CandidateStage::Scored {
                proposal,
                score,
                ..
            } if proposal == child.proposal() && score == child.score()
        ) {
            return Err(CampaignTransitionError::WrongCandidateStage {
                candidate: child.candidate(),
            });
        }
    }
    current.composite = Some(CompositeState {
        plan,
        stage: CompositeStage::AwaitingTrial,
    });
    state.phase = CampaignPhase::Trialing;
    Ok(())
}

fn record_composite_composition_failure(
    state: &mut CampaignState,
    round: RoundId,
    failure: CompositionFailure,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    require_phase(state, CampaignPhase::Composing, event)?;
    state.require_no_open_effects()?;
    failure.validate().map_err(|_| {
        CampaignValidationError::InconsistentState("composition failure identity is invalid")
    })?;
    if failure.parent() != state.base_revision {
        return Err(CampaignTransitionError::RevisionMismatch);
    }
    let current = state.current_round(round)?;
    if !matches!(
        current.verdict,
        Some(RoundVerdict::ComposeComposite {
            candidate,
            composition,
            ..
        }) if candidate == failure.candidate() && composition == failure.id()
    ) {
        return Err(CampaignTransitionError::InvalidVerdictCandidate {
            candidate: failure.candidate(),
        });
    }
    for child in failure.children() {
        let child_state = current
            .candidates
            .iter()
            .find(|candidate| candidate.id == child.candidate())
            .ok_or(CampaignTransitionError::UnknownCandidate {
                candidate: child.candidate(),
            })?;
        if !matches!(
            child_state.stage,
            CandidateStage::Scored {
                proposal,
                score,
                ..
            } if proposal == child.proposal() && score == child.score()
        ) {
            return Err(CampaignTransitionError::WrongCandidateStage {
                candidate: child.candidate(),
            });
        }
    }
    state.phase = CampaignPhase::Terminal;
    state.terminal = Some(TerminalState::CompositionFallback { round, failure });
    Ok(())
}

fn record_composite_score(
    state: &mut CampaignState,
    round: RoundId,
    candidate: CandidateId,
    score: ScoreResultId,
    outcome: CompositeOutcome,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    require_phase(state, CampaignPhase::Scoring, event)?;
    let (revision, fallback) = {
        let current = state.current_round_mut(round)?;
        let composite = current
            .composite
            .as_mut()
            .filter(|composite| composite.plan.candidate() == candidate)
            .ok_or(CampaignTransitionError::UnknownCandidate { candidate })?;
        let CompositeStage::Trialed { trial } = composite.stage else {
            return Err(CampaignTransitionError::WrongCandidateStage { candidate });
        };
        composite.stage = CompositeStage::Scored {
            trial,
            score,
            outcome,
        };
        (composite.plan.revision(), composite.plan.fallback())
    };
    match outcome {
        CompositeOutcome::Verified => state.phase = CampaignPhase::Auditing,
        CompositeOutcome::NotVerified | CompositeOutcome::Inconclusive => {
            state.require_no_open_effects()?;
            state.phase = CampaignPhase::Terminal;
            state.terminal = Some(TerminalState::CompositeFallback {
                round,
                candidate,
                revision,
                score,
                outcome,
                fallback,
            });
        }
    }
    Ok(())
}

fn record_composition(
    state: &mut CampaignState,
    round: RoundId,
    candidate: CandidateId,
    composition: CompositionId,
    revision: Digest,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    require_phase(state, CampaignPhase::Composing, event)?;
    state.require_no_open_effects()?;
    let current = state.current_round_mut(round)?;
    if !matches!(
        current.verdict,
        Some(RoundVerdict::Compose {
            candidate: selected,
            ..
        }) if selected == candidate
    ) {
        return Err(CampaignTransitionError::InvalidVerdictCandidate { candidate });
    }
    let candidate_state = current
        .candidates
        .iter_mut()
        .find(|state| state.id == candidate)
        .ok_or(CampaignTransitionError::UnknownCandidate { candidate })?;
    let CandidateStage::Scored {
        proposal,
        trial,
        score,
    } = candidate_state.stage
    else {
        return Err(CampaignTransitionError::WrongCandidateStage { candidate });
    };
    candidate_state.stage = CandidateStage::Composed {
        proposal,
        trial,
        score,
        composition,
        revision,
    };
    state.phase = CampaignPhase::Auditing;
    Ok(())
}

fn record_final_verdict(
    state: &mut CampaignState,
    verdict: FinalVerdict,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    require_phase(state, CampaignPhase::Auditing, event)?;
    state.require_no_open_effects()?;
    let candidate = verdict.candidate();
    let revision = verdict.revision();
    let report = verdict.report();
    let current = state
        .rounds
        .last_mut()
        .ok_or(CampaignTransitionError::UnknownCandidate { candidate })?;
    if let Some(composite) = current
        .composite
        .as_mut()
        .filter(|composite| composite.plan.candidate() == candidate)
    {
        let CompositeStage::Scored {
            trial,
            score,
            outcome: CompositeOutcome::Verified,
        } = composite.stage
        else {
            return Err(CampaignTransitionError::WrongCandidateStage { candidate });
        };
        if revision != composite.plan.revision() {
            return Err(CampaignTransitionError::RevisionMismatch);
        }
        composite.stage = CompositeStage::Audited {
            trial,
            score,
            report,
        };
    } else {
        let candidate_state = current
            .candidates
            .iter_mut()
            .find(|state| state.id == candidate)
            .ok_or(CampaignTransitionError::UnknownCandidate { candidate })?;
        let CandidateStage::Composed {
            proposal,
            trial,
            score,
            composition,
            revision: composed_revision,
        } = candidate_state.stage
        else {
            return Err(CampaignTransitionError::WrongCandidateStage { candidate });
        };
        if revision != composed_revision {
            return Err(CampaignTransitionError::RevisionMismatch);
        }
        candidate_state.stage = CandidateStage::Audited {
            proposal,
            trial,
            score,
            composition,
            revision,
            report,
        };
    }
    state.final_verdict = Some(verdict);
    match verdict {
        FinalVerdict::Verified { .. } => {
            state.phase = CampaignPhase::AwaitingApproval;
            state.approval = Some(ApprovalState::Awaiting { revision });
        }
        FinalVerdict::NotVerified { .. } => {
            state.require_no_open_effects()?;
            state.phase = CampaignPhase::Terminal;
            state.terminal = Some(TerminalState::NotVerified(verdict));
        }
        FinalVerdict::Inconclusive { .. } => {
            state.require_no_open_effects()?;
            state.phase = CampaignPhase::Terminal;
            state.terminal = Some(TerminalState::AuditInconclusive(verdict));
        }
    }
    Ok(())
}

fn record_approval(
    state: &mut CampaignState,
    decision: ApprovalDecision,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    require_phase(state, CampaignPhase::AwaitingApproval, event)?;
    state.require_no_open_effects()?;
    let revision = match state.approval.as_ref() {
        Some(ApprovalState::Awaiting { revision }) => *revision,
        _ => {
            return Err(CampaignTransitionError::IllegalTransition {
                phase: state.phase,
                event: event.name(),
            });
        }
    };
    if decision.revision() != revision {
        return Err(CampaignTransitionError::RevisionMismatch);
    }
    match decision {
        ApprovalDecision::Approved { id, revision } => {
            state.approval = Some(ApprovalState::Approved { id, revision });
            state.activation = Some(ActivationState::Awaiting { revision });
            state.phase = CampaignPhase::Activating;
        }
        ApprovalDecision::Rejected {
            id,
            revision,
            ref reason,
        } => {
            state.require_no_open_effects()?;
            state.approval = Some(ApprovalState::Rejected {
                id,
                revision,
                reason: reason.clone(),
            });
            state.phase = CampaignPhase::Terminal;
            state.terminal = Some(TerminalState::ApprovalRejected(decision));
        }
    }
    Ok(())
}

fn record_activation(
    state: &mut CampaignState,
    outcome: ActivationOutcome,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    require_phase(state, CampaignPhase::Activating, event)?;
    state.require_no_open_effects()?;
    let Some(ActivationState::Awaiting { revision }) = state.activation else {
        return Err(CampaignTransitionError::IllegalTransition {
            phase: state.phase,
            event: event.name(),
        });
    };
    if outcome.requested_revision() != revision {
        return Err(CampaignTransitionError::RevisionMismatch);
    }
    match outcome {
        ActivationOutcome::Activated { revision, receipt } => {
            state.activation = Some(ActivationState::Activated { revision, receipt });
            state.monitoring = Some(MonitoringState::Active {
                revision,
                activation: receipt,
            });
            state.phase = CampaignPhase::Monitoring;
        }
        ActivationOutcome::Superseded {
            requested_revision,
            active_revision,
            receipt,
        } => {
            state.require_no_open_effects()?;
            state.activation = Some(ActivationState::Superseded {
                requested_revision,
                active_revision,
                receipt,
            });
            state.phase = CampaignPhase::Terminal;
            state.terminal = Some(TerminalState::Superseded(outcome));
        }
    }
    Ok(())
}

fn record_monitoring(
    state: &mut CampaignState,
    outcome: MonitoringOutcome,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    require_phase(state, CampaignPhase::Monitoring, event)?;
    let Some(MonitoringState::Active { revision, .. }) = state.monitoring else {
        return Err(CampaignTransitionError::IllegalTransition {
            phase: state.phase,
            event: event.name(),
        });
    };
    if outcome.monitored_revision() != revision {
        return Err(CampaignTransitionError::RevisionMismatch);
    }
    state.require_no_open_effects()?;
    state.monitoring = Some(MonitoringState::Completed(outcome));
    state.phase = CampaignPhase::Terminal;
    state.terminal = Some(TerminalState::MonitoringCompleted(outcome));
    Ok(())
}

fn pause(
    state: &mut CampaignState,
    reason: CampaignNote,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    if state.phase == CampaignPhase::Paused {
        return Err(CampaignTransitionError::IllegalTransition {
            phase: state.phase,
            event: event.name(),
        });
    }
    state.pause = Some(PauseState {
        previous_phase: state.phase,
        reason,
    });
    state.phase = CampaignPhase::Paused;
    Ok(())
}

fn resume(state: &mut CampaignState, event: &CampaignEvent) -> Result<(), CampaignTransitionError> {
    require_phase(state, CampaignPhase::Paused, event)?;
    let pause = state
        .pause
        .take()
        .ok_or(CampaignValidationError::InconsistentState(
            "paused campaign has no pause record",
        ))?;
    state.phase = pause.previous_phase;
    Ok(())
}

fn intend_effect(
    state: &mut CampaignState,
    intent: EffectIntent,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    intent.validate()?;
    if !intent.kind.allowed_in(state.phase) {
        return Err(CampaignTransitionError::IllegalTransition {
            phase: state.phase,
            event: event.name(),
        });
    }
    let expected_revision = state
        .revision
        .checked_add(1)
        .ok_or(CampaignTransitionError::RevisionExhausted)?;
    if intent.campaign != state.id || intent.transition_revision != expected_revision {
        return Err(CampaignTransitionError::RevisionMismatch);
    }
    if state.effects.contains_key(&intent.id) {
        return Err(CampaignTransitionError::DuplicateEffect { effect: intent.id });
    }
    let effects_intended = state
        .usage
        .effects_intended
        .checked_add(1)
        .ok_or(CampaignTransitionError::BudgetExceeded)?;
    let reserved = state
        .usage
        .reserved
        .checked_add(intent.budget.resources)
        .ok_or(CampaignTransitionError::BudgetExceeded)?;
    let total = state
        .usage
        .used
        .checked_add(reserved)
        .ok_or(CampaignTransitionError::BudgetExceeded)?;
    if effects_intended > state.budget.max_effects || !total.fits_within(state.budget.resources) {
        return Err(CampaignTransitionError::BudgetExceeded);
    }
    state.usage.effects_intended = effects_intended;
    state.usage.reserved = reserved;
    state.effects.insert(
        intent.id,
        EffectState {
            intent,
            lifecycle: EffectLifecycle::Intended {
                lease_epoch: LeaseEpoch::initial(),
            },
        },
    );
    Ok(())
}

fn lease_effect(
    state: &mut CampaignState,
    effect: EffectId,
    epoch: LeaseEpoch,
    event: &CampaignEvent,
) -> Result<(), CampaignTransitionError> {
    epoch.validate()?;
    let effect_state = state
        .effects
        .get_mut(&effect)
        .ok_or(CampaignTransitionError::UnknownEffect { effect })?;
    if !effect_state.intent.kind.allowed_in(state.phase) {
        return Err(CampaignTransitionError::IllegalTransition {
            phase: state.phase,
            event: event.name(),
        });
    }
    let expected = match effect_state.lifecycle {
        EffectLifecycle::Intended { lease_epoch }
        | EffectLifecycle::Reconciled { lease_epoch, .. } => lease_epoch,
        EffectLifecycle::Leased { .. } => {
            return Err(CampaignTransitionError::ReconciliationRequired { effect });
        }
        EffectLifecycle::Settled { .. } => {
            return Err(CampaignTransitionError::EffectAlreadySettled { effect });
        }
    };
    check_epoch(effect, expected, epoch)?;
    effect_state.lifecycle = EffectLifecycle::Leased { epoch };
    Ok(())
}

fn reconcile_effect(
    state: &mut CampaignState,
    effect: EffectId,
    leased_epoch: LeaseEpoch,
    next_epoch: LeaseEpoch,
    outcome: EffectReconciliation,
) -> Result<(), CampaignTransitionError> {
    leased_epoch.validate()?;
    next_epoch.validate()?;
    let effect_state = state
        .effects
        .get(&effect)
        .ok_or(CampaignTransitionError::UnknownEffect { effect })?;
    let current = match effect_state.lifecycle {
        EffectLifecycle::Leased { epoch } => epoch,
        EffectLifecycle::Settled { .. } => {
            return Err(CampaignTransitionError::EffectAlreadySettled { effect });
        }
        _ => return Err(CampaignTransitionError::EffectNotLeased { effect }),
    };
    check_epoch(effect, current, leased_epoch)?;
    let expected_next = current.next()?;
    check_epoch(effect, expected_next, next_epoch)?;

    match outcome {
        EffectReconciliation::RetryAuthorized { evidence } => {
            let effect_state = state
                .effects
                .get_mut(&effect)
                .ok_or(CampaignTransitionError::UnknownEffect { effect })?;
            effect_state.lifecycle = EffectLifecycle::Reconciled {
                lease_epoch: next_epoch,
                evidence,
            };
            Ok(())
        }
        EffectReconciliation::Recovered { receipt } => {
            settle_at_reconciliation(state, effect, next_epoch, receipt)
        }
        EffectReconciliation::FencedInfrastructureUnknown { receipt } => {
            if !matches!(receipt.outcome, EffectOutcome::InfrastructureUnknown { .. }) {
                return Err(CampaignTransitionError::FenceRequiresInfrastructureUnknown);
            }
            settle_at_reconciliation(state, effect, next_epoch, receipt)
        }
    }
}

fn settle_effect(
    state: &mut CampaignState,
    effect: EffectId,
    epoch: LeaseEpoch,
    receipt: EffectReceipt,
) -> Result<(), CampaignTransitionError> {
    epoch.validate()?;
    let effect_state = state
        .effects
        .get(&effect)
        .ok_or(CampaignTransitionError::UnknownEffect { effect })?;
    let current = match effect_state.lifecycle {
        EffectLifecycle::Leased { epoch } => epoch,
        EffectLifecycle::Settled { .. } => {
            return Err(CampaignTransitionError::EffectAlreadySettled { effect });
        }
        EffectLifecycle::Reconciled { lease_epoch, .. }
        | EffectLifecycle::Intended { lease_epoch } => {
            check_epoch(effect, lease_epoch, epoch)?;
            return Err(CampaignTransitionError::EffectNotLeased { effect });
        }
    };
    check_epoch(effect, current, epoch)?;
    settle_at_reconciliation(state, effect, epoch, receipt)
}

fn settle_at_reconciliation(
    state: &mut CampaignState,
    effect: EffectId,
    epoch: LeaseEpoch,
    receipt: EffectReceipt,
) -> Result<(), CampaignTransitionError> {
    let budget = state
        .effects
        .get(&effect)
        .ok_or(CampaignTransitionError::UnknownEffect { effect })?
        .intent
        .budget;
    let charged = receipt.charged_usage(budget)?;
    state.usage.reserved = state.usage.reserved.checked_sub(budget.resources).ok_or(
        CampaignValidationError::InconsistentState(
            "effect reservation exceeds campaign reservation",
        ),
    )?;
    state.usage.used = state
        .usage
        .used
        .checked_add(charged)
        .ok_or(CampaignTransitionError::BudgetExceeded)?;
    let effect_state = state
        .effects
        .get_mut(&effect)
        .ok_or(CampaignTransitionError::UnknownEffect { effect })?;
    effect_state.lifecycle = EffectLifecycle::Settled { epoch, receipt };
    Ok(())
}

fn check_epoch(
    effect: EffectId,
    expected: LeaseEpoch,
    actual: LeaseEpoch,
) -> Result<(), CampaignTransitionError> {
    if actual == expected {
        return Ok(());
    }
    if actual.value() < expected.value() {
        return Err(CampaignTransitionError::StaleLeaseEpoch {
            effect,
            expected: expected.value(),
            actual: actual.value(),
        });
    }
    Err(CampaignTransitionError::InvalidLeaseEpoch {
        effect,
        expected: expected.value(),
        actual: actual.value(),
    })
}

fn terminal_effect_replay(
    state: &CampaignState,
    event: &CampaignEvent,
) -> Result<bool, CampaignTransitionError> {
    let (effect, epoch, receipt) = match event {
        CampaignEvent::EffectSettled {
            effect,
            epoch,
            receipt,
        } => (*effect, *epoch, *receipt),
        CampaignEvent::EffectReconciled {
            effect,
            leased_epoch,
            next_epoch,
            outcome,
        } => {
            leased_epoch.validate()?;
            next_epoch.validate()?;
            check_epoch(*effect, leased_epoch.next()?, *next_epoch)?;
            let receipt = match outcome {
                EffectReconciliation::Recovered { receipt } => *receipt,
                EffectReconciliation::FencedInfrastructureUnknown { receipt } => {
                    if !matches!(receipt.outcome, EffectOutcome::InfrastructureUnknown { .. }) {
                        return Err(CampaignTransitionError::FenceRequiresInfrastructureUnknown);
                    }
                    *receipt
                }
                EffectReconciliation::RetryAuthorized { .. } => return Ok(false),
            };
            (*effect, *next_epoch, receipt)
        }
        _ => return Ok(false),
    };
    let Some(effect_state) = state.effects.get(&effect) else {
        return Ok(false);
    };
    let EffectLifecycle::Settled {
        epoch: stored_epoch,
        receipt: stored_receipt,
    } = effect_state.lifecycle
    else {
        return Ok(false);
    };
    if stored_epoch == epoch && stored_receipt == receipt {
        return Ok(true);
    }
    Err(CampaignTransitionError::ConflictingTerminalReceipt { effect })
}

fn validate_revision(revision: u64, field: &'static str) -> Result<(), CampaignValidationError> {
    if revision == 0 || revision > MAX_STORED_NUMBER {
        return Err(CampaignValidationError::NumberOutOfRange { field });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn digest(value: u8) -> Digest {
        Digest::of(&[value])
    }

    fn started() -> CampaignEvent {
        CampaignEvent::Started {
            campaign: CampaignId::from_uuid(Uuid::from_u128(1)),
            cohort: CohortId::from_uuid(Uuid::from_u128(2)),
            base_revision: digest(3),
            policy: PolicyIdentity::from_digest(digest(4)),
            budget: CampaignBudget::new(32, ResourceUsage::new(100, 1_000, 10_000)).unwrap(),
        }
    }

    fn id<T>(value: u8, make: impl FnOnce(Digest) -> T) -> T {
        make(digest(value))
    }

    fn state_in_mining() -> (CampaignState, RoundId) {
        let state = apply_campaign(None, &started()).unwrap();
        let round = id(10, RoundId::from_digest);
        let state = state.apply(&CampaignEvent::RoundStarted { round }).unwrap();
        (state, round)
    }

    fn state_in_scoring() -> (CampaignState, RoundId, CandidateId) {
        let (state, round) = state_in_mining();
        let state = state
            .apply(&CampaignEvent::MiningCompleted {
                round,
                result: id(11, MiningResultId::from_digest),
            })
            .unwrap();
        let candidate = id(12, CandidateId::from_digest);
        let state = state
            .apply(&CampaignEvent::CandidateProposed {
                round,
                candidate,
                parent: CandidateParent::BaseRevision(state.base_revision()),
                proposal: id(13, ProposalId::from_digest),
            })
            .unwrap()
            .apply(&CampaignEvent::ProposalsCompleted { round })
            .unwrap()
            .apply(&CampaignEvent::CandidateTrialRecorded {
                round,
                candidate,
                trial: id(14, TrialResultId::from_digest),
            })
            .unwrap()
            .apply(&CampaignEvent::TrialsCompleted { round })
            .unwrap()
            .apply(&CampaignEvent::CandidateScoreRecorded {
                round,
                candidate,
                score: id(15, ScoreResultId::from_digest),
            })
            .unwrap();
        (state, round, candidate)
    }

    #[test]
    fn transition_table_reaches_verified_activation_and_monitoring_terminal() {
        let (state, round, candidate) = state_in_scoring();
        let revision = digest(20);
        let state = state
            .apply(&CampaignEvent::RoundVerdictRecorded {
                round,
                verdict: RoundVerdict::Compose {
                    candidate,
                    basis: id(16, RoundVerdictId::from_digest),
                },
            })
            .unwrap()
            .apply(&CampaignEvent::CompositionRecorded {
                round,
                candidate,
                composition: id(17, CompositionId::from_digest),
                revision,
            })
            .unwrap()
            .apply(&CampaignEvent::FinalVerdictRecorded {
                verdict: FinalVerdict::Verified {
                    candidate,
                    revision,
                    report: id(18, AuditReportId::from_digest),
                },
            })
            .unwrap()
            .apply(&CampaignEvent::ApprovalRecorded {
                decision: ApprovalDecision::Approved {
                    id: id(19, ApprovalId::from_digest),
                    revision,
                },
            })
            .unwrap();
        assert_eq!(state.phase(), CampaignPhase::Activating);

        let activation = id(21, ActivationReceiptId::from_digest);
        let state = state
            .apply(&CampaignEvent::ActivationRecorded {
                outcome: ActivationOutcome::Activated {
                    revision,
                    receipt: activation,
                },
            })
            .unwrap()
            .apply(&CampaignEvent::MonitoringCompleted {
                outcome: MonitoringOutcome::Healthy {
                    revision,
                    report: id(22, MonitoringReportId::from_digest),
                },
            })
            .unwrap();
        assert_eq!(state.phase(), CampaignPhase::Terminal);
        assert!(matches!(
            state.terminal(),
            Some(TerminalState::MonitoringCompleted(
                MonitoringOutcome::Healthy { .. }
            ))
        ));
    }

    #[test]
    fn transition_table_rejects_wrong_phase_and_terminal_mutation() {
        let state = apply_campaign(None, &started()).unwrap();
        let round = id(30, RoundId::from_digest);
        assert!(matches!(
            state.apply(&CampaignEvent::MiningCompleted {
                round,
                result: id(31, MiningResultId::from_digest),
            }),
            Err(CampaignTransitionError::IllegalTransition {
                phase: CampaignPhase::Ready,
                ..
            })
        ));

        let (state, round, _) = state_in_scoring();
        let state = state
            .apply(&CampaignEvent::RoundVerdictRecorded {
                round,
                verdict: RoundVerdict::NoUpdate {
                    basis: id(32, RoundVerdictId::from_digest),
                },
            })
            .unwrap();
        assert!(matches!(
            state.apply(&CampaignEvent::Paused {
                reason: CampaignNote::new("too late").unwrap(),
            }),
            Err(CampaignTransitionError::TerminalMutation { .. })
        ));
    }

    #[test]
    fn effect_identity_binds_campaign_revision_kind_and_work() {
        let campaign = CampaignId::from_uuid(Uuid::from_u128(40));
        let other_campaign = CampaignId::from_uuid(Uuid::from_u128(41));
        let work = id(42, EffectWorkId::from_digest);
        let effect = EffectId::derive(campaign, 7, EffectKind::Mine, work);
        assert_eq!(
            effect,
            EffectId::derive(campaign, 7, EffectKind::Mine, work)
        );
        assert_ne!(
            effect,
            EffectId::derive(campaign, 8, EffectKind::Mine, work)
        );
        assert_ne!(
            effect,
            EffectId::derive(campaign, 7, EffectKind::Trial, work)
        );
        assert_ne!(
            effect,
            EffectId::derive(other_campaign, 7, EffectKind::Mine, work)
        );
        assert_ne!(
            effect,
            EffectId::derive(
                campaign,
                7,
                EffectKind::Mine,
                id(43, EffectWorkId::from_digest)
            )
        );
    }

    #[test]
    fn uncertain_effect_requires_reconciliation_and_fences_stale_worker() {
        let (state, _) = state_in_mining();
        let intent = EffectIntent::new(
            state.id(),
            state.revision() + 1,
            EffectKind::Mine,
            id(50, EffectWorkId::from_digest),
            EffectBudget::new(ResourceUsage::new(10, 100, 1_000)).unwrap(),
        )
        .unwrap();
        let effect = intent.id();
        let state = state
            .apply(&CampaignEvent::EffectIntended { intent })
            .unwrap()
            .apply(&CampaignEvent::EffectLeased {
                effect,
                epoch: LeaseEpoch::initial(),
            })
            .unwrap();
        assert_eq!(
            state.apply(&CampaignEvent::EffectLeased {
                effect,
                epoch: LeaseEpoch::initial(),
            }),
            Err(CampaignTransitionError::ReconciliationRequired { effect })
        );

        let epoch_two = LeaseEpoch::new(2).unwrap();
        let state = state
            .apply(&CampaignEvent::EffectReconciled {
                effect,
                leased_epoch: LeaseEpoch::initial(),
                next_epoch: epoch_two,
                outcome: EffectReconciliation::RetryAuthorized {
                    evidence: id(51, ReconciliationId::from_digest),
                },
            })
            .unwrap();
        let receipt = EffectReceipt {
            id: id(52, EffectReceiptId::from_digest),
            outcome: EffectOutcome::Succeeded {
                output: id(53, EffectOutputId::from_digest),
            },
            accounting: EffectAccounting::Known(ResourceUsage::new(2, 20, 200)),
        };
        assert!(matches!(
            state.apply(&CampaignEvent::EffectSettled {
                effect,
                epoch: LeaseEpoch::initial(),
                receipt,
            }),
            Err(CampaignTransitionError::StaleLeaseEpoch { .. })
        ));
        let state = state
            .apply(&CampaignEvent::EffectLeased {
                effect,
                epoch: epoch_two,
            })
            .unwrap();
        assert!(matches!(
            state.apply(&CampaignEvent::EffectSettled {
                effect,
                epoch: LeaseEpoch::initial(),
                receipt,
            }),
            Err(CampaignTransitionError::StaleLeaseEpoch { .. })
        ));

        let settled = state
            .apply(&CampaignEvent::EffectSettled {
                effect,
                epoch: epoch_two,
                receipt,
            })
            .unwrap();
        let replayed = settled
            .apply(&CampaignEvent::EffectSettled {
                effect,
                epoch: epoch_two,
                receipt,
            })
            .unwrap();
        assert_eq!(replayed, settled);
        assert_eq!(replayed.revision(), settled.revision());

        let conflicting = EffectReceipt {
            id: id(54, EffectReceiptId::from_digest),
            ..receipt
        };
        assert_eq!(
            settled.apply(&CampaignEvent::EffectSettled {
                effect,
                epoch: epoch_two,
                receipt: conflicting,
            }),
            Err(CampaignTransitionError::ConflictingTerminalReceipt { effect })
        );
    }

    #[test]
    fn open_effect_blocks_stage_advance_and_cannot_be_newly_leased_while_paused() {
        let (state, round) = state_in_mining();
        let intent = EffectIntent::new(
            state.id(),
            state.revision() + 1,
            EffectKind::Mine,
            id(55, EffectWorkId::from_digest),
            EffectBudget::new(ResourceUsage::new(1, 1, 1)).unwrap(),
        )
        .unwrap();
        let effect = intent.id();
        let state = state
            .apply(&CampaignEvent::EffectIntended { intent })
            .unwrap();
        assert_eq!(
            state.apply(&CampaignEvent::MiningCompleted {
                round,
                result: id(56, MiningResultId::from_digest),
            }),
            Err(CampaignTransitionError::OpenEffects)
        );

        let state = state
            .apply(&CampaignEvent::Paused {
                reason: CampaignNote::new("operator investigation").unwrap(),
            })
            .unwrap();
        assert!(matches!(
            state.apply(&CampaignEvent::EffectLeased {
                effect,
                epoch: LeaseEpoch::initial(),
            }),
            Err(CampaignTransitionError::IllegalTransition {
                phase: CampaignPhase::Paused,
                ..
            })
        ));
    }

    #[test]
    fn fenced_unknown_receipt_charges_the_full_reservation() {
        let (state, _) = state_in_mining();
        let reservation = ResourceUsage::new(10, 100, 1_000);
        let intent = EffectIntent::new(
            state.id(),
            state.revision() + 1,
            EffectKind::Mine,
            id(60, EffectWorkId::from_digest),
            EffectBudget::new(reservation).unwrap(),
        )
        .unwrap();
        let effect = intent.id();
        let state = state
            .apply(&CampaignEvent::EffectIntended { intent })
            .unwrap()
            .apply(&CampaignEvent::EffectLeased {
                effect,
                epoch: LeaseEpoch::initial(),
            })
            .unwrap()
            .apply(&CampaignEvent::EffectReconciled {
                effect,
                leased_epoch: LeaseEpoch::initial(),
                next_epoch: LeaseEpoch::new(2).unwrap(),
                outcome: EffectReconciliation::FencedInfrastructureUnknown {
                    receipt: EffectReceipt {
                        id: id(61, EffectReceiptId::from_digest),
                        outcome: EffectOutcome::InfrastructureUnknown {
                            uncertainty: id(62, EffectUncertaintyId::from_digest),
                        },
                        accounting: EffectAccounting::ReservationCharged,
                    },
                },
            })
            .unwrap();
        assert_eq!(state.usage().used, reservation);
        assert_eq!(state.usage().reserved, ResourceUsage::default());
    }

    #[test]
    fn serde_and_validation_reject_unbounded_values() {
        assert!(CampaignNote::new(" ").is_err());
        assert!(CampaignNote::new("x".repeat(MAX_NOTE_BYTES + 1)).is_err());
        assert!(CampaignBudget::new(MAX_EFFECTS + 1, ResourceUsage::default()).is_err());

        let state = apply_campaign(None, &started()).unwrap();
        let encoded = serde_json::to_vec(&state).unwrap();
        let decoded: CampaignState = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, state);
        decoded.validate().unwrap();
    }
}
