//! Local source observation. Only cited files are read; Git HEAD is not a freshness signal.
use crate::{EvidenceState, LineRange, MemoryKind, MemoryMetadata, SourceEvidence};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    process::Command,
};

const MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct WorkspaceSources {
    root: PathBuf,
    repository: String,
}

impl WorkspaceSources {
    pub fn open(workspace: &Path) -> io::Result<Self> {
        let root =
            PathBuf::from(git(workspace, &["rev-parse", "--show-toplevel"])?).canonicalize()?;
        // Root commits survive clones and unrelated branch/revision changes. Unrelated histories
        // with the same root are one repository for memory purposes, not an authorization domain.
        let roots = git(&root, &["rev-list", "--max-parents=0", "HEAD"])?;
        let mut roots = roots.lines().collect::<Vec<_>>();
        roots.sort_unstable();
        let repository = digest(roots.join("\n").as_bytes());
        Ok(Self { root, repository })
    }
    pub fn repository(&self) -> &str {
        &self.repository
    }
    pub fn capture(&self, path: &str, range: Option<LineRange>) -> io::Result<SourceEvidence> {
        let checked_revision = git(&self.root, &["rev-parse", "HEAD"])?;
        let bytes = self.read(path)?;
        if range.as_ref().is_some_and(|r| {
            r.start == 0 || r.end < r.start || r.end as usize > bytes.split(|b| *b == b'\n').count()
        }) {
            return Err(io::Error::other("invalid source line range"));
        }
        Ok(SourceEvidence::File {
            repository: self.repository.clone(),
            path: path.into(),
            range,
            checked_revision,
            content_digest: digest(&bytes),
        })
    }
    pub fn assess(&self, metadata: &MemoryMetadata) -> EvidenceState {
        let mut citations = metadata.evidence.iter().collect::<Vec<_>>();
        if let MemoryKind::LessonProposal { behavior_test, .. } = &metadata.kind {
            citations.push(behavior_test);
        }
        if citations.is_empty() {
            return EvidenceState::Unverified;
        }
        let mut stale = false;
        for citation in citations {
            match citation {
                SourceEvidence::File {
                    repository,
                    path,
                    content_digest,
                    ..
                } if repository == &self.repository => match self.read(path) {
                    Ok(bytes) => stale |= digest(&bytes) != *content_digest,
                    Err(_) => {
                        return EvidenceState::Unavailable {
                            reason: format!("source unavailable: {path}"),
                        };
                    }
                },
                SourceEvidence::File { .. } => {
                    return EvidenceState::Unavailable {
                        reason: "repository is not mounted".into(),
                    };
                }
                SourceEvidence::Artifact { .. } => {
                    return EvidenceState::Unavailable {
                        reason: "artifact source is not mounted".into(),
                    };
                }
            }
        }
        if stale {
            EvidenceState::Stale
        } else {
            EvidenceState::Current
        }
    }
    fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        let path = Path::new(path);
        if path.as_os_str().is_empty()
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(io::Error::other("source path must be repository relative"));
        }
        let file = open_source(&self.root, path)?;
        if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_SOURCE_BYTES {
            return Err(io::Error::other("source is not a bounded regular file"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_SOURCE_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_SOURCE_BYTES {
            return Err(io::Error::other("source exceeds byte bound"));
        }
        Ok(bytes)
    }
}
fn git(root: &Path, arguments: &[&str]) -> io::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("repository identity unavailable"));
    }
    Ok(String::from_utf8(output.stdout)
        .map_err(io::Error::other)?
        .trim()
        .to_owned())
}
pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(unix)]
fn open_source(root: &Path, path: &Path) -> io::Result<fs::File> {
    use rustix::fs::{Mode, OFlags, open, openat};
    let mut directory = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let components = path.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        if index + 1 < components.len() {
            flags |= OFlags::DIRECTORY;
        }
        directory = openat(&directory, component.as_os_str(), flags, Mode::empty())?;
    }
    Ok(directory.into())
}
#[cfg(not(unix))]
fn open_source(_root: &Path, _path: &Path) -> io::Result<fs::File> {
    Err(io::Error::other(
        "race-safe source observation is unavailable on this platform",
    ))
}
