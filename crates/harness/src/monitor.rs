//! Descriptive trace monitoring and one deterministic configuration recovery.
//! No model quality inference, generated code execution, or live policy changes.
use crate::{Digest, TargetProfile, inference::UsdCost};
use serde::{Deserialize, Serialize};

pub(crate) mod evaluation;
pub const DEFAULT_READ_OUTPUT_BYTES: u32 = 32768;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedBehavior {
    pub release: Digest,
    pub native_read_output_bytes: u32,
    pub build: Option<Digest>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub parent: Option<Digest>,
    pub native_read_output_bytes: u32,
    /// Operator provenance only, never an executable instruction.
    pub note: String,
}
impl Release {
    pub(crate) fn baseline() -> Self {
        Self {
            parent: None,
            native_read_output_bytes: DEFAULT_READ_OUTPUT_BYTES,
            note: "compiled native read envelope".into(),
        }
    }
    pub fn id(&self) -> Digest {
        Digest::of_value(self).expect("release serializes")
    }
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if !(4096..=DEFAULT_READ_OUTPUT_BYTES).contains(&self.native_read_output_bytes)
            || self.note.len() > 1024
        {
            return Err("read envelope must be 4096..32768 bytes; provenance at most 1024 bytes");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Cohort {
    pub target: TargetProfile,
    pub build: Option<Digest>,
    pub behavior: Digest,
    pub release: Option<Digest>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    User,
    Repair,
    Evaluation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signature {
    JobOutcome,
    UnresolvedEffect,
    UnresolvedJob,
    ProviderError,
    MissingUsage,
    VerificationFailure,
    UserCorrection,
    ReadUnderfill,
    EvaluatorFailure,
    Cost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalClass {
    Operational,
    TaskQualitySignal,
    EvaluatorFailure,
}
impl Signature {
    pub const fn class(self) -> SignalClass {
        match self {
            Self::VerificationFailure | Self::UserCorrection | Self::ReadUnderfill => {
                SignalClass::TaskQualitySignal
            }
            Self::EvaluatorFailure => SignalClass::EvaluatorFailure,
            _ => SignalClass::Operational,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Measure {
    pub class: SignalClass,
    pub cohort: Cohort,
    pub signature: Signature,
    pub opportunities: u64,
    pub failures: u64,
    pub measured: u64,
    pub recorded_usd: UsdCost,
}
impl Measure {
    pub(crate) fn new(
        cohort: Cohort,
        signature: Signature,
        opportunities: u64,
        failures: u64,
        measured: u64,
    ) -> Self {
        Self {
            class: signature.class(),
            cohort,
            signature,
            opportunities,
            failures,
            measured,
            recorded_usd: UsdCost::ZERO,
        }
    }
}

/// Replaceable contribution from one durable opportunity (including reconciliation).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Measurement {
    pub identity: Digest,
    pub value: Measure,
}
impl Measurement {
    pub(crate) fn new(identity: Digest, value: Measure) -> Self {
        Self { identity, value }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Sampling {
    pub considered: u64,
    pub selected: u64,
    pub skipped_sampling: u64,
    pub skipped_origin: u64,
    pub skipped_no_hypothesis: u64,
    pub skipped_capacity: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MonitorStatus {
    pub cursor: u64,
    pub sample_every: u32,
    pub sampling: Sampling,
    pub last_error: Option<String>,
    pub active: Digest,
    pub previous: Option<Digest>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadCase {
    pub bytes: Vec<u8>,
    pub offset: usize,
    pub max_bytes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EpisodeState {
    Diagnosed,
    Evaluating,
    Rejected { reason: String },
    Uncertain { reason: String },
    Promoted { release: Digest },
    Superseded,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Episode {
    pub id: Digest,
    pub origin: Origin,
    pub sequence: u64,
    pub cohort: Cohort,
    pub regressed: Digest,
    pub parent: Digest,
    pub hypothesis: String,
    pub source_diff: Digest,
    pub trace_receipt: Digest,
    pub regression: Digest,
    pub heldout: Digest,
    pub candidate: Option<Digest>,
    pub result: Option<Digest>,
    pub state: EpisodeState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MonitorReport {
    pub status: MonitorStatus,
    /// Page of cohort/signature counters, not pooled across model/build/environment.
    pub measures: Vec<Measure>,
    pub episodes: Vec<Episode>,
    pub comparisons: Vec<Comparison>,
    pub next: Option<usize>,
    pub interpretation: String,
}

/// A descriptive pair, never a significance test or a promotion gate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Comparison {
    pub signature: Signature,
    pub before: Measure,
    pub after: Measure,
    pub assessment: String,
}
