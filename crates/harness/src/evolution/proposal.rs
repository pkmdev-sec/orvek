use super::{
    ManifestError, MiningBundleRoot, ModelIdentity, PolicyIdentity, ProposalId, ProtocolIdentity,
    ValidatedHarnessRevision,
};
use crate::Digest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};
use thiserror::Error;

const PROPOSAL_SCHEMA_VERSION: u32 = 1;
const MIN_CANDIDATES: u16 = 2;
const MAX_CANDIDATES: u16 = 16;
const MAX_PATCH_BYTES: usize = 64 * 1024;
const MAX_PROVIDER_ENVELOPE_BYTES: usize = 96 * 1024;

macro_rules! proposal_digest_id {
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

proposal_digest_id!(ProposalRequestRoot);
proposal_digest_id!(ProposalIntentId);
proposal_digest_id!(ProposalProviderReceiptId);
proposal_digest_id!(ProposalBatchRoot);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalRequest {
    root: ProposalRequestRoot,
    parent: Digest,
    evidence: MiningBundleRoot,
    policy: PolicyIdentity,
    model: ModelIdentity,
    protocol: ProtocolIdentity,
    candidate_count: u16,
    max_patch_bytes: usize,
}

impl ProposalRequest {
    pub fn new(
        parent: Digest,
        evidence: MiningBundleRoot,
        policy: PolicyIdentity,
        model: ModelIdentity,
        protocol: ProtocolIdentity,
        candidate_count: u16,
        max_patch_bytes: usize,
    ) -> Result<Self, ProposalError> {
        if !(MIN_CANDIDATES..=MAX_CANDIDATES).contains(&candidate_count) {
            return Err(ProposalError::InvalidCandidateCount {
                minimum: MIN_CANDIDATES,
                maximum: MAX_CANDIDATES,
            });
        }
        if max_patch_bytes == 0 || max_patch_bytes > MAX_PATCH_BYTES {
            return Err(ProposalError::InvalidPatchLimit {
                maximum: MAX_PATCH_BYTES,
            });
        }
        let identity = ProposalRequestIdentity {
            schema_version: PROPOSAL_SCHEMA_VERSION,
            parent,
            evidence,
            policy,
            model,
            protocol,
            candidate_count,
            max_patch_bytes,
        };
        let root = ProposalRequestRoot::from_digest(
            Digest::of_value(&identity).map_err(ProposalError::Canonicalization)?,
        );
        Ok(Self {
            root,
            parent,
            evidence,
            policy,
            model,
            protocol,
            candidate_count,
            max_patch_bytes,
        })
    }

    pub const fn root(self) -> ProposalRequestRoot {
        self.root
    }

    pub const fn parent(self) -> Digest {
        self.parent
    }

    pub const fn evidence(self) -> MiningBundleRoot {
        self.evidence
    }

    pub const fn policy(self) -> PolicyIdentity {
        self.policy
    }

    pub const fn model(self) -> ModelIdentity {
        self.model
    }

    pub const fn protocol(self) -> ProtocolIdentity {
        self.protocol
    }

    pub const fn candidate_count(self) -> u16 {
        self.candidate_count
    }

    pub const fn max_patch_bytes(self) -> usize {
        self.max_patch_bytes
    }

    pub fn intents(self) -> Vec<ProposalIntent> {
        (0..self.candidate_count)
            .map(|index| ProposalIntent::derive(self.root, index))
            .collect()
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ProposalRequestIdentity {
    schema_version: u32,
    parent: Digest,
    evidence: MiningBundleRoot,
    policy: PolicyIdentity,
    model: ModelIdentity,
    protocol: ProtocolIdentity,
    candidate_count: u16,
    max_patch_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalIntent {
    id: ProposalIntentId,
    request: ProposalRequestRoot,
    index: u16,
}

impl ProposalIntent {
    fn derive(request: ProposalRequestRoot, index: u16) -> Self {
        let identity = format!("orvek:proposal-intent:v1:{request}:{index}");
        Self {
            id: ProposalIntentId::from_digest(Digest::of(identity.as_bytes())),
            request,
            index,
        }
    }

    pub const fn id(self) -> ProposalIntentId {
        self.id
    }

    pub const fn request(self) -> ProposalRequestRoot {
        self.request
    }

    pub const fn index(self) -> u16 {
        self.index
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderUnknownReceipt {
    receipt: ProposalProviderReceiptId,
    charged_tokens: u64,
}

impl ProviderUnknownReceipt {
    pub const fn receipt(self) -> ProposalProviderReceiptId {
        self.receipt
    }

    pub const fn charged_tokens(self) -> u64 {
        self.charged_tokens
    }
}

enum ProposalAttemptOutcome {
    Settled {
        receipt: ProposalProviderReceiptId,
        payload: Vec<u8>,
    },
    InfrastructureUnknown(ProviderUnknownReceipt),
}

pub struct ProposalAttempt {
    intent: ProposalIntent,
    outcome: ProposalAttemptOutcome,
}

impl ProposalAttempt {
    pub fn settled(
        intent: ProposalIntent,
        receipt: ProposalProviderReceiptId,
        payload: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            intent,
            outcome: ProposalAttemptOutcome::Settled {
                receipt,
                payload: payload.into(),
            },
        }
    }

    pub fn infrastructure_unknown(
        intent: ProposalIntent,
        receipt: ProposalProviderReceiptId,
        charged_tokens: u64,
    ) -> Result<Self, ProposalError> {
        if charged_tokens == 0 {
            return Err(ProposalError::EmptyUnknownCharge);
        }
        Ok(Self {
            intent,
            outcome: ProposalAttemptOutcome::InfrastructureUnknown(ProviderUnknownReceipt {
                receipt,
                charged_tokens,
            }),
        })
    }

    pub const fn intent(&self) -> ProposalIntent {
        self.intent
    }

    /// A billable intent is reconciled by receipt; callers must not regenerate it blindly.
    pub const fn retry_permitted(&self) -> bool {
        false
    }

    pub const fn infrastructure_unknown_receipt(&self) -> Option<ProviderUnknownReceipt> {
        match &self.outcome {
            ProposalAttemptOutcome::InfrastructureUnknown(receipt) => Some(*receipt),
            ProposalAttemptOutcome::Settled { .. } => None,
        }
    }
}

impl fmt::Debug for ProposalAttempt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = match &self.outcome {
            ProposalAttemptOutcome::Settled { .. } => "settled",
            ProposalAttemptOutcome::InfrastructureUnknown(_) => "infrastructure_unknown",
        };
        formatter
            .debug_struct("ProposalAttempt")
            .field("intent", &self.intent)
            .field("status", &status)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiversityDimension {
    Budgets,
    Instructions,
    RecoveryReminders,
    Skills,
    SubagentRoles,
    Verifier,
}

#[derive(Clone, Eq, PartialEq)]
pub struct BoundedProposal {
    id: ProposalId,
    intent: ProposalIntentId,
    provider_receipt: ProposalProviderReceiptId,
    dimensions: BTreeSet<DiversityDimension>,
    canonical_patch: Vec<u8>,
    revision: ValidatedHarnessRevision,
}

impl BoundedProposal {
    pub const fn id(&self) -> ProposalId {
        self.id
    }

    pub const fn intent(&self) -> ProposalIntentId {
        self.intent
    }

    pub const fn provider_receipt(&self) -> ProposalProviderReceiptId {
        self.provider_receipt
    }

    pub fn dimensions(&self) -> &BTreeSet<DiversityDimension> {
        &self.dimensions
    }

    pub fn canonical_patch_bytes(&self) -> &[u8] {
        &self.canonical_patch
    }

    pub const fn revision(&self) -> &ValidatedHarnessRevision {
        &self.revision
    }
}

impl fmt::Debug for BoundedProposal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundedProposal")
            .field("id", &self.id)
            .field("intent", &self.intent)
            .field("provider_receipt", &self.provider_receipt)
            .field("dimensions", &self.dimensions)
            .field("revision", &self.revision.digest())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ProposalBatch {
    request: ProposalRequest,
    proposals: Vec<BoundedProposal>,
    root: ProposalBatchRoot,
    canonical: Vec<u8>,
}

impl ProposalBatch {
    pub const fn request(&self) -> ProposalRequest {
        self.request
    }

    pub fn proposals(&self) -> &[BoundedProposal] {
        &self.proposals
    }

    pub const fn root(&self) -> ProposalBatchRoot {
        self.root
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical
    }
}

impl fmt::Debug for ProposalBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProposalBatch")
            .field("request", &self.request.root)
            .field("proposals", &self.proposals.len())
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

pub fn validate_proposal_batch(
    request: ProposalRequest,
    parent: &ValidatedHarnessRevision,
    attempts: impl IntoIterator<Item = ProposalAttempt>,
) -> Result<ProposalBatch, ProposalError> {
    if request.parent != parent.digest() {
        return Err(ProposalError::RequestParentMismatch);
    }
    if request.policy != parent.policy_identity() {
        return Err(ProposalError::RequestPolicyMismatch);
    }

    let attempts = attempts.into_iter().collect::<Vec<_>>();
    if attempts.len() != usize::from(request.candidate_count) {
        return Err(ProposalError::WrongAttemptCount {
            expected: usize::from(request.candidate_count),
            actual: attempts.len(),
        });
    }
    let expected = request
        .intents()
        .into_iter()
        .map(|intent| (intent.id(), intent))
        .collect::<BTreeMap<_, _>>();
    let mut ordered_attempts = BTreeMap::new();
    for attempt in attempts {
        let intent = attempt.intent;
        if expected.get(&intent.id()) != Some(&intent) {
            return Err(ProposalError::UnexpectedIntent);
        }
        if ordered_attempts.insert(intent.id(), attempt).is_some() {
            return Err(ProposalError::DuplicateIntent);
        }
    }

    let mut proposals = Vec::with_capacity(ordered_attempts.len());
    let mut revisions = BTreeSet::new();
    for (_, attempt) in ordered_attempts {
        let (receipt, payload) = match attempt.outcome {
            ProposalAttemptOutcome::Settled { receipt, payload } => (receipt, payload),
            ProposalAttemptOutcome::InfrastructureUnknown(unknown) => {
                return Err(ProposalError::ProviderUnknown {
                    intent: attempt.intent.id(),
                    receipt: unknown.receipt,
                    charged_tokens: unknown.charged_tokens,
                });
            }
        };
        let proposal = validate_settled_output(request, parent, attempt.intent, receipt, &payload)?;
        if !revisions.insert(proposal.revision.behavior_digest()) {
            return Err(ProposalError::DuplicateCandidate);
        }
        proposals.push(proposal);
    }
    proposals.sort_unstable_by_key(BoundedProposal::id);

    let records = proposals
        .iter()
        .map(ProposalRecord::from)
        .collect::<Vec<_>>();
    let content = ProposalBatchContent {
        schema_version: PROPOSAL_SCHEMA_VERSION,
        request: request.root,
        proposals: records,
    };
    let canonical = serde_json::to_vec(&content).map_err(ProposalError::Canonicalization)?;
    let root = ProposalBatchRoot::from_digest(Digest::of(&canonical));
    Ok(ProposalBatch {
        request,
        proposals,
        root,
        canonical,
    })
}

fn validate_settled_output(
    request: ProposalRequest,
    parent: &ValidatedHarnessRevision,
    intent: ProposalIntent,
    receipt: ProposalProviderReceiptId,
    payload: &[u8],
) -> Result<BoundedProposal, ProposalError> {
    if payload.len() > MAX_PROVIDER_ENVELOPE_BYTES {
        return Err(ProposalError::ProviderOutputTooLarge {
            maximum: MAX_PROVIDER_ENVELOPE_BYTES,
        });
    }
    let envelope: ProviderProposalEnvelope =
        serde_json::from_slice(payload).map_err(ProposalError::InvalidProviderOutput)?;
    if envelope.schema_version != PROPOSAL_SCHEMA_VERSION {
        return Err(ProposalError::UnsupportedSchema {
            actual: envelope.schema_version,
        });
    }
    if envelope.request != request.root {
        return Err(ProposalError::OutputBindingMismatch { field: "request" });
    }
    if envelope.parent != request.parent {
        return Err(ProposalError::OutputBindingMismatch { field: "parent" });
    }
    if envelope.evidence != request.evidence {
        return Err(ProposalError::OutputBindingMismatch { field: "evidence" });
    }
    if envelope.policy != request.policy {
        return Err(ProposalError::OutputBindingMismatch { field: "policy" });
    }
    if envelope.intent != intent.id() {
        return Err(ProposalError::OutputBindingMismatch { field: "intent" });
    }

    let dimensions = envelope.dimensions.iter().copied().collect::<BTreeSet<_>>();
    if dimensions.is_empty() || dimensions.len() != envelope.dimensions.len() {
        return Err(ProposalError::InvalidDiversityDimensions);
    }
    let patch = envelope.patch.into_map();
    let actual_dimensions = patch
        .keys()
        .map(|field| dimension_for_field(field).ok_or(ProposalError::ForbiddenPatchField))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if actual_dimensions != dimensions {
        return Err(ProposalError::DiversityMismatch);
    }

    let parent_json: Value = serde_json::from_slice(parent.canonical_bytes())
        .map_err(ProposalError::Canonicalization)?;
    let behavior = parent_json
        .get("behavior")
        .and_then(Value::as_object)
        .ok_or(ProposalError::InvalidParentManifest)?;
    for (field, value) in &patch {
        if behavior.get(field) == Some(value) {
            return Err(ProposalError::RedundantPatchField {
                field: dimension_for_field(field).expect("patch fields were checked"),
            });
        }
    }

    let canonical_patch = serde_json::to_vec(&patch).map_err(ProposalError::Canonicalization)?;
    if canonical_patch.len() > request.max_patch_bytes {
        return Err(ProposalError::PatchTooLarge {
            maximum: request.max_patch_bytes,
        });
    }
    let revision = parent.apply_patch_json(&canonical_patch)?;
    if revision.behavior_digest() == parent.behavior_digest() {
        return Err(ProposalError::NoOpPatch);
    }
    let identity = ProposalIdentity {
        schema_version: PROPOSAL_SCHEMA_VERSION,
        request: request.root,
        intent: intent.id(),
        provider_receipt: receipt,
        dimensions: &dimensions,
        patch: Digest::of(&canonical_patch),
        revision: revision.digest(),
    };
    let id = ProposalId::from_digest(
        Digest::of_value(&identity).map_err(ProposalError::Canonicalization)?,
    );
    Ok(BoundedProposal {
        id,
        intent: intent.id(),
        provider_receipt: receipt,
        dimensions,
        canonical_patch,
        revision,
    })
}

fn dimension_for_field(field: &str) -> Option<DiversityDimension> {
    match field {
        "budgets" => Some(DiversityDimension::Budgets),
        "instructions" => Some(DiversityDimension::Instructions),
        "recovery_reminders" => Some(DiversityDimension::RecoveryReminders),
        "skills" => Some(DiversityDimension::Skills),
        "subagent_roles" => Some(DiversityDimension::SubagentRoles),
        "verifier" => Some(DiversityDimension::Verifier),
        _ => None,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderProposalEnvelope {
    schema_version: u32,
    request: ProposalRequestRoot,
    intent: ProposalIntentId,
    parent: Digest,
    evidence: MiningBundleRoot,
    policy: PolicyIdentity,
    dimensions: Vec<DiversityDimension>,
    patch: ProviderManifestPatch,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProviderManifestPatch {
    budgets: Option<Value>,
    instructions: Option<Value>,
    recovery_reminders: Option<Value>,
    skills: Option<Value>,
    subagent_roles: Option<Value>,
    verifier: Option<Value>,
}

impl ProviderManifestPatch {
    fn into_map(self) -> BTreeMap<String, Value> {
        let mut fields = BTreeMap::new();
        for (name, value) in [
            ("budgets", self.budgets),
            ("instructions", self.instructions),
            ("recovery_reminders", self.recovery_reminders),
            ("skills", self.skills),
            ("subagent_roles", self.subagent_roles),
            ("verifier", self.verifier),
        ] {
            if let Some(value) = value {
                fields.insert(name.to_owned(), value);
            }
        }
        fields
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ProposalIdentity<'a> {
    schema_version: u32,
    request: ProposalRequestRoot,
    intent: ProposalIntentId,
    provider_receipt: ProposalProviderReceiptId,
    dimensions: &'a BTreeSet<DiversityDimension>,
    patch: Digest,
    revision: Digest,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ProposalRecord<'a> {
    id: ProposalId,
    intent: ProposalIntentId,
    provider_receipt: ProposalProviderReceiptId,
    dimensions: &'a BTreeSet<DiversityDimension>,
    patch: Digest,
    revision: Digest,
}

impl<'a> From<&'a BoundedProposal> for ProposalRecord<'a> {
    fn from(value: &'a BoundedProposal) -> Self {
        Self {
            id: value.id,
            intent: value.intent,
            provider_receipt: value.provider_receipt,
            dimensions: &value.dimensions,
            patch: Digest::of(&value.canonical_patch),
            revision: value.revision.digest(),
        }
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ProposalBatchContent<'a> {
    schema_version: u32,
    request: ProposalRequestRoot,
    proposals: Vec<ProposalRecord<'a>>,
}

#[derive(Debug, Error)]
pub enum ProposalError {
    #[error("proposal count must be between {minimum} and {maximum}")]
    InvalidCandidateCount { minimum: u16, maximum: u16 },
    #[error("proposal patch limit must be between 1 and {maximum} bytes")]
    InvalidPatchLimit { maximum: usize },
    #[error("proposal request parent does not match the supplied revision")]
    RequestParentMismatch,
    #[error("proposal request policy does not match the supplied revision")]
    RequestPolicyMismatch,
    #[error("proposal batch expected {expected} attempts but received {actual}")]
    WrongAttemptCount { expected: usize, actual: usize },
    #[error("proposal batch contains an intent outside its request")]
    UnexpectedIntent,
    #[error("proposal batch contains the same intent more than once")]
    DuplicateIntent,
    #[error("provider attempt is unknown and must be reconciled without retry")]
    ProviderUnknown {
        intent: ProposalIntentId,
        receipt: ProposalProviderReceiptId,
        charged_tokens: u64,
    },
    #[error("an infrastructure-unknown proposal attempt must charge a conservative token budget")]
    EmptyUnknownCharge,
    #[error("provider proposal output exceeds the maximum of {maximum} bytes")]
    ProviderOutputTooLarge { maximum: usize },
    #[error("provider proposal output is malformed")]
    InvalidProviderOutput(#[source] serde_json::Error),
    #[error("unsupported proposal schema version {actual}")]
    UnsupportedSchema { actual: u32 },
    #[error("provider proposal {field} binding does not match its request")]
    OutputBindingMismatch { field: &'static str },
    #[error("proposal diversity dimensions must be non-empty and distinct")]
    InvalidDiversityDimensions,
    #[error("proposal patch contains a field outside the behavior envelope")]
    ForbiddenPatchField,
    #[error("proposal diversity dimensions do not match its patch fields")]
    DiversityMismatch,
    #[error("proposal patch redundantly includes an unchanged {field:?} field")]
    RedundantPatchField { field: DiversityDimension },
    #[error("stored parent manifest does not contain a behavior object")]
    InvalidParentManifest,
    #[error("proposal patch exceeds its request limit of {maximum} bytes")]
    PatchTooLarge { maximum: usize },
    #[error("proposal patch does not change behavior")]
    NoOpPatch,
    #[error("proposal batch contains duplicate resulting behavior")]
    DuplicateCandidate,
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("proposal data could not be canonicalized")]
    Canonicalization(#[source] serde_json::Error),
}
