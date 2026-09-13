use crate::Digest;
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("artifact I/O: {0}")]
    Io(#[from] io::Error),
    #[error("artifact exceeds byte limit {0}")]
    Limit(u64),
    #[error("artifact digest mismatch: {0}")]
    Integrity(Digest),
    #[error("artifact path is not a regular file: {0}")]
    UnsafePath(PathBuf),
}

#[derive(Clone)]
pub struct ArtifactStore {
    root: PathBuf,
    max_bytes: u64,
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
    pub fn open(root: &Path, max_bytes: u64) -> Result<Self, ArtifactError> {
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
        })
    }

    pub fn put(&self, bytes: &[u8]) -> Result<Digest, ArtifactError> {
        if bytes.len() as u64 > self.max_bytes {
            return Err(ArtifactError::Limit(self.max_bytes));
        }
        let digest = Digest::of(bytes);
        let path = self.path(digest);
        if path.try_exists()? {
            self.read(digest)?;
            return Ok(digest);
        }
        let mut file = NamedTempFile::new_in(&self.root)?;
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
        Ok(digest)
    }

    pub fn read(&self, digest: Digest) -> Result<Vec<u8>, ArtifactError> {
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

    pub fn path(&self, digest: Digest) -> PathBuf {
        self.root.join(digest.to_string())
    }
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
