use super::{ExecutionLimits, RuntimeError};
use crate::{Digest, workspace::Entry};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    time::Duration,
};
use orvek_executor::{self as wire, Complete, Hello};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

pub(super) struct GuestReport {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub complete: Option<Complete>,
    pub entries: BTreeMap<String, Entry>,
    pub failure: Option<String>,
}
struct PendingFile {
    path: String,
    file: File,
    mode: u32,
    left: u64,
    hasher: Sha256,
}
pub(super) struct Receiver {
    pub report: GuestReport,
    root: PathBuf,
    nonce: String,
    job_id: String,
    readonly: bool,
    limits: ExecutionLimits,
    output_limit: u64,
    metadata_bytes: usize,
    file_bytes: u64,
    pending: Option<PendingFile>,
    hello: bool,
}
impl Receiver {
    pub fn new(
        root: PathBuf,
        nonce: String,
        job_id: String,
        readonly: bool,
        limits: ExecutionLimits,
        output_limit: u64,
    ) -> Self {
        Self {
            report: GuestReport {
                stdout: Vec::new(),
                stderr: Vec::new(),
                complete: None,
                entries: BTreeMap::new(),
                failure: None,
            },
            root,
            nonce,
            job_id,
            readonly,
            limits,
            output_limit,
            metadata_bytes: 0,
            file_bytes: 0,
            pending: None,
            hello: false,
        }
    }
    pub async fn receive(
        mut self,
        mut reader: impl AsyncRead + Unpin,
        abort: CancellationToken,
    ) -> GuestReport {
        let result = self.read(&mut reader).await;
        if let Err(error) = result {
            self.report.failure = Some(error.to_string());
            abort.cancel();
        }
        self.report
    }
    async fn read(&mut self, reader: &mut (impl AsyncRead + Unpin)) -> Result<(), RuntimeError> {
        let mut magic = [0; 8];
        tokio::time::timeout(Duration::from_secs(10), reader.read_exact(&mut magic))
            .await
            .map_err(|_| failure("executor handshake timeout"))??;
        if &magic != wire::MAGIC {
            return Err(failure("invalid executor magic"));
        }
        loop {
            let mut kind = [0];
            let count =
                if !self.hello || !self.report.entries.is_empty() || self.report.complete.is_some()
                {
                    tokio::time::timeout(Duration::from_secs(5), reader.read(&mut kind))
                        .await
                        .map_err(|_| failure("executor export idle timeout"))??
                } else {
                    reader.read(&mut kind).await?
                };
            if count == 0 {
                if self.report.complete.is_none() {
                    return Err(failure("incomplete executor transfer"));
                }
                return Ok(());
            }
            let mut length = [0; 4];
            tokio::time::timeout(Duration::from_secs(5), reader.read_exact(&mut length))
                .await
                .map_err(|_| failure("executor frame idle timeout"))??;
            let length = u32::from_le_bytes(length) as usize;
            if length > wire::MAX_FRAME_BYTES {
                return Err(failure("executor frame limit"));
            }
            let mut data = vec![0; length];
            tokio::time::timeout(Duration::from_secs(5), reader.read_exact(&mut data))
                .await
                .map_err(|_| failure("executor frame idle timeout"))??;
            self.frame(kind[0], &data)?;
        }
    }
    fn frame(&mut self, kind: u8, data: &[u8]) -> Result<(), RuntimeError> {
        if self.report.complete.is_some() {
            return Err(failure("data after executor completion"));
        }
        if !self.hello && kind != wire::HELLO {
            return Err(failure("missing executor handshake"));
        }
        match kind {
            wire::HELLO => {
                if self.hello {
                    return Err(failure("duplicate executor handshake"));
                }
                let hello: Hello = serde_json::from_slice(data)
                    .map_err(|_| failure("invalid executor handshake"))?;
                if hello.version != wire::VERSION
                    || hello.job_id != self.job_id
                    || hello.nonce != self.nonce
                    || hello.root_uid != 0
                    || hello.child_uid == 0
                    || hello.readonly != self.readonly
                {
                    return Err(failure("executor identity mismatch"));
                }
                if !self.readonly
                    && !hello.quota.is_some_and(|q| {
                        q.total_bytes > 0
                            && q.total_bytes <= self.limits.workspace_bytes
                            && q.total_inodes > 0
                            && q.total_inodes <= self.limits.workspace_inodes
                    })
                {
                    return Err(failure("executor quota mismatch"));
                }
                self.hello = true;
            }
            wire::STDOUT | wire::STDERR => {
                if !self.report.entries.is_empty() || self.pending.is_some() {
                    return Err(failure("late command output"));
                }
                if self
                    .report
                    .stdout
                    .len()
                    .saturating_add(self.report.stderr.len())
                    .saturating_add(data.len()) as u64
                    > self.output_limit
                {
                    return Err(failure("executor output limit"));
                }
                if kind == wire::STDOUT {
                    self.report.stdout.extend_from_slice(data);
                } else {
                    self.report.stderr.extend_from_slice(data);
                }
            }
            wire::ENTRY => {
                if self.readonly || self.pending.is_some() {
                    return Err(failure("unexpected export entry"));
                }
                self.metadata_bytes = self.metadata_bytes.saturating_add(data.len());
                if self.metadata_bytes > 8 * 1024 * 1024
                    || self.report.entries.len() as u64
                        >= self.limits.workspace_inodes.saturating_sub(1)
                {
                    return Err(failure("export metadata limit"));
                }
                let entry: wire::Entry =
                    serde_json::from_slice(data).map_err(|_| failure("invalid export entry"))?;
                let path = entry.path();
                if !safe(path) || self.report.entries.contains_key(path) {
                    return Err(failure("invalid or duplicate export path"));
                }
                for parent in Path::new(path)
                    .ancestors()
                    .skip(1)
                    .filter(|p| !p.as_os_str().is_empty())
                {
                    if !matches!(
                        parent.to_str().and_then(|p| self.report.entries.get(p)),
                        Some(Entry::Directory { .. })
                    ) {
                        return Err(failure("export parent is not a directory"));
                    }
                }
                match entry {
                    wire::Entry::Directory { path, mode } => {
                        check_mode(mode, true)?;
                        fs::create_dir(self.root.join(&path))?;
                        self.report.entries.insert(path, Entry::Directory { mode });
                    }
                    wire::Entry::Symlink { path, target } => {
                        if target.len() > 4096 || target.contains('\0') {
                            return Err(failure("invalid link target"));
                        }
                        self.report.entries.insert(path, Entry::Symlink { target });
                    }
                    wire::Entry::File { path, mode, bytes } => {
                        check_mode(mode, false)?;
                        self.file_bytes = self
                            .file_bytes
                            .checked_add(bytes)
                            .ok_or_else(|| failure("export size overflow"))?;
                        if self.file_bytes > self.limits.workspace_bytes {
                            return Err(failure("export byte limit"));
                        }
                        let file = OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .open(self.root.join(&path))?;
                        self.pending = Some(PendingFile {
                            path,
                            file,
                            mode,
                            left: bytes,
                            hasher: Sha256::new(),
                        });
                        if bytes == 0 {
                            self.finish_file()?;
                        }
                    }
                }
            }
            wire::FILE_DATA => {
                let file = self
                    .pending
                    .as_mut()
                    .ok_or_else(|| failure("unrequested file data"))?;
                if data.is_empty()
                    || data.len() > wire::CHUNK_BYTES
                    || data.len() as u64 > file.left
                {
                    return Err(failure("file data length mismatch"));
                }
                file.file.write_all(data)?;
                file.hasher.update(data);
                file.left -= data.len() as u64;
                if file.left == 0 {
                    self.finish_file()?;
                }
            }
            wire::COMPLETE => {
                if self.pending.is_some() {
                    return Err(failure("truncated file export"));
                }
                let complete: Complete = serde_json::from_slice(data)
                    .map_err(|_| failure("invalid completion frame"))?;
                if complete.version != wire::VERSION
                    || complete.nonce != self.nonce
                    || !complete.quiescent
                    || complete.entries != self.report.entries.len() as u64
                    || complete.bytes != self.file_bytes
                    || (complete.exported
                        && (self.readonly
                            || !matches!(complete.outcome, wire::Outcome::Exited { .. })))
                    || (!complete.exported && !self.report.entries.is_empty())
                    || (!self.readonly
                        && matches!(complete.outcome, wire::Outcome::Exited { .. })
                        && !complete.exported)
                {
                    return Err(failure("inconsistent executor completion"));
                }
                self.report.complete = Some(complete);
            }
            _ => return Err(failure("unknown executor frame")),
        }
        Ok(())
    }
    fn finish_file(&mut self) -> Result<(), RuntimeError> {
        let file = self
            .pending
            .take()
            .ok_or_else(|| failure("missing export file"))?;
        file.file.sync_all()?;
        fs::set_permissions(
            self.root.join(&file.path),
            fs::Permissions::from_mode(file.mode),
        )?;
        let digest: Digest = format!("{:x}", file.hasher.finalize())
            .parse()
            .map_err(|_| failure("invalid content identity"))?;
        self.report.entries.insert(
            file.path,
            Entry::File {
                content: digest,
                mode: file.mode,
            },
        );
        Ok(())
    }
}
fn safe(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains('\0')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && Path::new(path)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
}
fn check_mode(mode: u32, directory: bool) -> Result<(), RuntimeError> {
    if mode & !0o777 != 0 || mode & 0o400 == 0 || (directory && mode & 0o100 == 0) {
        Err(failure("unsupported export permissions"))
    } else {
        Ok(())
    }
}
fn failure(text: &str) -> RuntimeError {
    RuntimeError::Setup(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn receiver(root: &Path) -> Receiver {
        let limits = ExecutionLimits {
            workspace_bytes: 1024 * 1024,
            workspace_inodes: 16,
            ..ExecutionLimits::default()
        };
        let mut receiver = Receiver::new(
            root.to_owned(),
            "nonce".into(),
            "job".into(),
            false,
            limits,
            128,
        );
        receiver
            .frame(
                wire::HELLO,
                &serde_json::to_vec(&Hello {
                    version: wire::VERSION,
                    job_id: "job".into(),
                    nonce: "nonce".into(),
                    root_uid: 0,
                    child_uid: 1000,
                    readonly: false,
                    quota: Some(wire::Quota {
                        total_bytes: 1024 * 1024,
                        free_bytes: 1024 * 1024,
                        total_inodes: 16,
                        free_inodes: 15,
                    }),
                })
                .unwrap(),
            )
            .unwrap();
        receiver
    }
    #[test]
    fn traversal_duplicate_authority_and_oversized_files_are_rejected_before_writes() {
        for path in [
            "../escape",
            "/escape",
            "a/../escape",
            "./escape",
            "a//escape",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut receiver = receiver(directory.path());
            assert!(
                receiver
                    .frame(
                        wire::ENTRY,
                        &serde_json::to_vec(&wire::Entry::File {
                            path: path.into(),
                            mode: 0o644,
                            bytes: 1
                        })
                        .unwrap()
                    )
                    .is_err()
            );
            assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
        }
        let directory = tempfile::tempdir().unwrap();
        let mut receiver = receiver(directory.path());
        assert!(
            receiver
                .frame(
                    wire::ENTRY,
                    br#"{"kind":"file","path":"file","mode":420,"bytes":18446744073709551615}"#
                )
                .is_err()
        );
        assert!(receiver.frame(wire::HELLO, b"{}").is_err());
        assert!(
            receiver
                .frame(
                    wire::ENTRY,
                    br#"{"kind":"file","path":"file","mode":420,"bytes":0,"is_root":true}"#
                )
                .is_err()
        );
    }
    #[test]
    fn incomplete_or_reordered_exports_cannot_complete() {
        let directory = tempfile::tempdir().unwrap();
        let mut receiver = receiver(directory.path());
        receiver
            .frame(
                wire::ENTRY,
                &serde_json::to_vec(&wire::Entry::File {
                    path: "file".into(),
                    mode: 0o644,
                    bytes: 4,
                })
                .unwrap(),
            )
            .unwrap();
        receiver.frame(wire::FILE_DATA, b"xx").unwrap();
        let complete = Complete {
            version: wire::VERSION,
            nonce: "nonce".into(),
            outcome: wire::Outcome::Exited { code: 0 },
            exported: true,
            entries: 1,
            bytes: 4,
            quiescent: true,
        };
        assert!(
            receiver
                .frame(wire::COMPLETE, &serde_json::to_vec(&complete).unwrap())
                .is_err()
        );
        assert!(receiver.frame(wire::STDOUT, b"late").is_err());
        assert!(receiver.frame(wire::FILE_DATA, b"excess").is_err());
    }
    #[tokio::test]
    async fn tar_or_partial_protocol_bytes_are_never_interpreted_as_a_tree() {
        for bytes in [
            b"ustar malformed archive".as_slice(),
            b"TACTEX02\x04\xff\xff\xff\x7f".as_slice(),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let receiver = Receiver::new(
                directory.path().to_owned(),
                "nonce".into(),
                "job".into(),
                false,
                ExecutionLimits::default(),
                128,
            );
            let report = receiver.receive(bytes, CancellationToken::new()).await;
            assert!(report.failure.is_some());
            assert!(report.complete.is_none());
            assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
        }
    }
}
