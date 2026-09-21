//! Detached durable-host process and its configuration-aware local client.

use crate::app::{
    config::{AuthMode, Config},
    error::{Error, Result},
    shutdown,
};
use orvek_harness::{
    Digest, context,
    controller::{Host, HostInfo},
    inference::{Limits as ProviderLimits, ResponsesClient, Route, Transport},
    ipc::{self, Command, Request, Response, WatchFrame},
    runtime::DockerExecutor,
};
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    net::UnixStream,
    process::Command as ProcessCommand,
    time::{Instant, timeout},
};
use tokio_util::sync::CancellationToken;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const START_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub(crate) struct HostClient {
    root: Arc<PathBuf>,
}

impl HostClient {
    #[cfg(test)]
    pub(crate) fn fixture(root: &Path) -> Self {
        Self {
            root: Arc::new(root.to_owned()),
        }
    }

    pub(crate) fn for_config(config_path: &Path) -> Self {
        Self {
            root: Arc::new(state_directory(config_path)),
        }
    }

    pub(crate) async fn connect(config: &Config) -> Result<Self> {
        let client = Self::for_config(config.path());
        let expected = configuration_identity(config)?;
        match client.info().await {
            Ok(info) if info.accepting && info.config_identity == Some(expected) => {
                return Ok(client);
            }
            Ok(info) => {
                if !info.active_sessions.is_empty() {
                    return Err(Error::HostRequest(
                        "host configuration changed while tasks are active; those tasks continue with their original settings, and reload will be available when idle".into(),
                    ));
                }
                if !matches!(
                    client.query(Command::ShutdownIfIdle).await?,
                    Response::Shutdown { accepted: true }
                ) {
                    return Err(Error::HostRequest(
                        "host became busy before configuration reload; retry after it becomes idle"
                            .into(),
                    ));
                }
                let until = Instant::now() + CONNECT_TIMEOUT;
                while client.info().await.is_ok() {
                    if Instant::now() >= until {
                        return Err(Error::HostRequest(
                            "previous host has not finished its idle shutdown".into(),
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
            Err(Error::Connection(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) => {}
            Err(Error::HostRequest(message)) if message == ipc::UNSUPPORTED_PROTOCOL_VERSION => {
                if !client.shutdown_incompatible_host().await? {
                    return Err(Error::HostRequest(
                        "the previous host protocol is still running active tasks; retry after they finish"
                            .into(),
                    ));
                }
                let until = Instant::now() + CONNECT_TIMEOUT;
                while client.info().await.is_ok() {
                    if Instant::now() >= until {
                        return Err(Error::HostRequest(
                            "previous host has not finished its protocol upgrade shutdown".into(),
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
            Err(error) => return Err(error),
        }
        client.start(config).await?;
        if client.info().await?.config_identity != Some(expected) {
            return Err(Error::HostRequest(
                "configuration changed during host startup; reconnect using the current configuration"
                    .into(),
            ));
        }
        Ok(client)
    }

    async fn shutdown_incompatible_host(&self) -> Result<bool> {
        let mut request = Request::new(Command::ShutdownIfIdle);
        match self.call(&request, CONNECT_TIMEOUT).await {
            Ok(Response::Shutdown { accepted }) => Ok(accepted),
            Err(Error::HostRequest(message)) if message == ipc::UNSUPPORTED_PROTOCOL_VERSION => {
                // Protocol 2 required the shutdown request to use its exact version.
                request.version = 2;
                match self.call(&request, CONNECT_TIMEOUT).await? {
                    Response::Shutdown { accepted } => Ok(accepted),
                    _ => Err(Error::HostRequest(
                        "unsupported host shutdown response".into(),
                    )),
                }
            }
            Ok(_) => Err(Error::HostRequest(
                "unsupported host shutdown response".into(),
            )),
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn info(&self) -> Result<HostInfo> {
        match self.query(Command::Info).await? {
            Response::Info(info) => Ok(info),
            _ => Err(Error::HostRequest(
                "unsupported host information response".into(),
            )),
        }
    }

    pub(crate) async fn call(&self, request: &Request, deadline: Duration) -> Result<Response> {
        let mut stream = self.open().await?;
        timeout(CONNECT_TIMEOUT, ipc::write_frame(&mut stream, request))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "host write deadline"))??;
        let response: Response = timeout(deadline, ipc::read_frame(&mut stream))
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "host response deadline; reconnect to inspect the durable operation",
                )
            })??;
        if let Response::Error { message } = response {
            return Err(Error::HostRequest(message));
        }
        Ok(response)
    }

    pub(crate) async fn query(&self, command: Command) -> Result<Response> {
        self.call(&Request::new(command), CONNECT_TIMEOUT).await
    }

    pub(crate) async fn subscribe(
        &self,
        after: u64,
        session: orvek_harness::session::SessionId,
    ) -> Result<HostWatch> {
        let mut stream = self.open().await?;
        timeout(
            CONNECT_TIMEOUT,
            ipc::write_frame(
                &mut stream,
                &Request::new(Command::Watch {
                    after,
                    session: Some(session),
                }),
            ),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "host watch deadline"))??;
        Ok(HostWatch {
            stream,
            after,
            header: [0; 4],
            header_read: 0,
            body: Vec::new(),
            body_read: 0,
        })
    }

    async fn probe(&self) -> Result<()> {
        match self
            .query(Command::Sessions {
                offset: 0,
                limit: 1,
            })
            .await?
        {
            Response::Sessions(_) => Ok(()),
            _ => Err(Error::HostRequest("unexpected host handshake".into())),
        }
    }

    async fn open(&self) -> Result<UnixStream> {
        use std::os::unix::fs::FileTypeExt;

        let socket = self.root.join("host.sock");
        let uid = rustix::process::geteuid().as_raw();
        let metadata = fs::symlink_metadata(&socket)?;
        if !metadata.file_type().is_socket()
            || metadata.uid() != uid
            || metadata.mode() & 0o077 != 0
        {
            return Err(Error::HostRequest(
                "host socket owner or permissions are invalid".into(),
            ));
        }
        let stream = timeout(CONNECT_TIMEOUT, UnixStream::connect(socket))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "host connection deadline"))??;
        if stream.peer_cred()?.uid() != uid {
            return Err(Error::HostRequest(
                "host peer identity does not match this user".into(),
            ));
        }
        Ok(stream)
    }

    async fn start(&self, config: &Config) -> Result<()> {
        fs::create_dir_all(&*self.root)?;
        let metadata = fs::symlink_metadata(&*self.root)?;
        if !metadata.is_dir() || metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(Error::HostRequest(
                "host directory must belong to this user".into(),
            ));
        }
        fs::set_permissions(&*self.root, fs::Permissions::from_mode(0o700))?;
        let log_path = self.root.join("startup.log");
        let log = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&log_path)?;
        let mut process = ProcessCommand::new(std::env::current_exe()?);
        process
            .arg("--config")
            .arg(config.path())
            .arg("--auth")
            .arg(match config.auth().mode() {
                AuthMode::Auto => "auto",
                AuthMode::ApiKey => "api-key",
                AuthMode::ChatGpt => "chatgpt",
            })
            .arg("--auth-file")
            .arg(config.auth().file());
        if let Some(url) = config.agent().api_base_url() {
            process.arg("--api-base-url").arg(url);
        }
        if let Some(url) = config.agent().websocket_url() {
            process.arg("--websocket-url").arg(url);
        }
        process
            .arg("--max-subagents")
            .arg(config.agent().max_subagents().to_string())
            .arg("host")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        // The host owns its lifetime and receives no terminal process-group signals.
        unsafe {
            process.pre_exec(|| {
                rustix::process::setsid()
                    .map(|_| ())
                    .map_err(io::Error::from)
            });
        }
        let mut child = process.spawn()?;
        let until = Instant::now() + START_TIMEOUT;
        loop {
            if self.probe().await.is_ok() {
                return Ok(());
            }
            if child.try_wait()?.is_some() || Instant::now() >= until {
                let mut details = String::new();
                fs::File::open(&log_path)?
                    .take(4096)
                    .read_to_string(&mut details)?;
                return Err(Error::HostRequest(format!(
                    "local host did not become available: {}",
                    details.trim()
                )));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

pub(crate) struct HostWatch {
    stream: UnixStream,
    after: u64,
    header: [u8; 4],
    header_read: usize,
    body: Vec<u8>,
    body_read: usize,
}

impl HostWatch {
    pub(crate) const fn cursor(&self) -> u64 {
        self.after
    }

    pub(crate) async fn next(&mut self) -> Result<WatchFrame> {
        // Progress lives on the watcher so cancellation after a partial read
        // cannot discard bytes or corrupt the next frame.
        while self.header_read < self.header.len() {
            let count = self
                .stream
                .read(&mut self.header[self.header_read..])
                .await?;
            if count == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
            }
            self.header_read += count;
        }
        let size = u32::from_be_bytes(self.header) as usize;
        if size == 0 || size > ipc::MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "host watch frame exceeds limit",
            )
            .into());
        }
        if self.body.is_empty() {
            self.body.resize(size, 0);
        }
        while self.body_read < size {
            let count = self.stream.read(&mut self.body[self.body_read..]).await?;
            if count == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
            }
            self.body_read += count;
        }
        let frame: WatchFrame = serde_json::from_slice(&self.body)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        self.header_read = 0;
        self.body_read = 0;
        self.body.clear();
        if let WatchFrame::Journal(record) = &frame {
            if record.sequence <= self.after {
                return Err(Error::HostRequest(
                    "host journal cursor did not advance".into(),
                ));
            }
            self.after = record.sequence;
        }
        Ok(frame)
    }
}

pub(crate) fn state_directory(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("host/v1")
}

fn experimental_context_transitions() -> bool {
    std::env::var("ORVEK_EXPERIMENTAL_CONTEXT_TRANSITIONS").as_deref() == Ok("1")
}

fn configuration_identity(config: &Config) -> Result<Digest> {
    Digest::of_value(&configuration_identity_material(config)?)
        .map_err(|error| Error::HostRequest(error.to_string()))
}

fn configuration_identity_material(config: &Config) -> Result<serde_json::Value> {
    // Serialization redacts configured secrets. File metadata detects rotation
    // without hashing secret bytes into the host identity.
    let revision = match fs::metadata(config.path()) {
        Ok(metadata) => serde_json::json!({
            "bytes": metadata.len(),
            "inode": metadata.ino(),
            "modified": metadata.modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|value| value.as_nanos().to_string()),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => serde_json::Value::Null,
        Err(error) => return Err(error.into()),
    };
    let execution = config.agent().execution();
    let sandbox = execution.is_sandbox();
    // Client build metadata is deliberately excluded. A rebuild does not
    // change host configuration, and protocol compatibility is enforced by
    // the IPC boundary instead.
    let mut material = serde_json::json!({
        "version": 1,
        "config_path": config.path(),
        "file_revision": revision,
        "auth": config.auth(),
        "credential_identity": config.auth().credential_identity()?,
        "model_route_credentials": model_route_credential_identities(config)?,
        "memory": config.memory(),
        "skills": config.skills(),
        "children": config.subagents(),
        "max_children": config.agent().max_subagents(),
        "context_window_tokens": config.agent().context_window_tokens(),
        "completion_hook": config.agent().completion_hook().map(|command| json!({"version":1,"command":command})),
        "trace_recording_version": 1,
        "provider_transport_version": 2,
        "websocket_url": config.agent().websocket_url(),
        "api_base_url": config.agent().api_base_url(),
        "execution": execution,
        // Native identity must not depend on Docker selection; sandbox keeps
        // pinning the executor image and helper explicitly.
        "executor_image": sandbox.then(|| std::env::var("ORVEK_EXECUTOR_IMAGE").ok()).flatten(),
        "executor_helper": sandbox.then(|| std::env::var_os("ORVEK_EXECUTOR_HELPER").map(PathBuf::from)),
    });
    if experimental_context_transitions() {
        material["experimental_context_transitions"] = json!(1);
    }
    Ok(material)
}

/// A configured `websocket_url` opts into the WebSocket transport; every other
/// setup speaks HTTP Responses against `api_base_url`, the way OpenAI-compatible
/// endpoints and local bridges serve it.
fn route_transport(config: &Config) -> Transport {
    match config.agent().websocket_url() {
        Some(_) => Transport::WebSocket,
        None => Transport::Http,
    }
}

/// Attaches `[models.<name>]` overrides so mixed setups route each model to
/// its own endpoint and credential (for example GLM through the local bridge
/// while a direct OpenAI model uses the default route). Explicit model routes
/// fail closed when their configured credential is unavailable.
fn model_route_static_auth(
    model: orvek_harness::inference::Model,
    key_env: &str,
) -> Result<orvek_harness::inference::auth::Auth> {
    let key = crate::app::secret::SecretString::from_environment(key_env)
        .map_err(crate::app::error::AuthError::from)?
        .ok_or_else(|| {
            Error::HostRequest(format!(
                "[models.{}] requires {key_env}, but it is not set",
                model.as_str()
            ))
        })?;
    orvek_harness::inference::auth::Auth::api_key(
        orvek_harness::inference::auth::SecretString::new(key.expose_secret().to_owned()),
    )
    .map_err(|error| Error::HostRequest(error.to_string()))
}

fn model_route_credential_identities(config: &Config) -> Result<Vec<(&str, Digest)>> {
    config
        .model_routes()
        .iter()
        .filter_map(|(model, route)| {
            route
                .api_key_env
                .as_deref()
                .map(|key_env| (*model, key_env))
        })
        .map(|(model, key_env)| {
            let key = crate::app::secret::SecretString::from_environment(key_env)
                .map_err(crate::app::error::AuthError::from)?
                .ok_or_else(|| {
                    Error::HostRequest(format!(
                        "[models.{}] requires {key_env}, but it is not set",
                        model.as_str()
                    ))
                })?;
            Ok((model.as_str(), Digest::of(key.expose_secret().as_bytes())))
        })
        .collect()
}

fn model_routed_provider(provider: ResponsesClient, config: &Config) -> Result<ResponsesClient> {
    let mut provider = provider;
    for (model, route_config) in config.model_routes() {
        let auth = if let Some(key_env) = route_config.api_key_env.as_deref() {
            model_route_static_auth(*model, key_env)?
        } else if let Some(program) = config.auth().command() {
            orvek_harness::inference::auth::Auth::api_key_command(
                program.to_owned(),
                config.auth().refresh_interval(),
                config.auth().command_timeout(),
            )
            .map_err(|error| Error::HostRequest(error.to_string()))?
        } else {
            model_route_static_auth(*model, config.auth().api_key_env())?
        };
        let transport = match route_config.websocket_url.as_deref() {
            Some(_) => Transport::WebSocket,
            None => Transport::Http,
        };
        let route = Route::from_overrides(
            &auth,
            transport,
            route_config.api_base_url.as_deref(),
            route_config.websocket_url.as_deref(),
        )
        .map_err(|error| Error::HostRequest(error.to_string()))?;
        provider = provider.with_model_route(*model, auth, route);
    }
    Ok(provider)
}

fn provider_limits(max_request_bytes: usize) -> ProviderLimits {
    ProviderLimits {
        max_attempts: 1,
        max_request_bytes,
        // An OpenAI-compatible bridge stays silent while the model thinks,
        // so the idle window must cover a full reasoning phase. The total
        // window keeps headroom above the bridge's own upstream cap.
        idle_timeout: Duration::from_secs(600),
        total_timeout: Duration::from_secs(900),
        max_response_bytes: 64 * 1024 * 1024,
        max_event_bytes: 32 * 1024 * 1024,
        ..ProviderLimits::default()
    }
}

pub(crate) async fn serve(config: &Config) -> Result<()> {
    let auth = config.auth().load()?;
    let route = Route::from_overrides(
        &auth,
        route_transport(config),
        config.agent().api_base_url(),
        config.agent().websocket_url(),
    )?;
    let max_request_bytes = context::request_byte_limit(config.agent().context_window_tokens())
        .map_err(|error| Error::HostRequest(error.to_string()))?;
    let provider = ResponsesClient::new(auth, route, provider_limits(max_request_bytes))?;
    let provider = model_routed_provider(provider, config)?;
    // Native mode never touches Docker; sandbox mode keeps the verified
    // isolated workflow and its executor requirements.
    let host = if config.agent().execution().is_sandbox() {
        let image =
            std::env::var("ORVEK_EXECUTOR_IMAGE").unwrap_or_else(|_| "debian:bookworm-slim".into());
        let executor = DockerExecutor::connect(&image)
            .await
            .map_err(|error| Error::HostRequest(error.to_string()))?;
        Host::open_with_identity(
            &state_directory(config.path()),
            provider,
            executor,
            configuration_identity(config)?,
        )?
    } else {
        Host::open_native(
            &state_directory(config.path()),
            provider,
            configuration_identity(config)?,
        )?
    };
    let host = host.with_experimental_context_transitions(experimental_context_transitions());
    let host = if config.memory().enabled() || config.skills().enabled() {
        host.with_context_service(Arc::new(crate::core::context::ConfiguredContext::new(
            config,
        )))
    } else {
        host
    };
    let host =
        Arc::new(host.with_completion_hook(config.agent().completion_hook().map(str::to_owned)));
    host.set_subagent_policy(
        config.subagents().enabled(),
        config.subagents().allow_luna(),
        config.agent().max_subagents(),
    );
    let stop = CancellationToken::new();
    let signal_stop = stop.clone();
    let signal = tokio::spawn(async move {
        let _ = shutdown::signal().await;
        signal_stop.cancel();
    });
    let result = ipc::serve(host, stop).await;
    signal.abort();
    result.map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use orvek_harness::session::JournalRecord;
    use tokio::{io::AsyncWriteExt, net::UnixListener};

    fn listener(root: &Path) -> UnixListener {
        let socket = root.join("host.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(socket, fs::Permissions::from_mode(0o600)).unwrap();
        listener
    }

    fn fixture_config(root: &Path, contents: &str) -> Config {
        let command = root.join("fixture-auth");
        fs::write(&command, "#!/bin/sh\nprintf fixture-provider-token\n").unwrap();
        fs::set_permissions(&command, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("config.toml");
        fs::write(
            &path,
            format!("[auth]\nmode = \"api-key\"\ncommand = {command:?}\n{contents}"),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        Config::load_isolated(crate::app::config::ConfigOverrides {
            path: Some(path),
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn detached_host_accepts_bounded_high_reasoning_provider_streams() {
        let limits = provider_limits(2 * 1024 * 1024);
        assert_eq!(limits.max_response_bytes, 64 * 1024 * 1024);
        assert_eq!(limits.max_event_bytes, 32 * 1024 * 1024);
    }

    #[test]
    fn websocket_transport_requires_a_configured_url_and_http_is_the_default() {
        let directory = tempfile::tempdir().unwrap();
        let write = |contents: &str| {
            let path = directory.path().join("config.toml");
            fs::write(&path, contents).unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            crate::app::config::Config::load(crate::app::config::ConfigOverrides {
                path: Some(path),
                ..crate::app::config::ConfigOverrides::default()
            })
            .unwrap()
        };

        let config =
            write("[agent]\nmodel = \"glm-5.3\"\napi_base_url = \"http://127.0.0.1:11436/v1\"\n");
        assert_eq!(route_transport(&config), Transport::Http);

        let config = write(
            "[agent]\nmodel = \"glm-5.3\"\nwebsocket_url = \"wss://127.0.0.1:1/responses\"\n",
        );
        assert_eq!(route_transport(&config), Transport::WebSocket);
    }

    #[test]
    fn configured_model_route_fails_closed_when_its_key_is_missing() {
        let directory = tempfile::tempdir().unwrap();
        let config = fixture_config(
            directory.path(),
            r#"[models.spark]
api_base_url = "http://127.0.0.1:11436/v1"
api_key_env = "ORVEK_TEST_DEFINITELY_MISSING_ROUTE_KEY"
"#,
        );

        let error = configuration_identity_material(&config).unwrap_err();
        assert!(
            error.to_string().contains(
                "[models.gpt-5.3-codex-spark] requires ORVEK_TEST_DEFINITELY_MISSING_ROUTE_KEY"
            ),
            "{error}"
        );
    }

    #[test]
    fn trace_recording_has_a_versioned_host_identity() {
        let directory = tempfile::tempdir().unwrap();
        let config = fixture_config(directory.path(), "[agent]\n");
        let material = configuration_identity_material(&config).unwrap();
        assert_eq!(
            material["trace_recording_version"], 1,
            "an idle pre-trace host must not match the new runtime identity"
        );
        assert_eq!(
            material["provider_transport_version"], 2,
            "an idle host with the smaller stream bounds must be restarted"
        );
    }

    #[test]
    fn configured_completion_hook_has_a_versioned_delivery_identity() {
        let directory = tempfile::tempdir().unwrap();
        let config = fixture_config(
            directory.path(),
            "[agent]\ncompletion_hook = \"notify-local\"\n",
        );
        assert_eq!(
            configuration_identity_material(&config).unwrap()["completion_hook"],
            json!({"version":1,"command":"notify-local"})
        );
    }

    #[test]
    fn client_build_metadata_is_not_host_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let config = fixture_config(directory.path(), "[agent]\nmodel = \"glm-5.3\"\n");

        let material = configuration_identity_material(&config).unwrap();

        assert!(material.get("application_build").is_none());
        assert!(!material.to_string().contains(env!("ORVEK_BUILD_TIMESTAMP")));
    }

    #[tokio::test]
    async fn operator_call_uses_native_frames_and_same_user_socket() {
        let root = tempfile::tempdir().unwrap();
        let listener = listener(root.path());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: Request = ipc::read_frame(&mut stream).await.unwrap();
            assert!(matches!(
                request.command,
                Command::Sessions {
                    offset: 0,
                    limit: 1
                }
            ));
            ipc::write_frame(&mut stream, &Response::Sessions(Vec::new()))
                .await
                .unwrap();
        });
        let client = HostClient::fixture(root.path());

        client.probe().await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn incompatible_protocol_host_is_shutdown_with_the_previous_version() {
        let root = tempfile::tempdir().unwrap();
        let listener = listener(root.path());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let current: Request = ipc::read_frame(&mut stream).await.unwrap();
            assert_eq!(current.version, ipc::PROTOCOL_VERSION);
            assert!(matches!(current.command, Command::ShutdownIfIdle));
            ipc::write_frame(
                &mut stream,
                &Response::Error {
                    message: ipc::UNSUPPORTED_PROTOCOL_VERSION.into(),
                },
            )
            .await
            .unwrap();

            let (mut stream, _) = listener.accept().await.unwrap();
            let previous: Request = ipc::read_frame(&mut stream).await.unwrap();
            assert_eq!(previous.version, 2);
            assert!(matches!(previous.command, Command::ShutdownIfIdle));
            ipc::write_frame(&mut stream, &Response::Shutdown { accepted: true })
                .await
                .unwrap();
        });
        let client = HostClient::fixture(root.path());

        assert!(client.shutdown_incompatible_host().await.unwrap());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn publicly_accessible_socket_is_rejected_before_sending_commands() {
        let root = tempfile::tempdir().unwrap();
        let _listener = listener(root.path());
        fs::set_permissions(
            root.path().join("host.sock"),
            fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        let client = HostClient::fixture(root.path());

        assert!(matches!(client.probe().await, Err(Error::HostRequest(_))));
    }

    #[tokio::test]
    async fn watch_retains_partial_frame_bytes_across_cancellation() {
        let (stream, mut server) = UnixStream::pair().unwrap();
        let frame = WatchFrame::Journal(JournalRecord {
            sequence: 7,
            aggregate: "session".into(),
            kind: "session".into(),
            revision: 1,
            event: serde_json::json!({"type":"test"}),
        });
        let body = serde_json::to_vec(&frame).unwrap();
        let mut bytes = (body.len() as u32).to_be_bytes().to_vec();
        bytes.extend(body);
        let split = 9;
        server.write_all(&bytes[..split]).await.unwrap();
        let mut watch = HostWatch {
            stream,
            after: 0,
            header: [0; 4],
            header_read: 0,
            body: Vec::new(),
            body_read: 0,
        };

        assert!(
            tokio::time::timeout(Duration::from_millis(10), watch.next())
                .await
                .is_err()
        );
        server.write_all(&bytes[split..]).await.unwrap();

        assert!(matches!(
            watch.next().await.unwrap(),
            WatchFrame::Journal(_)
        ));
        assert_eq!(watch.cursor(), 7);
    }
}

#[cfg(test)]
#[path = "host_context_tests.rs"]
mod context_tests;
