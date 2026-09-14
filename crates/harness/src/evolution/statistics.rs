use super::{
    BehavioralTrialOutcome, CampaignId, CandidateId, CaseIdentity, CohortId, EvaluationCohortSpec,
    IndependentBlockId, LedgerStatus, MetricName, MetricScore, PairedTrialEvidence, RoundId,
    ScoreResultId, TrialError, TrialLedgerRole, TrialPairSpec,
};
use crate::Digest;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};
use thiserror::Error;

const SCORE_SCALE: i128 = 1_000_000;
const ERROR_SCALE: u64 = 1_000_000_000;
const MAX_NAME_BYTES: usize = 128;
const MAX_LABEL_BYTES: usize = 256;
const MAX_EXACT_BLOCKS: u32 = 20;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GateName(String);

impl GateName {
    pub fn new(value: impl Into<String>) -> Result<Self, ScoringError> {
        bounded_text(value.into(), MAX_NAME_BYTES, ScoringError::InvalidGateName).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for GateName {
    type Error = ScoringError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<GateName> for String {
    fn from(value: GateName) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StratumName(String);

impl StratumName {
    pub fn new(value: impl Into<String>) -> Result<Self, ScoringError> {
        bounded_text(
            value.into(),
            MAX_NAME_BYTES,
            ScoringError::InvalidStratumName,
        )
        .map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for StratumName {
    type Error = ScoringError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<StratumName> for String {
    fn from(value: StratumName) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CandidateLabel(String);

impl CandidateLabel {
    pub fn new(value: impl Into<String>) -> Result<Self, ScoringError> {
        bounded_text(
            value.into(),
            MAX_LABEL_BYTES,
            ScoringError::InvalidCandidateLabel,
        )
        .map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CandidateLabel {
    type Error = ScoringError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<CandidateLabel> for String {
    fn from(value: CandidateLabel) -> Self {
        value.0
    }
}

fn bounded_text(
    value: String,
    maximum: usize,
    error: ScoringError,
) -> Result<String, ScoringError> {
    if value.trim().is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(error);
    }
    Ok(value)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    Primary,
    Protected,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricPolicy {
    pub name: MetricName,
    pub kind: MetricKind,
    pub margin: MetricScore,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StratumPolicy {
    pub name: StratumName,
    pub cases: Vec<CaseIdentity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiplicityFamily {
    pub candidates: u64,
    pub rounds: u64,
    pub metrics: u64,
    pub strata: u64,
    pub composites: u64,
    pub fallbacks: u64,
    pub campaigns: u64,
    pub activation_attempts: u64,
}

impl MultiplicityFamily {
    pub fn hypotheses_per_decision(self) -> Result<u64, ScoringError> {
        self.metrics
            .checked_mul(self.strata)
            .ok_or(ScoringError::MultiplicityOverflow)
    }

    pub fn size(self) -> Result<u64, ScoringError> {
        [
            self.candidates,
            self.rounds,
            self.composites,
            self.fallbacks,
            self.campaigns,
            self.activation_attempts,
            self.metrics,
            self.strata,
        ]
        .into_iter()
        .try_fold(1_u64, |product, value| {
            if value == 0 {
                return Err(ScoringError::EmptyMultiplicityDimension);
            }
            product
                .checked_mul(value)
                .ok_or(ScoringError::MultiplicityOverflow)
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionCoordinates {
    pub candidate: u64,
    pub round: u64,
    pub composite: u64,
    pub fallback: u64,
    pub campaign: u64,
    pub activation_attempt: u64,
}

impl DecisionCoordinates {
    fn validate(self, family: MultiplicityFamily) -> Result<(), ScoringError> {
        for (value, limit) in [
            (self.candidate, family.candidates),
            (self.round, family.rounds),
            (self.composite, family.composites),
            (self.fallback, family.fallbacks),
            (self.campaign, family.campaigns),
            (self.activation_attempt, family.activation_attempts),
        ] {
            if value >= limit {
                return Err(ScoringError::CoordinatesOutsideFamily);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenScoringPolicy {
    pub schema_version: u32,
    pub estimator_version: String,
    pub calibration: String,
    pub critical_cases: BTreeSet<CaseIdentity>,
    pub strata: Vec<StratumPolicy>,
    pub metrics: Vec<MetricPolicy>,
    pub minimum_complete_blocks: u32,
    pub maximum_exact_blocks: u32,
    pub required_hard_gates: Vec<GateName>,
    pub multiplicity_family: MultiplicityFamily,
    pub adaptive_error_nanos_per_hypothesis: u64,
    pub final_error_nanos_per_hypothesis: u64,
}

impl FrozenScoringPolicy {
    pub(crate) fn validate(&self, cohort: &EvaluationCohortSpec) -> Result<(), ScoringError> {
        if self.schema_version != 1 {
            return Err(ScoringError::UnsupportedSchema);
        }
        bounded_text(
            self.estimator_version.clone(),
            MAX_NAME_BYTES,
            ScoringError::InvalidEstimatorVersion,
        )?;
        bounded_text(
            self.calibration.clone(),
            MAX_NAME_BYTES,
            ScoringError::InvalidCalibration,
        )?;
        if self.minimum_complete_blocks == 0
            || self.maximum_exact_blocks == 0
            || self.maximum_exact_blocks > MAX_EXACT_BLOCKS
        {
            return Err(ScoringError::InvalidBlockBounds);
        }

        let cases = cohort
            .blocks
            .iter()
            .flat_map(|block| block.cases.iter().map(move |case| (case.id, block.id)))
            .collect::<BTreeMap<_, _>>();
        if self
            .critical_cases
            .iter()
            .any(|case| !cases.contains_key(case))
        {
            return Err(ScoringError::UnknownCriticalCase);
        }
        if cases.values().copied().collect::<BTreeSet<_>>().len()
            > self.maximum_exact_blocks as usize
        {
            return Err(ScoringError::TooManyExactBlocks);
        }

        let mut stratum_names = BTreeSet::new();
        for stratum in &self.strata {
            if !stratum_names.insert(&stratum.name) || stratum.cases.is_empty() {
                return Err(ScoringError::InvalidStrata);
            }
            let unique = stratum.cases.iter().copied().collect::<BTreeSet<_>>();
            if unique.len() != stratum.cases.len()
                || unique.iter().any(|case| !cases.contains_key(case))
            {
                return Err(ScoringError::InvalidStrata);
            }
        }
        if self.strata.is_empty() {
            return Err(ScoringError::InvalidStrata);
        }

        let mut metric_names = BTreeSet::new();
        let mut primary_metrics = 0;
        for metric in &self.metrics {
            if !metric_names.insert(&metric.name) {
                return Err(ScoringError::InvalidMetrics);
            }
            if metric.kind == MetricKind::Primary {
                primary_metrics += 1;
            }
        }
        if self.metrics.is_empty() || primary_metrics != 1 {
            return Err(ScoringError::InvalidMetrics);
        }

        let gates = self.required_hard_gates.iter().collect::<BTreeSet<_>>();
        if gates.is_empty() || gates.len() != self.required_hard_gates.len() {
            return Err(ScoringError::InvalidRequiredGates);
        }

        let family = self.multiplicity_family;
        let family_size = family.size()?;
        if family.metrics != self.metrics.len() as u64 || family.strata != self.strata.len() as u64
        {
            return Err(ScoringError::MultiplicitySchemaMismatch);
        }
        validate_allocation(
            cohort.adaptive_promotion,
            family_size,
            self.adaptive_error_nanos_per_hypothesis,
        )?;
        validate_allocation(
            cohort.final_audit,
            family_size,
            self.final_error_nanos_per_hypothesis,
        )?;
        Ok(())
    }
}

fn validate_allocation(
    limit: super::LedgerLimit,
    family_size: u64,
    error_nanos_per_hypothesis: u64,
) -> Result<(), ScoringError> {
    if error_nanos_per_hypothesis == 0 || error_nanos_per_hypothesis > ERROR_SCALE {
        return Err(ScoringError::InvalidErrorAllocation);
    }
    let error_nanos = family_size
        .checked_mul(error_nanos_per_hypothesis)
        .ok_or(ScoringError::MultiplicityOverflow)?;
    if limit.queries != family_size || limit.error_nanos != error_nanos {
        return Err(ScoringError::IncompleteFamilyAllocation);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveTrialEvidence {
    pub spec: TrialPairSpec,
    pub evidence: PairedTrialEvidence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    Verified,
    NotVerified,
    Inconclusive,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateEvidence {
    pub name: GateName,
    pub status: GateStatus,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptivePromotionDataset {
    cohort: CohortId,
    adaptive_partition: super::TrialPartition,
    candidate: CandidateId,
    candidate_label: CandidateLabel,
    pairs: Vec<AdaptiveTrialEvidence>,
    hard_gate_evidence: Vec<GateEvidence>,
}

impl AdaptivePromotionDataset {
    pub fn new(
        cohort: CohortId,
        adaptive_partition: super::TrialPartition,
        candidate: CandidateId,
        candidate_label: CandidateLabel,
        pairs: Vec<AdaptiveTrialEvidence>,
        hard_gate_evidence: Vec<GateEvidence>,
    ) -> Result<Self, ScoringError> {
        if adaptive_partition.role() != TrialLedgerRole::AdaptivePromotion {
            return Err(ScoringError::WrongEvidenceRole);
        }
        let pair_keys = pairs
            .iter()
            .map(|pair| (pair.spec.case(), pair.spec.repeat()))
            .collect::<BTreeSet<_>>();
        if pair_keys.len() != pairs.len() {
            return Err(ScoringError::DuplicatePair);
        }
        let gate_names = hard_gate_evidence
            .iter()
            .map(|gate| &gate.name)
            .collect::<BTreeSet<_>>();
        if gate_names.len() != hard_gate_evidence.len() {
            return Err(ScoringError::DuplicateGateEvidence);
        }
        for gate in &hard_gate_evidence {
            bounded_text(gate.reason.clone(), 1024, ScoringError::InvalidGateReason)?;
        }
        Ok(Self {
            cohort,
            adaptive_partition,
            candidate,
            candidate_label,
            pairs,
            hard_gate_evidence,
        })
    }

    pub const fn cohort(&self) -> CohortId {
        self.cohort
    }

    pub const fn partition(&self) -> super::TrialPartition {
        self.adaptive_partition
    }

    pub const fn candidate(&self) -> CandidateId {
        self.candidate
    }

    pub fn candidate_label(&self) -> &CandidateLabel {
        &self.candidate_label
    }

    pub fn pairs(&self) -> &[AdaptiveTrialEvidence] {
        &self.pairs
    }

    pub fn hard_gate_evidence(&self) -> &[GateEvidence] {
        &self.hard_gate_evidence
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdaptiveVerdict {
    Verified,
    NotVerified,
    Inconclusive,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateResult {
    pub name: String,
    pub status: GateStatus,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactEffect {
    pub numerator: i128,
    pub denominator: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactRatio {
    pub numerator: u64,
    pub denominator: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseEffect {
    pub case: CaseIdentity,
    pub effect: ExactEffect,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockEffect {
    pub block: IndependentBlockId,
    pub effect: ExactEffect,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricStatistic {
    pub metric: MetricName,
    pub stratum: StratumName,
    pub case_effects: Vec<CaseEffect>,
    pub block_effects: Vec<BlockEffect>,
    pub observed_effect: ExactEffect,
    pub adjusted_effect: ExactEffect,
    pub minimum_block_effect: ExactEffect,
    pub maximum_block_effect: ExactEffect,
    pub p_value: ExactRatio,
    pub alpha: ExactRatio,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerTransition {
    pub before: LedgerStatus,
    pub query_debit: u64,
    pub error_nanos_debit: u64,
    pub after: LedgerStatus,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveScoreReport {
    pub cohort: CohortId,
    pub campaign: CampaignId,
    pub round: RoundId,
    pub candidate: CandidateId,
    pub verdict: AdaptiveVerdict,
    pub estimator_version: String,
    pub calibration: String,
    pub policy_digest: Digest,
    pub coordinates: DecisionCoordinates,
    pub gates: Vec<GateResult>,
    pub statistics: Vec<MetricStatistic>,
    pub ledger: LedgerTransition,
    pub evidence_root: Digest,
    pub production_activation: AdaptiveVerdict,
    pub production_activation_reason: String,
}

impl AdaptiveScoreReport {
    pub fn result_id(&self) -> Result<ScoreResultId, ScoringError> {
        Ok(ScoreResultId::from_digest(Digest::of_value(&(
            "orvek:adaptive-score-report:v1",
            self,
        ))?))
    }
}

#[derive(Debug, Error)]
pub enum ScoringError {
    #[error("gate name must be non-empty, bounded text")]
    InvalidGateName,
    #[error("stratum name must be non-empty, bounded text")]
    InvalidStratumName,
    #[error("candidate label must be non-empty, bounded text")]
    InvalidCandidateLabel,
    #[error("gate reason must be non-empty, bounded text")]
    InvalidGateReason,
    #[error("the frozen scoring schema is unsupported")]
    UnsupportedSchema,
    #[error("estimator version must be non-empty, bounded text")]
    InvalidEstimatorVersion,
    #[error("calibration must be non-empty, bounded text")]
    InvalidCalibration,
    #[error("exact-sign block bounds are invalid")]
    InvalidBlockBounds,
    #[error("the scoring policy names an unknown critical case")]
    UnknownCriticalCase,
    #[error("the frozen cohort exceeds the exact-sign block bound")]
    TooManyExactBlocks,
    #[error("strata must be non-empty, unique, and contain only frozen cases")]
    InvalidStrata,
    #[error("metrics must be unique and contain exactly one primary metric")]
    InvalidMetrics,
    #[error("required hard gates must be non-empty and unique")]
    InvalidRequiredGates,
    #[error("a multiplicity dimension must be positive")]
    EmptyMultiplicityDimension,
    #[error("the multiplicity family exceeds storage bounds")]
    MultiplicityOverflow,
    #[error("multiplicity metric and stratum counts differ from the frozen policy")]
    MultiplicitySchemaMismatch,
    #[error("per-hypothesis error allocation is invalid")]
    InvalidErrorAllocation,
    #[error("cohort ledgers must allocate exactly the complete multiplicity family")]
    IncompleteFamilyAllocation,
    #[error("decision coordinates are outside the frozen multiplicity family")]
    CoordinatesOutsideFamily,
    #[error("adaptive scoring requires adaptive-promotion evidence")]
    WrongEvidenceRole,
    #[error("adaptive evidence contains a duplicate case/repeat pair")]
    DuplicatePair,
    #[error("adaptive evidence contains duplicate hard-gate evidence")]
    DuplicateGateEvidence,
    #[error("adaptive evidence belongs to a different cohort, campaign, or candidate")]
    WrongEvaluationContext,
    #[error("adaptive evidence contains a pair outside the frozen manifest")]
    PairOutsideManifest,
    #[error("adaptive evidence contains an undeclared hard gate")]
    UndeclaredGate,
    #[error("a passing outcome omits a frozen metric")]
    MissingMetric,
    #[error("trial evidence is invalid: {0}")]
    Trial(#[from] TrialError),
    #[error("scoring content could not be canonicalized: {0}")]
    Canonicalization(#[from] serde_json::Error),
}

pub(crate) fn scoring_policy_digest(cohort: &EvaluationCohortSpec) -> Result<Digest, ScoringError> {
    Ok(Digest::of_value(&(
        "orvek:frozen-scoring-policy:v1",
        cohort.id,
        cohort.target,
        cohort.base_revision,
        cohort.evaluator,
        cohort.policy,
        cohort.partitions,
        &cohort.blocks,
        cohort.adaptive_promotion,
        cohort.final_audit,
        &cohort.scoring,
    ))?)
}

pub(crate) fn evaluate_adaptive(
    cohort: &EvaluationCohortSpec,
    campaign: CampaignId,
    round: RoundId,
    dataset: &AdaptivePromotionDataset,
    ledger: LedgerStatus,
    coordinates: DecisionCoordinates,
) -> Result<AdaptiveScoreReport, ScoringError> {
    cohort.scoring.validate(cohort)?;
    coordinates.validate(cohort.scoring.multiplicity_family)?;
    if dataset.cohort != cohort.id {
        return Err(ScoringError::WrongEvaluationContext);
    }

    let policy_digest = scoring_policy_digest(cohort)?;
    let evidence_root = evidence_root(dataset)?;
    let transition = reserve_ledger(cohort, ledger)?;
    if transition.query_debit == 0 {
        return Ok(AdaptiveScoreReport {
            cohort: cohort.id,
            campaign,
            round,
            candidate: dataset.candidate,
            verdict: AdaptiveVerdict::Inconclusive,
            estimator_version: cohort.scoring.estimator_version.clone(),
            calibration: cohort.scoring.calibration.clone(),
            policy_digest,
            coordinates,
            gates: vec![gate(
                "ledger_capacity",
                GateStatus::Inconclusive,
                "Store-global query or error ledger has no allocation for this decision",
            )],
            statistics: Vec::new(),
            ledger: transition,
            evidence_root,
            production_activation: AdaptiveVerdict::Inconclusive,
            production_activation_reason: production_activation_reason(cohort),
        });
    }

    let manifest = cohort
        .blocks
        .iter()
        .flat_map(|block| {
            block
                .cases
                .iter()
                .map(move |case| (case.id, (block.id, case.repeats)))
        })
        .collect::<BTreeMap<_, _>>();
    let mut observed = BTreeMap::new();
    let mut unknown_count = 0_usize;
    let mut drift_count = 0_usize;
    for pair in &dataset.pairs {
        pair.evidence.validate_for_pair(pair.spec)?;
        let Some((expected_block, repeats)) = manifest.get(&pair.spec.case()).copied() else {
            return Err(ScoringError::PairOutsideManifest);
        };
        if pair.spec.repeat() >= repeats {
            return Err(ScoringError::PairOutsideManifest);
        }
        let context = pair.spec.context();
        if context.campaign() != campaign || context.candidate() != dataset.candidate {
            return Err(ScoringError::WrongEvaluationContext);
        }
        let runtime = context.runtime();
        let drifted = pair.spec.block() != expected_block
            || context.partition() != dataset.adaptive_partition
            || runtime.model() != cohort.target.model
            || runtime.protocol() != cohort.target.protocol
            || runtime.evaluator() != cohort.evaluator
            || runtime.environment() != cohort.target.environment;
        if drifted {
            drift_count += 1;
        }
        if matches!(pair.evidence, PairedTrialEvidence::InfrastructureUnknown(_)) {
            unknown_count += 1;
        }
        if observed
            .insert((pair.spec.case(), pair.spec.repeat()), &pair.evidence)
            .is_some()
        {
            return Err(ScoringError::DuplicatePair);
        }
    }

    let expected_pairs = manifest
        .iter()
        .flat_map(|(case, (_, repeats))| (0..*repeats).map(move |repeat| (*case, repeat)))
        .collect::<BTreeSet<_>>();
    let missing_count = expected_pairs
        .iter()
        .filter(|key| !observed.contains_key(key))
        .count();
    let partition_matches =
        dataset.adaptive_partition.commitment() == cohort.partitions.adaptive_promotion;

    let mut gates = Vec::new();
    if missing_count > 0 || unknown_count > 0 {
        gates.push(gate(
            "data_completeness",
            GateStatus::Inconclusive,
            format!(
                "{missing_count} paired trials are missing; {unknown_count} trials are InfrastructureUnknown"
            ),
        ));
    } else {
        gates.push(gate(
            "data_completeness",
            GateStatus::Verified,
            "all frozen pairs are complete",
        ));
    }
    if drift_count > 0 || !partition_matches {
        gates.push(gate(
            "pairing_integrity",
            GateStatus::Inconclusive,
            format!(
                "{drift_count} pairs have identity or block drift; partition match={partition_matches}"
            ),
        ));
    } else {
        gates.push(gate(
            "pairing_integrity",
            GateStatus::Verified,
            "all paired identities match the frozen cohort",
        ));
    }

    let external = dataset
        .hard_gate_evidence
        .iter()
        .map(|item| (&item.name, item))
        .collect::<BTreeMap<_, _>>();
    if external
        .keys()
        .any(|name| !cohort.scoring.required_hard_gates.contains(name))
    {
        return Err(ScoringError::UndeclaredGate);
    }
    for name in &cohort.scoring.required_hard_gates {
        if let Some(item) = external.get(name) {
            gates.push(gate(name.as_str(), item.status, item.reason.clone()));
        } else {
            gates.push(gate(
                name.as_str(),
                GateStatus::Inconclusive,
                "required hard-gate evidence is missing",
            ));
        }
    }

    let failures = observed
        .iter()
        .filter_map(|((case, _), evidence)| match evidence {
            PairedTrialEvidence::Usable(receipt)
                if matches!(
                    receipt.candidate(),
                    BehavioralTrialOutcome::BehavioralFailure { .. }
                ) =>
            {
                Some(*case)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if missing_count > 0 || unknown_count > 0 {
        gates.push(gate(
            "correctness",
            GateStatus::Inconclusive,
            "candidate correctness evidence is incomplete",
        ));
        gates.push(gate(
            "critical_cases",
            GateStatus::Inconclusive,
            "critical-case evidence is incomplete",
        ));
    } else {
        gates.push(gate(
            "correctness",
            if failures.is_empty() {
                GateStatus::Verified
            } else {
                GateStatus::NotVerified
            },
            format!("{} candidate BehavioralFailure outcomes", failures.len()),
        ));
        let critical_failures = failures
            .iter()
            .filter(|case| cohort.scoring.critical_cases.contains(case))
            .count();
        gates.push(gate(
            "critical_cases",
            if critical_failures == 0 {
                GateStatus::Verified
            } else {
                GateStatus::NotVerified
            },
            format!("{critical_failures} critical candidate failures"),
        ));
    }

    let underpowered = cohort
        .scoring
        .strata
        .iter()
        .filter_map(|stratum| {
            let blocks = stratum
                .cases
                .iter()
                .filter_map(|case| manifest.get(case).map(|(block, _)| *block))
                .collect::<BTreeSet<_>>()
                .len();
            (blocks < cohort.scoring.minimum_complete_blocks as usize)
                .then_some((stratum.name.as_str(), blocks))
        })
        .collect::<Vec<_>>();
    if underpowered.is_empty() {
        gates.push(gate(
            "minimum_power",
            GateStatus::Verified,
            format!(
                "each stratum has at least {} independent blocks",
                cohort.scoring.minimum_complete_blocks
            ),
        ));
    } else {
        gates.push(gate(
            "minimum_power",
            GateStatus::Inconclusive,
            format!("complete independent blocks are below the frozen minimum: {underpowered:?}"),
        ));
    }

    let can_score = missing_count == 0
        && unknown_count == 0
        && drift_count == 0
        && partition_matches
        && underpowered.is_empty();
    let mut statistics = Vec::new();
    let mut metrics = cohort.scoring.metrics.iter().collect::<Vec<_>>();
    metrics.sort_by(|left, right| left.name.cmp(&right.name));
    let mut strata = cohort.scoring.strata.iter().collect::<Vec<_>>();
    strata.sort_by(|left, right| left.name.cmp(&right.name));
    for metric in metrics {
        for stratum in &strata {
            let gate_name = statistical_gate_name(metric, &stratum.name);
            if !can_score {
                gates.push(gate(
                    gate_name,
                    GateStatus::Inconclusive,
                    "paired block statistic is unavailable",
                ));
                continue;
            }
            let (statistic, status) = score_metric(
                &observed,
                &manifest,
                metric,
                stratum,
                cohort.scoring.adaptive_error_nanos_per_hypothesis,
            )?;
            statistics.push(statistic);
            gates.push(gate(
                gate_name,
                status,
                if status == GateStatus::Verified {
                    "adjusted block effect is positive at the frozen exact-sign allocation"
                } else {
                    "effect margin or frozen exact-sign allocation was not satisfied"
                },
            ));
        }
    }

    let verdict = combine_gate_statuses(&gates);
    Ok(AdaptiveScoreReport {
        cohort: cohort.id,
        campaign,
        round,
        candidate: dataset.candidate,
        verdict,
        estimator_version: cohort.scoring.estimator_version.clone(),
        calibration: cohort.scoring.calibration.clone(),
        policy_digest,
        coordinates,
        gates,
        statistics,
        ledger: transition,
        evidence_root,
        production_activation: AdaptiveVerdict::Inconclusive,
        production_activation_reason: production_activation_reason(cohort),
    })
}

fn reserve_ledger(
    cohort: &EvaluationCohortSpec,
    before: LedgerStatus,
) -> Result<LedgerTransition, ScoringError> {
    let query_debit = cohort
        .scoring
        .multiplicity_family
        .hypotheses_per_decision()?;
    let error_nanos_debit = query_debit
        .checked_mul(cohort.scoring.adaptive_error_nanos_per_hypothesis)
        .ok_or(ScoringError::MultiplicityOverflow)?;
    let query_used = before.query_used.checked_add(query_debit);
    let error_used = before.error_used_nanos.checked_add(error_nanos_debit);
    let has_capacity = query_used.is_some_and(|used| used <= before.query_limit)
        && error_used.is_some_and(|used| used <= before.error_limit_nanos);
    if !has_capacity {
        return Ok(LedgerTransition {
            before,
            query_debit: 0,
            error_nanos_debit: 0,
            after: before,
        });
    }
    Ok(LedgerTransition {
        before,
        query_debit,
        error_nanos_debit,
        after: LedgerStatus {
            query_limit: before.query_limit,
            query_used: query_used.expect("capacity checked"),
            error_limit_nanos: before.error_limit_nanos,
            error_used_nanos: error_used.expect("capacity checked"),
        },
    })
}

fn score_metric(
    observed: &BTreeMap<(CaseIdentity, u32), &PairedTrialEvidence>,
    manifest: &BTreeMap<CaseIdentity, (IndependentBlockId, u32)>,
    metric: &MetricPolicy,
    stratum: &StratumPolicy,
    error_nanos_per_hypothesis: u64,
) -> Result<(MetricStatistic, GateStatus), ScoringError> {
    let mut case_effects = Vec::new();
    for case in &stratum.cases {
        let (_, repeats) = manifest[case];
        let mut repeat_sum = Rational::zero();
        for repeat in 0..repeats {
            let PairedTrialEvidence::Usable(receipt) = observed[&(*case, repeat)] else {
                unreachable!("scoring excludes incomplete evidence");
            };
            repeat_sum = repeat_sum.add(
                score(receipt.candidate(), &metric.name)?
                    .sub(score(receipt.parent(), &metric.name)?),
            );
        }
        case_effects.push((*case, repeat_sum.divide(repeats as i128)));
    }

    let mut by_block = BTreeMap::<IndependentBlockId, Vec<Rational>>::new();
    for (case, effect) in &case_effects {
        by_block.entry(manifest[case].0).or_default().push(*effect);
    }
    let block_effects = by_block
        .into_iter()
        .map(|(block, effects)| {
            let count = effects.len() as i128;
            let effect = effects
                .into_iter()
                .fold(Rational::zero(), Rational::add)
                .divide(count);
            (block, effect)
        })
        .collect::<Vec<_>>();
    let values = block_effects
        .iter()
        .map(|(_, effect)| *effect)
        .collect::<Vec<_>>();
    let observed_effect = mean(&values);
    let margin = Rational::new(metric.margin.millionths() as i128, SCORE_SCALE);
    let adjusted_values = values
        .iter()
        .map(|value| match metric.kind {
            MetricKind::Primary => value.sub(margin),
            MetricKind::Protected => value.add(margin),
        })
        .collect::<Vec<_>>();
    let adjusted_effect = mean(&adjusted_values);
    let p_value = exact_sign_flip_p_value(&adjusted_values);
    let alpha = ExactRatio::new(error_nanos_per_hypothesis, ERROR_SCALE);
    let verified = adjusted_effect.numerator > 0 && ratio_le(p_value, alpha);
    let minimum = values.iter().copied().min().expect("non-empty stratum");
    let maximum = values.iter().copied().max().expect("non-empty stratum");
    Ok((
        MetricStatistic {
            metric: metric.name.clone(),
            stratum: stratum.name.clone(),
            case_effects: case_effects
                .into_iter()
                .map(|(case, effect)| CaseEffect {
                    case,
                    effect: effect.into(),
                })
                .collect(),
            block_effects: block_effects
                .into_iter()
                .map(|(block, effect)| BlockEffect {
                    block,
                    effect: effect.into(),
                })
                .collect(),
            observed_effect: observed_effect.into(),
            adjusted_effect: adjusted_effect.into(),
            minimum_block_effect: minimum.into(),
            maximum_block_effect: maximum.into(),
            p_value,
            alpha,
        },
        if verified {
            GateStatus::Verified
        } else {
            GateStatus::NotVerified
        },
    ))
}

fn score(outcome: &BehavioralTrialOutcome, metric: &MetricName) -> Result<Rational, ScoringError> {
    match outcome {
        BehavioralTrialOutcome::BehavioralFailure { .. } => Ok(Rational::zero()),
        BehavioralTrialOutcome::Pass { metrics } => metrics
            .get(metric)
            .map(|score| Rational::new(score.millionths() as i128, SCORE_SCALE))
            .ok_or(ScoringError::MissingMetric),
    }
}

fn mean(values: &[Rational]) -> Rational {
    values
        .iter()
        .copied()
        .fold(Rational::zero(), Rational::add)
        .divide(values.len() as i128)
}

fn exact_sign_flip_p_value(values: &[Rational]) -> ExactRatio {
    let observed = values.iter().copied().fold(Rational::zero(), Rational::add);
    let total = 1_u64 << values.len();
    let mut extreme = 0_u64;
    for mask in 0..total {
        let permuted = values
            .iter()
            .enumerate()
            .fold(Rational::zero(), |sum, (index, value)| {
                if mask & (1_u64 << index) == 0 {
                    sum.sub(*value)
                } else {
                    sum.add(*value)
                }
            });
        if permuted >= observed {
            extreme += 1;
        }
    }
    ExactRatio::new(extreme, total)
}

fn ratio_le(left: ExactRatio, right: ExactRatio) -> bool {
    u128::from(left.numerator) * u128::from(right.denominator)
        <= u128::from(right.numerator) * u128::from(left.denominator)
}

fn statistical_gate_name(metric: &MetricPolicy, stratum: &StratumName) -> String {
    let prefix = match metric.kind {
        MetricKind::Primary => "useful_effect",
        MetricKind::Protected => "protected_noninferiority",
    };
    format!("{prefix}:{}:{}", metric.name.as_str(), stratum.as_str())
}

fn combine_gate_statuses(gates: &[GateResult]) -> AdaptiveVerdict {
    if gates
        .iter()
        .any(|gate| gate.status == GateStatus::Inconclusive)
    {
        AdaptiveVerdict::Inconclusive
    } else if gates
        .iter()
        .any(|gate| gate.status == GateStatus::NotVerified)
    {
        AdaptiveVerdict::NotVerified
    } else {
        AdaptiveVerdict::Verified
    }
}

fn evidence_root(dataset: &AdaptivePromotionDataset) -> Result<Digest, ScoringError> {
    let mut pairs = dataset.pairs.iter().collect::<Vec<_>>();
    pairs.sort_by_key(|pair| (pair.spec.case(), pair.spec.repeat()));
    let mut gates = dataset.hard_gate_evidence.iter().collect::<Vec<_>>();
    gates.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(Digest::of_value(&(
        "orvek:adaptive-promotion-evidence:v1",
        dataset.cohort,
        dataset.adaptive_partition,
        dataset.candidate,
        pairs,
        gates,
    ))?)
}

fn production_activation_reason(cohort: &EvaluationCohortSpec) -> String {
    format!(
        "{} calibration cannot authorize production activation",
        cohort.scoring.calibration
    )
}

fn gate(name: impl Into<String>, status: GateStatus, reason: impl Into<String>) -> GateResult {
    GateResult {
        name: name.into(),
        status,
        reason: reason.into(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Rational {
    numerator: i128,
    denominator: i128,
}

impl Rational {
    const fn zero() -> Self {
        Self {
            numerator: 0,
            denominator: 1,
        }
    }

    fn new(numerator: i128, denominator: i128) -> Self {
        debug_assert!(denominator > 0);
        let divisor = gcd(numerator.unsigned_abs(), denominator as u128) as i128;
        Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        }
    }

    fn add(self, other: Self) -> Self {
        Self::new(
            self.numerator * other.denominator + other.numerator * self.denominator,
            self.denominator * other.denominator,
        )
    }

    fn sub(self, other: Self) -> Self {
        Self::new(
            self.numerator * other.denominator - other.numerator * self.denominator,
            self.denominator * other.denominator,
        )
    }

    fn divide(self, divisor: i128) -> Self {
        Self::new(self.numerator, self.denominator * divisor)
    }
}

impl Ord for Rational {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.numerator * other.denominator).cmp(&(other.numerator * self.denominator))
    }
}

impl PartialOrd for Rational {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl From<Rational> for ExactEffect {
    fn from(value: Rational) -> Self {
        Self {
            numerator: value.numerator,
            denominator: value.denominator as u64,
        }
    }
}

impl ExactRatio {
    fn new(numerator: u64, denominator: u64) -> Self {
        let divisor = gcd(u128::from(numerator), u128::from(denominator)) as u64;
        Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        }
    }
}

fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}
