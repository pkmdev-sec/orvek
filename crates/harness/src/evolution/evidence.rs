use super::{AuditEpochId, CaseIdentity, CohortId, IndependentBlockId};
use crate::Digest;
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;
use uuid::Uuid;

macro_rules! mining_digest_id {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
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

mining_digest_id!(MiningEvidenceId);
mining_digest_id!(VerifierReceiptId);
mining_digest_id!(ClassifierReceiptId);
mining_digest_id!(MiningObservationId);
mining_digest_id!(MiningBundleRoot);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCause {
    BudgetExhaustion,
    CandidateCrash,
    CompletionRejected,
    ModelRefusal,
    ProtocolTimeout,
    ToolFailure,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CausalStatus {
    CandidateAttributable,
    HarnessAddressable,
    Mixed,
    Unresolved,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureMechanism {
    CompletionDiscipline,
    ContextHandling,
    InstructionFollowing,
    RecoveryStrategy,
    ToolSelection,
    VerificationStrategy,
    Unclassified,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedFailureFact {
    receipt: VerifierReceiptId,
    terminal_cause: TerminalCause,
    causal_status: CausalStatus,
}

impl VerifiedFailureFact {
    pub const fn new(
        receipt: VerifierReceiptId,
        terminal_cause: TerminalCause,
        causal_status: CausalStatus,
    ) -> Self {
        Self {
            receipt,
            terminal_cause,
            causal_status,
        }
    }

    pub const fn receipt(self) -> VerifierReceiptId {
        self.receipt
    }

    pub const fn terminal_cause(self) -> TerminalCause {
        self.terminal_cause
    }

    pub const fn causal_status(self) -> CausalStatus {
        self.causal_status
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MechanismHypothesis {
    receipt: ClassifierReceiptId,
    mechanism: FailureMechanism,
}

impl MechanismHypothesis {
    pub fn new(
        receipt: ClassifierReceiptId,
        mechanism: FailureMechanism,
    ) -> Result<Self, MiningError> {
        if mechanism == FailureMechanism::Unclassified {
            return Err(MiningError::UnclassifiedHypothesis);
        }
        Ok(Self { receipt, mechanism })
    }

    pub const fn receipt(self) -> ClassifierReceiptId {
        self.receipt
    }

    pub const fn mechanism(self) -> FailureMechanism {
        self.mechanism
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionSummary {
    protected_terms: u32,
    sensitive_values: u32,
    control_characters: u32,
    truncated: bool,
}

impl RedactionSummary {
    pub const fn protected_terms(self) -> u32 {
        self.protected_terms
    }

    pub const fn sensitive_values(self) -> u32 {
        self.sensitive_values
    }

    pub const fn control_characters(self) -> u32 {
        self.control_characters
    }

    pub const fn truncated(self) -> bool {
        self.truncated
    }

    pub(crate) const fn new(
        protected_terms: u32,
        sensitive_values: u32,
        control_characters: u32,
        truncated: bool,
    ) -> Self {
        Self {
            protected_terms,
            sensitive_values,
            control_characters,
            truncated,
        }
    }
}

/// Bounded, normalized trace text that remains data rather than an instruction channel.
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SanitizedEvidenceText {
    source: MiningEvidenceId,
    text: String,
    redactions: RedactionSummary,
}

impl SanitizedEvidenceText {
    pub const fn source(&self) -> MiningEvidenceId {
        self.source
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub const fn redactions(&self) -> RedactionSummary {
        self.redactions
    }

    pub(crate) fn new(
        source: MiningEvidenceId,
        text: String,
        redactions: RedactionSummary,
    ) -> Self {
        Self {
            source,
            text,
            redactions,
        }
    }
}

impl fmt::Debug for SanitizedEvidenceText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SanitizedEvidenceText")
            .field("source", &self.source)
            .field("bytes", &self.text.len())
            .field("redactions", &self.redactions)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MiningFailureEvidence {
    id: MiningObservationId,
    evidence: MiningEvidenceId,
    case: CaseIdentity,
    block: IndependentBlockId,
    fact: VerifiedFailureFact,
    hypothesis: Option<MechanismHypothesis>,
    detail: SanitizedEvidenceText,
}

impl MiningFailureEvidence {
    pub fn new(
        evidence: MiningEvidenceId,
        case: CaseIdentity,
        block: IndependentBlockId,
        fact: VerifiedFailureFact,
        hypothesis: Option<MechanismHypothesis>,
        detail: SanitizedEvidenceText,
    ) -> Result<Self, MiningError> {
        if detail.source() != evidence {
            return Err(MiningError::EvidenceSourceMismatch);
        }
        let identity = MiningFailureIdentity {
            evidence,
            case,
            block,
            fact,
            hypothesis,
            detail: &detail,
        };
        let id = MiningObservationId::from_digest(
            Digest::of_value(&identity).map_err(MiningError::Canonicalization)?,
        );
        Ok(Self {
            id,
            evidence,
            case,
            block,
            fact,
            hypothesis,
            detail,
        })
    }

    pub const fn id(&self) -> MiningObservationId {
        self.id
    }

    pub const fn evidence(&self) -> MiningEvidenceId {
        self.evidence
    }

    pub const fn case(&self) -> CaseIdentity {
        self.case
    }

    pub const fn block(&self) -> IndependentBlockId {
        self.block
    }

    pub const fn fact(&self) -> VerifiedFailureFact {
        self.fact
    }

    pub const fn hypothesis(&self) -> Option<MechanismHypothesis> {
        self.hypothesis
    }

    pub const fn detail(&self) -> &SanitizedEvidenceText {
        &self.detail
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct MiningFailureIdentity<'a> {
    evidence: MiningEvidenceId,
    case: CaseIdentity,
    block: IndependentBlockId,
    fact: VerifiedFailureFact,
    hypothesis: Option<MechanismHypothesis>,
    detail: &'a SanitizedEvidenceText,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MiningPassEvidence {
    id: MiningObservationId,
    evidence: MiningEvidenceId,
    case: CaseIdentity,
    block: IndependentBlockId,
    receipt: VerifierReceiptId,
    summary: SanitizedEvidenceText,
}

impl MiningPassEvidence {
    pub fn new(
        evidence: MiningEvidenceId,
        case: CaseIdentity,
        block: IndependentBlockId,
        receipt: VerifierReceiptId,
        summary: SanitizedEvidenceText,
    ) -> Result<Self, MiningError> {
        if summary.source() != evidence {
            return Err(MiningError::EvidenceSourceMismatch);
        }
        let identity = MiningPassIdentity {
            evidence,
            case,
            block,
            receipt,
            summary: &summary,
        };
        let id = MiningObservationId::from_digest(
            Digest::of_value(&identity).map_err(MiningError::Canonicalization)?,
        );
        Ok(Self {
            id,
            evidence,
            case,
            block,
            receipt,
            summary,
        })
    }

    pub const fn id(&self) -> MiningObservationId {
        self.id
    }

    pub const fn evidence(&self) -> MiningEvidenceId {
        self.evidence
    }

    pub const fn case(&self) -> CaseIdentity {
        self.case
    }

    pub const fn block(&self) -> IndependentBlockId {
        self.block
    }

    pub const fn receipt(&self) -> VerifierReceiptId {
        self.receipt
    }

    pub const fn summary(&self) -> &SanitizedEvidenceText {
        &self.summary
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct MiningPassIdentity<'a> {
    evidence: MiningEvidenceId,
    case: CaseIdentity,
    block: IndependentBlockId,
    receipt: VerifierReceiptId,
    summary: &'a SanitizedEvidenceText,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MiningObservation {
    Failure(MiningFailureEvidence),
    Pass(MiningPassEvidence),
}

impl MiningObservation {
    pub const fn id(&self) -> MiningObservationId {
        match self {
            Self::Failure(failure) => failure.id(),
            Self::Pass(pass) => pass.id(),
        }
    }
}

impl From<MiningFailureEvidence> for MiningObservation {
    fn from(value: MiningFailureEvidence) -> Self {
        Self::Failure(value)
    }
}

impl From<MiningPassEvidence> for MiningObservation {
    fn from(value: MiningPassEvidence) -> Self {
        Self::Pass(value)
    }
}

#[derive(Debug, Error)]
pub enum MiningError {
    #[error("mining evidence text must not be empty")]
    EmptyText,
    #[error("raw mining evidence exceeds the compiled maximum of {maximum} bytes")]
    RawTextTooLarge { maximum: usize },
    #[error("sanitization accepts at most {maximum} protected terms")]
    TooManyProtectedTerms { maximum: usize },
    #[error("a protected term exceeds the compiled maximum of {maximum} bytes")]
    ProtectedTermTooLarge { maximum: usize },
    #[error("sanitized text and observation refer to different mining evidence")]
    EvidenceSourceMismatch,
    #[error("a classifier hypothesis must name a classified mechanism")]
    UnclassifiedHypothesis,
    #[error("mining limit {field} must be between 1 and {maximum}")]
    InvalidLimit { field: &'static str, maximum: usize },
    #[error("mining input contains {actual} observations; the maximum is {maximum}")]
    TooManyObservations { actual: usize, maximum: usize },
    #[error("mining input produced {actual} clusters; the maximum is {maximum}")]
    TooManyClusters { actual: usize, maximum: usize },
    #[error("one failure cluster contains {actual} observations; the maximum is {maximum}")]
    ClusterTooLarge { actual: usize, maximum: usize },
    #[error("mining evidence could not be canonicalized")]
    Canonicalization(#[source] serde_json::Error),
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct SealedArtifactRef {
    pub(crate) digest: Digest,
    pub(crate) cohort: CohortId,
    pub(crate) purpose: EvidencePurpose,
}

impl SealedArtifactRef {
    pub const fn cohort(self) -> CohortId {
        self.cohort
    }

    pub(crate) const fn registered(
        digest: Digest,
        cohort: CohortId,
        purpose: EvidencePurpose,
    ) -> Self {
        Self {
            digest,
            cohort,
            purpose,
        }
    }
}

impl fmt::Debug for SealedArtifactRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SealedArtifactRef")
            .field("cohort", &self.cohort)
            .field("purpose", &self.purpose)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EvidencePurpose {
    Mining,
    AdaptivePromotion,
    FinalAudit,
}

impl EvidencePurpose {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Mining => "mining",
            Self::AdaptivePromotion => "adaptive_promotion",
            Self::FinalAudit => "final_audit",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "mining" => Ok(Self::Mining),
            "adaptive_promotion" => Ok(Self::AdaptivePromotion),
            "final_audit" => Ok(Self::FinalAudit),
            _ => Err("unknown evolution evidence purpose"),
        }
    }
}

macro_rules! evidence_ref {
    ($name:ident) => {
        #[derive(Clone, Copy, Eq, PartialEq)]
        pub struct $name(pub(crate) SealedArtifactRef);

        impl $name {
            pub const fn cohort(self) -> CohortId {
                self.0.cohort()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.0)
                    .finish()
            }
        }
    };
}

evidence_ref!(MiningEvidenceRef);
evidence_ref!(AdaptivePromotionRef);
evidence_ref!(FinalAuditRef);

#[derive(Debug)]
pub(crate) struct EvidenceReservation {
    pub(crate) id: Uuid,
    pub(crate) cohort: CohortId,
    pub(crate) max_bytes: u64,
    pub(crate) purpose: EvidencePurpose,
}

macro_rules! reservation {
    ($name:ident) => {
        pub struct $name(pub(crate) EvidenceReservation);

        impl $name {
            pub const fn cohort(&self) -> CohortId {
                self.0.cohort
            }

            pub const fn max_bytes(&self) -> u64 {
                self.0.max_bytes
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($name))
                    .field("cohort", &self.0.cohort)
                    .field("max_bytes", &self.0.max_bytes)
                    .finish_non_exhaustive()
            }
        }
    };
}

reservation!(MiningEvidenceReservation);
reservation!(AdaptivePromotionReservation);
reservation!(FinalAuditReservation);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalAuditAccess {
    pub epoch: AuditEpochId,
    pub evidence: FinalAuditRef,
}
