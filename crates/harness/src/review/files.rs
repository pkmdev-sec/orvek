use super::{FileKind, FrozenFile, ReviewError, ReviewLimits};
use crate::artifacts::ArtifactStore;
use rustix::fs::{Mode, OFlags, openat};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path},
};

pub(super) fn safe(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains('\0')
        && !path.starts_with('/')
        && path.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.eq_ignore_ascii_case(".git")
        })
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}
pub(super) fn read_regular(path: &Path, limit: usize) -> Result<Vec<u8>, ReviewError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    read_file(file, limit)
}
fn read_file(file: File, limit: usize) -> Result<Vec<u8>, ReviewError> {
    let before = file.metadata()?;
    if !before.is_file() || before.len() > limit as u64 {
        return Err(ReviewError::Limit("file bytes or type"));
    }
    let mut bytes = Vec::new();
    (&file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() > limit {
        return Err(ReviewError::Limit("file bytes"));
    }
    if stamp(&before) != stamp(&after) || bytes.len() != after.len() as usize {
        return Err(ReviewError::Changed);
    }
    Ok(bytes)
}
fn stamp(meta: &fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64, u32) {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
        meta.mode(),
    )
}

pub(super) struct Root {
    directory: File,
    identity: (u64, u64),
}
impl Root {
    pub fn open(path: &Path) -> Result<Self, ReviewError> {
        let directory = File::from(
            openat(
                rustix::fs::CWD,
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let meta = directory.metadata()?;
        Ok(Self {
            directory,
            identity: (meta.dev(), meta.ino()),
        })
    }
    pub fn still_at(&self, path: &Path) -> bool {
        fs::symlink_metadata(path)
            .is_ok_and(|meta| meta.is_dir() && (meta.dev(), meta.ino()) == self.identity)
    }
    pub fn capture(
        &self,
        path: &str,
        store: &ArtifactStore,
        limits: &ReviewLimits,
    ) -> Result<Option<FrozenFile>, ReviewError> {
        if !safe(path) {
            return Err(ReviewError::Path);
        }
        let parts = path.split('/').collect::<Vec<_>>();
        let mut parent = self.directory.try_clone()?;
        for part in &parts[..parts.len() - 1] {
            match openat(
                &parent,
                *part,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => parent = File::from(fd),
                Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR) => return Ok(None),
                Err(error) => return Err(std::io::Error::from(error).into()),
            }
        }
        let name = parts[parts.len() - 1];
        let stat = match rustix::fs::statat(&parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => return Err(std::io::Error::from(error).into()),
        };
        let kind = rustix::fs::FileType::from_raw_mode(stat.st_mode);
        #[allow(clippy::useless_conversion)]
        let permissions = u32::from(stat.st_mode) & 0o7777;
        let (bytes, kind, mode, permissions) = if kind == rustix::fs::FileType::Symlink {
            let target =
                rustix::fs::readlinkat(&parent, name, Vec::new()).map_err(std::io::Error::from)?;
            if target.as_bytes().len() > 4096 {
                return Err(ReviewError::Limit("link target"));
            }
            (
                target.as_bytes().to_vec(),
                FileKind::Symlink,
                0o120000,
                permissions,
            )
        } else if kind == rustix::fs::FileType::Directory {
            return Ok(None);
        } else if kind == rustix::fs::FileType::RegularFile {
            let file = File::from(
                openat(
                    &parent,
                    name,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(std::io::Error::from)?,
            );
            let meta = file.metadata()?;
            if meta.ino() != stat.st_ino || meta.dev() != stat.st_dev as u64 {
                return Err(ReviewError::Changed);
            }
            if meta.dev() != self.identity.0 {
                return Err(ReviewError::Unsupported("cross-filesystem source file"));
            }
            (
                read_file(file, limits.max_file_bytes)?,
                FileKind::File,
                if meta.mode() & 0o111 != 0 {
                    0o100755
                } else {
                    0o100644
                },
                meta.mode() & 0o7777,
            )
        } else {
            return Err(ReviewError::Unsupported("special working-tree file"));
        };
        let final_stat = rustix::fs::statat(&parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(std::io::Error::from)?;
        if stat.st_ino != final_stat.st_ino
            || stat.st_dev != final_stat.st_dev
            || stat.st_mode != final_stat.st_mode
            || stat.st_size != final_stat.st_size
            || stat.st_mtime != final_stat.st_mtime
            || stat.st_mtime_nsec != final_stat.st_mtime_nsec
            || stat.st_ctime != final_stat.st_ctime
            || stat.st_ctime_nsec != final_stat.st_ctime_nsec
        {
            return Err(ReviewError::Changed);
        }
        Ok(Some(FrozenFile {
            kind,
            mode,
            permissions: Some(permissions),
            content: store.put(&bytes)?,
            bytes: bytes.len(),
            git_object: None,
        }))
    }
}
