//! Strict container execution with immutable inputs and bounded guest storage.
//!
//! Only readonly source/helper binds cross the host boundary. Mutable source,
//! cache, temporary and shared-memory files live on separate quota-limited tmpfs
//! mounts. Validated normal-exit exports are published only after quiescence and
//! a baseline check; conflicts retain the guest result without replacing source.

mod transport;
mod workspace;
use crate::Digest;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::{Instant, timeout},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
pub use workspace::RetainedGuest;

const WRITABLE_MOUNT_OPTIONS: &str = "rw,exec,nosuid,nodev";

/// Workspace commands may fetch dependencies. Verification keeps network inputs closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionPolicy {
    Protected,
    Workspace,
}

impl ExecutionPolicy {
    fn network(self) -> &'static str {
        match self {
            Self::Protected => "none",
            Self::Workspace => "bridge",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionRequest {
    pub job_id: Uuid,
    pub workspace: PathBuf,
    pub command: String,
    pub readonly: bool,
    pub timeout_ms: u64,
    pub output_bytes: u64,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum ExecutionStatus {
    Exited(i32),
    Cancelled,
    TimedOut,
    OutputLimit,
    Failed(String),
    Unknown(String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub job_id: Uuid,
    pub status: ExecutionStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub elapsed_ms: u64,
    pub image_id: String,
    pub container_name: String,
}
#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("executor I/O: {0}")]
    Io(#[from] io::Error),
    #[error("executor setup: {0}")]
    Setup(String),
    #[error("executor setup exceeded its deadline")]
    Deadline,
    #[error("invalid execution request: {0}")]
    Request(&'static str),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionLimits {
    pub memory_bytes: u64,
    pub pids: u32,
    pub workspace_bytes: u64,
    pub workspace_inodes: u64,
    pub cache_bytes: u64,
    pub cache_inodes: u64,
    pub temporary_bytes: u64,
    pub temporary_inodes: u64,
    pub retained_guests: u32,
}
impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 512 * 1024 * 1024,
            pids: 64,
            workspace_bytes: 256 * 1024 * 1024,
            workspace_inodes: 32768,
            cache_bytes: 64 * 1024 * 1024,
            cache_inodes: 8192,
            temporary_bytes: 32 * 1024 * 1024,
            temporary_inodes: 4096,
            retained_guests: 8,
        }
    }
}
impl ExecutionLimits {
    fn validate(&self) -> Result<(), RuntimeError> {
        if self.memory_bytes < 32 * 1024 * 1024
            || self.memory_bytes > 8 * 1024 * 1024 * 1024
            || self.pids < 8
            || self.pids > 1024
            || self.retained_guests == 0
            || self.retained_guests > 32
        {
            return Err(RuntimeError::Request("invalid execution resource limits"));
        }
        for (bytes, inodes) in [
            (self.workspace_bytes, self.workspace_inodes),
            (self.cache_bytes, self.cache_inodes),
            (self.temporary_bytes, self.temporary_inodes),
        ] {
            if !(1024 * 1024..=1024 * 1024 * 1024).contains(&bytes)
                || bytes % 4096 != 0
                || !(16..=100000).contains(&inodes)
            {
                return Err(RuntimeError::Request("invalid tmpfs quota"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionEnvironment {
    pub daemon_id: String,
    pub endpoint: String,
    pub protocol_version: u32,
    pub image_id: String,
    pub memory_bytes: u64,
    pub pids: u32,
    pub cpus: u32,
    pub network: String,
    pub helper_digest: Digest,
    pub architecture: String,
    pub workspace_bytes: u64,
    pub workspace_inodes: u64,
    pub cache_bytes: u64,
    pub cache_inodes: u64,
    pub temporary_bytes: u64,
    pub temporary_inodes: u64,
    pub source_transport: String,
    #[serde(default)]
    pub writable_mount_options: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionFence {
    pub daemon_id: String,
    pub endpoint: String,
    pub task_id: Uuid,
    pub generation: u64,
    pub job_id: Uuid,
    pub container_name: String,
    pub observed_absent: bool,
}

pub struct DockerExecutor {
    image_id: String,
    architecture: String,
    helper: PathBuf,
    helper_digest: Digest,
    limits: ExecutionLimits,
    docker: Arc<Docker>,
}
impl DockerExecutor {
    pub async fn connect(image: &str) -> Result<Self, RuntimeError> {
        Self::connect_selected(image, None, ExecutionLimits::default()).await
    }
    pub async fn connect_with_helper(
        image: &str,
        helper: &Path,
        limits: ExecutionLimits,
    ) -> Result<Self, RuntimeError> {
        Self::connect_selected(image, Some(helper), limits).await
    }
    async fn connect_selected(
        image: &str,
        helper: Option<&Path>,
        limits: ExecutionLimits,
    ) -> Result<Self, RuntimeError> {
        limits.validate()?;
        if image.is_empty() || image.len() > 512 {
            return Err(RuntimeError::Request("invalid image reference"));
        }
        let docker = Arc::new(Docker::connect().await?);
        let output = docker
            .output(
                &[
                    "image".into(),
                    "inspect".into(),
                    "--format".into(),
                    "{{.Id}} {{.Os}} {{.Architecture}} {{with (index .Config \"Volumes\")}}{{len .}}{{else}}0{{end}}".into(),
                    image.into(),
                ],
                Duration::from_secs(15),
            )
            .await?;
        if !output.success {
            return Err(RuntimeError::Setup(output.message()));
        }
        let text = std::str::from_utf8(&output.stdout)
            .map_err(|_| RuntimeError::Setup("invalid image metadata".into()))?;
        let fields: Vec<_> = text.split_whitespace().collect();
        if fields.len() != 4
            || fields[1] != "linux"
            || !fields[0]
                .strip_prefix("sha256:")
                .is_some_and(|hash| hash.parse::<Digest>().is_ok())
        {
            return Err(RuntimeError::Setup(
                "execution requires a pinned Linux image".into(),
            ));
        }
        if fields[3] != "0" {
            return Err(RuntimeError::Setup(
                "strict execution rejects images declaring writable VOLUME mounts".into(),
            ));
        }
        let architecture = match fields[2] {
            "arm64" => "aarch64",
            "amd64" => "x86_64",
            _ => {
                return Err(RuntimeError::Setup(
                    "unsupported executor architecture".into(),
                ));
            }
        };
        let helper = helper
            .map(Path::to_owned)
            .or_else(|| std::env::var_os("ORVEK_EXECUTOR_HELPER").map(PathBuf::from))
            .unwrap_or(
                std::env::current_exe()?
                    .parent()
                    .ok_or(RuntimeError::Request("binary directory unavailable"))?
                    .join(format!("orvek-executor-linux-{architecture}")),
            );
        let bytes = helper_bytes(&helper)?;
        validate_elf(&bytes, architecture)?;
        Ok(Self {
            image_id: fields[0].into(),
            architecture: architecture.into(),
            helper: helper.canonicalize()?,
            helper_digest: Digest::of(&bytes),
            limits,
            docker,
        })
    }
    pub fn image_id(&self) -> &str {
        &self.image_id
    }
    pub fn environment(&self) -> ExecutionEnvironment {
        self.environment_for(ExecutionPolicy::Protected)
    }

    pub fn environment_for(&self, policy: ExecutionPolicy) -> ExecutionEnvironment {
        ExecutionEnvironment {
            daemon_id: self.docker.daemon_id.clone(),
            endpoint: self.docker.endpoint.clone(),
            protocol_version: orvek_executor::VERSION,
            image_id: self.image_id.clone(),
            memory_bytes: self.limits.memory_bytes,
            pids: self.limits.pids,
            cpus: 1,
            network: policy.network().into(),
            helper_digest: self.helper_digest,
            architecture: self.architecture.clone(),
            workspace_bytes: self.limits.workspace_bytes,
            workspace_inodes: self.limits.workspace_inodes,
            cache_bytes: self.limits.cache_bytes,
            cache_inodes: self.limits.cache_inodes,
            temporary_bytes: self.limits.temporary_bytes,
            temporary_inodes: self.limits.temporary_inodes,
            source_transport: "readonly_snapshot_and_validated_export".into(),
            writable_mount_options: WRITABLE_MOUNT_OPTIONS.into(),
        }
    }
    pub fn retained_guest(
        &self,
        request: &ExecutionRequest,
    ) -> Result<Option<RetainedGuest>, RuntimeError> {
        workspace::retained(&request.workspace, request.job_id, &self.limits)
    }

    pub async fn run(
        &self,
        request: &ExecutionRequest,
        cancellation: CancellationToken,
    ) -> Result<ExecutionResult, RuntimeError> {
        self.run_with_policy(request, ExecutionPolicy::Protected, cancellation)
            .await
    }

    pub async fn run_with_policy(
        &self,
        request: &ExecutionRequest,
        policy: ExecutionPolicy,
        cancellation: CancellationToken,
    ) -> Result<ExecutionResult, RuntimeError> {
        if request.timeout_ms == 0
            || request.timeout_ms > 3600000
            || request.output_bytes == 0
            || request.output_bytes > 16 * 1024 * 1024
            || request.command.is_empty()
            || request.command.len() > 65536
        {
            return Err(RuntimeError::Request(
                "execution request must have bounded command, deadline and output",
            ));
        }
        let started = Instant::now();
        let name = container_name(request.job_id);
        let mut result = ExecutionResult {
            job_id: request.job_id,
            status: ExecutionStatus::Cancelled,
            stdout: Vec::new(),
            stderr: Vec::new(),
            elapsed_ms: 0,
            image_id: self.image_id.clone(),
            container_name: name.clone(),
        };
        if cancellation.is_cancelled() {
            return Ok(result);
        }
        self.docker.verify_identity().await?;
        let mut staged =
            match workspace::Workspace::prepare(&request.workspace, &self.limits, request.readonly)
            {
                Ok(workspace) => workspace,
                Err(error) => {
                    result.status = ExecutionStatus::Failed(error.to_string());
                    return Ok(result);
                }
            };
        if elapsed_timeout(started, request.timeout_ms) {
            result.status = ExecutionStatus::TimedOut;
            return Ok(result);
        }
        let bytes = helper_bytes(&self.helper)?;
        if Digest::of(&bytes) != self.helper_digest {
            return Err(RuntimeError::Setup(
                "executor helper changed; reconnect before execution".into(),
            ));
        }
        let helper = staged.directory().join("orvek-executor");
        let mut file = fs::File::create(&helper)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o555))?;
        let source_target = if request.readonly {
            "/workspace"
        } else {
            "/source"
        };
        let mut create = vec![
            "create".into(),
            "--interactive".into(),
            "--name".into(),
            name.clone(),
            "--label".into(),
            "tact.managed=true".into(),
            "--label".into(),
            format!("tact.job={}", request.job_id),
            format!("--network={}", policy.network()),
            "--ipc=private".into(),
            "--cgroupns=private".into(),
            "--read-only".into(),
            "--no-healthcheck".into(),
            "--cap-drop=ALL".into(),
            "--security-opt=no-new-privileges".into(),
            "--pids-limit".into(),
            self.limits.pids.to_string(),
            "--memory".into(),
            self.limits.memory_bytes.to_string(),
            "--memory-swap".into(),
            self.limits.memory_bytes.to_string(),
            "--cpus=1".into(),
            "--log-driver=none".into(),
            "--user=0:0".into(),
            "--workdir=/".into(),
            "--entrypoint=/orvek-executor".into(),
            "--mount".into(),
            readonly_bind(&staged.source, source_target)?,
            "--mount".into(),
            readonly_bind(&helper, "/orvek-executor")?,
        ];
        for capability in ["SETUID", "SETGID", "KILL", "CHOWN", "DAC_READ_SEARCH"] {
            create.push(format!("--cap-add={capability}"));
        }
        if !request.readonly {
            create.push("--tmpfs".into());
            create.push(tmpfs(
                "/workspace",
                self.limits.workspace_bytes,
                self.limits.workspace_inodes,
                "uid=0,gid=0,mode=0755",
            ));
        }
        for (path, bytes, inodes) in [
            ("/cache", self.limits.cache_bytes, self.limits.cache_inodes),
            (
                "/tmp",
                self.limits.temporary_bytes,
                self.limits.temporary_inodes,
            ),
            (
                "/dev/shm",
                self.limits.temporary_bytes,
                self.limits.temporary_inodes,
            ),
        ] {
            create.push("--tmpfs".into());
            create.push(tmpfs(path, bytes, inodes, "mode=1777"));
        }
        create.push(self.image_id.clone());
        let created = self
            .docker
            .output(
                &create,
                remaining(started, request.timeout_ms).min(Duration::from_secs(30)),
            )
            .await;
        match created {
            Ok(output) if output.success => {}
            Ok(output) => {
                result.status = ExecutionStatus::Failed(output.message());
                if let Err(error) = self.remove(request.job_id).await {
                    result.status = ExecutionStatus::Unknown(format!(
                        "creation failed ({}); container could not be fenced: {error}",
                        output.message()
                    ));
                }
                return Ok(result);
            }
            Err(error) => {
                result.status = if matches!(error, RuntimeError::Deadline) {
                    ExecutionStatus::TimedOut
                } else {
                    ExecutionStatus::Failed(error.to_string())
                };
                if let Err(fence) = self.remove(request.job_id).await {
                    result.status = ExecutionStatus::Unknown(format!(
                        "container creation failed ({error}); fencing failed: {fence}"
                    ));
                }
                return Ok(result);
            }
        }
        let mut cleanup = ContainerGuard::new(self.docker.clone(), request.job_id);
        let nonce = Uuid::new_v4().to_string();
        let spec = orvek_executor::Request {
            version: orvek_executor::VERSION,
            job_id: request.job_id.to_string(),
            nonce: nonce.clone(),
            command: request.command.clone(),
            readonly: request.readonly,
            timeout_ms: remaining(started, request.timeout_ms)
                .as_millis()
                .max(1)
                .try_into()
                .unwrap_or(u64::MAX),
            output_bytes: request.output_bytes,
            workspace_bytes: self.limits.workspace_bytes,
            workspace_inodes: self.limits.workspace_inodes,
            cache_bytes: self.limits.cache_bytes,
            cache_inodes: self.limits.cache_inodes,
            temporary_bytes: self.limits.temporary_bytes,
            temporary_inodes: self.limits.temporary_inodes,
        };
        let encoded = serde_json::to_vec(&spec)
            .map_err(|_| RuntimeError::Request("request encoding failed"))?;
        let mut command = self.docker.command();
        command
            .args(["start", "--attach", "--interactive", &name])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                result.status = ExecutionStatus::Failed(error.to_string());
                match self.remove(request.job_id).await {
                    Ok(()) => cleanup.disarm(),
                    Err(fence) => {
                        result.status = ExecutionStatus::Unknown(format!(
                            "attach spawn failed ({error}); fencing failed: {fence}"
                        ))
                    }
                }
                return Ok(result);
            }
        };
        let abort = CancellationToken::new();
        let receiver = transport::Receiver::new(
            staged.incoming.clone(),
            nonce,
            request.job_id.to_string(),
            request.readonly,
            self.limits.clone(),
            request.output_bytes,
        );
        let mut read = tokio::spawn(
            receiver.receive(child.stdout.take().expect("piped stdout"), abort.clone()),
        );
        let mut diagnostic = tokio::spawn(capture(
            child.stderr.take().expect("piped stderr"),
            64 * 1024,
        ));
        let mut input = child.stdin.take().expect("piped stdin");
        let write = tokio::spawn(async move {
            input
                .write_all(&(encoded.len() as u32).to_le_bytes())
                .await?;
            input.write_all(&encoded).await?;
            input.shutdown().await
        });
        let mut forced = tokio::select! {
            biased;
            () = cancellation.cancelled() => Some(ExecutionStatus::Cancelled),
            () = abort.cancelled() => Some(ExecutionStatus::Failed("invalid executor transfer".into())),
            _ = tokio::time::sleep(remaining(started, request.timeout_ms)) => Some(ExecutionStatus::TimedOut),
            status = child.wait() => match status {
                Ok(_) => None,
                Err(error) => Some(ExecutionStatus::Unknown(format!("Docker attach outcome unknown: {error}"))),
            }
        };
        if forced.is_some() {
            let _ = self
                .docker
                .output(
                    &["kill".into(), "--signal=TERM".into(), name.clone()],
                    Duration::from_secs(2),
                )
                .await;
            if !matches!(
                timeout(Duration::from_secs(2), child.wait()).await,
                Ok(Ok(_))
            ) {
                let _ = self.remove(request.job_id).await;
                let _ = child.kill().await;
            }
        }
        let state = self.inspect(request.job_id).await;
        let report = match timeout(Duration::from_secs(2), &mut read).await {
            Ok(Ok(report)) => Some(report),
            _ => None,
        };
        read.abort();
        let diagnostics = match timeout(Duration::from_secs(2), &mut diagnostic).await {
            Ok(Ok(Ok(bytes))) => bytes,
            _ => Vec::new(),
        };
        diagnostic.abort();
        write.abort();
        let stopped = match &state {
            Ok(Some(state)) => !state.state.running && state.state.status == "exited",
            Ok(None) => forced.is_some(),
            Err(_) => false,
        };
        if !stopped {
            forced = Some(ExecutionStatus::Unknown(format!(
                "container quiescence was not observed: {state:?}"
            )));
        }
        let removed = self.remove(request.job_id).await;
        match removed {
            Ok(()) => cleanup.disarm(),
            Err(error) => {
                forced = Some(ExecutionStatus::Unknown(format!(
                    "container removal was not confirmed: {error}"
                )));
            }
        }
        if forced.is_none() {
            if cancellation.is_cancelled() {
                forced = Some(ExecutionStatus::Cancelled);
            } else if elapsed_timeout(started, request.timeout_ms) {
                forced = Some(ExecutionStatus::TimedOut);
            }
        }
        result.status = forced
            .clone()
            .unwrap_or_else(|| ExecutionStatus::Failed("incomplete executor response".into()));
        if let Some(report) = report {
            result.stdout = report.stdout;
            result.stderr = report.stderr;
            if forced.is_none() {
                result.status = if state
                    .as_ref()
                    .ok()
                    .and_then(Option::as_ref)
                    .is_some_and(|state| state.state.oom_killed)
                {
                    ExecutionStatus::Failed("container memory limit exceeded".into())
                } else if let Some(failure) = report.failure {
                    ExecutionStatus::Failed(failure)
                } else if state
                    .as_ref()
                    .ok()
                    .and_then(Option::as_ref)
                    .is_none_or(|state| state.state.exit_code != 0)
                {
                    ExecutionStatus::Failed(format!(
                        "executor failed: {}",
                        bounded_text(&diagnostics)
                    ))
                } else if let Some(complete) = report.complete {
                    match complete.outcome {
                        orvek_executor::Outcome::Exited { code } => {
                            if !request.readonly {
                                match staged.validate_guest(report.entries) {
                                    Ok(guest) => match staged.publish(
                                        &guest,
                                        request.job_id,
                                        &cancellation,
                                        started + Duration::from_millis(request.timeout_ms),
                                    ) {
                                        Ok(_) if cancellation.is_cancelled() => ExecutionStatus::Cancelled,
                                        Ok(_) if elapsed_timeout(started, request.timeout_ms) => ExecutionStatus::TimedOut,
                                        Ok(true) => ExecutionStatus::Exited(code),
                                        Ok(false) => ExecutionStatus::Failed("workspace conflict; validated guest retained for recovery".into()),
                                        Err(error) => ExecutionStatus::Unknown(error.to_string()),
                                    },
                                    Err(error) => ExecutionStatus::Failed(format!("invalid workspace export: {error}")),
                                }
                            } else {
                                ExecutionStatus::Exited(code)
                            }
                        }
                        orvek_executor::Outcome::Cancelled => ExecutionStatus::Cancelled,
                        orvek_executor::Outcome::TimedOut => ExecutionStatus::TimedOut,
                        orvek_executor::Outcome::OutputLimit => ExecutionStatus::OutputLimit,
                        orvek_executor::Outcome::MemoryLimit => {
                            ExecutionStatus::Failed("container memory limit exceeded".into())
                        }
                        orvek_executor::Outcome::QuotaLimit => ExecutionStatus::Failed(
                            "workspace byte or inode quota exhausted".into(),
                        ),
                        orvek_executor::Outcome::InvalidWorkspace => ExecutionStatus::Failed(
                            "unsupported or incomplete guest workspace".into(),
                        ),
                        orvek_executor::Outcome::SupervisorError => {
                            ExecutionStatus::Failed("sandbox supervisor failed".into())
                        }
                    }
                } else {
                    ExecutionStatus::Failed("executor completion missing".into())
                };
            }
        }
        result.elapsed_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        Ok(result)
    }

    pub async fn reconcile(&self, job_id: Uuid) -> Result<(), RuntimeError> {
        self.remove(job_id).await
    }
    pub async fn reconcile_job(
        &self,
        task_id: Uuid,
        generation: u64,
        job_id: Uuid,
    ) -> Result<ExecutionFence, RuntimeError> {
        self.remove(job_id).await?;
        Ok(ExecutionFence {
            daemon_id: self.docker.daemon_id.clone(),
            endpoint: self.docker.endpoint.clone(),
            task_id,
            generation,
            job_id,
            container_name: container_name(job_id),
            observed_absent: true,
        })
    }
    pub async fn reconcile_jobs(
        &self,
        task_id: Uuid,
        generation: u64,
        jobs: &[Uuid],
    ) -> Result<Vec<ExecutionFence>, RuntimeError> {
        if jobs.len() > 128 {
            return Err(RuntimeError::Request("too many recovery jobs"));
        }
        let mut receipts = Vec::new();
        for &job in jobs {
            receipts.push(self.reconcile_job(task_id, generation, job).await?);
        }
        Ok(receipts)
    }
    async fn inspect(&self, job_id: Uuid) -> Result<Option<ContainerState>, RuntimeError> {
        self.docker.inspect(job_id).await
    }
    async fn remove(&self, job_id: Uuid) -> Result<(), RuntimeError> {
        self.docker.remove(job_id).await
    }
}

#[derive(Debug, Deserialize)]
struct ContainerState {
    state: State,
    managed: Option<String>,
    job: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct State {
    running: bool,
    exit_code: i32,
    status: String,
    #[serde(rename = "OOMKilled")]
    oom_killed: bool,
}
struct Docker {
    program: PathBuf,
    endpoint: String,
    daemon_id: String,
    config: tempfile::TempDir,
}
struct DockerOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
impl DockerOutput {
    fn message(&self) -> String {
        bounded_text(&self.stderr)
    }
}
impl Docker {
    async fn connect() -> Result<Self, RuntimeError> {
        if std::env::var_os("DOCKER_HOST").is_some_and(|v| !v.is_empty()) {
            return Err(RuntimeError::Setup(
                "select a local context instead of DOCKER_HOST".into(),
            ));
        }
        let program = [
            "/opt/homebrew/bin/docker",
            "/usr/local/bin/docker",
            "/usr/bin/docker",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .ok_or_else(|| RuntimeError::Setup("Docker CLI unavailable".into()))?
        .canonicalize()?;
        let mut command = Command::new(&program);
        command.args([
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}",
        ]);
        let output = bounded_command(command, Duration::from_secs(10)).await?;
        let endpoint = std::str::from_utf8(&output.stdout)
            .map_err(|_| RuntimeError::Setup("invalid Docker context".into()))?
            .trim()
            .to_owned();
        if !output.success || !endpoint.starts_with("unix://") {
            return Err(RuntimeError::Setup(
                "strict execution requires a local Unix Docker socket".into(),
            ));
        }
        let mut docker = Self {
            program,
            endpoint,
            daemon_id: String::new(),
            config: tempfile::tempdir()?,
        };
        let output = docker.output(
            &[
                "info".into(),
                "--format".into(),
                concat!(
                    r#"{"memory":{{.MemoryLimit}},"swap":{{.SwapLimit}},"pids":{{.PidsLimit}},"#,
                    r#""cgroup":{{json .CgroupVersion}},"security":{{json .SecurityOptions}},"id":{{json .ID}}}"#,
                ).into(),
            ],
            Duration::from_secs(10),
        ).await?;
        #[derive(Deserialize)]
        struct Backend {
            id: String,
            memory: bool,
            swap: bool,
            pids: bool,
            cgroup: String,
            security: Vec<String>,
        }
        let backend: Backend = serde_json::from_slice(&output.stdout)
            .map_err(|_| RuntimeError::Setup("Docker resource capabilities unavailable".into()))?;
        if !output.success
            || backend.id.is_empty()
            || !backend.memory
            || !backend.swap
            || !backend.pids
            || backend.cgroup != "2"
            || !backend
                .security
                .iter()
                .any(|s| s == "name=seccomp,profile=builtin")
            || backend
                .security
                .iter()
                .any(|s| s.contains("rootless") || s == "name=userns")
        {
            return Err(RuntimeError::Setup("strict execution requires cgroup v2 memory/swap/PID controls and builtin seccomp on a non-rootless, non-userns-remapped local Docker backend".into()));
        }
        docker.daemon_id = backend.id;
        Ok(docker)
    }

    async fn verify_identity(&self) -> Result<(), RuntimeError> {
        let identity = self
            .output(
                &["info".into(), "--format".into(), "{{.ID}}".into()],
                Duration::from_secs(3),
            )
            .await?;
        if !identity.success
            || std::str::from_utf8(&identity.stdout).ok().map(str::trim)
                != Some(self.daemon_id.as_str())
        {
            return Err(RuntimeError::Setup("Docker daemon identity changed; old jobs require reconciliation on their original backend".into()));
        }
        Ok(())
    }
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.config.path())
            .env("DOCKER_CONFIG", self.config.path())
            .args(["--host", &self.endpoint]);
        command
    }
    async fn output(
        &self,
        args: &[String],
        duration: Duration,
    ) -> Result<DockerOutput, RuntimeError> {
        let mut command = self.command();
        command.args(args);
        bounded_command(command, duration).await
    }
    async fn inspect(&self, job_id: Uuid) -> Result<Option<ContainerState>, RuntimeError> {
        let name = container_name(job_id);
        let output = self.output(
            &[
                "container".into(),
                "inspect".into(),
                "--format".into(),
                concat!(
                    r#"{"state":{{json .State}},"managed":{{json (index .Config.Labels "tact.managed")}},"#,
                    r#""job":{{json (index .Config.Labels "tact.job")}}}"#,
                ).into(),
                name.clone(),
            ],
            Duration::from_secs(3),
        ).await?;
        if output.success {
            let state = serde_json::from_slice(&output.stdout)
                .map_err(|_| RuntimeError::Setup("invalid Docker state".into()))?;
            return Ok(Some(state));
        }
        let absent = self
            .output(
                &[
                    "container".into(),
                    "ls".into(),
                    "--all".into(),
                    "--filter".into(),
                    format!("name=^/{name}$"),
                    "--format".into(),
                    "{{.ID}}".into(),
                ],
                Duration::from_secs(3),
            )
            .await?;
        if absent.success && absent.stdout.iter().all(u8::is_ascii_whitespace) {
            return Ok(None);
        }
        Err(RuntimeError::Setup(output.message()))
    }

    async fn remove(&self, job_id: Uuid) -> Result<(), RuntimeError> {
        self.verify_identity().await?;
        let Some(state) = self.inspect(job_id).await? else {
            self.verify_identity().await?;
            return Ok(());
        };
        if state.managed.as_deref() != Some("true")
            || state.job.as_deref() != Some(job_id.to_string().as_str())
        {
            return Err(RuntimeError::Setup(
                "refusing to remove an unowned container".into(),
            ));
        }
        let removal = self
            .output(
                &["rm".into(), "--force".into(), container_name(job_id)],
                Duration::from_secs(3),
            )
            .await?;
        // Another cancellation/recovery coordinator may already be removing
        // this job. Only observed absence establishes the fence, never rm's
        // exit status or its "already in progress" diagnostic.
        let absent = tokio::time::timeout(Duration::from_secs(5), async {
            while self.inspect(job_id).await?.is_some() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok::<(), RuntimeError>(())
        })
        .await;
        match absent {
            Ok(result) => result?,
            Err(_) => {
                return Err(RuntimeError::Setup(format!(
                    "container absence was not confirmed before its deadline; removal diagnostics: {}",
                    removal.message()
                )));
            }
        }
        self.verify_identity().await
    }
}
struct ContainerGuard {
    docker: Arc<Docker>,
    job: Uuid,
    armed: bool,
}
impl ContainerGuard {
    fn new(docker: Arc<Docker>, job: Uuid) -> Self {
        Self {
            docker,
            job,
            armed: true,
        }
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for ContainerGuard {
    fn drop(&mut self) {
        if self.armed
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            let docker = self.docker.clone();
            let job = self.job;
            handle.spawn(async move {
                let _ = docker.remove(job).await;
            });
        }
    }
}
async fn bounded_command(
    mut command: Command,
    duration: Duration,
) -> Result<DockerOutput, RuntimeError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let mut stdout = tokio::spawn(capture(
        child.stdout.take().expect("piped stdout"),
        64 * 1024,
    ));
    let mut stderr = tokio::spawn(capture(
        child.stderr.take().expect("piped stderr"),
        64 * 1024,
    ));
    let status = match timeout(duration, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            stdout.abort();
            stderr.abort();
            return Err(error.into());
        }
        Err(_) => {
            let _ = child.kill().await;
            stdout.abort();
            stderr.abort();
            return Err(RuntimeError::Deadline);
        }
    };
    let captures = timeout(Duration::from_secs(1), async {
        tokio::join!(&mut stdout, &mut stderr)
    })
    .await;
    stdout.abort();
    stderr.abort();
    let (stdout, stderr) = captures.map_err(|_| RuntimeError::Deadline)?;
    let stdout = stdout.map_err(|e| io::Error::other(e.to_string()))??;
    let stderr = stderr.map_err(|e| io::Error::other(e.to_string()))??;
    Ok(DockerOutput {
        success: status.success(),
        stdout,
        stderr,
    })
}
async fn capture(mut reader: impl AsyncRead + Unpin, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len().saturating_add(count) > limit {
            return Err(io::Error::other("Docker diagnostic limit"));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}
fn remaining(start: Instant, millis: u64) -> Duration {
    Duration::from_millis(millis).saturating_sub(start.elapsed())
}
fn elapsed_timeout(start: Instant, millis: u64) -> bool {
    start.elapsed() >= Duration::from_millis(millis)
}
fn container_name(job: Uuid) -> String {
    format!("tact-job-{job}")
}
fn tmpfs(path: &str, bytes: u64, inodes: u64, extra: &str) -> String {
    // Docker defaults tmpfs to noexec; builds and installed workspace tools must run.
    format!("{path}:{WRITABLE_MOUNT_OPTIONS},size={bytes},nr_inodes={inodes},{extra}")
}
fn readonly_bind(path: &Path, target: &str) -> Result<String, RuntimeError> {
    let path = path
        .to_str()
        .filter(|p| !p.contains(','))
        .ok_or(RuntimeError::Request("bind path is not representable"))?;
    Ok(format!("type=bind,source={path},target={target},readonly"))
}
fn helper_bytes(path: &Path) -> Result<Vec<u8>, RuntimeError> {
    let meta = fs::symlink_metadata(path).map_err(|_| {
        RuntimeError::Setup(
            "Linux executor helper missing; set ORVEK_EXECUTOR_HELPER or install the architecture-specific sidecar".into(),
        )
    })?;
    if !meta.is_file() || meta.len() > 8 * 1024 * 1024 {
        return Err(RuntimeError::Setup("invalid executor helper file".into()));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(RuntimeError::Setup("executor helper too large".into()));
    }
    Ok(bytes)
}
fn validate_elf(bytes: &[u8], arch: &str) -> Result<(), RuntimeError> {
    let expected = if arch == "aarch64" { 183u16 } else { 62u16 };
    if bytes.len() < 20
        || &bytes[..4] != b"\x7fELF"
        || bytes[4] != 2
        || bytes[5] != 1
        || u16::from_le_bytes([bytes[18], bytes[19]]) != expected
    {
        return Err(RuntimeError::Setup(
            "executor helper architecture does not match the image".into(),
        ));
    }
    if bytes.len() < 64 {
        return Err(RuntimeError::Setup("truncated ELF header".into()));
    }
    let offset = u64::from_le_bytes(
        bytes[32..40]
            .try_into()
            .map_err(|_| RuntimeError::Request("invalid ELF"))?,
    ) as usize;
    let stride = u16::from_le_bytes([bytes[54], bytes[55]]) as usize;
    let count = u16::from_le_bytes([bytes[56], bytes[57]]) as usize;
    if stride < 56
        || count > 256
        || offset
            .checked_add(stride.saturating_mul(count))
            .is_none_or(|end| end > bytes.len())
    {
        return Err(RuntimeError::Setup("invalid ELF program table".into()));
    }
    for index in 0..count {
        let at = offset + index * stride;
        if u32::from_le_bytes(
            bytes[at..at + 4]
                .try_into()
                .map_err(|_| RuntimeError::Request("invalid ELF"))?,
        ) == 3
        {
            return Err(RuntimeError::Setup(
                "executor helper must be statically linked".into(),
            ));
        }
    }
    Ok(())
}
fn bounded_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).into_owned()
}
