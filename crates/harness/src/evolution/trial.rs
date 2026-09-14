use super::{
    CampaignId, CandidateId, CaseIdentity, EffectId, EnvironmentIdentity, EvaluatorIdentity,
    IndependentBlockId, LeaseEpoch, ModelIdentity, PartitionCommitment, ProtocolIdentity,
};
use crate::{Digest, contract::Limits};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};
use thiserror::Error;
use uuid::Uuid;

const MAX_METRIC_NAME_BYTES: usize = 128;
const MAX_PARTITION_EPOCH: u64 = i64::MAX as u64;
const SCORE_SCALE: u32 = 1_000_000;

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

digest_id!(InputCommitment);
digest_id!(TrialPairId);
digest_id!(TrialKeyId);
digest_id!(IsolatedRunReceiptId);
digest_id!(PairedTrialReceiptId);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IsolationInstanceId(Uuid);

impl IsolationInstanceId {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialLedgerRole {
    AdaptivePromotion,
    FinalAudit,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialSide {
    Parent,
    Candidate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialPartition {
    role: TrialLedgerRole,
    epoch: u64,
    commitment: PartitionCommitment,
}

impl TrialPartition {
    pub fn new(
        role: TrialLedgerRole,
        epoch: u64,
        commitment: PartitionCommitment,
    ) -> Result<Self, TrialError> {
        if epoch == 0 || epoch > MAX_PARTITION_EPOCH {
            return Err(TrialError::InvalidPartitionEpoch);
        }
        Ok(Self {
            role,
            epoch,
            commitment,
        })
    }

    pub const fn role(self) -> TrialLedgerRole {
        self.role
    }

    pub const fn epoch(self) -> u64 {
        self.epoch
    }

    pub const fn commitment(self) -> PartitionCommitment {
        self.commitment
    }

    fn validate(self) -> Result<(), TrialError> {
        Self::new(self.role, self.epoch, self.commitment).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialRuntimeIdentity {
    model: ModelIdentity,
    protocol: ProtocolIdentity,
    evaluator: EvaluatorIdentity,
    environment: EnvironmentIdentity,
}

impl TrialRuntimeIdentity {
    pub const fn new(
        model: ModelIdentity,
        protocol: ProtocolIdentity,
        evaluator: EvaluatorIdentity,
        environment: EnvironmentIdentity,
    ) -> Self {
        Self {
            model,
            protocol,
            evaluator,
            environment,
        }
    }

    pub const fn model(self) -> ModelIdentity {
        self.model
    }

    pub const fn protocol(self) -> ProtocolIdentity {
        self.protocol
    }

    pub const fn evaluator(self) -> EvaluatorIdentity {
        self.evaluator
    }

    pub const fn environment(self) -> EnvironmentIdentity {
        self.environment
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialContext {
    campaign: CampaignId,
    candidate: CandidateId,
    partition: TrialPartition,
    runtime: TrialRuntimeIdentity,
    limits: Limits,
}

impl TrialContext {
    pub fn new(
        campaign: CampaignId,
        candidate: CandidateId,
        partition: TrialPartition,
        runtime: TrialRuntimeIdentity,
        limits: Limits,
    ) -> Result<Self, TrialError> {
        partition.validate()?;
        limits.validate().map_err(|_| TrialError::InvalidLimits)?;
        Ok(Self {
            campaign,
            candidate,
            partition,
            runtime,
            limits,
        })
    }

    pub const fn campaign(self) -> CampaignId {
        self.campaign
    }

    pub const fn candidate(self) -> CandidateId {
        self.candidate
    }

    pub const fn partition(self) -> TrialPartition {
        self.partition
    }

    pub const fn runtime(self) -> TrialRuntimeIdentity {
        self.runtime
    }

    pub const fn limits(self) -> Limits {
        self.limits
    }

    fn validate(self) -> Result<(), TrialError> {
        Self::new(
            self.campaign,
            self.candidate,
            self.partition,
            self.runtime,
            self.limits,
        )
        .map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialPairSpec {
    id: TrialPairId,
    context: TrialContext,
    block: IndependentBlockId,
    case: CaseIdentity,
    repeat: u32,
    input: InputCommitment,
}

impl TrialPairSpec {
    pub fn new(
        context: TrialContext,
        block: IndependentBlockId,
        case: CaseIdentity,
        repeat: u32,
        input: InputCommitment,
    ) -> Result<Self, TrialError> {
        context.validate()?;
        let id = pair_id(context, block, case, repeat, input)?;
        Ok(Self {
            id,
            context,
            block,
            case,
            repeat,
            input,
        })
    }

    pub const fn id(self) -> TrialPairId {
        self.id
    }

    pub const fn context(self) -> TrialContext {
        self.context
    }

    pub const fn block(self) -> IndependentBlockId {
        self.block
    }

    pub const fn case(self) -> CaseIdentity {
        self.case
    }

    pub const fn repeat(self) -> u32 {
        self.repeat
    }

    pub const fn input(self) -> InputCommitment {
        self.input
    }

    pub fn key(self, side: TrialSide) -> Result<TrialKey, TrialError> {
        self.validate()?;
        TrialKey::new(self, side)
    }

    pub fn ordered_sides(self) -> [TrialSide; 2] {
        let schedule = Digest::of(format!("orvek:trial-order:v1:{}", self.id).as_bytes());
        if schedule.to_string().as_bytes()[0] & 1 == 0 {
            [TrialSide::Parent, TrialSide::Candidate]
        } else {
            [TrialSide::Candidate, TrialSide::Parent]
        }
    }

    pub(crate) fn validate(self) -> Result<(), TrialError> {
        self.context.validate()?;
        if self.id != pair_id(self.context, self.block, self.case, self.repeat, self.input)? {
            return Err(TrialError::TamperedPairIdentity);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialKey {
    id: TrialKeyId,
    pair: TrialPairId,
    context: TrialContext,
    block: IndependentBlockId,
    case: CaseIdentity,
    repeat: u32,
    side: TrialSide,
    input: InputCommitment,
}

impl TrialKey {
    fn new(pair: TrialPairSpec, side: TrialSide) -> Result<Self, TrialError> {
        let id = key_id(pair, side)?;
        Ok(Self {
            id,
            pair: pair.id,
            context: pair.context,
            block: pair.block,
            case: pair.case,
            repeat: pair.repeat,
            side,
            input: pair.input,
        })
    }

    pub const fn id(self) -> TrialKeyId {
        self.id
    }

    pub const fn pair(self) -> TrialPairId {
        self.pair
    }

    pub const fn context(self) -> TrialContext {
        self.context
    }

    pub const fn block(self) -> IndependentBlockId {
        self.block
    }

    pub const fn case(self) -> CaseIdentity {
        self.case
    }

    pub const fn repeat(self) -> u32 {
        self.repeat
    }

    pub const fn side(self) -> TrialSide {
        self.side
    }

    pub const fn input(self) -> InputCommitment {
        self.input
    }

    fn validate(self) -> Result<(), TrialError> {
        let pair = TrialPairSpec {
            id: self.pair,
            context: self.context,
            block: self.block,
            case: self.case,
            repeat: self.repeat,
            input: self.input,
        };
        pair.validate()?;
        if self.id != key_id(pair, self.side)? {
            return Err(TrialError::TamperedTrialKey);
        }
        Ok(())
    }
}

/// The only isolation profile admitted for evolution trials.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialIsolationProfile {
    StrictCandidateTrialV1,
}

impl TrialIsolationProfile {
    pub const fn strict() -> Self {
        Self::StrictCandidateTrialV1
    }

    pub const fn fresh_exclusive_workspace(self) -> bool {
        true
    }

    pub const fn network_enabled(self) -> bool {
        false
    }

    pub const fn private_ipc(self) -> bool {
        true
    }

    pub const fn host_control_available(self) -> bool {
        false
    }

    pub const fn store_available(self) -> bool {
        false
    }

    pub const fn credential_access(self) -> bool {
        false
    }

    pub const fn sealed_artifact_access(self) -> bool {
        false
    }

    pub const fn shared_writable_mounts(self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialTransportCapability {
    DurableStartInspectCollectFence,
    FenceUnknownAfterStart,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "receipt", rename_all = "snake_case")]
pub enum TrialTransportTerminal {
    Collected(Digest),
    Fenced(Digest),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialRunAssignment {
    key: TrialKey,
    isolation_instance: IsolationInstanceId,
    isolation: TrialIsolationProfile,
    transport: TrialTransportCapability,
}

impl TrialRunAssignment {
    pub(crate) const fn new(
        key: TrialKey,
        isolation_instance: IsolationInstanceId,
        transport: TrialTransportCapability,
    ) -> Self {
        Self {
            key,
            isolation_instance,
            isolation: TrialIsolationProfile::strict(),
            transport,
        }
    }

    pub const fn key(self) -> TrialKey {
        self.key
    }

    pub const fn isolation_instance(self) -> IsolationInstanceId {
        self.isolation_instance
    }

    pub const fn isolation(self) -> TrialIsolationProfile {
        self.isolation
    }

    pub const fn transport(self) -> TrialTransportCapability {
        self.transport
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MetricName(String);

impl MetricName {
    pub fn new(value: impl Into<String>) -> Result<Self, TrialError> {
        let value = value.into();
        if value.trim().is_empty()
            || value.len() > MAX_METRIC_NAME_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(TrialError::InvalidMetricName);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MetricName {
    type Error = TrialError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<MetricName> for String {
    fn from(value: MetricName) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MetricScore(u32);

impl MetricScore {
    pub fn from_millionths(value: u32) -> Result<Self, TrialError> {
        if value > SCORE_SCALE {
            return Err(TrialError::InvalidMetricScore);
        }
        Ok(Self(value))
    }

    pub const fn millionths(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehavioralFailureReason {
    AttributableCandidateCrash,
    BudgetExhaustion,
    EvaluatorRejection,
    ProtocolTimeout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InfrastructureUnknownReason {
    EnvironmentDrift,
    EvaluatorDrift,
    MissingReceipt,
    ModelDrift,
    TamperedReceipt,
    TransportLoss,
    UnreconciledExternalAttempt,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum BehavioralTrialOutcome {
    #[serde(rename = "Pass")]
    Pass {
        metrics: BTreeMap<MetricName, MetricScore>,
    },
    #[serde(rename = "BehavioralFailure")]
    BehavioralFailure { reason: BehavioralFailureReason },
}

impl BehavioralTrialOutcome {
    pub fn pass(metrics: BTreeMap<MetricName, MetricScore>) -> Self {
        Self::Pass { metrics }
    }

    pub const fn failure(reason: BehavioralFailureReason) -> Self {
        Self::BehavioralFailure { reason }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TerminalTrialOutcome {
    #[serde(rename = "Pass")]
    Pass {
        metrics: BTreeMap<MetricName, MetricScore>,
    },
    #[serde(rename = "BehavioralFailure")]
    BehavioralFailure { reason: BehavioralFailureReason },
    #[serde(rename = "InfrastructureUnknown")]
    InfrastructureUnknown { reason: InfrastructureUnknownReason },
}

impl TerminalTrialOutcome {
    pub fn pass(metrics: BTreeMap<MetricName, MetricScore>) -> Self {
        Self::Pass { metrics }
    }

    pub const fn behavioral_failure(reason: BehavioralFailureReason) -> Self {
        Self::BehavioralFailure { reason }
    }

    pub const fn infrastructure_unknown(reason: InfrastructureUnknownReason) -> Self {
        Self::InfrastructureUnknown { reason }
    }

    pub const fn unknown_reason(&self) -> Option<InfrastructureUnknownReason> {
        match self {
            Self::InfrastructureUnknown { reason } => Some(*reason),
            Self::Pass { .. } | Self::BehavioralFailure { .. } => None,
        }
    }

    fn behavioral(&self) -> Option<BehavioralTrialOutcome> {
        match self {
            Self::Pass { metrics } => Some(BehavioralTrialOutcome::Pass {
                metrics: metrics.clone(),
            }),
            Self::BehavioralFailure { reason } => {
                Some(BehavioralTrialOutcome::BehavioralFailure { reason: *reason })
            }
            Self::InfrastructureUnknown { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedIsolatedRunReceipt")]
pub struct IsolatedRunReceipt {
    id: IsolatedRunReceiptId,
    key: TrialKey,
    effect: EffectId,
    lease_epoch: LeaseEpoch,
    isolation_instance: IsolationInstanceId,
    isolation: TrialIsolationProfile,
    capability: TrialTransportCapability,
    terminal: TrialTransportTerminal,
    outcome: TerminalTrialOutcome,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UncheckedIsolatedRunReceipt {
    id: IsolatedRunReceiptId,
    key: TrialKey,
    effect: EffectId,
    lease_epoch: LeaseEpoch,
    isolation_instance: IsolationInstanceId,
    isolation: TrialIsolationProfile,
    capability: TrialTransportCapability,
    terminal: TrialTransportTerminal,
    outcome: TerminalTrialOutcome,
}

impl IsolatedRunReceipt {
    pub fn collected(
        run: &TrialRunAssignment,
        effect: EffectId,
        lease_epoch: LeaseEpoch,
        outcome: TerminalTrialOutcome,
        runtime_receipt: Digest,
    ) -> Result<Self, TrialError> {
        Self::new(
            run,
            effect,
            lease_epoch,
            TrialTransportTerminal::Collected(runtime_receipt),
            outcome,
        )
    }

    pub fn fenced_unknown(
        run: &TrialRunAssignment,
        effect: EffectId,
        lease_epoch: LeaseEpoch,
        reason: InfrastructureUnknownReason,
        fence_receipt: Digest,
    ) -> Result<Self, TrialError> {
        Self::new(
            run,
            effect,
            lease_epoch,
            TrialTransportTerminal::Fenced(fence_receipt),
            TerminalTrialOutcome::InfrastructureUnknown { reason },
        )
    }

    fn new(
        run: &TrialRunAssignment,
        effect: EffectId,
        lease_epoch: LeaseEpoch,
        terminal: TrialTransportTerminal,
        outcome: TerminalTrialOutcome,
    ) -> Result<Self, TrialError> {
        run.key.validate()?;
        validate_terminal(terminal, &outcome)?;
        let id = receipt_id(
            run.key,
            effect,
            lease_epoch,
            run.isolation_instance,
            run.isolation,
            run.transport,
            terminal,
            &outcome,
        )?;
        Ok(Self {
            id,
            key: run.key,
            effect,
            lease_epoch,
            isolation_instance: run.isolation_instance,
            isolation: run.isolation,
            capability: run.transport,
            terminal,
            outcome,
        })
    }

    pub const fn id(&self) -> IsolatedRunReceiptId {
        self.id
    }

    pub const fn key(&self) -> TrialKey {
        self.key
    }

    pub const fn effect(&self) -> EffectId {
        self.effect
    }

    pub const fn lease_epoch(&self) -> LeaseEpoch {
        self.lease_epoch
    }

    pub const fn isolation_instance(&self) -> IsolationInstanceId {
        self.isolation_instance
    }

    pub const fn isolation(&self) -> TrialIsolationProfile {
        self.isolation
    }

    pub const fn capability(&self) -> TrialTransportCapability {
        self.capability
    }

    pub const fn terminal(&self) -> TrialTransportTerminal {
        self.terminal
    }

    pub const fn outcome(&self) -> &TerminalTrialOutcome {
        &self.outcome
    }

    /// A terminal attempt is never locally retryable. Campaign reconciliation owns retries.
    pub const fn retry_permitted(&self) -> bool {
        false
    }

    fn validate(&self) -> Result<(), TrialError> {
        self.key.validate()?;
        validate_terminal(self.terminal, &self.outcome)?;
        let expected = receipt_id(
            self.key,
            self.effect,
            self.lease_epoch,
            self.isolation_instance,
            self.isolation,
            self.capability,
            self.terminal,
            &self.outcome,
        )?;
        if self.id != expected {
            return Err(TrialError::TamperedRunReceipt);
        }
        Ok(())
    }
}

impl TryFrom<UncheckedIsolatedRunReceipt> for IsolatedRunReceipt {
    type Error = TrialError;

    fn try_from(value: UncheckedIsolatedRunReceipt) -> Result<Self, Self::Error> {
        let receipt = Self {
            id: value.id,
            key: value.key,
            effect: value.effect,
            lease_epoch: value.lease_epoch,
            isolation_instance: value.isolation_instance,
            isolation: value.isolation,
            capability: value.capability,
            terminal: value.terminal,
            outcome: value.outcome,
        };
        receipt.validate()?;
        Ok(receipt)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedTrialReceipt {
    id: PairedTrialReceiptId,
    pair: TrialPairId,
    effect: EffectId,
    lease_epoch: LeaseEpoch,
    parent_receipt: IsolatedRunReceiptId,
    candidate_receipt: IsolatedRunReceiptId,
    parent: BehavioralTrialOutcome,
    candidate: BehavioralTrialOutcome,
}

impl PairedTrialReceipt {
    pub const fn id(&self) -> PairedTrialReceiptId {
        self.id
    }

    pub const fn pair(&self) -> TrialPairId {
        self.pair
    }

    pub const fn effect(&self) -> EffectId {
        self.effect
    }

    pub const fn lease_epoch(&self) -> LeaseEpoch {
        self.lease_epoch
    }

    pub const fn parent_receipt(&self) -> IsolatedRunReceiptId {
        self.parent_receipt
    }

    pub const fn candidate_receipt(&self) -> IsolatedRunReceiptId {
        self.candidate_receipt
    }

    pub const fn parent(&self) -> &BehavioralTrialOutcome {
        &self.parent
    }

    pub const fn candidate(&self) -> &BehavioralTrialOutcome {
        &self.candidate
    }

    fn validate_for_pair(&self, expected: TrialPairSpec) -> Result<(), TrialError> {
        expected.validate()?;
        if self.pair != expected.id {
            return Err(TrialError::TamperedPairedReceipt);
        }
        let id = PairedTrialReceiptId::from_digest(Digest::of_value(&(
            "orvek:paired-trial-receipt:v1",
            expected.id,
            self.effect,
            self.lease_epoch,
            self.parent_receipt,
            self.candidate_receipt,
            &self.parent,
            &self.candidate,
        ))?);
        if self.id != id {
            return Err(TrialError::TamperedPairedReceipt);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedInfrastructureUnknown {
    pair: TrialPairId,
    reason: InfrastructureUnknownReason,
}

impl PairedInfrastructureUnknown {
    pub const fn pair(self) -> TrialPairId {
        self.pair
    }

    pub const fn reason(self) -> InfrastructureUnknownReason {
        self.reason
    }

    pub const fn retry_permitted(self) -> bool {
        false
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum PairedTrialEvidence {
    Usable(PairedTrialReceipt),
    InfrastructureUnknown(PairedInfrastructureUnknown),
}

impl PairedTrialEvidence {
    fn unknown(pair: TrialPairId, reason: InfrastructureUnknownReason) -> Self {
        Self::InfrastructureUnknown(PairedInfrastructureUnknown { pair, reason })
    }

    pub(super) fn validate_for_pair(&self, expected: TrialPairSpec) -> Result<(), TrialError> {
        expected.validate()?;
        match self {
            Self::Usable(receipt) => receipt.validate_for_pair(expected),
            Self::InfrastructureUnknown(unknown) if unknown.pair == expected.id => Ok(()),
            Self::InfrastructureUnknown(_) => Err(TrialError::TamperedPairedReceipt),
        }
    }
}

pub fn classify_paired_trial(
    expected: TrialPairSpec,
    parent: Option<&IsolatedRunReceipt>,
    candidate: Option<&IsolatedRunReceipt>,
) -> Result<PairedTrialEvidence, TrialError> {
    expected.validate()?;
    let (Some(parent), Some(candidate)) = (parent, candidate) else {
        return Ok(PairedTrialEvidence::unknown(
            expected.id,
            InfrastructureUnknownReason::MissingReceipt,
        ));
    };
    parent.validate()?;
    candidate.validate()?;

    let expected_parent = expected.key(TrialSide::Parent)?;
    let expected_candidate = expected.key(TrialSide::Candidate)?;
    for (expected_key, actual) in [
        (expected_parent, parent.key),
        (expected_candidate, candidate.key),
    ] {
        if expected_key != actual {
            return Ok(PairedTrialEvidence::unknown(
                expected.id,
                binding_mismatch(expected_key, actual),
            ));
        }
    }

    if parent.effect != candidate.effect || parent.lease_epoch != candidate.lease_epoch {
        return Ok(PairedTrialEvidence::unknown(
            expected.id,
            InfrastructureUnknownReason::TamperedReceipt,
        ));
    }
    if parent.isolation != TrialIsolationProfile::strict()
        || candidate.isolation != TrialIsolationProfile::strict()
        || parent.isolation_instance == candidate.isolation_instance
    {
        return Ok(PairedTrialEvidence::unknown(
            expected.id,
            InfrastructureUnknownReason::EnvironmentDrift,
        ));
    }
    if let Some(reason) = parent
        .outcome
        .unknown_reason()
        .or_else(|| candidate.outcome.unknown_reason())
    {
        return Ok(PairedTrialEvidence::unknown(expected.id, reason));
    }

    let parent_outcome = parent
        .outcome
        .behavioral()
        .ok_or(TrialError::NonBehavioralPair)?;
    let candidate_outcome = candidate
        .outcome
        .behavioral()
        .ok_or(TrialError::NonBehavioralPair)?;
    let id = PairedTrialReceiptId::from_digest(Digest::of_value(&(
        "orvek:paired-trial-receipt:v1",
        expected.id,
        parent.effect,
        parent.lease_epoch,
        parent.id,
        candidate.id,
        &parent_outcome,
        &candidate_outcome,
    ))?);
    Ok(PairedTrialEvidence::Usable(PairedTrialReceipt {
        id,
        pair: expected.id,
        effect: parent.effect,
        lease_epoch: parent.lease_epoch,
        parent_receipt: parent.id,
        candidate_receipt: candidate.id,
        parent: parent_outcome,
        candidate: candidate_outcome,
    }))
}

fn pair_id(
    context: TrialContext,
    block: IndependentBlockId,
    case: CaseIdentity,
    repeat: u32,
    input: InputCommitment,
) -> Result<TrialPairId, TrialError> {
    Ok(TrialPairId::from_digest(Digest::of_value(&(
        "orvek:trial-pair:v1",
        context,
        block,
        case,
        repeat,
        input,
    ))?))
}

fn key_id(pair: TrialPairSpec, side: TrialSide) -> Result<TrialKeyId, TrialError> {
    Ok(TrialKeyId::from_digest(Digest::of_value(&(
        "orvek:trial-key:v1",
        pair.id,
        pair.context,
        pair.block,
        pair.case,
        pair.repeat,
        side,
        pair.input,
    ))?))
}

#[allow(clippy::too_many_arguments)]
fn receipt_id(
    key: TrialKey,
    effect: EffectId,
    lease_epoch: LeaseEpoch,
    isolation_instance: IsolationInstanceId,
    isolation: TrialIsolationProfile,
    capability: TrialTransportCapability,
    terminal: TrialTransportTerminal,
    outcome: &TerminalTrialOutcome,
) -> Result<IsolatedRunReceiptId, TrialError> {
    Ok(IsolatedRunReceiptId::from_digest(Digest::of_value(&(
        "orvek:isolated-run-receipt:v1",
        key,
        effect,
        lease_epoch,
        isolation_instance,
        isolation,
        capability,
        terminal,
        outcome,
    ))?))
}

fn validate_terminal(
    terminal: TrialTransportTerminal,
    outcome: &TerminalTrialOutcome,
) -> Result<(), TrialError> {
    if matches!(terminal, TrialTransportTerminal::Fenced(_)) && outcome.unknown_reason().is_none() {
        return Err(TrialError::FencedAttemptMustBeUnknown);
    }
    Ok(())
}

fn binding_mismatch(expected: TrialKey, actual: TrialKey) -> InfrastructureUnknownReason {
    let expected_runtime = expected.context.runtime;
    let actual_runtime = actual.context.runtime;
    if expected_runtime.model != actual_runtime.model {
        InfrastructureUnknownReason::ModelDrift
    } else if expected_runtime.evaluator != actual_runtime.evaluator {
        InfrastructureUnknownReason::EvaluatorDrift
    } else if expected_runtime.environment != actual_runtime.environment
        || expected_runtime.protocol != actual_runtime.protocol
    {
        InfrastructureUnknownReason::EnvironmentDrift
    } else {
        InfrastructureUnknownReason::TamperedReceipt
    }
}

#[derive(Debug, Error)]
pub enum TrialError {
    #[error("partition epoch must be positive and within storage bounds")]
    InvalidPartitionEpoch,
    #[error("trial limits must all be positive")]
    InvalidLimits,
    #[error("metric name must be non-empty, bounded, and contain no control characters")]
    InvalidMetricName,
    #[error("metric score must be in the inclusive range 0..=1,000,000")]
    InvalidMetricScore,
    #[error("the trial pair identity does not match its frozen content")]
    TamperedPairIdentity,
    #[error("the trial key identity does not match its frozen content")]
    TamperedTrialKey,
    #[error("the isolated run receipt does not match its hash-bound content")]
    TamperedRunReceipt,
    #[error("the paired trial receipt does not match its hash-bound content")]
    TamperedPairedReceipt,
    #[error("a fenced attempt must terminate as infrastructure unknown")]
    FencedAttemptMustBeUnknown,
    #[error("an infrastructure-unknown outcome cannot form usable paired evidence")]
    NonBehavioralPair,
    #[error("trial evidence could not be canonicalized")]
    Canonicalization(#[from] serde_json::Error),
}
