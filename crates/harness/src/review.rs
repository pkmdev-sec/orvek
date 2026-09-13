//! Frozen read-only Git review data. No task/evidence state is created here.
mod catalog;
mod files;
mod snapshots;
pub use catalog::{ReviewBranch, ReviewCatalog, ReviewCommit, catalog};
pub use snapshots::{inspect_snapshots, inspect_snapshots_with_limits};
mod git;

use crate::{
    Digest,
    artifacts::{ArtifactError, ArtifactStore},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewRange {
    Snapshots { before: Digest, after: Digest },
    WorkingTree { base: Option<String> },
    Staged { base: Option<String> },
    Commit { revision: String },
    Between { base: String, head: String },
}
#[derive(Clone, Debug)]
pub struct ReviewLimits {
    pub max_files: usize,
    pub max_file_bytes: usize,
    pub max_snapshot_bytes: usize,
    pub max_output_bytes: usize,
    pub max_metadata_bytes: usize,
    pub timeout: Duration,
}
impl Default for ReviewLimits {
    fn default() -> Self {
        Self {
            max_files: 10000,
            max_file_bytes: 16 * 1024 * 1024,
            max_snapshot_bytes: 64 * 1024 * 1024,
            max_output_bytes: 32 * 1024 * 1024,
            max_metadata_bytes: 4 * 1024 * 1024,
            timeout: Duration::from_secs(30),
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    #[error("Git is unavailable")]
    Unavailable,
    #[error("invalid review range")]
    Range,
    #[error("invalid review limits")]
    Limits,
    #[error("review exceeds its {0} bound")]
    Limit(&'static str),
    #[error("unsupported review source: {0}")]
    Unsupported(&'static str),
    #[error("review path is not representable or escapes the source")]
    Path,
    #[error("Git returned invalid review data")]
    Protocol,
    #[error("Git review failed: {0}")]
    Git(String),
    #[error("source changed during review capture; retry after it settles")]
    Changed,
    #[error("review capture cancelled")]
    Cancelled,
    #[error("review capture timed out")]
    TimedOut,
    #[error("Git process-tree termination could not be established")]
    Unknown,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error("invalid review artifact")]
    Corrupt,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Directory,
    File,
    Symlink,
    Gitlink,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FrozenFile {
    pub kind: FileKind,
    pub mode: u32,
    pub permissions: Option<u32>,
    pub content: Digest,
    pub bytes: usize,
    pub git_object: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FrozenTree {
    pub version: u32,
    pub files: BTreeMap<String, FrozenFile>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewManifest {
    pub version: u32,
    pub source_identity: Digest,
    pub repository: String,
    pub range: ReviewRange,
    pub base_revision: Option<String>,
    pub head_revision: Option<String>,
    pub before: Digest,
    pub after: Digest,
    pub patch: Digest,
    pub git_executable: Digest,
    #[serde(default)]
    pub metadata_changes: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewInspection {
    pub manifest: Digest,
    pub source_identity: Digest,
    pub patch: Digest,
    pub before: Digest,
    pub after: Digest,
    pub files: usize,
    #[serde(default)]
    pub metadata_changes: Vec<String>,
    pub base_revision: Option<String>,
    pub head_revision: Option<String>,
}

pub async fn inspect(
    workspace: &Path,
    range: ReviewRange,
    artifacts: &ArtifactStore,
    cancellation: &CancellationToken,
) -> Result<ReviewInspection, ReviewError> {
    inspect_with_limits(
        workspace,
        range,
        artifacts,
        cancellation,
        ReviewLimits::default(),
    )
    .await
}
pub async fn inspect_with_limits(
    workspace: &Path,
    range: ReviewRange,
    artifacts: &ArtifactStore,
    cancellation: &CancellationToken,
    limits: ReviewLimits,
) -> Result<ReviewInspection, ReviewError> {
    validate_limits(&limits)?;
    if matches!(range, ReviewRange::Snapshots { .. }) {
        return Err(ReviewError::Range);
    }
    let git = git::Git::new(workspace, &limits, cancellation).await?;
    let original_head = git.resolve("HEAD").await?;
    let (base, head) = match &range {
        ReviewRange::Snapshots { .. } => return Err(ReviewError::Range),
        ReviewRange::WorkingTree { base } | ReviewRange::Staged { base } => (
            Some(git.resolve(base.as_deref().unwrap_or("HEAD")).await?),
            None,
        ),
        ReviewRange::Between { base, head } => (
            Some(git.resolve(base).await?),
            Some(git.resolve(head).await?),
        ),
        ReviewRange::Commit { revision } => {
            let head = git.resolve(revision).await?;
            let parent = commit_parent(&git, &head).await?;
            (parent, Some(head))
        }
    };
    let before = match &base {
        Some(base) => committed(&git, base, artifacts, &limits).await?,
        None => FrozenTree {
            version: 1,
            files: BTreeMap::new(),
        },
    };
    let (after, captured_index) = match &range {
        ReviewRange::WorkingTree { .. } => {
            let (index, identity) = copy_index(&git, &limits).await?;
            let first = working(&git, &index, artifacts, &limits).await?;
            let second = working(&git, &index, artifacts, &limits).await?;
            if first != second {
                return Err(ReviewError::Changed);
            }
            (first, Some(identity))
        }
        ReviewRange::Staged { .. } => {
            let (index, identity) = copy_index(&git, &limits).await?;
            (
                indexed(&git, &index, artifacts, &limits).await?,
                Some(identity),
            )
        }
        _ => (
            committed(
                &git,
                head.as_ref().ok_or(ReviewError::Protocol)?,
                artifacts,
                &limits,
            )
            .await?,
            None,
        ),
    };
    let patch = diff(&git, &before, &after, artifacts, &limits).await?;
    if git.resolve("HEAD").await? != original_head {
        return Err(ReviewError::Changed);
    }
    if let Some(identity) = captured_index
        && index_identity(&git, &limits).await? != identity
    {
        return Err(ReviewError::Changed);
    }
    if matches!(range, ReviewRange::WorkingTree { .. }) {
        let index = git.scratch().join("source-index");
        if working(&git, &index, artifacts, &limits).await? != after {
            return Err(ReviewError::Changed);
        }
    }
    git.check()?;
    let before_id = put_json(artifacts, &before, limits.max_metadata_bytes)?;
    let after_id = put_json(artifacts, &after, limits.max_metadata_bytes)?;
    let patch_id = artifacts.put(&patch)?;
    let source_identity =
        Digest::of_value(&(1u32, &range, &base, &head, before_id, after_id, patch_id))
            .map_err(|_| ReviewError::Corrupt)?;
    let manifest = ReviewManifest {
        version: 1,
        source_identity,
        repository: git.root.to_string_lossy().into_owned(),
        range,
        base_revision: base.clone(),
        head_revision: head.clone(),
        before: before_id,
        after: after_id,
        patch: patch_id,
        git_executable: git.executable,
        metadata_changes: Vec::new(),
    };
    let manifest = put_json(artifacts, &manifest, limits.max_metadata_bytes)?;
    git.check()?;
    Ok(ReviewInspection {
        manifest,
        source_identity,
        patch: patch_id,
        before: before_id,
        after: after_id,
        files: after.files.len(),
        metadata_changes: Vec::new(),
        base_revision: base,
        head_revision: head,
    })
}
#[derive(Clone)]
struct Object {
    path: String,
    mode: u32,
    oid: String,
}
async fn committed(
    git: &git::Git,
    revision: &str,
    store: &ArtifactStore,
    limits: &ReviewLimits,
) -> Result<FrozenTree, ReviewError> {
    let output = git
        .isolated(
            &[
                "ls-tree".into(),
                "-r".into(),
                "-z".into(),
                "--full-tree".into(),
                revision.into(),
            ],
            None,
            Vec::new(),
            limits.max_metadata_bytes,
        )
        .await?;
    freeze_objects(git, tree_objects(&output, limits)?, store, limits).await
}
fn tree_objects(bytes: &[u8], limits: &ReviewLimits) -> Result<Vec<Object>, ReviewError> {
    let mut entries = Vec::new();
    for entry in bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
    {
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or(ReviewError::Protocol)?;
        let meta = git::text(&entry[..tab])?
            .split_whitespace()
            .collect::<Vec<_>>();
        if meta.len() != 3 {
            return Err(ReviewError::Protocol);
        }
        let mode = u32::from_str_radix(meta[0], 8).map_err(|_| ReviewError::Protocol)?;
        let path = git::text(&entry[tab + 1..])?.to_owned();
        if !files::safe(&path) {
            return Err(ReviewError::Path);
        }
        if !matches!(
            (mode, meta[1]),
            (0o100644 | 0o100755 | 0o120000, "blob") | (0o160000, "commit")
        ) {
            return Err(ReviewError::Unsupported("Git tree entry"));
        }
        entries.push(Object {
            path,
            mode,
            oid: meta[2].into(),
        });
        if entries.len() > limits.max_files {
            return Err(ReviewError::Limit("file count"));
        }
    }
    Ok(entries)
}
async fn indexed(
    git: &git::Git,
    index: &Path,
    store: &ArtifactStore,
    limits: &ReviewLimits,
) -> Result<FrozenTree, ReviewError> {
    let bytes = git
        .isolated(
            &["ls-files".into(), "--stage".into(), "-z".into()],
            Some(index),
            Vec::new(),
            limits.max_metadata_bytes,
        )
        .await?;
    let mut entries = Vec::new();
    for entry in bytes.split(|b| *b == 0).filter(|entry| !entry.is_empty()) {
        let tab = entry
            .iter()
            .position(|b| *b == b'\t')
            .ok_or(ReviewError::Protocol)?;
        let fields = git::text(&entry[..tab])?
            .split_whitespace()
            .collect::<Vec<_>>();
        if fields.len() != 3 || fields[2] != "0" {
            return Err(ReviewError::Unsupported("unmerged index"));
        }
        let path = git::text(&entry[tab + 1..])?.to_owned();
        if !files::safe(&path) {
            return Err(ReviewError::Path);
        }
        entries.push(Object {
            path,
            mode: u32::from_str_radix(fields[0], 8).map_err(|_| ReviewError::Protocol)?,
            oid: fields[1].into(),
        });
        if entries.len() > limits.max_files {
            return Err(ReviewError::Limit("file count"));
        }
    }
    freeze_objects(git, entries, store, limits).await
}
async fn freeze_objects(
    git: &git::Git,
    objects: Vec<Object>,
    store: &ArtifactStore,
    limits: &ReviewLimits,
) -> Result<FrozenTree, ReviewError> {
    let mut input = Vec::new();
    for object in &objects {
        if !git::oid_valid(&object.oid, &git.object_format) {
            return Err(ReviewError::Protocol);
        }
        if object.mode != 0o160000 {
            input.extend_from_slice(object.oid.as_bytes());
            input.push(b'\n');
        }
    }
    let checked = git
        .isolated(
            &["cat-file".into(), "--batch-check".into()],
            None,
            input.clone(),
            limits.max_metadata_bytes,
        )
        .await?;
    let mut total = 0usize;
    for line in git::text(&checked)?.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 || fields[1] != "blob" {
            return Err(ReviewError::Protocol);
        }
        let size: usize = fields[2].parse().map_err(|_| ReviewError::Protocol)?;
        if size > limits.max_file_bytes {
            return Err(ReviewError::Limit("file bytes"));
        }
        total = total
            .checked_add(size)
            .ok_or(ReviewError::Limit("snapshot bytes"))?;
        if total > limits.max_snapshot_bytes {
            return Err(ReviewError::Limit("snapshot bytes"));
        }
    }
    let bytes = git
        .isolated(
            &["cat-file".into(), "--batch".into()],
            None,
            input,
            limits
                .max_snapshot_bytes
                .saturating_add(limits.max_metadata_bytes),
        )
        .await?;
    let mut at = 0usize;
    let mut files = BTreeMap::new();
    total = 0;
    for object in objects {
        git.check()?;
        let (kind, data) = if object.mode == 0o160000 {
            (
                FileKind::Gitlink,
                format!("Subproject commit {}\n", object.oid).into_bytes(),
            )
        } else {
            let end = bytes[at..]
                .iter()
                .position(|byte| *byte == b'\n')
                .ok_or(ReviewError::Protocol)?
                + at;
            let header = git::text(&bytes[at..end])?
                .split_whitespace()
                .collect::<Vec<_>>();
            if header.len() != 3 || header[0] != object.oid || header[1] != "blob" {
                return Err(ReviewError::Protocol);
            }
            let size: usize = header[2].parse().map_err(|_| ReviewError::Protocol)?;
            at = end + 1;
            let data = bytes
                .get(at..at.checked_add(size).ok_or(ReviewError::Protocol)?)
                .ok_or(ReviewError::Protocol)?
                .to_vec();
            at += size;
            if bytes.get(at) != Some(&b'\n') {
                return Err(ReviewError::Protocol);
            }
            at += 1;
            verify_object(&data, &object.oid, &git.object_format)?;
            (
                match object.mode {
                    0o100644 | 0o100755 => FileKind::File,
                    0o120000 => FileKind::Symlink,
                    _ => return Err(ReviewError::Unsupported("index mode")),
                },
                data,
            )
        };
        total = total.saturating_add(data.len());
        if data.len() > limits.max_file_bytes || total > limits.max_snapshot_bytes {
            return Err(ReviewError::Limit("snapshot bytes"));
        }
        let file = FrozenFile {
            kind,
            mode: object.mode,
            permissions: None,
            content: store.put(&data)?,
            bytes: data.len(),
            git_object: Some(object.oid),
        };
        if files.insert(object.path, file).is_some() {
            return Err(ReviewError::Protocol);
        }
    }
    if at != bytes.len() {
        return Err(ReviewError::Protocol);
    }
    Ok(FrozenTree { version: 1, files })
}
fn verify_object(bytes: &[u8], oid: &str, format: &str) -> Result<(), ReviewError> {
    use sha2::Digest as _;
    let header = format!("blob {}\0", bytes.len());
    let actual = if format == "sha256" {
        let mut hash = sha2::Sha256::new();
        hash.update(header.as_bytes());
        hash.update(bytes);
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    } else {
        use sha1::Digest as _;
        let mut hash = sha1::Sha1::new();
        hash.update(header.as_bytes());
        hash.update(bytes);
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    if actual != oid {
        return Err(ReviewError::Corrupt);
    }
    Ok(())
}
async fn index_state(
    git: &git::Git,
    limits: &ReviewLimits,
) -> Result<(Vec<u8>, BTreeMap<String, Vec<u8>>), ReviewError> {
    let output = git
        .original(&["rev-parse", "--git-path", "index"], 16384)
        .await?;
    let path = std::path::PathBuf::from(git::text(&output)?.trim_end());
    let path = if path.is_absolute() {
        path
    } else {
        git.root.join(path)
    };
    let bytes = files::read_regular(&path, limits.max_metadata_bytes)?;
    let mut shared = BTreeMap::new();
    let mut total = bytes.len();
    for (count, entry) in fs::read_dir(path.parent().ok_or(ReviewError::Path)?)?.enumerate() {
        if count >= 4096 {
            return Err(ReviewError::Limit("Git directory entries"));
        }
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(oid) = name.strip_prefix("sharedindex.") {
            if !git::oid_valid(oid, &git.object_format) {
                return Err(ReviewError::Protocol);
            }
            if shared.len() >= 16 {
                return Err(ReviewError::Limit("shared indexes"));
            }
            let content = files::read_regular(&entry.path(), limits.max_metadata_bytes)?;
            total = total.saturating_add(content.len());
            if total > limits.max_metadata_bytes {
                return Err(ReviewError::Limit("index bytes"));
            }
            shared.insert(name.into(), content);
        }
    }
    Ok((bytes, shared))
}
async fn index_identity(git: &git::Git, limits: &ReviewLimits) -> Result<Digest, ReviewError> {
    Digest::of_value(&index_state(git, limits).await?).map_err(|_| ReviewError::Corrupt)
}
async fn copy_index(
    git: &git::Git,
    limits: &ReviewLimits,
) -> Result<(std::path::PathBuf, Digest), ReviewError> {
    let state = index_state(git, limits).await?;
    let identity = Digest::of_value(&state).map_err(|_| ReviewError::Corrupt)?;
    let copy = git.scratch().join("source-index");
    fs::write(&copy, &state.0)?;
    for (name, bytes) in state.1 {
        fs::write(git.metadata.join(name), bytes)?;
    }
    Ok((copy, identity))
}
async fn working(
    git: &git::Git,
    index: &Path,
    store: &ArtifactStore,
    limits: &ReviewLimits,
) -> Result<FrozenTree, ReviewError> {
    let indexed = indexed(git, index, store, limits).await?;
    if indexed
        .files
        .values()
        .any(|file| file.kind == FileKind::Gitlink)
    {
        return Err(ReviewError::Unsupported(
            "working-tree submodules; inspect the submodule separately",
        ));
    }
    let paths = git
        .isolated(
            &[
                "ls-files".into(),
                "--cached".into(),
                "--others".into(),
                "--exclude-standard".into(),
                "-z".into(),
            ],
            Some(index),
            Vec::new(),
            limits.max_metadata_bytes,
        )
        .await?;
    let root = files::Root::open(&git.root)?;
    let mut files = BTreeMap::new();
    let mut total = 0usize;
    for path in paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        git.check()?;
        let path = git::text(path)?;
        if files.contains_key(path) {
            continue;
        }
        if let Some(file) = root.capture(path, store, limits)? {
            total = total.saturating_add(file.bytes);
            if total > limits.max_snapshot_bytes {
                return Err(ReviewError::Limit("snapshot bytes"));
            }
            files.insert(path.into(), file);
            if files.len() > limits.max_files {
                return Err(ReviewError::Limit("file count"));
            }
        }
    }
    if !root.still_at(&git.root) {
        return Err(ReviewError::Changed);
    }
    Ok(FrozenTree { version: 1, files })
}
async fn write_tree(
    git: &git::Git,
    tree: &FrozenTree,
    name: &str,
    store: &ArtifactStore,
    limits: &ReviewLimits,
) -> Result<String, ReviewError> {
    let index = git.scratch().join(format!("{name}-index"));
    git.isolated(
        &["read-tree".into(), "--empty".into()],
        Some(&index),
        Vec::new(),
        8192,
    )
    .await?;
    let mut paths = Vec::new();
    for file in tree
        .files
        .values()
        .filter(|file| matches!(file.kind, FileKind::File | FileKind::Symlink))
    {
        use std::os::unix::ffi::OsStrExt;
        paths.extend(git::quote(store.path(file.content).as_os_str().as_bytes()));
        paths.push(b'\n');
    }
    let hashes = if paths.is_empty() {
        Vec::new()
    } else {
        git.isolated(
            &[
                "hash-object".into(),
                "-w".into(),
                "--no-filters".into(),
                "--stdin-paths".into(),
            ],
            None,
            paths,
            limits.max_metadata_bytes,
        )
        .await?
    };
    let mut hashes = git::text(&hashes)?.lines();
    let mut input = Vec::new();
    for (path, file) in &tree.files {
        git.check()?;
        if file.kind == FileKind::Directory {
            continue;
        }
        let oid = if file.kind == FileKind::Gitlink {
            file.git_object.as_deref().ok_or(ReviewError::Protocol)?
        } else {
            hashes.next().ok_or(ReviewError::Protocol)?
        };
        if !git::oid_valid(oid, &git.object_format) {
            return Err(ReviewError::Protocol);
        }
        if file.kind != FileKind::Gitlink {
            let content = store.read(file.content)?;
            if content.len() != file.bytes {
                return Err(ReviewError::Corrupt);
            }
            verify_object(&content, oid, &git.object_format)?;
        }
        input.extend_from_slice(format!("{:o} {oid}\t{path}", file.mode).as_bytes());
        input.push(0);
    }
    if hashes.next().is_some() {
        return Err(ReviewError::Protocol);
    }
    git.isolated(
        &["update-index".into(), "-z".into(), "--index-info".into()],
        Some(&index),
        input,
        8192,
    )
    .await?;
    let output = git
        .isolated(&["write-tree".into()], Some(&index), Vec::new(), 256)
        .await?;
    let oid = git::text(&output)?.trim();
    if !git::oid_valid(oid, &git.object_format) {
        return Err(ReviewError::Protocol);
    }
    Ok(oid.into())
}
async fn commit_parent(git: &git::Git, revision: &str) -> Result<Option<String>, ReviewError> {
    let bytes = git
        .isolated(
            &[
                "rev-list".into(),
                "--parents".into(),
                "--max-count=1".into(),
                revision.into(),
            ],
            None,
            Vec::new(),
            8192,
        )
        .await?;
    let parts = git::text(&bytes)?.split_whitespace().collect::<Vec<_>>();
    if parts.first() != Some(&revision) {
        return Err(ReviewError::Protocol);
    }
    Ok(parts.get(1).map(|parent| parent.to_string()))
}
fn put_json(
    store: &ArtifactStore,
    value: &impl Serialize,
    limit: usize,
) -> Result<Digest, ReviewError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ReviewError::Corrupt)?;
    if bytes.len() > limit {
        return Err(ReviewError::Limit("manifest bytes"));
    }
    Ok(store.put(&bytes)?)
}
fn validate_limits(limits: &ReviewLimits) -> Result<(), ReviewError> {
    if limits.max_files == 0
        || limits.max_files > 100000
        || limits.max_file_bytes == 0
        || limits.max_file_bytes > 64 * 1024 * 1024
        || limits.max_snapshot_bytes == 0
        || limits.max_snapshot_bytes > 256 * 1024 * 1024
        || limits.max_output_bytes == 0
        || limits.max_output_bytes > 64 * 1024 * 1024
        || limits.max_metadata_bytes == 0
        || limits.max_metadata_bytes > 16 * 1024 * 1024
        || limits.timeout.is_zero()
        || limits.timeout > Duration::from_secs(120)
    {
        Err(ReviewError::Limits)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSide {
    Before,
    After,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFilePage {
    pub files: Vec<(String, FrozenFile)>,
    pub total: usize,
    pub next_offset: Option<usize>,
}
/// Returns range-bound metadata. Content remains a separate immutable artifact.
pub fn file(
    artifacts: &ArtifactStore,
    manifest: Digest,
    side: ReviewSide,
    path: &str,
) -> Result<Option<FrozenFile>, ReviewError> {
    if !files::safe(path) {
        return Err(ReviewError::Path);
    }
    let tree = frozen_tree(artifacts, manifest, side)?;
    Ok(tree.files.get(path).cloned())
}
pub fn page(
    artifacts: &ArtifactStore,
    manifest: Digest,
    side: ReviewSide,
    offset: usize,
    limit: usize,
) -> Result<ReviewFilePage, ReviewError> {
    if limit == 0 || limit > 128 {
        return Err(ReviewError::Limits);
    }
    let tree = frozen_tree(artifacts, manifest, side)?;
    let total = tree.files.len();
    if offset > total {
        return Err(ReviewError::Range);
    }
    let files = tree
        .files
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let next = offset + files.len();
    Ok(ReviewFilePage {
        files,
        total,
        next_offset: (next < total).then_some(next),
    })
}
pub fn manifest(artifacts: &ArtifactStore, digest: Digest) -> Result<ReviewManifest, ReviewError> {
    let value: ReviewManifest = read_json(artifacts, digest)?;
    if value.version != 1
        || value.source_identity
            != Digest::of_value(&(
                1u32,
                &value.range,
                &value.base_revision,
                &value.head_revision,
                value.before,
                value.after,
                value.patch,
            ))
            .map_err(|_| ReviewError::Corrupt)?
    {
        return Err(ReviewError::Corrupt);
    }
    Ok(value)
}
fn frozen_tree(
    artifacts: &ArtifactStore,
    id: Digest,
    side: ReviewSide,
) -> Result<FrozenTree, ReviewError> {
    let value = manifest(artifacts, id)?;
    let tree: FrozenTree = read_json(
        artifacts,
        match side {
            ReviewSide::Before => value.before,
            ReviewSide::After => value.after,
        },
    )?;
    if tree.version != 1 || tree.files.len() > 100000 {
        return Err(ReviewError::Corrupt);
    }
    let mut total = 0usize;
    for (path, file) in &tree.files {
        if !files::safe(path)
            || !matches!(
                (file.kind, file.mode),
                (FileKind::Directory, 0o040000)
                    | (FileKind::File, 0o100644 | 0o100755)
                    | (FileKind::Symlink, 0o120000)
                    | (FileKind::Gitlink, 0o160000)
            )
            || file.permissions.is_some_and(|mode| mode > 0o7777)
            || file.bytes > 64 * 1024 * 1024
        {
            return Err(ReviewError::Corrupt);
        }
        total = total.saturating_add(file.bytes);
        if total > 256 * 1024 * 1024 {
            return Err(ReviewError::Corrupt);
        }
    }
    Ok(tree)
}
fn read_json<T: serde::de::DeserializeOwned>(
    artifacts: &ArtifactStore,
    digest: Digest,
) -> Result<T, ReviewError> {
    let bytes = files::read_regular(&artifacts.path(digest), 16 * 1024 * 1024)?;
    if Digest::of(&bytes) != digest {
        return Err(ReviewError::Corrupt);
    }
    serde_json::from_slice(&bytes).map_err(|_| ReviewError::Corrupt)
}

async fn diff(
    git: &git::Git,
    before: &FrozenTree,
    after: &FrozenTree,
    artifacts: &ArtifactStore,
    limits: &ReviewLimits,
) -> Result<Vec<u8>, ReviewError> {
    let before_tree = write_tree(git, before, "before", artifacts, limits).await?;
    let after_tree = write_tree(git, after, "after", artifacts, limits).await?;
    git.isolated(
        &[
            "diff-tree".into(),
            "--no-commit-id".into(),
            "-r".into(),
            "-p".into(),
            "--binary".into(),
            "--full-index".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--no-renames".into(),
            "--submodule=short".into(),
            "--src-prefix=a/".into(),
            "--dst-prefix=b/".into(),
            "--unified=2147483647".into(),
            before_tree,
            after_tree,
        ],
        None,
        Vec::new(),
        limits.max_output_bytes,
    )
    .await
}
