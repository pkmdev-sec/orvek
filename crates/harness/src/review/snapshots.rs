use super::{
    Digest, FileKind, FrozenFile, FrozenTree, ReviewError, ReviewInspection, ReviewLimits,
    ReviewManifest, ReviewRange, diff, files, git, put_json, validate_limits,
};
use crate::{
    artifacts::ArtifactStore,
    workspace::{Entry, Snapshot},
};
use std::collections::{BTreeMap, BTreeSet};
use tokio_util::sync::CancellationToken;

/// Review immutable task artifacts without opening a live repository or worktree.
pub async fn inspect_snapshots(
    before: &Snapshot,
    after: &Snapshot,
    artifacts: &ArtifactStore,
    cancellation: &CancellationToken,
) -> Result<ReviewInspection, ReviewError> {
    inspect_snapshots_with_limits(
        before,
        after,
        artifacts,
        cancellation,
        ReviewLimits::default(),
    )
    .await
}
pub async fn inspect_snapshots_with_limits(
    before: &Snapshot,
    after: &Snapshot,
    artifacts: &ArtifactStore,
    cancellation: &CancellationToken,
    limits: ReviewLimits,
) -> Result<ReviewInspection, ReviewError> {
    validate_limits(&limits)?;
    if before.policy != after.policy {
        return Err(ReviewError::Unsupported("snapshot policies differ"));
    }
    let git = git::Git::for_snapshots(&limits, cancellation).await?;
    let before_tree = freeze(&git, before, artifacts, &limits)?;
    let after_tree = freeze(&git, after, artifacts, &limits)?;
    let source_before = before
        .publish(artifacts)
        .map_err(|_| ReviewError::Corrupt)?;
    let source_after = after.publish(artifacts).map_err(|_| ReviewError::Corrupt)?;
    let range = ReviewRange::Snapshots {
        before: source_before,
        after: source_after,
    };
    let metadata_changes = metadata_changes(&before_tree, &after_tree);
    let patch = diff(&git, &before_tree, &after_tree, artifacts, &limits).await?;
    let before = put_json(artifacts, &before_tree, limits.max_metadata_bytes)?;
    let after = put_json(artifacts, &after_tree, limits.max_metadata_bytes)?;
    let patch = artifacts.put(&patch)?;
    let base_revision: Option<String> = None;
    let head_revision: Option<String> = None;
    let source_identity = Digest::of_value(&(
        1u32,
        &range,
        &base_revision,
        &head_revision,
        before,
        after,
        patch,
    ))
    .map_err(|_| ReviewError::Corrupt)?;
    let manifest = put_json(
        artifacts,
        &ReviewManifest {
            version: 1,
            source_identity,
            repository: "Immutable task snapshots".into(),
            range,
            base_revision: None,
            head_revision: None,
            before,
            after,
            patch,
            git_executable: git.executable,
            metadata_changes: metadata_changes.clone(),
        },
        limits.max_metadata_bytes,
    )?;
    git.check()?;
    Ok(ReviewInspection {
        manifest,
        source_identity,
        patch,
        before,
        after,
        files: after_tree.files.len(),
        base_revision: None,
        head_revision: None,
        metadata_changes,
    })
}
fn freeze(
    git: &git::Git,
    snapshot: &Snapshot,
    artifacts: &ArtifactStore,
    limits: &ReviewLimits,
) -> Result<FrozenTree, ReviewError> {
    if snapshot.version != 1 || snapshot.policy.max_files == 0 || snapshot.policy.max_bytes == 0 {
        return Err(ReviewError::Unsupported("snapshot version or policy"));
    }
    if snapshot.entries.len() > limits.max_files
        || snapshot.entries.len() > snapshot.policy.max_files
    {
        return Err(ReviewError::Limit("snapshot entries"));
    }
    let mut frozen = BTreeMap::new();
    let mut total = 0usize;
    for (path, entry) in &snapshot.entries {
        git.check()?;
        if !files::safe(path) {
            return Err(ReviewError::Path);
        }
        let (kind, mode, permissions, content, bytes) = match entry {
            Entry::File { content, mode } => {
                if *mode > 0o7777 {
                    return Err(ReviewError::Unsupported("file permissions"));
                }
                let bytes = files::read_regular(&artifacts.path(*content), limits.max_file_bytes)?;
                if Digest::of(&bytes) != *content {
                    return Err(ReviewError::Corrupt);
                }
                (
                    FileKind::File,
                    if mode & 0o111 == 0 {
                        0o100644
                    } else {
                        0o100755
                    },
                    Some(*mode),
                    *content,
                    bytes.len(),
                )
            }
            Entry::Directory { mode } => {
                if *mode > 0o7777 {
                    return Err(ReviewError::Unsupported("directory permissions"));
                }
                (
                    FileKind::Directory,
                    0o040000,
                    Some(*mode),
                    artifacts.put(&[])?,
                    0,
                )
            }
            Entry::Symlink { target } => {
                if target.len() > 4096 {
                    return Err(ReviewError::Limit("link target"));
                }
                (
                    FileKind::Symlink,
                    0o120000,
                    None,
                    artifacts.put(target.as_bytes())?,
                    target.len(),
                )
            }
        };
        total = total.saturating_add(bytes);
        if total > limits.max_snapshot_bytes || total as u64 > snapshot.policy.max_bytes {
            return Err(ReviewError::Limit("snapshot bytes"));
        }
        frozen.insert(
            path.clone(),
            FrozenFile {
                kind,
                mode,
                permissions,
                content,
                bytes,
                git_object: None,
            },
        );
    }
    Ok(FrozenTree {
        version: 1,
        files: frozen,
    })
}
fn metadata_changes(before: &FrozenTree, after: &FrozenTree) -> Vec<String> {
    before
        .files
        .keys()
        .chain(after.files.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| {
            let left = before.files.get(*path);
            let right = after.files.get(*path);
            if left == right {
                return false;
            }
            if left
                .into_iter()
                .chain(right)
                .any(|file| file.kind == FileKind::Directory)
            {
                return true;
            }
            let noncanonical = |file: &FrozenFile| {
                file.kind == FileKind::File && file.permissions != Some(file.mode & 0o777)
            };
            match (left, right) {
                (Some(left), Some(right))
                    if left.kind == FileKind::File && right.kind == FileKind::File =>
                {
                    left.permissions != right.permissions
                        && (noncanonical(left) || noncanonical(right))
                }
                (_, Some(right)) => noncanonical(right),
                _ => false,
            }
        })
        .cloned()
        .collect()
}
