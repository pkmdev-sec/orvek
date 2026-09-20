use super::{ReviewError, ReviewLimits};
use crate::Digest;
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
use std::{
    ffi::OsString,
    fs,
    os::unix::{ffi::OsStrExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

pub(super) struct Git {
    _scratch: tempfile::TempDir,
    pub root: PathBuf,
    pub metadata: PathBuf,
    pub object_format: String,
    pub executable: Digest,
    program: PathBuf,
    deadline: Instant,
    cancel: CancellationToken,
    alternate: Option<OsString>,
}
impl Git {
    pub async fn new(
        workspace: &Path,
        limits: &ReviewLimits,
        cancel: &CancellationToken,
    ) -> Result<Self, ReviewError> {
        let mut git = Self::prepare(Some(workspace), limits, cancel)?;
        let root = git
            .original(&["rev-parse", "--show-toplevel"], 1024 * 16)
            .await?;
        let discovered = PathBuf::from(text(&root)?.trim_end()).canonicalize()?;
        if !git.root.starts_with(&discovered) {
            return Err(ReviewError::Unsupported("redirected Git worktree"));
        }
        let directory = git
            .original(&["rev-parse", "--absolute-git-dir"], 16384)
            .await?;
        let directory = PathBuf::from(text(&directory)?.trim_end()).canonicalize()?;
        let marker = discovered.join(".git");
        let marker_meta = fs::symlink_metadata(&marker)?;
        let expected = if marker_meta.is_dir() {
            marker.canonicalize()?
        } else if marker_meta.is_file() {
            let bytes = super::files::read_regular(&marker, 16384)?;
            let target = text(&bytes)?
                .trim_end()
                .strip_prefix("gitdir: ")
                .ok_or(ReviewError::Protocol)?;
            discovered.join(target).canonicalize()?
        } else {
            return Err(ReviewError::Unsupported("Git directory symlink"));
        };
        if expected != directory {
            return Err(ReviewError::Unsupported("redirected Git worktree"));
        }
        git.root = discovered;
        let format = git
            .original(&["rev-parse", "--show-object-format"], 128)
            .await?;
        git.object_format = text(&format)?.trim().into();
        if !matches!(git.object_format.as_str(), "sha1" | "sha256") {
            return Err(ReviewError::Unsupported("Git object format"));
        }
        let common = git
            .original(&["rev-parse", "--git-common-dir"], 1024 * 16)
            .await?;
        let common = PathBuf::from(text(&common)?.trim_end());
        let common = if common.is_absolute() {
            common
        } else {
            git.root.join(common)
        }
        .canonicalize()?;
        let objects = common.join("objects");
        if !objects.is_dir() {
            return Err(ReviewError::Protocol);
        }
        git.initialize().await?;
        git.alternate = Some(OsString::from(
            String::from_utf8(quote(objects.as_os_str().as_bytes()))
                .map_err(|_| ReviewError::Protocol)?,
        ));
        match super::files::read_regular(&common.join("info/exclude"), limits.max_metadata_bytes) {
            Ok(bytes) => fs::write(git.metadata.join("info/exclude"), bytes)?,
            Err(ReviewError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(git)
    }
    fn prepare(
        workspace: Option<&Path>,
        limits: &ReviewLimits,
        cancel: &CancellationToken,
    ) -> Result<Self, ReviewError> {
        let scratch = tempfile::tempdir()?;
        for path in ["home", "template", "hooks"] {
            fs::create_dir(scratch.path().join(path))?;
        }
        fs::write(scratch.path().join("empty"), [])?;
        let program = [
            "/usr/bin/git",
            "/opt/homebrew/bin/git",
            "/usr/local/bin/git",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .ok_or(ReviewError::Unavailable)?
        .canonicalize()?;
        let executable = Digest::of(&fs::read(&program)?);
        let metadata = scratch.path().join("git");
        let root = match workspace {
            Some(path) => path.canonicalize()?,
            None => {
                let root = scratch.path().join("source");
                fs::create_dir(&root)?;
                root
            }
        };
        Ok(Self {
            _scratch: scratch,
            root,
            metadata,
            object_format: String::new(),
            executable,
            program,
            deadline: Instant::now() + limits.timeout,
            cancel: cancel.clone(),
            alternate: None,
        })
    }
    async fn initialize(&self) -> Result<(), ReviewError> {
        self.run(
            &[
                "init".into(),
                "--bare".into(),
                format!("--object-format={}", self.object_format).into(),
                format!(
                    "--template={}",
                    self._scratch.path().join("template").display()
                )
                .into(),
                self.metadata.clone().into_os_string(),
            ],
            false,
            None,
            Vec::new(),
            1024 * 16,
        )
        .await?;
        fs::create_dir_all(self.metadata.join("info"))?;
        fs::write(
            self.metadata.join("info/attributes"),
            b"* -text -ident -filter -working-tree-encoding !diff\n",
        )?;
        Ok(())
    }
    pub async fn for_snapshots(
        limits: &ReviewLimits,
        cancel: &CancellationToken,
    ) -> Result<Self, ReviewError> {
        let mut git = Self::prepare(None, limits, cancel)?;
        git.object_format = "sha1".into();
        git.initialize().await?;
        Ok(git)
    }
    pub fn check(&self) -> Result<(), ReviewError> {
        if self.cancel.is_cancelled() {
            return Err(ReviewError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(ReviewError::TimedOut);
        }
        Ok(())
    }
    pub fn scratch(&self) -> &Path {
        self._scratch.path()
    }
    pub async fn original(&self, args: &[&str], limit: usize) -> Result<Vec<u8>, ReviewError> {
        self.run(
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            false,
            None,
            Vec::new(),
            limit,
        )
        .await
    }
    pub async fn isolated(
        &self,
        args: &[String],
        index: Option<&Path>,
        input: Vec<u8>,
        limit: usize,
    ) -> Result<Vec<u8>, ReviewError> {
        self.run(
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            true,
            index,
            input,
            limit,
        )
        .await
    }
    pub async fn resolve(&self, reference: &str) -> Result<String, ReviewError> {
        let (reference, suffix) = revision_parts(reference)?;
        // Resolve names without object peeling in source metadata. All object
        // traversal happens in fresh metadata with no remote configuration.
        let bytes = self
            .original(
                &["rev-parse", "--verify", "--end-of-options", reference],
                256,
            )
            .await?;
        let oid = text(&bytes)?.trim();
        if !oid_valid(oid, &self.object_format) {
            return Err(ReviewError::Protocol);
        }
        let expression = format!("{oid}{suffix}^{{commit}}");
        let bytes = self
            .isolated(
                &[
                    "rev-parse".into(),
                    "--verify".into(),
                    "--end-of-options".into(),
                    expression,
                ],
                None,
                Vec::new(),
                256,
            )
            .await?;
        let oid = text(&bytes)?.trim();
        if !oid_valid(oid, &self.object_format) {
            return Err(ReviewError::Protocol);
        }
        Ok(oid.into())
    }
    async fn run(
        &self,
        args: &[OsString],
        isolated: bool,
        index: Option<&Path>,
        input: Vec<u8>,
        limit: usize,
    ) -> Result<Vec<u8>, ReviewError> {
        self.check()?;
        let mut command = Command::new(&self.program);
        command
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self._scratch.path().join("home"))
            .env("XDG_CONFIG_HOME", self._scratch.path().join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self._scratch.path().join("empty"))
            .env("GIT_CONFIG_SYSTEM", self._scratch.path().join("empty"))
            .env("GIT_CONFIG_COUNT", "0")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .args([
                "--no-pager",
                "--literal-pathspecs",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.untrackedCache=false",
                "-c",
                "core.commitGraph=false",
                "-c",
                "core.multiPackIndex=false",
                "-c",
                "core.bare=false",
                "-c",
                "core.quotePath=true",
                "-c",
                "log.showSignature=false",
                "-c",
                "protocol.allow=never",
                "-c",
                "credential.helper=",
                "-c",
                "gc.auto=0",
                "-c",
                "maintenance.auto=false",
                "-c",
                "submodule.recurse=false",
                "-c",
                "diff.algorithm=myers",
                "-c",
                "color.ui=false",
            ])
            .args([
                OsString::from("-c"),
                format!(
                    "core.hooksPath={}",
                    self._scratch.path().join("hooks").display()
                )
                .into(),
                "-c".into(),
                format!(
                    "core.excludesFile={}",
                    self._scratch.path().join("empty").display()
                )
                .into(),
            ]);
        if isolated {
            command
                .env("GIT_DIR", &self.metadata)
                .env("GIT_WORK_TREE", &self.root);
            if let Some(alternate) = &self.alternate {
                command.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", alternate);
            }
        }
        if let Some(index) = index {
            command.env("GIT_INDEX_FILE", index);
        }
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command.as_std_mut().process_group(0);
        let mut child = command.spawn()?;
        let mut group = Group::new(child.id().ok_or(ReviewError::Unknown)?);
        let mut stdin = child.stdin.take().ok_or(ReviewError::Protocol)?;
        let write = tokio::spawn(async move {
            stdin.write_all(&input).await?;
            stdin.shutdown().await
        });
        let mut stdout = tokio::spawn(capture(
            child.stdout.take().ok_or(ReviewError::Protocol)?,
            limit,
        ));
        let mut stderr = tokio::spawn(capture(
            child.stderr.take().ok_or(ReviewError::Protocol)?,
            8192,
        ));
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        let result = tokio::select! {()=self.cancel.cancelled()=>Err(ReviewError::Cancelled),result=timeout(remaining,child.wait())=>match result {Ok(Ok(status))=>Ok(status),Ok(Err(error))=>Err(error.into()),Err(_)=>Err(ReviewError::TimedOut)}};
        if result.is_err() {
            group.kill();
            let _ = timeout(Duration::from_secs(1), child.wait()).await;
        }
        let quiet = group.quiesce().await;
        write.abort();
        let captures = timeout(Duration::from_secs(1), async {
            tokio::join!(&mut stdout, &mut stderr)
        })
        .await;
        stdout.abort();
        stderr.abort();
        if !quiet {
            return Err(ReviewError::Unknown);
        }
        let status = result?;
        let (out, err) = captures.map_err(|_| ReviewError::Unknown)?;
        let out = out.map_err(|_| ReviewError::Protocol)??;
        let err = err.map_err(|_| ReviewError::Protocol)??;
        if !status.success() {
            return Err(ReviewError::Git(String::from_utf8_lossy(&err).into_owned()));
        }
        self.check()?;
        Ok(out)
    }
}
struct Group {
    pid: Pid,
    armed: bool,
}
impl Group {
    fn new(pid: u32) -> Self {
        Self {
            pid: Pid::from_raw(pid as i32).expect("child PID"),
            armed: true,
        }
    }
    fn kill(&self) {
        let _ = kill_process_group(self.pid, Signal::KILL);
    }
    async fn quiesce(&mut self) -> bool {
        self.kill();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if matches!(
                test_kill_process_group(self.pid),
                Err(rustix::io::Errno::SRCH)
            ) {
                self.armed = false;
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}
impl Drop for Group {
    fn drop(&mut self) {
        if self.armed {
            self.kill();
        }
    }
}
async fn capture(mut reader: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>, ReviewError> {
    let mut out = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let n = reader.read(&mut buffer).await?;
        if n == 0 {
            return Ok(out);
        }
        if out.len().saturating_add(n) > limit {
            return Err(ReviewError::Limit("Git output"));
        }
        out.extend_from_slice(&buffer[..n]);
    }
}
pub(super) fn text(bytes: &[u8]) -> Result<&str, ReviewError> {
    std::str::from_utf8(bytes).map_err(|_| ReviewError::Protocol)
}
pub(super) fn oid_valid(oid: &str, format: &str) -> bool {
    oid.len() == if format == "sha256" { 64 } else { 40 }
        && oid
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
pub(super) fn quote(bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![b'"'];
    for &byte in bytes {
        match byte {
            b'"' | b'\\' => {
                out.push(b'\\');
                out.push(byte);
            }
            0x20..=0x7e => out.push(byte),
            _ => out.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
        }
    }
    out.push(b'"');
    out
}

fn revision_parts(value: &str) -> Result<(&str, &str), ReviewError> {
    if value.is_empty() || value.len() > 512 || value.starts_with('-') {
        return Err(ReviewError::Range);
    }
    let at = value.find(['^', '~']).unwrap_or(value.len());
    let (name, suffix) = value.split_at(at);
    if name.is_empty()
        || name.contains("..")
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
    {
        return Err(ReviewError::Range);
    }
    let mut rest = suffix;
    while !rest.is_empty() {
        if !rest.starts_with(['^', '~']) {
            return Err(ReviewError::Range);
        }
        rest = &rest[1..];
        let n = rest.bytes().take_while(u8::is_ascii_digit).count();
        if n > 6 || (n > 0 && rest[..n].parse::<u32>().map_err(|_| ReviewError::Range)? > 100000) {
            return Err(ReviewError::Range);
        }
        rest = &rest[n..];
    }
    Ok((name, suffix))
}
