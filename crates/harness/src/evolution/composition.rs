use super::{
    BoundedProposal, CandidateId, CompositionId, ManifestError, ProposalId, ScoreResultId,
    ValidatedHarnessRevision,
};
use crate::Digest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

const COMPOSITION_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositeField {
    Budgets,
    Instructions,
    RecoveryReminders,
    Skills,
    SubagentRoles,
    Verifier,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionFallback {
    KeepParent,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum CompositionFailureReason {
    FieldConflict {
        field: CompositeField,
        first: CandidateId,
        second: CandidateId,
    },
    CombinedManifestRejected,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionChild {
    candidate: CandidateId,
    proposal: ProposalId,
    score: ScoreResultId,
    patch: Digest,
    revision: Digest,
}

impl CompositionChild {
    pub const fn candidate(self) -> CandidateId {
        self.candidate
    }

    pub const fn proposal(self) -> ProposalId {
        self.proposal
    }

    pub const fn score(self) -> ScoreResultId {
        self.score
    }

    pub const fn patch(self) -> Digest {
        self.patch
    }

    pub const fn revision(self) -> Digest {
        self.revision
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompositePlan {
    schema_version: u32,
    id: CompositionId,
    candidate: CandidateId,
    parent: Digest,
    children: Vec<CompositionChild>,
    patch: Digest,
    revision: Digest,
    fallback: CompositionFallback,
}

impl CompositePlan {
    pub const fn id(&self) -> CompositionId {
        self.id
    }

    pub const fn candidate(&self) -> CandidateId {
        self.candidate
    }

    pub const fn parent(&self) -> Digest {
        self.parent
    }

    pub fn children(&self) -> &[CompositionChild] {
        &self.children
    }

    pub const fn patch(&self) -> Digest {
        self.patch
    }

    pub const fn revision(&self) -> Digest {
        self.revision
    }

    pub const fn fallback(&self) -> CompositionFallback {
        self.fallback
    }

    pub(crate) fn validate(&self) -> Result<(), CompositionError> {
        if self.schema_version != COMPOSITION_SCHEMA_VERSION {
            return Err(CompositionError::UnsupportedSchema {
                actual: self.schema_version,
            });
        }
        validate_children(&self.children)?;
        let identity = CompositionIdentity {
            schema_version: self.schema_version,
            parent: self.parent,
            children: &self.children,
            patch: self.patch,
            revision: self.revision,
            fallback: self.fallback,
        };
        let id = composition_id(&identity)?;
        if id != self.id || composite_candidate(id) != self.candidate {
            return Err(CompositionError::IdentityMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionFailure {
    schema_version: u32,
    id: CompositionId,
    candidate: CandidateId,
    parent: Digest,
    children: Vec<CompositionChild>,
    reason: CompositionFailureReason,
    fallback: CompositionFallback,
}

impl CompositionFailure {
    pub const fn id(&self) -> CompositionId {
        self.id
    }

    pub const fn candidate(&self) -> CandidateId {
        self.candidate
    }

    pub const fn parent(&self) -> Digest {
        self.parent
    }

    pub fn children(&self) -> &[CompositionChild] {
        &self.children
    }

    pub const fn reason(&self) -> CompositionFailureReason {
        self.reason
    }

    pub const fn fallback(&self) -> CompositionFallback {
        self.fallback
    }

    pub(crate) fn validate(&self) -> Result<(), CompositionError> {
        if self.schema_version != COMPOSITION_SCHEMA_VERSION {
            return Err(CompositionError::UnsupportedSchema {
                actual: self.schema_version,
            });
        }
        validate_children(&self.children)?;
        let identity = CompositionFailureIdentity {
            schema_version: self.schema_version,
            parent: self.parent,
            children: &self.children,
            reason: self.reason,
            fallback: self.fallback,
        };
        let id = composition_id(&identity)?;
        if id != self.id || composite_candidate(id) != self.candidate {
            return Err(CompositionError::IdentityMismatch);
        }
        Ok(())
    }

    pub(crate) fn from_inputs<'a>(
        parent: &ValidatedHarnessRevision,
        inputs: impl IntoIterator<Item = CompositionInput<'a>>,
        reason: CompositionFailureReason,
    ) -> Result<Self, CompositionError> {
        let (_, children) = canonical_children(parent, inputs)?;
        let fallback = CompositionFallback::KeepParent;
        let identity = CompositionFailureIdentity {
            schema_version: COMPOSITION_SCHEMA_VERSION,
            parent: parent.digest(),
            children: &children,
            reason,
            fallback,
        };
        let id = composition_id(&identity)?;
        let failure = Self {
            schema_version: COMPOSITION_SCHEMA_VERSION,
            id,
            candidate: composite_candidate(id),
            parent: parent.digest(),
            children,
            reason,
            fallback,
        };
        failure.validate()?;
        Ok(failure)
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct SelectedHarness {
    candidate: CandidateId,
    composition: CompositionId,
    revision: ValidatedHarnessRevision,
}

impl SelectedHarness {
    pub const fn candidate(&self) -> CandidateId {
        self.candidate
    }

    pub const fn composition(&self) -> CompositionId {
        self.composition
    }

    pub const fn revision(&self) -> &ValidatedHarnessRevision {
        &self.revision
    }
}

impl std::fmt::Debug for SelectedHarness {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SelectedHarness")
            .field("candidate", &self.candidate)
            .field("composition", &self.composition)
            .field("revision", &self.revision.digest())
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerifiedComposition {
    Composed(Box<ComposedHarness>),
    FellBack(CompositionFailure),
}

#[derive(Clone, Copy)]
pub struct CompositionInput<'a> {
    candidate: CandidateId,
    score: ScoreResultId,
    proposal: &'a BoundedProposal,
}

impl<'a> CompositionInput<'a> {
    pub const fn new(
        candidate: CandidateId,
        score: ScoreResultId,
        proposal: &'a BoundedProposal,
    ) -> Self {
        Self {
            candidate,
            score,
            proposal,
        }
    }

    pub const fn candidate(self) -> CandidateId {
        self.candidate
    }

    pub const fn score(self) -> ScoreResultId {
        self.score
    }

    pub const fn proposal(self) -> &'a BoundedProposal {
        self.proposal
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ComposedHarness {
    plan: CompositePlan,
    canonical_patch: Vec<u8>,
    revision: ValidatedHarnessRevision,
}

impl ComposedHarness {
    pub const fn plan(&self) -> &CompositePlan {
        &self.plan
    }

    pub fn canonical_patch_bytes(&self) -> &[u8] {
        &self.canonical_patch
    }

    pub const fn revision(&self) -> &ValidatedHarnessRevision {
        &self.revision
    }
}

impl std::fmt::Debug for ComposedHarness {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ComposedHarness")
            .field("plan", &self.plan)
            .field("revision", &self.revision.digest())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
pub enum CompositionError {
    #[error("a composite requires at least two verified candidates")]
    TooFewCandidates,
    #[error("candidate {candidate} occurs more than once in the composition")]
    DuplicateCandidate { candidate: CandidateId },
    #[error("proposal {proposal} occurs more than once in the composition")]
    DuplicateProposal { proposal: ProposalId },
    #[error("candidate {candidate} proposal is not based on the composition parent")]
    ParentMismatch { candidate: CandidateId },
    #[error("candidate {candidate} proposal revision differs from its canonical patch")]
    ProposalRevisionMismatch { candidate: CandidateId },
    #[error("candidates {first} and {second} replace {field:?} with different values")]
    FieldConflict {
        field: CompositeField,
        first: CandidateId,
        second: CandidateId,
    },
    #[error("composition patch is not canonical behavior JSON")]
    InvalidPatch(#[source] serde_json::Error),
    #[error("composition identity could not be canonicalized")]
    Canonicalization(#[source] serde_json::Error),
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("unsupported composition schema version {actual}")]
    UnsupportedSchema { actual: u32 },
    #[error("composition children are not in canonical order")]
    NonCanonicalChildren,
    #[error("composition identity differs from its canonical lineage")]
    IdentityMismatch,
}

#[derive(Serialize)]
struct CompositionIdentity<'a> {
    schema_version: u32,
    parent: Digest,
    children: &'a [CompositionChild],
    patch: Digest,
    revision: Digest,
    fallback: CompositionFallback,
}

#[derive(Serialize)]
struct CompositionFailureIdentity<'a> {
    schema_version: u32,
    parent: Digest,
    children: &'a [CompositionChild],
    reason: CompositionFailureReason,
    fallback: CompositionFallback,
}

#[derive(Serialize)]
struct SelectedCompositionIdentity {
    schema_version: u32,
    kind: &'static str,
    parent: Digest,
    candidate: CandidateId,
    proposal: ProposalId,
    score: ScoreResultId,
    revision: Digest,
}

pub fn compose_candidate(
    parent: &ValidatedHarnessRevision,
    input: CompositionInput<'_>,
) -> Result<SelectedHarness, CompositionError> {
    let revision = validate_input(parent, input)?;
    let composition = selected_composition_id(
        parent.digest(),
        input.candidate,
        input.proposal.id(),
        input.score,
        revision.digest(),
    )?;
    Ok(SelectedHarness {
        candidate: input.candidate,
        composition,
        revision,
    })
}

pub fn compose_candidates<'a>(
    parent: &ValidatedHarnessRevision,
    inputs: impl IntoIterator<Item = CompositionInput<'a>>,
) -> Result<ComposedHarness, CompositionError> {
    let (inputs, children) = canonical_children(parent, inputs)?;
    let mut merged = BTreeMap::<CompositeField, Value>::new();
    let mut owners = BTreeMap::<CompositeField, CandidateId>::new();
    for input in inputs {
        let patch = serde_json::from_slice::<BTreeMap<CompositeField, Value>>(
            input.proposal.canonical_patch_bytes(),
        )
        .map_err(CompositionError::InvalidPatch)?;
        for (field, value) in patch {
            if let Some(existing) = merged.get(&field) {
                if existing != &value {
                    return Err(CompositionError::FieldConflict {
                        field,
                        first: owners[&field],
                        second: input.candidate,
                    });
                }
            } else {
                owners.insert(field, input.candidate);
                merged.insert(field, value);
            }
        }
    }

    let canonical_patch =
        serde_json::to_vec(&merged).map_err(CompositionError::Canonicalization)?;
    let revision = parent.apply_patch_json(&canonical_patch)?;
    let patch = Digest::of(&canonical_patch);
    let fallback = CompositionFallback::KeepParent;
    let identity = CompositionIdentity {
        schema_version: COMPOSITION_SCHEMA_VERSION,
        parent: parent.digest(),
        children: &children,
        patch,
        revision: revision.digest(),
        fallback,
    };
    let id = composition_id(&identity)?;
    let plan = CompositePlan {
        schema_version: COMPOSITION_SCHEMA_VERSION,
        id,
        candidate: composite_candidate(id),
        parent: parent.digest(),
        children,
        patch,
        revision: revision.digest(),
        fallback,
    };
    plan.validate()?;
    Ok(ComposedHarness {
        plan,
        canonical_patch,
        revision,
    })
}

fn canonical_children<'a>(
    parent: &ValidatedHarnessRevision,
    inputs: impl IntoIterator<Item = CompositionInput<'a>>,
) -> Result<(Vec<CompositionInput<'a>>, Vec<CompositionChild>), CompositionError> {
    let mut inputs = inputs.into_iter().collect::<Vec<_>>();
    if inputs.len() < 2 {
        return Err(CompositionError::TooFewCandidates);
    }
    inputs.sort_by_key(|input| (input.candidate, input.proposal.id()));
    let mut candidates = BTreeSet::new();
    let mut proposals = BTreeSet::new();
    let mut children = Vec::with_capacity(inputs.len());
    for input in &inputs {
        if !candidates.insert(input.candidate) {
            return Err(CompositionError::DuplicateCandidate {
                candidate: input.candidate,
            });
        }
        if !proposals.insert(input.proposal.id()) {
            return Err(CompositionError::DuplicateProposal {
                proposal: input.proposal.id(),
            });
        }
        let revision = validate_input(parent, *input)?;
        children.push(CompositionChild {
            candidate: input.candidate,
            proposal: input.proposal.id(),
            score: input.score,
            patch: Digest::of(input.proposal.canonical_patch_bytes()),
            revision: revision.digest(),
        });
    }
    Ok((inputs, children))
}

fn validate_input(
    parent: &ValidatedHarnessRevision,
    input: CompositionInput<'_>,
) -> Result<ValidatedHarnessRevision, CompositionError> {
    if input.proposal.revision().parent() != parent.digest() {
        return Err(CompositionError::ParentMismatch {
            candidate: input.candidate,
        });
    }
    let reconstructed = parent.apply_patch_json(input.proposal.canonical_patch_bytes())?;
    if reconstructed.digest() != input.proposal.revision().digest() {
        return Err(CompositionError::ProposalRevisionMismatch {
            candidate: input.candidate,
        });
    }
    Ok(reconstructed)
}

fn validate_children(children: &[CompositionChild]) -> Result<(), CompositionError> {
    if children.len() < 2 {
        return Err(CompositionError::TooFewCandidates);
    }
    let mut previous = None;
    let mut proposals = BTreeSet::new();
    for child in children {
        if previous.is_some_and(|candidate| candidate >= child.candidate) {
            return Err(CompositionError::NonCanonicalChildren);
        }
        if !proposals.insert(child.proposal) {
            return Err(CompositionError::DuplicateProposal {
                proposal: child.proposal,
            });
        }
        previous = Some(child.candidate);
    }
    Ok(())
}

pub(crate) fn selected_composition_id(
    parent: Digest,
    candidate: CandidateId,
    proposal: ProposalId,
    score: ScoreResultId,
    revision: Digest,
) -> Result<CompositionId, CompositionError> {
    let identity = SelectedCompositionIdentity {
        schema_version: COMPOSITION_SCHEMA_VERSION,
        kind: "selected_candidate",
        parent,
        candidate,
        proposal,
        score,
        revision,
    };
    serde_json::to_vec(&identity)
        .map(|canonical| CompositionId::from_digest(Digest::of(&canonical)))
        .map_err(CompositionError::Canonicalization)
}

fn composition_id(identity: &impl Serialize) -> Result<CompositionId, CompositionError> {
    serde_json::to_vec(identity)
        .map(|canonical| CompositionId::from_digest(Digest::of(&canonical)))
        .map_err(CompositionError::Canonicalization)
}

fn composite_candidate(composition: CompositionId) -> CandidateId {
    CandidateId::from_digest(Digest::of(
        format!("orvek:composite-candidate:v1:{composition}").as_bytes(),
    ))
}
