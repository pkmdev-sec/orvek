//! Typed errors exposed by the binary's internal module boundaries.

use crate::tui::session::SessionError;
use miette::Diagnostic;
use orvek_harness::{
    controller::HostError,
    inference::{FailureKind, auth::AuthError as ProviderAuthError},
};
use orvek_memory::{MemoryError, RemoteClientError};
use std::{
    env::VarError, error::Error as StdError, io, path::PathBuf, result::Result as StdResult,
};
use thiserror::Error;

pub(crate) type Result<T> = StdResult<T, Error>;
pub(crate) type AuthResult<T> = StdResult<T, AuthError>;

#[derive(Debug, Diagnostic, Error)]
pub(crate) enum Error {
    #[error("auxiliary request cancelled")]
    AuxiliaryCancelled,
    #[error("task ended with {outcome:?}")]
    TaskOutcome {
        outcome: orvek_harness::state::Outcome,
    },
    #[error(transparent)]
    Host(#[from] HostError),
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("host connection: {0}")]
    Connection(#[from] io::Error),
    #[error(transparent)]
    ExternalEditor(#[from] ExternalEditorError),
    #[error("host request: {0}")]
    HostRequest(String),
    #[error(transparent)]
    Inference(#[from] FailureKind),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    MemoryTransfer(#[from] MemoryTransferError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("update failed: {0}")]
    Update(#[source] Box<dyn StdError + Send + Sync>),
}

#[derive(Debug, Error)]
pub(crate) enum MemoryTransferError {
    #[error("memory archive transfer failed: {0}")]
    Archive(#[source] MemoryError),
    #[error("remote memory is not configured")]
    RemoteNotConfigured,
    #[error("failed to read the local memory snapshot for push: {0}")]
    Local(#[source] MemoryError),
    #[error(
        "local memories kept changing while the remote snapshot was synchronized; retry once writes settle"
    )]
    LocalChanged,
    #[error("memory push failed: {0}")]
    Push(#[source] RemoteClientError),
    #[error("remote memory rejected the push: {0}")]
    PushStore(#[source] MemoryError),
    #[error("memory pull failed: {0}")]
    Pull(#[source] RemoteClientError),
    #[error("remote memory rejected the pull: {0}")]
    PullStore(#[source] MemoryError),
    #[error("failed to merge pulled memories into the local store: {0}")]
    Merge(#[source] MemoryError),
}

impl Error {
    pub(crate) fn update(source: impl StdError + Send + Sync + 'static) -> Self {
        Self::Update(Box::new(source))
    }
}

#[derive(Debug, Error)]
pub(crate) enum ExternalEditorError {
    #[error("$EDITOR is unavailable: {0}")]
    Unavailable(#[source] VarError),
    #[error("failed to parse $EDITOR value `{command}`")]
    Parse { command: String },
    #[error("failed to create an external-editor draft: {0}")]
    CreateDraft(#[source] io::Error),
    #[error("failed to write the external-editor draft: {0}")]
    WriteDraft(#[source] io::Error),
    #[error("failed to launch external editor `{program}`: {source}")]
    Launch {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("failed to read the external-editor draft: {0}")]
    ReadDraft(#[source] io::Error),
}

#[derive(Debug, Error)]
pub(crate) enum AuthError {
    #[error(transparent)]
    ChatGpt(#[from] ProviderAuthError),
    #[error("failed to inspect ChatGPT credential file {path}: {source}")]
    InspectCredentialFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("OPENAI_API_KEY is not set; set it or select ChatGPT authentication")]
    ApiKeyUnavailable,
    #[error(
        "no ChatGPT credentials found at {path} and OPENAI_API_KEY is not set; run `orvek auth login` or set OPENAI_API_KEY"
    )]
    CredentialsUnavailable { path: PathBuf },
    #[error(transparent)]
    Secret(#[from] SecretError),
}

#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    #[error("could not determine the config directory; set ORVEK_HOME or pass --config")]
    ConfigHomeUnavailable,
    #[error("could not determine the credential directory; set CODEX_HOME or pass --auth-file")]
    AuthHomeUnavailable,
    #[error("failed to determine the current directory: {0}")]
    CurrentDirectory(#[source] io::Error),
    #[error("failed to read configuration file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse configuration file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[cfg(unix)]
    #[error(
        "configuration file {path} contains inline secrets but has insecure permissions {mode:#o}; remove all group and other permissions"
    )]
    InsecureSecretPermissions { path: PathBuf, mode: u32 },
    #[cfg(not(unix))]
    #[error(
        "configuration file {path} contains inline secrets, but this platform's file privacy cannot be verified"
    )]
    UnsupportedSecretPermissions { path: PathBuf },
    #[error("failed to serialize the effective configuration: {0}")]
    Serialize(#[source] toml::ser::Error),
    #[error("agent context window must be between 16384 and 1000000 tokens, got {0}")]
    ContextWindowTokens(u64),
    #[error("maximum concurrent subagents must be between 1 and 32, got {0}")]
    MaxSubagents(usize),
    #[error("authentication command timeout must be greater than zero")]
    AuthCommandTimeout,
    #[error("unknown model `{0}` in the [models] table")]
    UnknownModel(String),
    #[error("model `{0}` needs api_base_url or websocket_url in the [models] table")]
    ModelRouteWithoutEndpoint(String),
    #[error(
        "legacy compaction strategy `{0}` is no longer supported; use `provider` context projection"
    )]
    UnsupportedCompactionStrategy(String),
    #[error(transparent)]
    RemoteMemory(#[from] RemoteMemoryConfigError),
    #[error("failed to update configuration file {path}: {source}")]
    UpdateParse {
        path: PathBuf,
        #[source]
        source: toml_edit::TomlError,
    },
    #[error("failed to write configuration file {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error)]
pub(crate) enum EndpointUrlError {
    #[error("the URL must not be empty or whitespace-only")]
    Empty,
    #[error("the URL is not valid")]
    Parse(#[source] url::ParseError),
    #[error("the URL must use the http or https scheme")]
    UnsupportedScheme,
    #[error("non-loopback URLs must use https")]
    InsecureTransport,
    #[error("the URL must not contain credentials")]
    Credentials,
}

#[derive(Debug, Error)]
pub(crate) enum RemoteMemoryConfigError {
    #[error(
        "remote memory configuration requires an endpoint, namespace, bearer token, and at least one workspace root"
    )]
    Incomplete,
    #[error("remote memory endpoint is invalid: {0}")]
    Endpoint(#[source] EndpointUrlError),
    #[error("remote memory namespace must be non-empty and have no leading or trailing whitespace")]
    NamespaceWhitespace,
    #[error("remote memory namespace must not contain control characters")]
    NamespaceControl,
    #[error(
        "remote memory namespace must use at most 128 ASCII letters, digits, periods, hyphens, or underscores"
    )]
    NamespaceInvalid,
    #[error("failed to resolve remote memory workspace root {path}: {source}")]
    ResolveWorkspaceRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("remote memory workspace root is not a directory: {0}")]
    WorkspaceRootNotDirectory(PathBuf),
}

#[derive(Debug, Error)]
pub(crate) enum RuntimeError {
    #[error(
        "interactive mode requires terminal stdin and stdout; use `orvek run <PROMPT>` for JSONL output"
    )]
    InteractiveTerminal,
    #[error("--resume can be combined only with `run` or interactive mode")]
    ResumeWithCommand,
    #[error("terminal operation failed: {0}")]
    Terminal(#[source] io::Error),
    #[error("failed to configure remote memory: {0}")]
    RemoteMemory(#[source] RemoteClientError),
    #[error("invalid session ID: {0}")]
    InvalidSessionId(#[source] uuid::Error),
    #[error("failed to resolve workspace {path}: {source}")]
    ResolveWorkspace {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("workspace is not a directory: {0}")]
    WorkspaceNotDirectory(PathBuf),
    #[error("failed to listen for a shutdown signal: {0}")]
    ShutdownSignal(#[source] io::Error),
}

#[derive(Debug, Error)]
#[error("{name} is not valid Unicode")]
pub(crate) struct SecretError {
    pub(crate) name: String,
}
