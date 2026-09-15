use super::{
    CausalStatus, FailureMechanism, MiningBundleRoot, MiningError, MiningEvidenceId,
    MiningFailureEvidence, MiningObservation, MiningObservationId, MiningPassEvidence,
    RedactionSummary, SanitizedEvidenceText, TerminalCause,
};
use crate::Digest;
use serde::Serialize;
use std::collections::BTreeMap;

const SIGNATURE_VERSION: u32 = 1;
const MAX_RAW_TEXT_BYTES: usize = 64 * 1024;
const MAX_SANITIZED_TEXT_BYTES: usize = 4 * 1024;
const MAX_PROTECTED_TERMS: usize = 128;
const MAX_PROTECTED_TERM_BYTES: usize = 4 * 1024;
const MAX_OBSERVATIONS: usize = 16_384;
const MAX_CLUSTERS: usize = 512;
const MAX_CLUSTER_MEMBERS: usize = 16_384;
const MAX_PASS_ANCHORS: usize = 2_048;
const REDACTED: &str = "[REDACTED]";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactSource {
    VerifierReceipt,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MechanismSource {
    BoundedClassifier,
    Unclassified,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FailureSignature {
    version: u32,
    terminal_cause_source: FactSource,
    terminal_cause: TerminalCause,
    causal_status_source: FactSource,
    causal_status: CausalStatus,
    mechanism_source: MechanismSource,
    abstract_mechanism: FailureMechanism,
}

impl FailureSignature {
    fn from_failure(failure: &MiningFailureEvidence) -> Self {
        let fact = failure.fact();
        let (mechanism_source, abstract_mechanism) = failure.hypothesis().map_or(
            (
                MechanismSource::Unclassified,
                FailureMechanism::Unclassified,
            ),
            |hypothesis| (MechanismSource::BoundedClassifier, hypothesis.mechanism()),
        );
        Self {
            version: SIGNATURE_VERSION,
            terminal_cause_source: FactSource::VerifierReceipt,
            terminal_cause: fact.terminal_cause(),
            causal_status_source: FactSource::VerifierReceipt,
            causal_status: fact.causal_status(),
            mechanism_source,
            abstract_mechanism,
        }
    }

    pub const fn version(self) -> u32 {
        self.version
    }

    pub const fn terminal_cause(self) -> TerminalCause {
        self.terminal_cause
    }

    pub const fn causal_status(self) -> CausalStatus {
        self.causal_status
    }

    pub const fn mechanism_source(self) -> MechanismSource {
        self.mechanism_source
    }

    pub const fn abstract_mechanism(self) -> FailureMechanism {
        self.abstract_mechanism
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MiningLimits {
    max_observations: usize,
    max_clusters: usize,
    max_cluster_members: usize,
    max_pass_anchors: usize,
}

impl MiningLimits {
    pub fn new(
        max_observations: usize,
        max_clusters: usize,
        max_cluster_members: usize,
        max_pass_anchors: usize,
    ) -> Result<Self, MiningError> {
        validate_limit("observations", max_observations, MAX_OBSERVATIONS)?;
        validate_limit("clusters", max_clusters, MAX_CLUSTERS)?;
        validate_limit("cluster members", max_cluster_members, MAX_CLUSTER_MEMBERS)?;
        validate_limit("pass anchors", max_pass_anchors, MAX_PASS_ANCHORS)?;
        Ok(Self {
            max_observations,
            max_clusters,
            max_cluster_members,
            max_pass_anchors,
        })
    }
}

impl Default for MiningLimits {
    fn default() -> Self {
        Self {
            max_observations: MAX_OBSERVATIONS,
            max_clusters: MAX_CLUSTERS,
            max_cluster_members: MAX_CLUSTER_MEMBERS,
            max_pass_anchors: MAX_PASS_ANCHORS,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FailureCluster {
    rank: u32,
    signature: FailureSignature,
    support: u32,
    representative: MiningFailureEvidence,
    members: Vec<MiningObservationId>,
}

impl FailureCluster {
    pub const fn rank(&self) -> u32 {
        self.rank
    }

    pub const fn signature(&self) -> FailureSignature {
        self.signature
    }

    pub const fn support(&self) -> u32 {
        self.support
    }

    pub const fn representative(&self) -> &MiningFailureEvidence {
        &self.representative
    }

    pub fn members(&self) -> &[MiningObservationId] {
        &self.members
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct MiningBundleContent {
    schema_version: u32,
    clusters: Vec<FailureCluster>,
    pass_anchors: Vec<MiningPassEvidence>,
    omitted_pass_anchors: u32,
}

#[derive(Clone, Eq, PartialEq)]
pub struct MiningEvidenceBundle {
    content: MiningBundleContent,
    root: MiningBundleRoot,
    canonical: Vec<u8>,
}

impl MiningEvidenceBundle {
    pub fn clusters(&self) -> &[FailureCluster] {
        &self.content.clusters
    }

    pub fn pass_anchors(&self) -> &[MiningPassEvidence] {
        &self.content.pass_anchors
    }

    pub const fn omitted_pass_anchors(&self) -> u32 {
        self.content.omitted_pass_anchors
    }

    pub const fn root(&self) -> MiningBundleRoot {
        self.root
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical
    }
}

impl std::fmt::Debug for MiningEvidenceBundle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MiningEvidenceBundle")
            .field("root", &self.root)
            .field("clusters", &self.content.clusters.len())
            .field("pass_anchors", &self.content.pass_anchors.len())
            .field("omitted_pass_anchors", &self.content.omitted_pass_anchors)
            .finish_non_exhaustive()
    }
}

/// Converts trace text into bounded data. The result is still untrusted and is never executed.
pub fn sanitize_untrusted_text(
    source: MiningEvidenceId,
    raw: &str,
    protected_terms: &[&str],
) -> Result<SanitizedEvidenceText, MiningError> {
    if raw.is_empty() {
        return Err(MiningError::EmptyText);
    }
    if raw.len() > MAX_RAW_TEXT_BYTES {
        return Err(MiningError::RawTextTooLarge {
            maximum: MAX_RAW_TEXT_BYTES,
        });
    }
    if protected_terms.len() > MAX_PROTECTED_TERMS {
        return Err(MiningError::TooManyProtectedTerms {
            maximum: MAX_PROTECTED_TERMS,
        });
    }
    if protected_terms
        .iter()
        .any(|term| term.len() > MAX_PROTECTED_TERM_BYTES)
    {
        return Err(MiningError::ProtectedTermTooLarge {
            maximum: MAX_PROTECTED_TERM_BYTES,
        });
    }

    let mut terms = protected_terms
        .iter()
        .copied()
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    terms
        .sort_unstable_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    terms.dedup();

    let mut protected_terms_count = 0_u32;
    let mut text = raw.to_owned();
    for term in terms {
        let matches = text.match_indices(term).count();
        if matches > 0 {
            protected_terms_count = protected_terms_count.saturating_add(matches as u32);
            text = text.replace(term, REDACTED);
        }
    }

    let mut sensitive_values = 0_u32;
    let mut assignment_redacted = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (body, newline) = line
            .strip_suffix('\n')
            .map_or((line, ""), |body| (body, "\n"));
        if let Some(value_start) = sensitive_value_start(body) {
            assignment_redacted.push_str(&body[..value_start]);
            assignment_redacted.push_str(REDACTED);
            sensitive_values = sensitive_values.saturating_add(1);
        } else {
            assignment_redacted.push_str(body);
        }
        assignment_redacted.push_str(newline);
    }

    let mut control_characters = 0_u32;
    let mut normalized = String::with_capacity(assignment_redacted.len());
    for character in assignment_redacted.chars() {
        if character.is_control() {
            control_characters = control_characters.saturating_add(1);
            if matches!(character, '\n' | '\r' | '\t') {
                normalized.push(' ');
            } else {
                normalized.push('\u{fffd}');
            }
        } else {
            normalized.push(character);
        }
    }

    let truncated = normalized.len() > MAX_SANITIZED_TEXT_BYTES;
    if truncated {
        let mut boundary = MAX_SANITIZED_TEXT_BYTES;
        while !normalized.is_char_boundary(boundary) {
            boundary -= 1;
        }
        normalized.truncate(boundary);
    }

    Ok(SanitizedEvidenceText::new(
        source,
        normalized,
        RedactionSummary::new(
            protected_terms_count,
            sensitive_values,
            control_characters,
            truncated,
        ),
    ))
}

pub fn mine_evidence(
    observations: impl IntoIterator<Item = MiningObservation>,
    limits: MiningLimits,
) -> Result<MiningEvidenceBundle, MiningError> {
    let observations = observations
        .into_iter()
        .map(|observation| (observation.id(), observation))
        .collect::<BTreeMap<_, _>>();
    if observations.len() > limits.max_observations {
        return Err(MiningError::TooManyObservations {
            actual: observations.len(),
            maximum: limits.max_observations,
        });
    }

    let mut grouped = BTreeMap::<FailureSignature, Vec<MiningFailureEvidence>>::new();
    let mut pass_anchors = Vec::new();
    for observation in observations.into_values() {
        match observation {
            MiningObservation::Failure(failure) => grouped
                .entry(FailureSignature::from_failure(&failure))
                .or_default()
                .push(failure),
            MiningObservation::Pass(pass) => pass_anchors.push(pass),
        }
    }
    if grouped.len() > limits.max_clusters {
        return Err(MiningError::TooManyClusters {
            actual: grouped.len(),
            maximum: limits.max_clusters,
        });
    }

    let mut clusters = Vec::with_capacity(grouped.len());
    for (signature, mut members) in grouped {
        members.sort_unstable_by_key(MiningFailureEvidence::id);
        if members.len() > limits.max_cluster_members {
            return Err(MiningError::ClusterTooLarge {
                actual: members.len(),
                maximum: limits.max_cluster_members,
            });
        }
        let support = members.len() as u32;
        let representative = members[0].clone();
        let member_ids = members.into_iter().map(|member| member.id()).collect();
        clusters.push(FailureCluster {
            rank: 0,
            signature,
            support,
            representative,
            members: member_ids,
        });
    }
    clusters.sort_unstable_by(|left, right| {
        right
            .support
            .cmp(&left.support)
            .then_with(|| left.signature.cmp(&right.signature))
    });
    for (index, cluster) in clusters.iter_mut().enumerate() {
        cluster.rank = (index + 1) as u32;
    }

    pass_anchors.sort_unstable_by_key(MiningPassEvidence::id);
    let omitted_pass_anchors = pass_anchors.len().saturating_sub(limits.max_pass_anchors) as u32;
    pass_anchors.truncate(limits.max_pass_anchors);

    let content = MiningBundleContent {
        schema_version: 1,
        clusters,
        pass_anchors,
        omitted_pass_anchors,
    };
    let canonical = serde_json::to_vec(&content).map_err(MiningError::Canonicalization)?;
    let root = MiningBundleRoot::from_digest(Digest::of(&canonical));
    Ok(MiningEvidenceBundle {
        content,
        root,
        canonical,
    })
}

fn validate_limit(field: &'static str, value: usize, maximum: usize) -> Result<(), MiningError> {
    if value == 0 || value > maximum {
        return Err(MiningError::InvalidLimit { field, maximum });
    }
    Ok(())
}

fn sensitive_value_start(line: &str) -> Option<usize> {
    const KEYS: [&str; 9] = [
        "api_key",
        "apikey",
        "authorization",
        "credential",
        "password",
        "passwd",
        "secret",
        "token",
        "bearer",
    ];

    let lower = line.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut earliest = None;
    for key in KEYS {
        for (start, _) in lower.match_indices(key) {
            if start > 0 && is_identifier_byte(bytes[start - 1]) {
                continue;
            }
            let after_key = start + key.len();
            if after_key < bytes.len() && is_identifier_byte(bytes[after_key]) {
                continue;
            }
            let mut cursor = after_key;
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            if key == "bearer" {
                if cursor == after_key || cursor >= bytes.len() {
                    continue;
                }
            } else {
                if !matches!(bytes.get(cursor), Some(b':' | b'=')) {
                    continue;
                }
                cursor += 1;
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
                if cursor >= bytes.len() {
                    continue;
                }
            }
            earliest = Some(earliest.map_or(cursor, |current: usize| current.min(cursor)));
        }
    }
    earliest
}

const fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}
