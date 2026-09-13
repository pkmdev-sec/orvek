//! Authenticated human review notes are input data, not evaluator certificates.
use crate::{Digest, StoreError, artifacts::ArtifactStore, session::SessionId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Approved,
    ChangesRequested,
    Comment,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFeedback {
    pub version: u32,
    pub session: SessionId,
    pub manifest: Digest,
    pub source_identity: Digest,
    pub disposition: Disposition,
    pub body: String,
}

pub fn read(artifacts: &ArtifactStore, digest: Digest) -> Result<ReviewFeedback, StoreError> {
    let bytes = artifacts.read(digest)?;
    if bytes.len() > 68 * 1024 {
        return Err(StoreError::Invalid("review feedback exceeds its bound"));
    }
    let feedback: ReviewFeedback = serde_json::from_slice(&bytes)?;
    if feedback.version != 1 || feedback.body.len() > 64 * 1024 {
        return Err(StoreError::Invalid("unsupported review feedback"));
    }
    let manifest = crate::review::manifest(artifacts, feedback.manifest)
        .map_err(|_| StoreError::Integrity("review feedback has no valid source manifest"))?;
    if manifest.source_identity != feedback.source_identity {
        return Err(StoreError::Integrity(
            "review feedback source identity mismatch",
        ));
    }
    Ok(feedback)
}
