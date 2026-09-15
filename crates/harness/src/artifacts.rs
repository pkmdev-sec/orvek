use crate::Digest;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};
use tempfile::{Builder as TempFileBuilder, NamedTempFile};
use thiserror::Error;
use uuid::Uuid;

const ARTIFACT_TEMP_PREFIX: &str = ".orvek-artifact-";

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("artifact I/O: {0}")]
    Io(#[from] io::Error),
    #[error("artifact exceeds byte limit {0}")]
    Limit(u64),
    #[error("artifact store exceeds global byte limit {0}")]
    Quota(u64),
    #[error("artifact digest mismatch: {0}")]
    Integrity(Digest),
    #[error("artifact path is not a regular file: {0}")]
    UnsafePath(PathBuf),
    #[error("artifact quota state: {0}")]
    State(#[from] rusqlite::Error),
    #[error("artifact quota lock is poisoned")]
    Lock,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PublicArtifactRef(Digest);

impl PublicArtifactRef {
    pub const fn from_digest(digest: Digest) -> Self {
        Self(digest)
    }

    pub const fn digest(self) -> Digest {
        self.0
    }
}

#[derive(Clone)]
pub struct ArtifactStore {
    root: PathBuf,
    max_bytes: u64,
    quota: Arc<ArtifactQuota>,
}

pub(crate) struct ArtifactQuota {
    roots: Vec<PathBuf>,
    database: Option<PathBuf>,
    max_bytes: u64,
    lock: Mutex<()>,
    _owner: Option<Arc<fs::File>>,
}

#[derive(Clone)]
pub(crate) struct ArtifactStaging {
    root: PathBuf,
    quota: Arc<ArtifactQuota>,
}

/// One verified immutable download. Evict before reading another blob so the
/// cache cannot exceed the store's single-artifact byte limit.
#[derive(Default)]
pub(crate) struct DownloadCache {
    current: Option<(Digest, Vec<u8>)>,
}

impl DownloadCache {
    pub fn read<'a>(
        &'a mut self,
        store: &ArtifactStore,
        digest: Digest,
    ) -> Result<&'a [u8], ArtifactError> {
        if self
            .current
            .as_ref()
            .is_none_or(|(cached, _)| *cached != digest)
        {
            self.current = None;
            self.current = Some((digest, store.read(digest)?));
        }
        Ok(&self
            .current
            .as_ref()
            .expect("successful read installs a download")
            .1)
    }
}

impl ArtifactStore {
    pub(crate) fn open(root: &Path, max_bytes: u64) -> Result<Self, ArtifactError> {
        let quota = Arc::new(ArtifactQuota {
            roots: vec![root.to_owned()],
            database: None,
            max_bytes,
            lock: Mutex::new(()),
            _owner: None,
        });
        Self::open_with_quota(root, max_bytes, quota)
    }

    pub(crate) fn open_host(
        public_root: &Path,
        sealed_root: &Path,
        staging_root: &Path,
        database: &Path,
        max_bytes: u64,
        owner: Arc<fs::File>,
    ) -> Result<(Self, Self, ArtifactStaging), ArtifactError> {
        for root in [public_root, sealed_root, staging_root] {
            fs::create_dir_all(root)?;
        }
        let quota = Arc::new(ArtifactQuota {
            roots: vec![
                public_root.to_owned(),
                sealed_root.to_owned(),
                staging_root.to_owned(),
            ],
            database: Some(database.to_owned()),
            max_bytes,
            lock: Mutex::new(()),
            _owner: Some(owner),
        });
        let public = Self::open_with_quota(public_root, max_bytes, quota.clone())?;
        let sealed = Self::open_with_quota(sealed_root, max_bytes, quota.clone())?;
        let staging = ArtifactStaging::open(staging_root, quota)?;
        Ok((public, sealed, staging))
    }

    fn open_with_quota(
        root: &Path,
        max_bytes: u64,
        quota: Arc<ArtifactQuota>,
    ) -> Result<Self, ArtifactError> {
        fs::create_dir_all(root)?;
        if fs::symlink_metadata(root)?.file_type().is_symlink() {
            return Err(ArtifactError::UnsafePath(root.to_owned()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            root: root.canonicalize()?,
            max_bytes,
            quota,
        })
    }

    pub fn write(&self, bytes: &[u8]) -> Result<PublicArtifactRef, ArtifactError> {
        self.put(bytes).map(PublicArtifactRef::from_digest)
    }

    pub fn resolve(&self, artifact: PublicArtifactRef) -> Result<Vec<u8>, ArtifactError> {
        self.read(artifact.digest())
    }

    pub(crate) fn put(&self, bytes: &[u8]) -> Result<Digest, ArtifactError> {
        if bytes.len() as u64 > self.max_bytes {
            return Err(ArtifactError::Limit(self.max_bytes));
        }
        let digest = Digest::of(bytes);
        let path = self.path(digest);
        if path.try_exists()? {
            self.read(digest)?;
            return Ok(digest);
        }
        let _guard = self.quota.lock()?;
        if path.try_exists()? {
            self.read(digest)?;
            return Ok(digest);
        }
        self.quota.ensure_capacity_locked(bytes.len() as u64)?;
        self.put_unchecked(digest, bytes)?;
        Ok(digest)
    }

    pub(crate) fn put_unchecked(&self, digest: Digest, bytes: &[u8]) -> Result<(), ArtifactError> {
        if Digest::of(bytes) != digest {
            return Err(ArtifactError::Integrity(digest));
        }
        let path = self.path(digest);
        if path.try_exists()? {
            self.read(digest)?;
            return Ok(());
        }
        let mut file = TempFileBuilder::new()
            .prefix(ARTIFACT_TEMP_PREFIX)
            .tempfile_in(&self.root)?;
        file.write_all(bytes)?;
        file.as_file().sync_all()?;
        match file.persist_noclobber(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                self.read(digest)?;
            }
            Err(error) => return Err(error.error.into()),
        }
        fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }

    pub(crate) fn read(&self, digest: Digest) -> Result<Vec<u8>, ArtifactError> {
        let path = self.path(digest);
        let meta = fs::symlink_metadata(&path)?;
        if !meta.file_type().is_file() {
            return Err(ArtifactError::UnsafePath(path));
        }
        if meta.len() > self.max_bytes {
            return Err(ArtifactError::Limit(self.max_bytes));
        }
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(self.max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > self.max_bytes {
            return Err(ArtifactError::Limit(self.max_bytes));
        }
        if Digest::of(&bytes) != digest {
            return Err(ArtifactError::Integrity(digest));
        }
        Ok(bytes)
    }

    pub(crate) fn path(&self, digest: Digest) -> PathBuf {
        self.root.join(digest.to_string())
    }
}

impl ArtifactStore {
    pub(crate) fn collect_temporary_locked(&self) -> Result<(), ArtifactError> {
        let mut removed = false;
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| ArtifactError::UnsafePath(path.clone()))?;
            if !name.starts_with(ARTIFACT_TEMP_PREFIX) {
                continue;
            }
            if !fs::symlink_metadata(&path)?.file_type().is_file() {
                return Err(ArtifactError::UnsafePath(path));
            }
            fs::remove_file(path)?;
            removed = true;
        }
        if removed {
            fs::File::open(&self.root)?.sync_all()?;
        }
        Ok(())
    }

    pub(crate) fn digests_locked(&self) -> Result<Vec<Digest>, ArtifactError> {
        let mut digests = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file() {
                return Err(ArtifactError::UnsafePath(path));
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| ArtifactError::UnsafePath(path.clone()))?;
            let digest = name
                .parse()
                .map_err(|_| ArtifactError::UnsafePath(path.clone()))?;
            digests.push(digest);
        }
        digests.sort();
        Ok(digests)
    }

    pub(crate) fn remove_locked(&self, digest: Digest) -> Result<(), ArtifactError> {
        match fs::remove_file(self.path(digest)) {
            Ok(()) => fs::File::open(&self.root)?.sync_all().map_err(Into::into),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

impl ArtifactQuota {
    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, ()>, ArtifactError> {
        self.lock.lock().map_err(|_| ArtifactError::Lock)
    }

    pub(crate) fn ensure_capacity_locked(&self, additional: u64) -> Result<(), ArtifactError> {
        let used = self.used_bytes()?;
        let reserved = self.unmaterialized_reservations()?;
        if used
            .checked_add(reserved)
            .and_then(|total| total.checked_add(additional))
            .is_none_or(|total| total > self.max_bytes)
        {
            return Err(ArtifactError::Quota(self.max_bytes));
        }
        Ok(())
    }

    fn used_bytes(&self) -> Result<u64, ArtifactError> {
        self.roots.iter().try_fold(0u64, |total, root| {
            directory_bytes(root).and_then(|bytes| {
                total
                    .checked_add(bytes)
                    .ok_or(ArtifactError::Quota(self.max_bytes))
            })
        })
    }

    fn unmaterialized_reservations(&self) -> Result<u64, ArtifactError> {
        let Some(database) = &self.database else {
            return Ok(0);
        };
        let connection = rusqlite::Connection::open(database)?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='evolution_artifact_reservations')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(0);
        }
        let reserved: i64 = connection.query_row(
            "SELECT COALESCE(SUM(max_bytes),0) FROM evolution_artifact_reservations
             WHERE staged_digest IS NULL",
            [],
            |row| row.get(0),
        )?;
        u64::try_from(reserved).map_err(|_| ArtifactError::Quota(self.max_bytes))
    }
}

impl ArtifactStaging {
    fn open(root: &Path, quota: Arc<ArtifactQuota>) -> Result<Self, ArtifactError> {
        fs::create_dir_all(root)?;
        if fs::symlink_metadata(root)?.file_type().is_symlink() {
            return Err(ArtifactError::UnsafePath(root.to_owned()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            root: root.canonicalize()?,
            quota,
        })
    }

    pub(crate) fn write_locked(
        &self,
        reservation: Uuid,
        bytes: &[u8],
    ) -> Result<Digest, ArtifactError> {
        let digest = Digest::of(bytes);
        let path = self.path(reservation);
        let mut file = NamedTempFile::new_in(&self.root)?;
        file.write_all(bytes)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(&path).map_err(|error| error.error)?;
        fs::File::open(&self.root)?.sync_all()?;
        Ok(digest)
    }

    pub(crate) fn promote_locked(
        &self,
        reservation: Uuid,
        digest: Digest,
        destination: &ArtifactStore,
    ) -> Result<(), ArtifactError> {
        let source = self.path(reservation);
        let bytes = verified_file(&source, digest, destination.max_bytes)?;
        let target = destination.path(digest);
        if target.try_exists()? {
            destination.read(digest)?;
            fs::remove_file(source)?;
        } else {
            fs::rename(source, &target)?;
            if destination.read(digest)? != bytes {
                return Err(ArtifactError::Integrity(digest));
            }
        }
        fs::File::open(&self.root)?.sync_all()?;
        fs::File::open(&destination.root)?.sync_all()?;
        Ok(())
    }

    pub(crate) fn remove_locked(&self, reservation: Uuid) -> Result<(), ArtifactError> {
        let path = self.path(reservation);
        match fs::remove_file(path) {
            Ok(()) => fs::File::open(&self.root)?.sync_all().map_err(Into::into),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn entries_locked(&self) -> Result<Vec<PathBuf>, ArtifactError> {
        let mut entries = fs::read_dir(&self.root)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort();
        Ok(entries)
    }

    pub(crate) fn quota(&self) -> Arc<ArtifactQuota> {
        self.quota.clone()
    }

    pub(crate) fn has_locked(&self, reservation: Uuid) -> Result<bool, ArtifactError> {
        self.path(reservation).try_exists().map_err(Into::into)
    }

    pub(crate) fn collect_all_locked(&self) -> Result<(), ArtifactError> {
        for path in self.entries_locked()? {
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file() {
                return Err(ArtifactError::UnsafePath(path));
            }
            fs::remove_file(path)?;
        }
        fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }

    fn path(&self, reservation: Uuid) -> PathBuf {
        self.root.join(reservation.to_string())
    }
}

fn directory_bytes(root: &Path) -> Result<u64, ArtifactError> {
    let mut total = 0u64;
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() {
            return Err(ArtifactError::UnsafePath(path));
        }
        total = total
            .checked_add(metadata.len())
            .ok_or(ArtifactError::Quota(u64::MAX))?;
    }
    Ok(total)
}

fn verified_file(path: &Path, digest: Digest, max_bytes: u64) -> Result<Vec<u8>, ArtifactError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(ArtifactError::UnsafePath(path.to_owned()));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(ArtifactError::Limit(max_bytes));
    }
    if Digest::of(&bytes) != digest {
        return Err(ArtifactError::Integrity(digest));
    }
    Ok(bytes)
}

#[cfg(test)]
mod download_tests {
    use super::*;

    #[test]
    fn chunks_share_verified_bytes_and_evicted_content_is_reverified() {
        let directory = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(directory.path(), 1024).unwrap();
        let first = store.put(b"immutable original").unwrap();
        let second = store.put(b"another artifact").unwrap();
        let mut cache = DownloadCache::default();
        assert_eq!(cache.read(&store, first).unwrap(), b"immutable original");
        fs::write(store.path(first), b"tampered after first chunk").unwrap();
        assert_eq!(cache.read(&store, first).unwrap(), b"immutable original");
        assert_eq!(cache.read(&store, second).unwrap(), b"another artifact");
        assert!(matches!(
            cache.read(&store, first),
            Err(ArtifactError::Integrity(_))
        ));
        assert!(cache.current.is_none());
    }
}
