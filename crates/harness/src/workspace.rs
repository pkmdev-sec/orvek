use crate::{
    Digest,
    artifacts::{ArtifactError, ArtifactStore},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};
use thiserror::Error;
mod reconcile;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnapshotPolicy {
    pub excluded_roots: Vec<String>,
    pub max_files: usize,
    pub max_bytes: u64,
}

impl Default for SnapshotPolicy {
    fn default() -> Self {
        Self {
            excluded_roots: vec![
                ".git".into(),
                ".orvek".into(),
                ".Trash".into(),
                "target".into(),
                "node_modules".into(),
                ".venv".into(),
                ".terraform".into(),
                ".env".into(),
            ],
            max_files: 100_000,
            max_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    File { content: Digest, mode: u32 },
    Directory { mode: u32 },
    Symlink { target: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub policy: SnapshotPolicy,
    pub entries: BTreeMap<String, Entry>,
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace snapshot policies differ")]
    ReconcilePolicy,
    #[error("private and user workspace changes conflict at {paths:?} (truncated: {truncated})")]
    ReconcileConflict { paths: Vec<String>, truncated: bool },
    #[error("workspace I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error("snapshot encoding: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported or escaping workspace path: {0}")]
    UnsafePath(PathBuf),
    #[error("snapshot exceeds its declared file or byte limit")]
    Limit,
    #[error("workspace changed while being captured")]
    ConcurrentChange,
    #[error("snapshot destination must be a new directory: {0}")]
    Destination(PathBuf),
    #[error("unsupported snapshot format")]
    Version,
}

impl Snapshot {
    pub fn verify_artifacts(&self, artifacts: &ArtifactStore) -> Result<(), WorkspaceError> {
        self.validate()?;
        let mut total = 0u64;
        for entry in self.entries.values() {
            if let Entry::File { content, .. } = entry {
                total = total.saturating_add(artifacts.read(*content)?.len() as u64);
                if total > self.policy.max_bytes {
                    return Err(WorkspaceError::Limit);
                }
            }
        }
        Ok(())
    }
    pub fn capture(
        root: &Path,
        policy: SnapshotPolicy,
        artifacts: &ArtifactStore,
    ) -> Result<Self, WorkspaceError> {
        let root = root.canonicalize().map_err(|source| WorkspaceError::Io {
            path: root.to_owned(),
            source,
        })?;
        if !root.is_dir() {
            return Err(WorkspaceError::UnsafePath(root));
        }
        let first = Self::scan(&root, policy.clone(), Some(artifacts))?;
        let second = Self::scan(&root, policy, None)?;
        if first != second {
            return Err(WorkspaceError::ConcurrentChange);
        }
        first.validate()?;
        Ok(first)
    }

    pub fn publish(&self, artifacts: &ArtifactStore) -> Result<Digest, WorkspaceError> {
        self.validate()?;
        Ok(artifacts.put(&serde_json::to_vec(self)?)?)
    }

    pub fn load(digest: Digest, artifacts: &ArtifactStore) -> Result<Self, WorkspaceError> {
        let snapshot: Self = serde_json::from_slice(&artifacts.read(digest)?)?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn matches(&self, root: &Path) -> Result<bool, WorkspaceError> {
        Ok(Self::scan(root, self.policy.clone(), None)? == *self)
    }

    pub fn matches_exact(&self, root: &Path) -> Result<bool, WorkspaceError> {
        let mut policy = self.policy.clone();
        policy.excluded_roots.clear();
        Ok(Self::scan(root, policy, None)?.entries == self.entries)
    }

    pub fn materialize(
        &self,
        destination: &Path,
        artifacts: &ArtifactStore,
        readonly: bool,
    ) -> Result<(), WorkspaceError> {
        self.verify_artifacts(artifacts)?;
        if destination.exists() {
            return Err(WorkspaceError::Destination(destination.to_owned()));
        }
        fs::create_dir(destination).map_err(|source| WorkspaceError::Io {
            path: destination.to_owned(),
            source,
        })?;
        let result = (|| {
            for (name, entry) in &self.entries {
                let path = destination.join(name);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|source| WorkspaceError::Io {
                        path: parent.to_owned(),
                        source,
                    })?;
                }
                match entry {
                    Entry::Directory { .. } => fs::create_dir_all(&path)
                        .map_err(|source| WorkspaceError::Io { path, source })?,
                    Entry::File { content, mode } => {
                        let bytes = artifacts.read(*content)?;
                        let mut file = fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&path)
                            .map_err(|source| WorkspaceError::Io {
                                path: path.clone(),
                                source,
                            })?;
                        file.write_all(&bytes)
                            .map_err(|source| WorkspaceError::Io {
                                path: path.clone(),
                                source,
                            })?;
                        set_mode(&path, *mode, readonly)?;
                        file.sync_all().map_err(|source| WorkspaceError::Io {
                            path: path.clone(),
                            source,
                        })?;
                    }
                    Entry::Symlink { .. } => {}
                }
            }
            for (name, entry) in &self.entries {
                if let Entry::Symlink { target } = entry {
                    let path = destination.join(name);
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(target, &path)
                        .map_err(|source| WorkspaceError::Io { path, source })?;
                    #[cfg(not(unix))]
                    return Err(WorkspaceError::UnsafePath(path));
                }
            }
            for (name, entry) in self.entries.iter().rev() {
                if let Entry::Directory { mode } = entry {
                    set_mode(&destination.join(name), *mode, readonly)?;
                    sync_directory(&destination.join(name))?;
                }
            }
            set_mode(destination, 0o755, readonly)?;
            sync_directory(destination)?;
            if let Some(parent) = destination.parent() {
                sync_directory(parent)?;
            }
            Ok(())
        })();
        if result.is_err() {
            // The destination was created by this operation and has never been handed to a worker.
            let _ = fs::remove_dir_all(destination);
        }
        result
    }

    pub fn changed_paths(&self, other: &Self) -> Vec<String> {
        let mut paths = self
            .entries
            .keys()
            .chain(other.entries.keys())
            .cloned()
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths.retain(|path| self.entries.get(path) != other.entries.get(path));
        paths
    }

    fn scan(
        root: &Path,
        policy: SnapshotPolicy,
        artifacts: Option<&ArtifactStore>,
    ) -> Result<Self, WorkspaceError> {
        if policy.max_files == 0 || policy.max_bytes == 0 {
            return Err(WorkspaceError::Limit);
        }
        for excluded in &policy.excluded_roots {
            if excluded.is_empty()
                || Path::new(excluded).components().count() != 1
                || !safe_relative(Path::new(excluded))
            {
                return Err(WorkspaceError::UnsafePath(excluded.into()));
            }
        }
        let mut scanner = Scanner {
            root,
            policy: &policy,
            artifacts,
            entries: BTreeMap::new(),
            bytes: 0,
            discovered: 0,
        };
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, open};
            let descriptor = open(
                root,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|source| WorkspaceError::Io {
                path: root.to_owned(),
                source: source.into(),
            })?;
            scanner.directory(Path::new(""), 0, &fs::File::from(descriptor))?;
        }
        #[cfg(not(unix))]
        return Err(WorkspaceError::UnsafePath(root.to_owned()));
        let mut entries = scanner.entries;
        let unsupported_links = entries
            .iter()
            .filter_map(|(name, entry)| {
                let Entry::Symlink { target } = entry else {
                    return None;
                };
                let resolved = resolve_link(Path::new(name), target).ok()?;
                (!matches!(
                    entries.get(&resolved),
                    Some(Entry::File { .. } | Entry::Directory { .. })
                ))
                .then(|| name.clone())
            })
            .collect::<Vec<_>>();
        for name in unsupported_links {
            entries.remove(&name);
        }
        Ok(Self {
            version: 1,
            policy,
            entries,
        })
    }

    fn validate(&self) -> Result<(), WorkspaceError> {
        if self.version != 1 {
            return Err(WorkspaceError::Version);
        }
        if self.policy.max_files == 0
            || self.policy.max_bytes == 0
            || self.entries.len() > self.policy.max_files
        {
            return Err(WorkspaceError::Limit);
        }
        for (name, entry) in &self.entries {
            let path = Path::new(name);
            if !safe_relative(path) {
                return Err(WorkspaceError::UnsafePath(path.to_owned()));
            }
            for parent in path
                .ancestors()
                .skip(1)
                .filter(|p| !p.as_os_str().is_empty())
            {
                if !matches!(
                    parent.to_str().and_then(|p| self.entries.get(p)),
                    Some(Entry::Directory { .. })
                ) {
                    return Err(WorkspaceError::UnsafePath(path.to_owned()));
                }
            }
            if let Entry::Symlink { target } = entry {
                let resolved = resolve_link(path, target)?;
                if !matches!(
                    self.entries.get(&resolved),
                    Some(Entry::File { .. } | Entry::Directory { .. })
                ) || Path::new(&resolved)
                    .ancestors()
                    .skip(1)
                    .filter(|p| !p.as_os_str().is_empty())
                    .any(|p| {
                        !matches!(
                            p.to_str().and_then(|p| self.entries.get(p)),
                            Some(Entry::Directory { .. })
                        )
                    })
                {
                    return Err(WorkspaceError::UnsafePath(path.to_owned()));
                }
            }
        }
        Ok(())
    }
}

struct Scanner<'a> {
    root: &'a Path,
    policy: &'a SnapshotPolicy,
    artifacts: Option<&'a ArtifactStore>,
    entries: BTreeMap<String, Entry>,
    bytes: u64,
    discovered: usize,
}

#[cfg(unix)]
impl Scanner<'_> {
    fn directory(
        &mut self,
        relative: &Path,
        depth: usize,
        directory: &fs::File,
    ) -> Result<(), WorkspaceError> {
        use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, openat, readlinkat, statat};
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        if depth > 64 {
            return Err(WorkspaceError::Limit);
        }
        let absolute = self.root.join(relative);
        let error = |source: rustix::io::Errno| WorkspaceError::Io {
            path: absolute.clone(),
            source: source.into(),
        };
        let entries = Dir::read_from(directory).map_err(error)?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(error)?;
            let name = entry.file_name();
            if name.to_bytes() != b"." && name.to_bytes() != b".." {
                if self
                    .policy
                    .excluded_roots
                    .iter()
                    .any(|excluded| excluded.as_bytes() == name.to_bytes())
                {
                    continue;
                }
                if self.discovered >= self.policy.max_files {
                    return Err(WorkspaceError::Limit);
                }
                self.discovered += 1;
                names.push(name.to_owned());
            }
        }
        names.sort();
        for name in names {
            let path = relative.join(OsStr::from_bytes(name.as_bytes()));
            let Some(key) = path.to_str().map(str::to_owned) else {
                continue;
            };
            if self.entries.len() >= self.policy.max_files {
                return Err(WorkspaceError::Limit);
            }
            let meta =
                statat(directory, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW).map_err(error)?;
            match FileType::from_raw_mode(meta.st_mode) {
                FileType::Symlink => {
                    let target =
                        readlinkat(directory, name.as_c_str(), Vec::new()).map_err(error)?;
                    let Ok(target) = target.to_str() else {
                        continue;
                    };
                    if Path::new(target).is_absolute() || resolve_link(&path, target).is_err() {
                        continue;
                    }
                    self.entries.insert(
                        key,
                        Entry::Symlink {
                            target: target.to_owned(),
                        },
                    );
                }
                FileType::Directory => {
                    let descriptor = openat(
                        directory,
                        name.as_c_str(),
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(error)?;
                    let child = fs::File::from(descriptor);
                    let meta = child.metadata().map_err(|source| WorkspaceError::Io {
                        path: path.clone(),
                        source,
                    })?;
                    self.entries
                        .insert(key, Entry::Directory { mode: mode(&meta) });
                    self.directory(&path, depth + 1, &child)?;
                }
                FileType::RegularFile => {
                    let descriptor = openat(
                        directory,
                        name.as_c_str(),
                        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                        Mode::empty(),
                    )
                    .map_err(error)?;
                    let file = fs::File::from(descriptor);
                    let meta = file.metadata().map_err(|source| WorkspaceError::Io {
                        path: path.clone(),
                        source,
                    })?;
                    use std::os::unix::fs::MetadataExt;
                    if !meta.is_file() || meta.nlink() != 1 {
                        continue;
                    }
                    let remaining = self.policy.max_bytes.saturating_sub(self.bytes);
                    if meta.len() > remaining {
                        return Err(WorkspaceError::Limit);
                    }
                    let mut bytes = Vec::new();
                    file.take(remaining.saturating_add(1))
                        .read_to_end(&mut bytes)
                        .map_err(|source| WorkspaceError::Io {
                            path: path.clone(),
                            source,
                        })?;
                    self.bytes = self.bytes.saturating_add(bytes.len() as u64);
                    if self.bytes > self.policy.max_bytes {
                        return Err(WorkspaceError::Limit);
                    }
                    let content = match self.artifacts {
                        Some(store) => store.put(&bytes)?,
                        None => Digest::of(&bytes),
                    };
                    self.entries.insert(
                        key,
                        Entry::File {
                            content,
                            mode: mode(&meta),
                        },
                    );
                }
                _ => continue,
            }
        }
        Ok(())
    }
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn sync_directory(path: &Path) -> Result<(), WorkspaceError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| WorkspaceError::Io {
            path: path.to_owned(),
            source,
        })
}

fn resolve_link(path: &Path, target: &str) -> Result<String, WorkspaceError> {
    let mut parts = path
        .parent()
        .unwrap_or(Path::new(""))
        .components()
        .map(|p| p.as_os_str().to_owned())
        .collect::<Vec<_>>();
    for part in Path::new(target).components() {
        match part {
            Component::Normal(name) => parts.push(name.to_owned()),
            Component::ParentDir if !parts.is_empty() => {
                parts.pop();
            }
            Component::CurDir => {}
            _ => return Err(WorkspaceError::UnsafePath(path.to_owned())),
        }
    }
    let resolved: PathBuf = parts.into_iter().collect();
    resolved
        .to_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| WorkspaceError::UnsafePath(path.to_owned()))
}

#[cfg(unix)]
fn mode(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn mode(meta: &fs::Metadata) -> u32 {
    if meta.permissions().readonly() {
        0o444
    } else {
        0o644
    }
}

fn set_mode(path: &Path, mode: u32, readonly: bool) -> Result<(), WorkspaceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if readonly { mode & !0o222 } else { mode }),
        )
        .map_err(|source| WorkspaceError::Io {
            path: path.to_owned(),
            source,
        })?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path)
            .map_err(|source| WorkspaceError::Io {
                path: path.to_owned(),
                source,
            })?
            .permissions();
        permissions.set_readonly(readonly || mode & 0o222 == 0);
        fs::set_permissions(path, permissions).map_err(|source| WorkspaceError::Io {
            path: path.to_owned(),
            source,
        })?;
    }
    Ok(())
}
