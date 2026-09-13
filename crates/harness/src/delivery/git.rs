use super::{
    Control, DeliveryError, GitCommandReceipt, GitIdentity, PatchLimits, utility_identity,
};
use crate::{
    artifacts::ArtifactStore,
    workspace::{Entry, Snapshot},
};
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs, io,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

pub(super) struct GitWorkspace<'a> {
    scratch: Scratch,
    program: PathBuf,
    identity: GitIdentity,
    limits: &'a PatchLimits,
    artifacts: &'a ArtifactStore,
    control: &'a Control<'a>,
    commands: Vec<GitCommandReceipt>,
}

impl<'a> GitWorkspace<'a> {
    pub async fn new(
        scratch: &Path,
        limits: &'a PatchLimits,
        artifacts: &'a ArtifactStore,
        control: &'a Control<'a>,
    ) -> Result<Self, DeliveryError> {
        control.check()?;
        if !fs::symlink_metadata(scratch)
            .map_err(|_| DeliveryError::InvalidScratch)?
            .file_type()
            .is_dir()
        {
            return Err(DeliveryError::InvalidScratch);
        }
        let scratch = Scratch(
            tempfile::Builder::new()
                .prefix("tact-patch-")
                .tempdir_in(scratch)?,
        );
        for name in ["home", "templates", "hooks"] {
            fs::create_dir(scratch.0.path().join(name))?;
        }
        fs::write(scratch.0.path().join("empty-config"), b"")?;
        fs::write(scratch.0.path().join("empty-attributes"), b"")?;
        let (program, executable_digest) = utility_identity()?;
        let mut git = Self {
            identity: GitIdentity {
                executable: program.to_string_lossy().into_owned(),
                executable_digest,
                version: String::new(),
            },
            scratch,
            program,
            limits,
            artifacts,
            control,
            commands: Vec::new(),
        };
        let version = git
            .run(
                "version",
                &["--version".into()],
                None,
                None,
                Vec::new(),
                1024,
                false,
            )
            .await?;
        let version = std::str::from_utf8(&version)
            .map_err(|_| DeliveryError::GitProtocol)?
            .trim();
        if !version.starts_with("git version ") {
            return Err(DeliveryError::GitProtocol);
        }
        git.identity.version = version.into();
        git.run(
            "initialize",
            &[
                "init".into(),
                "--bare".into(),
                "--object-format=sha1".into(),
                format!("--template={}", git.root().join("templates").display()).into(),
                git.root().join("metadata").into_os_string(),
            ],
            None,
            None,
            Vec::new(),
            limits.max_diagnostic_bytes,
            false,
        )
        .await?;
        fs::create_dir_all(git.root().join("metadata/info"))?;
        fs::write(
            git.root().join("metadata/info/attributes"),
            b"* -text -ident -filter -working-tree-encoding !diff\n",
        )?;
        git.run(
            "empty-index",
            &["read-tree".into(), "--empty".into()],
            None,
            Some("empty-index"),
            Vec::new(),
            limits.max_diagnostic_bytes,
            true,
        )
        .await?;
        Ok(git)
    }
    pub fn root(&self) -> &Path {
        self.scratch.0.path()
    }
    pub fn finish(self) -> (GitIdentity, Vec<GitCommandReceipt>) {
        (self.identity, self.commands)
    }

    pub async fn tree(
        &mut self,
        snapshot: &Snapshot,
        materialized: &Path,
        index: &str,
    ) -> Result<String, DeliveryError> {
        self.control.check()?;
        let files: Vec<_> = snapshot
            .entries
            .iter()
            .filter(|(_, entry)| matches!(entry, Entry::File { .. }))
            .collect();
        let mut object_ids = BTreeMap::new();
        if !files.is_empty() {
            let mut paths = Vec::new();
            for (path, _) in &files {
                paths.extend(quote_path(materialized.join(path).as_os_str().as_bytes()));
                paths.push(b'\n');
            }
            let bytes = self
                .run(
                    "hash-files",
                    &[
                        "hash-object".into(),
                        "-w".into(),
                        "--stdin-paths".into(),
                        "--no-filters".into(),
                    ],
                    None,
                    Some("empty-index"),
                    paths,
                    files.len() * 41 + 1,
                    true,
                )
                .await?;
            let ids = object_lines(&bytes)?;
            if ids.len() != files.len() {
                return Err(DeliveryError::GitProtocol);
            }
            for ((path, _), id) in files.into_iter().zip(ids) {
                object_ids.insert(path.clone(), id);
            }
        }
        for (path, entry) in &snapshot.entries {
            if let Entry::Symlink { target } = entry {
                let bytes = self
                    .run(
                        "hash-symlink",
                        &[
                            "hash-object".into(),
                            "-w".into(),
                            "--stdin".into(),
                            "--no-filters".into(),
                        ],
                        None,
                        Some("empty-index"),
                        target.as_bytes().to_vec(),
                        128,
                        true,
                    )
                    .await?;
                let mut ids = object_lines(&bytes)?;
                if ids.len() != 1 {
                    return Err(DeliveryError::GitProtocol);
                }
                object_ids.insert(path.clone(), ids.remove(0));
            }
        }
        let mut input = Vec::new();
        for (path, entry) in &snapshot.entries {
            let mode = match entry {
                Entry::File { mode, .. } => {
                    if mode & 0o111 == 0 {
                        0o100644
                    } else {
                        0o100755
                    }
                }
                Entry::Symlink { .. } => 0o120000,
                Entry::Directory { .. } => continue,
            };
            let id = object_ids.get(path).ok_or(DeliveryError::GitProtocol)?;
            input.extend_from_slice(format!("{mode:o} {id}\t").as_bytes());
            input.extend_from_slice(path.as_bytes());
            input.push(0);
        }
        self.run(
            "index-tree",
            &["update-index".into(), "-z".into(), "--index-info".into()],
            None,
            Some(index),
            input,
            self.limits.max_diagnostic_bytes,
            true,
        )
        .await?;
        let bytes = self
            .run(
                "write-tree",
                &["write-tree".into()],
                None,
                Some(index),
                Vec::new(),
                128,
                true,
            )
            .await?;
        let mut ids = object_lines(&bytes)?;
        if ids.len() != 1 {
            return Err(DeliveryError::GitProtocol);
        }
        Ok(ids.remove(0))
    }
    pub async fn diff(&mut self, base: &str, candidate: &str) -> Result<Vec<u8>, DeliveryError> {
        self.run(
            "generate-patch",
            &[
                "diff-tree".into(),
                "--no-commit-id".into(),
                "-r".into(),
                "-p".into(),
                "--binary".into(),
                "--full-index".into(),
                "--no-renames".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                "--no-color".into(),
                "--src-prefix=a/".into(),
                "--dst-prefix=b/".into(),
                base.into(),
                candidate.into(),
                "--".into(),
            ],
            None,
            Some("empty-index"),
            Vec::new(),
            self.limits.max_patch_bytes,
            true,
        )
        .await
    }
    pub async fn apply(&mut self, worktree: &Path, patch: &[u8]) -> Result<(), DeliveryError> {
        let mut arguments = vec![
            "apply".into(),
            "--binary".into(),
            "--whitespace=nowarn".into(),
        ];
        if patch.is_empty() {
            arguments.push("--allow-empty".into());
        }
        arguments.push("-".into());
        self.run(
            "apply-patch",
            &arguments,
            Some(worktree),
            Some("empty-index"),
            patch.to_vec(),
            self.limits.max_diagnostic_bytes,
            true,
        )
        .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn run(
        &mut self,
        step: &'static str,
        args: &[OsString],
        worktree: Option<&Path>,
        index: Option<&str>,
        input: Vec<u8>,
        stdout_limit: usize,
        repository: bool,
    ) -> Result<Vec<u8>, DeliveryError> {
        let remaining = self.control.remaining()?;
        let started = Instant::now();
        let mut command = Command::new(&self.program);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.root().join("home"))
            .env("XDG_CONFIG_HOME", self.root().join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", self.root().join("empty-config"))
            .env("GIT_CONFIG_GLOBAL", self.root().join("empty-config"))
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_LITERAL_PATHSPECS", "1")
            .env("GIT_CEILING_DIRECTORIES", self.root())
            .env("LC_ALL", "C")
            .env("LANG", "C");
        if let Some(index) = index {
            command.env("GIT_INDEX_FILE", self.root().join(index));
        }
        let mut actual: Vec<OsString> = vec!["--no-pager".into()];
        for setting in [
            format!("core.hooksPath={}", self.root().join("hooks").display()),
            format!(
                "core.attributesFile={}",
                self.root().join("empty-attributes").display()
            ),
            "core.fsmonitor=false".into(),
            "core.untrackedCache=false".into(),
            "core.autocrlf=false".into(),
            "core.safecrlf=false".into(),
            "core.filemode=true".into(),
            "core.symlinks=true".into(),
            "core.quotePath=true".into(),
            "core.protectHFS=true".into(),
            "core.protectNTFS=true".into(),
            "credential.helper=".into(),
            "protocol.allow=never".into(),
            "submodule.recurse=false".into(),
            "pack.threads=1".into(),
            "gc.auto=0".into(),
            "gc.autoDetach=false".into(),
            "maintenance.auto=false".into(),
        ] {
            actual.push("-c".into());
            actual.push(setting.into());
        }
        if repository {
            actual.push("--git-dir".into());
            actual.push(self.root().join("metadata").into_os_string());
        }
        if let Some(worktree) = worktree {
            actual.push("--work-tree".into());
            actual.push(worktree.as_os_str().to_owned());
        }
        actual.extend_from_slice(args);
        command
            .args(&actual)
            .current_dir(worktree.unwrap_or(self.root()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command.as_std_mut().process_group(0);
        // Only the async-signal-safe umask syscall runs between fork and exec;
        // the parent process's umask and environment are never modified.
        unsafe {
            command.as_std_mut().pre_exec(|| {
                rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o022));
                Ok(())
            });
        }
        let stdin_digest = self.artifacts.put(&input)?;
        let mut child = command.spawn().map_err(|_| DeliveryError::GitUnavailable)?;
        let pid = Pid::from_raw(child.id().ok_or(DeliveryError::OutcomeUnknown)? as i32)
            .ok_or(DeliveryError::OutcomeUnknown)?;
        let mut group = ProcessGroup { pid, armed: true };
        let overflow = CancellationToken::new();
        let out = tokio::spawn(capture(
            child.stdout.take().ok_or(DeliveryError::OutcomeUnknown)?,
            stdout_limit,
            overflow.clone(),
        ));
        let err = tokio::spawn(capture(
            child.stderr.take().ok_or(DeliveryError::OutcomeUnknown)?,
            self.limits.max_diagnostic_bytes,
            overflow.clone(),
        ));
        let mut stdin = child.stdin.take().ok_or(DeliveryError::OutcomeUnknown)?;
        let writer = tokio::spawn(async move {
            stdin.write_all(&input).await?;
            stdin.shutdown().await
        });
        let status = tokio::select! {
            biased;
            ()=self.control.cancel.cancelled()=>Err(DeliveryError::Cancelled),
            ()=overflow.cancelled()=>Err(DeliveryError::Limit("Git output")),
            result=timeout(remaining,child.wait())=>match result{Ok(Ok(status))=>Ok(status),Ok(Err(_))=>Err(DeliveryError::OutcomeUnknown),Err(_)=>Err(DeliveryError::TimedOut)},
        };
        let status = match status {
            Ok(status) => status,
            Err(error) => {
                group.kill();
                let _ = child.start_kill();
                let settled = timeout(Duration::from_secs(1), child.wait()).await;
                out.abort();
                err.abort();
                writer.abort();
                if !matches!(settled, Ok(Ok(_))) || !group.wait_quiescent().await {
                    return Err(DeliveryError::OutcomeUnknown);
                }
                group.armed = false;
                return Err(error);
            }
        };
        if !group.wait_quiescent().await {
            group.kill();
            out.abort();
            err.abort();
            writer.abort();
            return Err(DeliveryError::OutcomeUnknown);
        }
        group.armed = false;
        let stdout = drain(out).await?;
        let stderr = drain(err).await?;
        let wrote = timeout(Duration::from_secs(1), writer)
            .await
            .map_err(|_| DeliveryError::OutcomeUnknown)?
            .map_err(|_| DeliveryError::OutcomeUnknown)?;
        if overflow.is_cancelled() {
            return Err(DeliveryError::Limit("Git output"));
        }
        let stdout_digest = self.artifacts.put(&stdout)?;
        let stderr_digest = self.artifacts.put(&stderr)?;
        if !status.success() {
            return Err(DeliveryError::GitFailed {
                step,
                exit_code: status.code(),
                stderr: stderr_digest,
            });
        }
        wrote.map_err(|_| DeliveryError::GitProtocol)?;
        self.control.check()?;
        let root = self.root().to_string_lossy();
        self.commands.push(GitCommandReceipt {
            step: step.into(),
            arguments: actual
                .iter()
                .map(|a| a.to_string_lossy().replace(root.as_ref(), "<scratch>"))
                .collect(),
            stdin_digest,
            stdout_digest,
            stderr_digest,
            exit_code: 0,
            elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            process_group_quiescent: true,
        });
        Ok(stdout)
    }
}

fn quote_path(bytes: &[u8]) -> Vec<u8> {
    let mut quoted = Vec::with_capacity(bytes.len() + 2);
    quoted.push(b'"');
    for &byte in bytes {
        if matches!(byte, b'"' | b'\\') {
            quoted.push(b'\\');
            quoted.push(byte);
        } else if byte < 32 || byte == 127 {
            quoted.extend_from_slice(format!("\\{byte:03o}").as_bytes());
        } else {
            quoted.push(byte);
        }
    }
    quoted.push(b'"');
    quoted
}
fn object_lines(bytes: &[u8]) -> Result<Vec<String>, DeliveryError> {
    let text = std::str::from_utf8(bytes).map_err(|_| DeliveryError::GitProtocol)?;
    text.lines()
        .map(|id| {
            if id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
                Ok(id.to_owned())
            } else {
                Err(DeliveryError::GitProtocol)
            }
        })
        .collect()
}
async fn capture(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
    overflow: CancellationToken,
) -> io::Result<Vec<u8>> {
    let mut remaining = limit;
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(output);
        }
        let accepted = count.min(remaining);
        remaining = remaining.saturating_sub(count);
        output.extend_from_slice(&buffer[..accepted]);
        if accepted < count {
            overflow.cancel();
            return Ok(output);
        }
    }
}
async fn drain(
    task: tokio::task::JoinHandle<io::Result<Vec<u8>>>,
) -> Result<Vec<u8>, DeliveryError> {
    timeout(Duration::from_secs(1), task)
        .await
        .map_err(|_| DeliveryError::OutcomeUnknown)?
        .map_err(|_| DeliveryError::OutcomeUnknown)?
        .map_err(Into::into)
}
struct ProcessGroup {
    pid: Pid,
    armed: bool,
}
impl ProcessGroup {
    fn kill(&self) {
        let _ = kill_process_group(self.pid, Signal::KILL);
    }
    fn quiescent(&self) -> bool {
        matches!(
            test_kill_process_group(self.pid),
            Err(rustix::io::Errno::SRCH)
        )
    }
    async fn wait_quiescent(&self) -> bool {
        // waitpid can return before the kernel retires the process group (and
        // platform Git launchers may still be reaping their own children).
        // Retain ownership until the entire group is actually absent.
        timeout(Duration::from_secs(1), async {
            loop {
                if self.quiescent() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .is_ok()
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if self.armed {
            self.kill();
        }
    }
}
struct Scratch(TempDir);
impl Drop for Scratch {
    fn drop(&mut self) {
        make_removable(self.0.path());
    }
}
fn make_removable(path: &Path) {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir()) {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                make_removable(&entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_kills_a_git_process_blocked_on_a_controlled_fifo() {
        let root = tempfile::tempdir().unwrap();
        let artifacts = ArtifactStore::open(&root.path().join("artifacts"), 1024 * 1024).unwrap();
        let limits = PatchLimits::default();
        let cancellation = CancellationToken::new();
        let control = Control::new(&limits, &cancellation);
        let mut git = GitWorkspace::new(root.path(), &limits, &artifacts, &control)
            .await
            .unwrap();
        let fifo = git.root().join("controlled-fifo");
        // rustix does not expose mkfifoat on macOS; use the POSIX syscall only
        // for this private, non-executable pipe fixture.
        unsafe extern "C" {
            fn mkfifo(path: *const std::ffi::c_char, mode: rustix::fs::RawMode) -> std::ffi::c_int;
        }
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { mkfifo(name.as_ptr(), 0o600) }, 0);
        let mut input = quote_path(fifo.as_os_str().as_bytes());
        input.push(b'\n');
        let before = git.commands.len();
        let observe_reader = async {
            timeout(Duration::from_secs(5), async {
                loop {
                    match rustix::fs::open(
                        &fifo,
                        rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::NONBLOCK,
                        rustix::fs::Mode::empty(),
                    ) {
                        Ok(writer) => {
                            cancellation.cancel();
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            drop(writer);
                            return;
                        }
                        Err(rustix::io::Errno::NXIO) => {
                            tokio::time::sleep(Duration::from_millis(1)).await
                        }
                        Err(error) => panic!("FIFO fixture failed: {error}"),
                    }
                }
            })
            .await
            .expect("Git must reach the controlled FIFO");
        };
        let arguments: [OsString; 3] = [
            "hash-object".into(),
            "--stdin-paths".into(),
            "--no-filters".into(),
        ];
        let (result, ()) = tokio::join!(
            git.run(
                "controlled-block",
                &arguments,
                None,
                Some("empty-index"),
                input,
                1024,
                true
            ),
            observe_reader
        );
        assert!(matches!(result, Err(DeliveryError::Cancelled)));
        assert_eq!(
            git.commands.len(),
            before,
            "a cancelled command cannot create a passing receipt"
        );
    }
}
