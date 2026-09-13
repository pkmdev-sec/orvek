use super::{ExecutionLimits, RuntimeError};
use crate::{
    Digest,
    artifacts::ArtifactStore,
    workspace::{Entry, Snapshot, SnapshotPolicy},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct RetainedGuest {
    pub job_id: Uuid,
    pub tree: PathBuf,
    pub baseline: Snapshot,
    pub guest: Snapshot,
    pub baseline_digest: Digest,
    pub guest_digest: Digest,
}

pub(super) struct Workspace {
    temporary: Option<TempDir>,
    pub root: PathBuf,
    pub source: PathBuf,
    pub incoming: PathBuf,
    pub baseline: Snapshot,
    pub artifacts: ArtifactStore,
    identity: (u64, u64, u32),
    _lease: File,
}
impl Workspace {
    pub fn prepare(
        root: &Path,
        limits: &ExecutionLimits,
        readonly: bool,
    ) -> Result<Self, RuntimeError> {
        let root = root.canonicalize()?;
        let meta = fs::symlink_metadata(&root)?;
        if !meta.is_dir()
            || meta.mode() & 0o7000 != 0
            || meta.mode() & 0o500 != 0o500
            || root.to_str().is_none_or(|s| s.contains(','))
        {
            return Err(RuntimeError::Request("invalid workspace path"));
        }
        let parent = root
            .parent()
            .ok_or(RuntimeError::Request("workspace cannot be filesystem root"))?;
        if !readonly {
            let recovery = parent.join(".orvek-guest-results");
            if recovery.try_exists()?
                && fs::read_dir(&recovery)?
                    .take(limits.retained_guests as usize)
                    .count()
                    >= limits.retained_guests as usize
            {
                return Err(RuntimeError::Setup(
                    "retained guest limit reached; preserve or resolve previous conflicts first"
                        .into(),
                ));
            }
        }
        let lock_name = format!(
            ".orvek-runtime-lock-{}",
            Digest::of(root.as_os_str().as_bytes())
        );
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(parent.join(lock_name))?;
        lease
            .try_lock_exclusive()
            .map_err(|_| RuntimeError::Setup("workspace already has an active execution".into()))?;
        let temporary = tempfile::Builder::new()
            .prefix(".orvek-runtime-")
            .tempdir_in(parent)?;
        let artifacts = ArtifactStore::open(
            &temporary.path().join("artifacts"),
            limits.workspace_bytes.max(8 * 1024 * 1024),
        )
        .map_err(error)?;
        let policy = SnapshotPolicy {
            excluded_roots: Vec::new(),
            max_files: limits.workspace_inodes.saturating_sub(1) as usize,
            max_bytes: limits.workspace_bytes,
        };
        let baseline = Snapshot::capture(&root, policy, &artifacts).map_err(error)?;
        for entry in baseline.entries.values() {
            match entry {
                Entry::File { mode, .. } if mode & !0o777 != 0 || mode & 0o400 == 0 => {
                    return Err(RuntimeError::Request("unsupported source file permissions"));
                }
                Entry::Directory { mode } if mode & !0o777 != 0 || mode & 0o500 != 0o500 => {
                    return Err(RuntimeError::Request(
                        "unsupported source directory permissions",
                    ));
                }
                _ => {}
            }
        }
        if serde_json::to_vec(&baseline).map_err(error)?.len() > 8 * 1024 * 1024 {
            return Err(RuntimeError::Request("source metadata exceeds limit"));
        }
        let source = temporary.path().join("source");
        baseline
            .materialize(&source, &artifacts, false)
            .map_err(error)?;
        fs::set_permissions(&source, fs::Permissions::from_mode(meta.mode() & 0o777))?;
        if fs::metadata(&source)?.uid() == 0 {
            use rustix::{
                fs::{AtFlags, CWD, chownat},
                process::{Gid, Uid},
            };
            for path in baseline.entries.keys() {
                chownat(
                    CWD,
                    source.join(path),
                    Some(Uid::from_raw(65534)),
                    Some(Gid::from_raw(65534)),
                    AtFlags::SYMLINK_NOFOLLOW,
                )
                .map_err(|e| RuntimeError::Io(e.into()))?;
            }
            chownat(
                CWD,
                &source,
                Some(Uid::from_raw(65534)),
                Some(Gid::from_raw(65534)),
                AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(|e| RuntimeError::Io(e.into()))?;
        }
        let incoming = temporary.path().join("incoming");
        fs::create_dir(&incoming)?;
        Ok(Self {
            temporary: Some(temporary),
            root,
            source,
            incoming,
            baseline,
            artifacts,
            identity: (meta.dev(), meta.ino(), meta.mode() & 0o7777),
            _lease: lease,
        })
    }
    pub fn directory(&self) -> &Path {
        self.temporary
            .as_ref()
            .expect("live workspace staging")
            .path()
    }
    pub fn validate_guest(
        &self,
        entries: std::collections::BTreeMap<String, Entry>,
    ) -> Result<Snapshot, RuntimeError> {
        let snapshot = Snapshot {
            version: 1,
            policy: self.baseline.policy.clone(),
            entries,
        };
        snapshot.publish(&self.artifacts).map_err(error)?;
        for (path, entry) in &snapshot.entries {
            if let Entry::Symlink { target } = entry {
                std::os::unix::fs::symlink(target, self.incoming.join(path))?;
            }
        }
        for (path, entry) in snapshot.entries.iter().rev() {
            if let Entry::Directory { mode } = entry {
                fs::set_permissions(self.incoming.join(path), fs::Permissions::from_mode(*mode))?;
                File::open(self.incoming.join(path))?.sync_all()?;
            }
        }
        let observed = Snapshot::capture(
            &self.incoming,
            self.baseline.policy.clone(),
            &self.artifacts,
        )
        .map_err(error)?;
        if observed != snapshot {
            return Err(RuntimeError::Setup(
                "guest tree did not match its received identity".into(),
            ));
        }
        File::open(&self.incoming)?.sync_all()?;
        Ok(snapshot)
    }
    pub fn publish(
        &mut self,
        guest: &Snapshot,
        job_id: Uuid,
        cancel: &CancellationToken,
        deadline: tokio::time::Instant,
    ) -> Result<bool, RuntimeError> {
        if cancel.is_cancelled() || tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        if !self.matches_baseline(&self.root) {
            if let Err(error) = self.retain(guest, job_id) {
                self.preserve_unknown();
                return Err(error);
            }
            return Ok(false);
        }
        if cancel.is_cancelled() || tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        if guest.entries == self.baseline.entries {
            return Ok(true);
        }
        fs::set_permissions(&self.incoming, fs::Permissions::from_mode(self.identity.2))?;
        let new_meta = fs::symlink_metadata(&self.incoming)?;
        exchange(&self.root, &self.incoming)?;
        if !self.matches_baseline(&self.incoming) {
            let current = fs::symlink_metadata(&self.root)?;
            if current.dev() != new_meta.dev() || current.ino() != new_meta.ino() {
                self.preserve_unknown();
                return Err(RuntimeError::Setup(
                    "publication raced with another owner; recovery trees retained".into(),
                ));
            }
            exchange(&self.root, &self.incoming)?;
            if let Err(error) = self.retain(guest, job_id) {
                self.preserve_unknown();
                return Err(error);
            }
            return Ok(false);
        }
        if !guest.matches_exact(&self.root).map_err(error)? {
            self.preserve_unknown();
            return Err(RuntimeError::Setup(
                "published workspace changed during validation; recovery retained".into(),
            ));
        }
        File::open(
            self.root
                .parent()
                .ok_or(RuntimeError::Request("workspace parent missing"))?,
        )?
        .sync_all()?;
        Ok(true)
    }
    fn matches_baseline(&self, path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok_and(|m| {
            m.is_dir()
                && m.dev() == self.identity.0
                && m.ino() == self.identity.1
                && (m.mode() & 0o7777) == self.identity.2
        }) && self.baseline.matches_exact(path).unwrap_or(false)
    }
    fn retain(&mut self, guest: &Snapshot, job_id: Uuid) -> Result<(), RuntimeError> {
        let root = self
            .root
            .parent()
            .ok_or(RuntimeError::Request("workspace parent missing"))?
            .join(".orvek-guest-results");
        fs::create_dir_all(&root)?;
        if fs::symlink_metadata(&root)?.file_type().is_symlink() {
            return Err(RuntimeError::Setup(
                "unsafe guest recovery directory".into(),
            ));
        }
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let directory = root.join(job_id.to_string());
        fs::create_dir(&directory)?;
        let tree = directory.join("tree");
        fs::rename(&self.incoming, &tree)?;
        let record = RetainedGuest {
            job_id,
            tree,
            baseline: self.baseline.clone(),
            guest: guest.clone(),
            baseline_digest: Digest::of_value(&self.baseline).map_err(error)?,
            guest_digest: Digest::of_value(guest).map_err(error)?,
        };
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(directory.join("receipt.json"))?;
        file.write_all(&serde_json::to_vec(&record).map_err(error)?)?;
        file.sync_all()?;
        File::open(&directory)?.sync_all()?;
        File::open(root)?.sync_all()?;
        Ok(())
    }
    fn preserve_unknown(&mut self) {
        if let Some(temp) = self.temporary.take() {
            let _ = temp.keep();
        }
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        if let Some(temp) = &self.temporary {
            make_removable(temp.path());
        }
    }
}
fn make_removable(path: &Path) {
    if fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                make_removable(&entry.path());
            }
        }
    }
}
fn exchange(first: &Path, second: &Path) -> Result<(), RuntimeError> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        first,
        rustix::fs::CWD,
        second,
        rustix::fs::RenameFlags::EXCHANGE,
    )
    .map_err(|e| io_error(e.into()))
}
fn io_error(error: std::io::Error) -> RuntimeError {
    RuntimeError::Io(error)
}
fn error(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Setup(error.to_string())
}

pub(super) fn retained(
    workspace: &Path,
    job_id: Uuid,
    limits: &ExecutionLimits,
) -> Result<Option<RetainedGuest>, RuntimeError> {
    let workspace = workspace.canonicalize()?;
    let path = workspace
        .parent()
        .ok_or(RuntimeError::Request("workspace parent missing"))?
        .join(".orvek-guest-results")
        .join(job_id.to_string())
        .join("receipt.json");
    if !path.try_exists()? {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(RuntimeError::Setup("retained metadata limit".into()));
    }
    let record: RetainedGuest = serde_json::from_slice(&bytes).map_err(error)?;
    let expected_tree = workspace
        .parent()
        .ok_or(RuntimeError::Request("workspace parent missing"))?
        .join(".orvek-guest-results")
        .join(job_id.to_string())
        .join("tree");
    if record.tree != expected_tree
        || record.guest.version != 1
        || record.baseline.version != 1
        || !record.guest.policy.excluded_roots.is_empty()
        || record.guest.policy.max_bytes > limits.workspace_bytes
        || record.guest.policy.max_files as u64 >= limits.workspace_inodes
    {
        return Err(RuntimeError::Setup("invalid retained guest scope".into()));
    }
    if record.baseline_digest != Digest::of_value(&record.baseline).map_err(error)? {
        return Err(RuntimeError::Setup(
            "retained baseline integrity failure".into(),
        ));
    }
    if record.job_id != job_id
        || Digest::of_value(&record.guest).map_err(error)? != record.guest_digest
        || !record.guest.matches_exact(&record.tree).map_err(error)?
    {
        return Err(RuntimeError::Setup(
            "retained guest integrity failure".into(),
        ));
    }
    Ok(Some(record))
}
