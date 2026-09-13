//! Exact patch artifacts, independently reproduced from immutable snapshots.
//!
//! Git is a fixed host utility, not a workspace execution route. It runs only in
//! newly controlled scratch directories with separate Git metadata and indexes.
//! A receipt proves patch reproduction; it is not task-completion evidence by itself.

#[cfg(unix)]
mod git;

use crate::{
    Digest,
    artifacts::{ArtifactError, ArtifactStore},
    workspace::{Entry, Snapshot, WorkspaceError},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct PatchLimits {
    pub timeout: Duration,
    pub max_snapshot_bytes: u64,
    pub max_entries: usize,
    pub max_patch_bytes: usize,
    pub max_diagnostic_bytes: usize,
}
impl Default for PatchLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
            max_snapshot_bytes: 256 * 1024 * 1024,
            max_entries: 100000,
            max_patch_bytes: 64 * 1024 * 1024,
            max_diagnostic_bytes: 64 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error("invalid patch delivery limits")]
    InvalidLimits,
    #[error("patch scratch must be a caller-owned directory outside source workspaces")]
    InvalidScratch,
    #[error("the fixed Git utility is unavailable")]
    GitUnavailable,
    #[error("Git artifact command {step} failed with exit status {exit_code:?}")]
    GitFailed {
        step: &'static str,
        exit_code: Option<i32>,
        stderr: Digest,
    },
    #[error("Git returned malformed artifact data")]
    GitProtocol,
    #[error("patch delivery exceeded its {0} bound")]
    Limit(&'static str),
    #[error("patch delivery timed out")]
    TimedOut,
    #[error("patch delivery was cancelled")]
    Cancelled,
    #[error("Git process termination could not be confirmed")]
    OutcomeUnknown,
    #[error("snapshot policies differ; a patch cannot encode that metadata change")]
    PolicyChanged,
    #[error("Git cannot faithfully represent {path}: {reason}")]
    Unrepresentable { path: String, reason: &'static str },
    #[error("applying the patch did not reproduce candidate paths: {0:?}")]
    ReproductionMismatch(Vec<String>),
    #[error("patch delivery requires a Unix host")]
    UnsupportedPlatform,
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error("patch filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("patch receipt could not be encoded: {0}")]
    Json(#[from] serde_json::Error),
}
impl DeliveryError {
    pub fn requires_reconciliation(&self) -> bool {
        matches!(self, Self::OutcomeUnknown)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GitIdentity {
    pub executable: String,
    pub executable_digest: Digest,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GitCommandReceipt {
    pub step: String,
    /// Fixed command arguments, with the private scratch prefix replaced by <scratch>.
    pub arguments: Vec<String>,
    pub stdin_digest: Digest,
    pub stdout_digest: Digest,
    pub stderr_digest: Digest,
    pub exit_code: i32,
    pub elapsed_ms: u64,
    pub process_group_quiescent: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PatchValidationReceipt {
    pub version: u32,
    pub baseline: Digest,
    pub candidate: Digest,
    pub patch: Digest,
    pub applied_snapshot: Digest,
    /// Actual observation captured with no excluded roots.
    pub observed_snapshot: Digest,
    pub git: GitIdentity,
    pub commands: Vec<GitCommandReceipt>,
    pub changed_paths: Vec<String>,
    pub application: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PatchArtifact {
    pub patch: Digest,
    pub baseline: Digest,
    pub candidate: Digest,
    pub receipt_digest: Digest,
    pub receipt: PatchValidationReceipt,
}

pub struct PatchBuilder {
    limits: PatchLimits,
}
impl PatchBuilder {
    pub fn new(limits: PatchLimits) -> Result<Self, DeliveryError> {
        if limits.timeout.is_zero()
            || limits.timeout > Duration::from_secs(600)
            || limits.max_snapshot_bytes == 0
            || limits.max_snapshot_bytes > 1024 * 1024 * 1024
            || limits.max_entries == 0
            || limits.max_entries > 100000
            || limits.max_patch_bytes == 0
            || limits.max_patch_bytes > 256 * 1024 * 1024
            || limits.max_diagnostic_bytes == 0
            || limits.max_diagnostic_bytes > 1024 * 1024
        {
            return Err(DeliveryError::InvalidLimits);
        }
        Ok(Self { limits })
    }

    /// `scratch` is trusted host storage, never a user checkout or worker mount.
    /// Commands have a hard process deadline; bounded snapshot IO checks the same
    /// deadline before/after each synchronous kernel operation. No late receipt is issued.
    pub async fn build(
        &self,
        baseline: &Snapshot,
        candidate: &Snapshot,
        artifacts: &ArtifactStore,
        scratch: &Path,
        cancellation: CancellationToken,
    ) -> Result<PatchArtifact, DeliveryError> {
        #[cfg(unix)]
        {
            let control = Control::new(&self.limits, &cancellation);
            let identities = self.prepare(baseline, candidate, artifacts, &control)?;
            let mut git =
                git::GitWorkspace::new(scratch, &self.limits, artifacts, &control).await?;
            let baseline_dir = git.root().join("baseline");
            let candidate_dir = git.root().join("candidate");
            baseline.materialize(&baseline_dir, artifacts, false)?;
            control.check()?;
            candidate.materialize(&candidate_dir, artifacts, false)?;
            control.check()?;
            let base_tree = git.tree(baseline, &baseline_dir, "baseline-index").await?;
            let candidate_tree = git
                .tree(candidate, &candidate_dir, "candidate-index")
                .await?;
            let patch = git.diff(&base_tree, &candidate_tree).await?;
            let patch_id = artifacts.put(&patch)?;
            let applied = self
                .apply_and_compare(baseline, candidate, &patch, &mut git, artifacts, &control)
                .await?;
            self.publish(
                patch_id,
                identities,
                applied,
                baseline,
                candidate,
                git.finish(),
                artifacts,
                &control,
            )
        }
        #[cfg(not(unix))]
        Err(DeliveryError::UnsupportedPlatform)
    }

    /// Re-run validation against an independently stored patch. Corrupt artifacts,
    /// invalid patches, and approximate reconstructions never return a receipt.
    pub async fn verify_patch(
        &self,
        baseline: &Snapshot,
        candidate: &Snapshot,
        patch: Digest,
        artifacts: &ArtifactStore,
        scratch: &Path,
        cancellation: CancellationToken,
    ) -> Result<PatchArtifact, DeliveryError> {
        #[cfg(unix)]
        {
            let control = Control::new(&self.limits, &cancellation);
            let identities = self.prepare(baseline, candidate, artifacts, &control)?;
            let bytes = artifacts.read(patch)?;
            if bytes.len() > self.limits.max_patch_bytes {
                return Err(DeliveryError::Limit("patch bytes"));
            }
            let mut git =
                git::GitWorkspace::new(scratch, &self.limits, artifacts, &control).await?;
            let applied = self
                .apply_and_compare(baseline, candidate, &bytes, &mut git, artifacts, &control)
                .await?;
            self.publish(
                patch,
                identities,
                applied,
                baseline,
                candidate,
                git.finish(),
                artifacts,
                &control,
            )
        }
        #[cfg(not(unix))]
        Err(DeliveryError::UnsupportedPlatform)
    }

    fn prepare(
        &self,
        baseline: &Snapshot,
        candidate: &Snapshot,
        artifacts: &ArtifactStore,
        control: &Control<'_>,
    ) -> Result<(Digest, Digest), DeliveryError> {
        control.check()?;
        if baseline.policy != candidate.policy {
            return Err(DeliveryError::PolicyChanged);
        }
        for snapshot in [baseline, candidate] {
            if snapshot.entries.len() > self.limits.max_entries {
                return Err(DeliveryError::Limit("snapshot entries"));
            }
            let mut metadata_bytes = 0usize;
            for (path, entry) in &snapshot.entries {
                if path.len() > 4096 || path.contains('\0') {
                    return Err(DeliveryError::Limit("path bytes"));
                }
                if snapshot
                    .policy
                    .excluded_roots
                    .iter()
                    .any(|excluded| path == excluded || path.starts_with(&format!("{excluded}/")))
                {
                    return Err(DeliveryError::Unrepresentable {
                        path: path.clone(),
                        reason: "snapshot entries contradict its exclusions",
                    });
                }
                metadata_bytes = metadata_bytes.saturating_add(path.len());
                if let Entry::Symlink { target } = entry {
                    if target.len() > 4096 {
                        return Err(DeliveryError::Limit("symlink target bytes"));
                    }
                    metadata_bytes = metadata_bytes.saturating_add(target.len());
                }
                if metadata_bytes as u64 > self.limits.max_snapshot_bytes {
                    return Err(DeliveryError::Limit("snapshot metadata bytes"));
                }
            }
        }
        let identities = (baseline.publish(artifacts)?, candidate.publish(artifacts)?);
        representable(baseline, candidate)?;
        for snapshot in [baseline, candidate] {
            if snapshot.entries.len() > self.limits.max_entries {
                return Err(DeliveryError::Limit("snapshot entries"));
            }
            let mut total = 0u64;
            for entry in snapshot.entries.values() {
                control.check()?;
                let bytes = match entry {
                    Entry::File { content, .. } => artifacts.read(*content)?.len() as u64,
                    Entry::Symlink { target } => target.len() as u64,
                    Entry::Directory { .. } => 0,
                };
                total = total.saturating_add(bytes);
                if total > self.limits.max_snapshot_bytes || total > snapshot.policy.max_bytes {
                    return Err(DeliveryError::Limit("snapshot bytes"));
                }
            }
        }
        Ok(identities)
    }

    #[cfg(unix)]
    async fn apply_and_compare(
        &self,
        baseline: &Snapshot,
        candidate: &Snapshot,
        patch: &[u8],
        git: &mut git::GitWorkspace<'_>,
        artifacts: &ArtifactStore,
        control: &Control<'_>,
    ) -> Result<(Digest, Digest), DeliveryError> {
        control.check()?;
        let materialized = git.root().join("applied");
        baseline.materialize(&materialized, artifacts, false)?;
        control.check()?;
        git.apply(&materialized, patch).await?;
        control.check()?;
        let mut full_policy = candidate.policy.clone();
        full_policy.excluded_roots.clear();
        let observed = Snapshot::capture(&materialized, full_policy, artifacts)?;
        control.check()?;
        if observed.entries != candidate.entries {
            return Err(DeliveryError::ReproductionMismatch(
                observed.changed_paths(candidate),
            ));
        }
        let observed_digest = observed.publish(artifacts)?;
        let applied = Snapshot {
            policy: candidate.policy.clone(),
            ..observed
        };
        Ok((applied.publish(artifacts)?, observed_digest))
    }

    #[allow(clippy::too_many_arguments)]
    fn publish(
        &self,
        patch: Digest,
        identities: (Digest, Digest),
        applied: (Digest, Digest),
        baseline: &Snapshot,
        candidate: &Snapshot,
        git: (GitIdentity, Vec<GitCommandReceipt>),
        artifacts: &ArtifactStore,
        control: &Control<'_>,
    ) -> Result<PatchArtifact, DeliveryError> {
        control.check()?;
        if utility_identity()?.1 != git.0.executable_digest {
            return Err(DeliveryError::GitUnavailable);
        }
        let receipt = PatchValidationReceipt {
            version: 1,
            baseline: identities.0,
            candidate: identities.1,
            patch,
            applied_snapshot: applied.0,
            observed_snapshot: applied.1,
            git: git.0,
            commands: git.1,
            changed_paths: baseline.changed_paths(candidate),
            application:
                "git apply --binary --whitespace=nowarn (allow-empty only for empty artifacts); full snapshot equality"
                    .into(),
        };
        let receipt_digest = artifacts.put(&serde_json::to_vec(&receipt)?)?;
        control.check()?;
        Ok(PatchArtifact {
            patch,
            baseline: identities.0,
            candidate: identities.1,
            receipt_digest,
            receipt,
        })
    }
}

fn representable(baseline: &Snapshot, candidate: &Snapshot) -> Result<(), DeliveryError> {
    for snapshot in [baseline, candidate] {
        for path in snapshot.entries.keys() {
            if path
                .split('/')
                .any(|component| component.eq_ignore_ascii_case(".git"))
            {
                return Err(DeliveryError::Unrepresentable {
                    path: path.clone(),
                    reason: "Git metadata paths are forbidden",
                });
            }
        }
    }
    for path in baseline.changed_paths(candidate) {
        match (baseline.entries.get(&path), candidate.entries.get(&path)) {
            (_, Some(Entry::File { mode, .. })) if !matches!(mode, 0o644 | 0o755) => {
                return Err(DeliveryError::Unrepresentable {
                    path,
                    reason: "Git only records regular-file modes 0644 and 0755",
                });
            }
            (Some(Entry::File { mode: old, .. }), Some(Entry::File { mode: new, .. }))
                if old != new && !matches!(old, 0o644 | 0o755) =>
            {
                return Err(DeliveryError::Unrepresentable {
                    path,
                    reason: "full permission changes are not representable in Git",
                });
            }
            (Some(Entry::Directory { mode: old }), Some(Entry::Directory { mode: new }))
                if old != new =>
            {
                return Err(DeliveryError::Unrepresentable {
                    path,
                    reason: "Git does not record directory permission changes",
                });
            }
            (_, Some(Entry::Directory { mode })) if *mode != 0o755 => {
                return Err(DeliveryError::Unrepresentable {
                    path,
                    reason: "new directory modes other than 0755 are not represented",
                });
            }
            _ => {}
        }
        for snapshot in [baseline, candidate] {
            if matches!(snapshot.entries.get(&path), Some(Entry::Directory { .. }))
                && empty_directory(&path, &snapshot.entries)
            {
                return Err(DeliveryError::Unrepresentable {
                    path,
                    reason: "Git cannot encode an added or deleted empty directory",
                });
            }
        }
    }
    Ok(())
}
fn empty_directory(path: &str, entries: &BTreeMap<String, Entry>) -> bool {
    let prefix = format!("{path}/");
    !entries
        .iter()
        .any(|(name, entry)| name.starts_with(&prefix) && !matches!(entry, Entry::Directory { .. }))
}

struct Control<'a> {
    deadline: Instant,
    cancel: &'a CancellationToken,
}
impl<'a> Control<'a> {
    fn new(limits: &PatchLimits, cancel: &'a CancellationToken) -> Self {
        Self {
            deadline: Instant::now() + limits.timeout,
            cancel,
        }
    }
    fn check(&self) -> Result<(), DeliveryError> {
        if self.cancel.is_cancelled() {
            Err(DeliveryError::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(DeliveryError::TimedOut)
        } else {
            Ok(())
        }
    }
    fn remaining(&self) -> Result<Duration, DeliveryError> {
        self.check()?;
        Ok(self.deadline.saturating_duration_since(Instant::now()))
    }
}

fn utility_identity() -> Result<(PathBuf, Digest), DeliveryError> {
    let path = PathBuf::from("/usr/bin/git");
    let metadata = fs::metadata(&path).map_err(|_| DeliveryError::GitUnavailable)?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 * 1024 {
        return Err(DeliveryError::GitUnavailable);
    }
    let mut bytes = Vec::new();
    fs::File::open(&path)
        .map_err(|_| DeliveryError::GitUnavailable)?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| DeliveryError::GitUnavailable)?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(DeliveryError::GitUnavailable);
    }
    Ok((path, Digest::of(&bytes)))
}
