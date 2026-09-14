//! Authoritative task state and evidence-based completion, independent of inference and UI.

pub mod admission;
pub mod artifacts;
pub mod auxiliary;
pub mod capabilities;
pub mod completion;
pub mod context;
pub mod contract;
pub mod controller;
pub mod delivery;
pub mod digest;
mod evolution;
pub mod feedback;
pub mod import;
pub mod inference;
pub mod input;
#[cfg(unix)]
pub mod ipc;
pub mod manual;
pub mod review;
pub mod runtime;
pub mod session;
pub mod state;
pub mod store;
pub mod submission;
pub mod verification;
pub mod workspace;

pub use digest::Digest;
pub use evolution::{
    ActivationOutcome, ActivationReceiptId, ActivationState, AdaptivePromotionDataset,
    AdaptivePromotionRef, AdaptivePromotionReservation, AdaptiveScoreReport, AdaptiveTrialEvidence,
    AdaptiveVerdict, ApprovalDecision, ApprovalId, ApprovalState, AuditEpochId, AuditEpochStatus,
    AuditReportId, BaselineReason, BehavioralFailureReason, BehavioralTrialOutcome, BlockEffect,
    BoundedProposal, CampaignBudget, CampaignEvent, CampaignId, CampaignNote, CampaignPhase,
    CampaignState, CampaignTransitionError, CampaignUsage, CampaignValidationError, CandidateId,
    CandidateLabel, CandidateParent, CandidateStage, CandidateState, CaseEffect, CaseIdentity,
    CausalStatus, Channel, ClassifierReceiptId, CohortId, CohortLedger, ComposedHarness,
    CompositeField, CompositeOutcome, CompositePlan, CompositeStage, CompositeState,
    CompositionChild, CompositionError, CompositionFailure, CompositionFailureReason,
    CompositionFallback, CompositionId, CompositionInput, DecisionCoordinates, DiversityDimension,
    EffectAccounting, EffectBudget, EffectFailureId, EffectId, EffectIntent, EffectKind,
    EffectLifecycle, EffectOutcome, EffectOutputId, EffectReceipt, EffectReceiptId,
    EffectReconciliation, EffectState, EffectUncertaintyId, EffectWorkId, EnvironmentIdentity,
    EvaluationCase, EvaluationCohortSpec, EvaluatorIdentity, ExactEffect, ExactRatio, FactSource,
    FailureCluster, FailureMechanism, FailureSignature, FinalAuditAccess, FinalAuditRef,
    FinalAuditReservation, FinalVerdict, FrozenScoringPolicy, GateEvidence, GateName, GateResult,
    GateStatus, HarnessBinding, HarnessProvenance, IndependentBlock, IndependentBlockId,
    InfrastructureUnknownReason, InputCommitment, IsolatedRunReceipt, IsolatedRunReceiptId,
    IsolationInstanceId, LeaseEpoch, LedgerLimit, LedgerStatus, ManifestError, MechanismHypothesis,
    MechanismSource, MetricKind, MetricName, MetricPolicy, MetricScore, MetricStatistic,
    MiningBundleRoot, MiningError, MiningEvidenceBundle, MiningEvidenceId, MiningEvidenceRef,
    MiningEvidenceReservation, MiningFailureEvidence, MiningLimits, MiningObservation,
    MiningObservationId, MiningPassEvidence, MiningResultId, ModelIdentity, MonitoringOutcome,
    MonitoringReportId, MonitoringState, MultiplicityFamily, PairedInfrastructureUnknown,
    PairedTrialEvidence, PairedTrialReceipt, PairedTrialReceiptId, PartitionCommitment,
    PartitionCommitments, PauseState, PolicyIdentity, ProposalAttempt, ProposalBatch,
    ProposalBatchRoot, ProposalError, ProposalId, ProposalIntent, ProposalIntentId,
    ProposalProviderReceiptId, ProposalRequest, ProposalRequestRoot, ProtocolIdentity,
    ProviderUnknownReceipt, ReconciliationId, RedactionSummary, ResourceUsage, RollbackReceiptId,
    RoundId, RoundState, RoundVerdict, RoundVerdictId, SanitizedEvidenceText, ScoreResultId,
    ScoringError, SealedArtifactRef, SelectedHarness, StratumName, StratumPolicy, TargetProfile,
    TaskIdentity, TaskProfileIdentity, TerminalCause, TerminalState, TerminalTrialOutcome,
    TrialContext, TrialError, TrialIsolationProfile, TrialKey, TrialKeyId, TrialLedgerRole,
    TrialPairId, TrialPairSpec, TrialPartition, TrialResultId, TrialRunAssignment,
    TrialRuntimeIdentity, TrialSide, TrialTransportCapability, TrialTransportTerminal,
    ValidatedHarnessRevision, VerifiedComposition, VerifiedFailureFact, VerifierReceiptId,
    apply_campaign, classify_paired_trial, compose_candidate, compose_candidates, mine_evidence,
    sanitize_untrusted_text, validate_proposal_batch,
};
pub use store::{Store, StoreError};
