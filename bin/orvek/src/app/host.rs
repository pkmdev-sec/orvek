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

    pub(crate) async fn subscribe(&self, after: u64) -> Result<HostWatch> {
        let mut stream = self.open().await?;
        timeout(
            CONNECT_TIMEOUT,
            ipc::write_frame(&mut stream, &Request::new(Command::Watch { after })),
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
            .arg("--web-search")
            .arg(config.agent().web_search().to_string())
            .arg("--image-generation")
            .arg(config.agent().image_generation().to_string())
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

fn configuration_identity(config: &Config) -> Result<Digest> {
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
    let configuration = serde_json::json!({
        "version": 1,
        "application_build": env!("ORVEK_BUILD_TIMESTAMP"),
        "config_path": config.path(),
        "file_revision": revision,
        "auth": config.auth(),
        "credential_identity": config.auth().credential_identity()?,
        "mcp": config.mcp_servers(),
        "memory": config.memory(),
        "children": config.subagents(),
        "max_children": config.agent().max_subagents(),
        "context_window_tokens": config.agent().context_window_tokens(),
        "web_search": config.agent().web_search(),
        "image_generation": config.agent().image_generation(),
        "completion_hook": config.agent().completion_hook(),
        "websocket_url": config.agent().websocket_url(),
        "api_base_url": config.agent().api_base_url(),
        "executor_image": std::env::var("ORVEK_EXECUTOR_IMAGE").ok(),
        "executor_helper": std::env::var_os("ORVEK_EXECUTOR_HELPER").map(PathBuf::from),
    });
    Digest::of_value(&configuration).map_err(|error| Error::HostRequest(error.to_string()))
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
/// while a direct OpenAI model uses the default route). A route whose key
/// environment variable is unset is skipped with a warning so the host still
/// starts for the models that are configured.
fn model_routed_provider(provider: ResponsesClient, config: &Config) -> Result<ResponsesClient> {
    let mut provider = provider;
    for (model, route_config) in config.model_routes() {
        let key_env = route_config
            .api_key_env
            .as_deref()
            .unwrap_or(config.auth().api_key_env());
        let Ok(Some(key)) = crate::app::secret::SecretString::from_environment(key_env) else {
            eprintln!(
                "orvek: skipping [models.{}] route; {} is not set",
                model.as_str(),
                key_env
            );
            continue;
        };
        let auth = orvek_harness::inference::auth::Auth::api_key(
            orvek_harness::inference::auth::SecretString::new(key.expose_secret().to_owned()),
        )
        .map_err(|error| Error::HostRequest(error.to_string()))?;
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
    let provider = ResponsesClient::new(
        auth,
        route,
        ProviderLimits {
            max_attempts: 1,
            max_request_bytes,
            // An OpenAI-compatible bridge stays silent while the model thinks,
            // so the idle window must cover a full reasoning phase. The total
            // window keeps headroom above the bridge's own upstream cap.
            idle_timeout: Duration::from_secs(600),
            total_timeout: Duration::from_secs(900),
            ..ProviderLimits::default()
        },
    )?;
    let provider = model_routed_provider(provider, config)?;
    let image =
        std::env::var("ORVEK_EXECUTOR_IMAGE").unwrap_or_else(|_| "debian:bookworm-slim".into());
    let executor = DockerExecutor::connect(&image)
        .await
        .map_err(|error| Error::HostRequest(error.to_string()))?;
    let host = Arc::new(Host::open_with_identity(
        &state_directory(config.path()),
        provider,
        executor,
        configuration_identity(config)?,
    )?);
    host.set_subagent_policy(config.subagents().enabled(), config.agent().max_subagents());
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
