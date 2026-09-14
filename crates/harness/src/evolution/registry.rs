use super::FrozenScoringPolicy;
use crate::Digest;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt, str::FromStr};
use uuid::Uuid;

macro_rules! digest_identity {
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

        impl From<Digest> for $name {
            fn from(digest: Digest) -> Self {
                Self::from_digest(digest)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = &'static str;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }
    };
}

digest_identity!(ModelIdentity);
digest_identity!(ProtocolIdentity);
digest_identity!(EnvironmentIdentity);
digest_identity!(TaskProfileIdentity);
digest_identity!(EvaluatorIdentity);
digest_identity!(PolicyIdentity);
digest_identity!(PartitionCommitment);
digest_identity!(IndependentBlockId);
digest_identity!(CaseIdentity);
digest_identity!(TaskIdentity);

macro_rules! uuid_identity {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub const fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

uuid_identity!(CampaignId);
uuid_identity!(CohortId);
uuid_identity!(AuditEpochId);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Canary,
    Stable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaselineReason {
    UnregisteredTarget,
    LegacyImport,
    StoreFixture,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum HarnessProvenance {
    Registered,
    CompiledBaseline { reason: BaselineReason },
}

impl Channel {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Canary => "canary",
            Self::Stable => "stable",
        }
    }
}

impl FromStr for Channel {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "canary" => Ok(Self::Canary),
            "stable" => Ok(Self::Stable),
            _ => Err("unknown harness channel"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetProfile {
    pub model: ModelIdentity,
    pub protocol: ProtocolIdentity,
    pub environment: EnvironmentIdentity,
    pub task_profile: TaskProfileIdentity,
    pub channel: Channel,
}

impl TargetProfile {
    pub const fn new(
        model: ModelIdentity,
        protocol: ProtocolIdentity,
        environment: EnvironmentIdentity,
        task_profile: TaskProfileIdentity,
        channel: Channel,
    ) -> Self {
        Self {
            model,
            protocol,
            environment,
            task_profile,
            channel,
        }
    }
}

/// An immutable registry result. It contains identities, not manifest authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessBinding {
    target: TargetProfile,
    revision: Digest,
    behavior: Digest,
    envelope: Digest,
    policy: PolicyIdentity,
}

impl HarnessBinding {
    pub const fn target(self) -> TargetProfile {
        self.target
    }

    pub const fn revision(self) -> Digest {
        self.revision
    }

    pub const fn behavior(self) -> Digest {
        self.behavior
    }

    pub const fn envelope(self) -> Digest {
        self.envelope
    }

    pub const fn policy(self) -> PolicyIdentity {
        self.policy
    }

    pub(crate) const fn registered(
        target: TargetProfile,
        revision: Digest,
        behavior: Digest,
        envelope: Digest,
        policy: PolicyIdentity,
    ) -> Self {
        Self {
            target,
            revision,
            behavior,
            envelope,
            policy,
        }
    }

    pub(crate) const fn baseline(
        target: TargetProfile,
        revision: Digest,
        behavior: Digest,
        envelope: Digest,
        policy: PolicyIdentity,
    ) -> Self {
        Self::registered(target, revision, behavior, envelope, policy)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionCommitments {
    pub mining: PartitionCommitment,
    pub adaptive_promotion: PartitionCommitment,
    pub final_audit: PartitionCommitment,
}

impl PartitionCommitments {
    pub(crate) fn validate(self) -> Result<(), &'static str> {
        let distinct = BTreeSet::from([
            self.mining.digest(),
            self.adaptive_promotion.digest(),
            self.final_audit.digest(),
        ]);
        if distinct.len() != 3 {
            return Err("cohort partition commitments must be distinct");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationCase {
    pub id: CaseIdentity,
    pub task: TaskIdentity,
    pub repeats: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndependentBlock {
    pub id: IndependentBlockId,
    pub cases: Vec<EvaluationCase>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerLimit {
    pub queries: u64,
    pub error_nanos: u64,
}

impl LedgerLimit {
    pub const fn new(queries: u64, error_nanos: u64) -> Self {
        Self {
            queries,
            error_nanos,
        }
    }

    pub(crate) fn validate(self) -> Result<(), &'static str> {
        if self.queries == 0 || self.error_nanos == 0 {
            return Err("cohort ledger limits must be positive");
        }
        if self.queries > i64::MAX as u64 || self.error_nanos > i64::MAX as u64 {
            return Err("cohort ledger limits exceed storage bounds");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationCohortSpec {
    pub id: CohortId,
    pub target: TargetProfile,
    pub base_revision: Digest,
    pub evaluator: EvaluatorIdentity,
    pub policy: PolicyIdentity,
    pub partitions: PartitionCommitments,
    pub blocks: Vec<IndependentBlock>,
    pub adaptive_promotion: LedgerLimit,
    pub final_audit: LedgerLimit,
    pub audit_epoch: AuditEpochId,
    pub scoring: FrozenScoringPolicy,
}

impl EvaluationCohortSpec {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        self.partitions.validate()?;
        self.adaptive_promotion.validate()?;
        self.final_audit.validate()?;
        if self.blocks.is_empty() {
            return Err("evaluation cohort must contain independent blocks");
        }

        let mut blocks = BTreeSet::new();
        let mut cases = BTreeSet::new();
        for block in &self.blocks {
            if !blocks.insert(block.id) {
                return Err("independent block identities must be unique");
            }
            if block.cases.is_empty() {
                return Err("independent block must contain cases");
            }
            for case in &block.cases {
                if case.repeats == 0 {
                    return Err("evaluation case repeats must be positive");
                }
                if !cases.insert(case.id) {
                    return Err("evaluation case identities must be unique");
                }
            }
        }
        self.scoring
            .validate(self)
            .map_err(|_| "evaluation cohort scoring policy is invalid")?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CohortLedger {
    AdaptivePromotion,
    FinalAudit,
}

impl CohortLedger {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::AdaptivePromotion => "adaptive_promotion",
            Self::FinalAudit => "final_audit",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LedgerDebit {
    pub(crate) campaign: CampaignId,
    pub(crate) use_id: Digest,
    pub(crate) queries: u64,
    pub(crate) error_nanos: u64,
}

impl LedgerDebit {
    pub(crate) fn validate(self) -> Result<(), &'static str> {
        if self.queries == 0 && self.error_nanos == 0 {
            return Err("cohort ledger debit must spend queries or error");
        }
        if self.queries > i64::MAX as u64 || self.error_nanos > i64::MAX as u64 {
            return Err("cohort ledger debit exceeds storage bounds");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerStatus {
    pub query_limit: u64,
    pub query_used: u64,
    pub error_limit_nanos: u64,
    pub error_used_nanos: u64,
}

impl LedgerStatus {
    pub const fn queries_remaining(self) -> u64 {
        self.query_limit - self.query_used
    }

    pub const fn error_remaining_nanos(self) -> u64 {
        self.error_limit_nanos - self.error_used_nanos
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditEpochStatus {
    Active,
    Retired,
}
