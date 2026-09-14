mod campaign;
mod composition;
mod evidence;
mod harness;
mod mining;
mod proposal;
mod registry;
mod statistics;
mod trial;

pub use campaign::{
    ActivationOutcome, ActivationReceiptId, ActivationState, ApprovalDecision, ApprovalId,
    ApprovalState, AuditReportId, CampaignBudget, CampaignEvent, CampaignNote, CampaignPhase,
    CampaignState, CampaignTransitionError, CampaignUsage, CampaignValidationError, CandidateId,
    CandidateParent, CandidateStage, CandidateState, CompositeOutcome, CompositeStage,
    CompositeState, CompositionId, EffectAccounting, EffectBudget, EffectFailureId, EffectId,
    EffectIntent, EffectKind, EffectLifecycle, EffectOutcome, EffectOutputId, EffectReceipt,
    EffectReceiptId, EffectReconciliation, EffectState, EffectUncertaintyId, EffectWorkId,
    FinalVerdict, LeaseEpoch, MiningResultId, MonitoringOutcome, MonitoringReportId,
    MonitoringState, PauseState, ProposalId, ReconciliationId, ResourceUsage, RollbackReceiptId,
    RoundId, RoundState, RoundVerdict, RoundVerdictId, ScoreResultId, TerminalState, TrialResultId,
    apply_campaign,
};
pub(crate) use composition::selected_composition_id;
pub use composition::{
    ComposedHarness, CompositeField, CompositePlan, CompositionChild, CompositionError,
    CompositionFailure, CompositionFailureReason, CompositionFallback, CompositionInput,
    SelectedHarness, VerifiedComposition, compose_candidate, compose_candidates,
};
pub use evidence::{
    AdaptivePromotionRef, AdaptivePromotionReservation, CausalStatus, ClassifierReceiptId,
    FailureMechanism, FinalAuditAccess, FinalAuditRef, FinalAuditReservation, MechanismHypothesis,
    MiningBundleRoot, MiningError, MiningEvidenceId, MiningEvidenceRef, MiningEvidenceReservation,
    MiningFailureEvidence, MiningObservation, MiningObservationId, MiningPassEvidence,
    RedactionSummary, SanitizedEvidenceText, SealedArtifactRef, TerminalCause, VerifiedFailureFact,
    VerifierReceiptId,
};
pub(crate) use evidence::{EvidencePurpose, EvidenceReservation};
pub use harness::{ManifestError, ValidatedHarnessRevision};
pub use mining::{
    FactSource, FailureCluster, FailureSignature, MechanismSource, MiningEvidenceBundle,
    MiningLimits, mine_evidence, sanitize_untrusted_text,
};
pub use proposal::{
    BoundedProposal, DiversityDimension, ProposalAttempt, ProposalBatch, ProposalBatchRoot,
    ProposalError, ProposalIntent, ProposalIntentId, ProposalProviderReceiptId, ProposalRequest,
    ProposalRequestRoot, ProviderUnknownReceipt, validate_proposal_batch,
};
pub(crate) use registry::LedgerDebit;
pub use registry::{
    AuditEpochId, AuditEpochStatus, BaselineReason, CampaignId, CaseIdentity, Channel, CohortId,
    CohortLedger, EnvironmentIdentity, EvaluationCase, EvaluationCohortSpec, EvaluatorIdentity,
    HarnessBinding, HarnessProvenance, IndependentBlock, IndependentBlockId, LedgerLimit,
    LedgerStatus, ModelIdentity, PartitionCommitment, PartitionCommitments, PolicyIdentity,
    ProtocolIdentity, TargetProfile, TaskIdentity, TaskProfileIdentity,
};
pub use statistics::{
    AdaptivePromotionDataset, AdaptiveScoreReport, AdaptiveTrialEvidence, AdaptiveVerdict,
    BlockEffect, CandidateLabel, CaseEffect, DecisionCoordinates, ExactEffect, ExactRatio,
    FrozenScoringPolicy, GateEvidence, GateName, GateResult, GateStatus, MetricKind, MetricPolicy,
    MetricStatistic, MultiplicityFamily, ScoringError, StratumName, StratumPolicy,
};
pub(crate) use statistics::{evaluate_adaptive, scoring_policy_digest};
pub use trial::{
    BehavioralFailureReason, BehavioralTrialOutcome, InfrastructureUnknownReason, InputCommitment,
    IsolatedRunReceipt, IsolatedRunReceiptId, IsolationInstanceId, MetricName, MetricScore,
    PairedInfrastructureUnknown, PairedTrialEvidence, PairedTrialReceipt, PairedTrialReceiptId,
    TerminalTrialOutcome, TrialContext, TrialError, TrialIsolationProfile, TrialKey, TrialKeyId,
    TrialLedgerRole, TrialPairId, TrialPairSpec, TrialPartition, TrialRunAssignment,
    TrialRuntimeIdentity, TrialSide, TrialTransportCapability, TrialTransportTerminal,
    classify_paired_trial,
};
