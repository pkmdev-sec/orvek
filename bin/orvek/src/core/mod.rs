//! Session assembly for the durable host. No provider or execution loop lives here.

pub(crate) mod extensions;

use crate::app::{
    config::{Config, ReasoningEffort, ReasoningMode},
    error::{ConfigError, Error, Result, RuntimeError},
    host::HostClient,
};
use extensions::{Skill, SkillCatalog};
use orvek_harness::{
    Channel,
    inference::{Model, ModelSettings},
    ipc::{Command, Response, SessionView},
    session::{SessionAdmissionRequest, SessionId},
};
use orvek_memory::{RemoteMemoryClient, RemoteToken, SelectedMemoryStore};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) struct ConfiguredSession {
    pub(crate) client: HostClient,
    pub(crate) session: SessionView,
    pub(crate) skills: Arc<[Skill]>,
    pub(crate) memory_enabled: bool,
}

impl ConfiguredSession {
    pub(crate) async fn create(
        config: &Config,
        thinking: ReasoningEffort,
        reasoning_mode: ReasoningMode,
        model: Model,
    ) -> Result<Self> {
        let workspace = resolve_workspace(config.agent().workspace())?;
        let model = ModelSettings {
            model,
            thinking: thinking.into(),
            reasoning_mode: reasoning_mode.into(),
            fast_mode: config.agent().fast_mode(),
        };
        Self::admit(
            config,
            workspace,
            model,
            config.agent().context_window_tokens(),
        )
        .await
    }

    pub(crate) async fn create_successor(
        config: &Config,
        workspace: &Path,
        model: ModelSettings,
        context_window_tokens: u64,
    ) -> Result<Self> {
        let workspace = resolve_workspace(workspace)?;
        Self::admit(config, workspace, model, context_window_tokens).await
    }

    async fn admit(
        config: &Config,
        workspace: PathBuf,
        model: ModelSettings,
        context_window_tokens: u64,
    ) -> Result<Self> {
        // Fail fast on remote-memory misconfiguration (bad workspace roots, unresolvable
        // paths) at session start, rather than only when the user later runs `orvek memory`.
        configured_memory_store(config, &workspace)?;
        let catalog = SkillCatalog::load(config.skills());
        let skills = catalog
            .rendered_instructions()
            .map(SkillCatalog::available_in)
            .unwrap_or_default()
            .into();
        let client = HostClient::connect(config).await?;
        let response = client
            .query(Command::CreateSession {
                id: SessionId::new(),
                request: SessionAdmissionRequest::new(
                    workspace,
                    model,
                    context_window_tokens,
                    Channel::Stable,
                ),
            })
            .await?;
        let Response::Session(session) = response else {
            return Err(Error::HostRequest(
                "host did not return the created session".into(),
            ));
        };
        Ok(Self {
            client,
            session: *session,
            skills,
            memory_enabled: config.memory().enabled(),
        })
    }

    pub(crate) async fn resume_label(config: &Config, label: &str) -> Result<Self> {
        let client = HostClient::connect(config).await?;
        let selected = crate::tui::session::legacy_id(label)?;
        if selected.is_none() {
            let id = label.parse().map_err(RuntimeError::InvalidSessionId)?;
            match client.query(Command::Session { id }).await {
                Ok(Response::Session(view)) => return Ok(Self::from_view(config, client, *view)),
                Ok(_) => {
                    return Err(Error::HostRequest(
                        "unexpected session lookup response".into(),
                    ));
                }
                Err(error) => {
                    let entries =
                        crate::tui::session::legacy_sessions(&client, config.path()).await;
                    if let Ok(entries) = entries
                        && let Some(metadata) =
                            entries.into_iter().find(|entry| entry.session_id == label)
                    {
                        return Self::import_legacy(config, client, metadata).await;
                    }
                    return Err(error);
                }
            }
        }
        let selected = selected.expect("historical label was selected");
        let metadata = crate::tui::session::legacy_sessions(&client, config.path())
            .await?
            .into_iter()
            .find(|entry| entry.session_id == selected)
            .ok_or_else(|| {
                Error::HostRequest("historical session was not found in the bounded catalog".into())
            })?;
        Self::import_legacy(config, client, metadata).await
    }

    async fn import_legacy(
        config: &Config,
        client: HostClient,
        metadata: orvek_harness::import::LegacySessionMetadata,
    ) -> Result<Self> {
        let model = metadata.model.parse::<Model>().map_err(|_| {
            Error::HostRequest(format!(
                "historical model `{}` is unsupported; no replacement model was selected",
                metadata.model
            ))
        })?;
        let thinking = metadata
            .effort
            .value
            .as_ref()
            .and_then(|value| serde_json::from_value(serde_json::Value::String(value.clone())).ok())
            .ok_or_else(|| {
                Error::HostRequest("historical thinking setting is unsupported".into())
            })?;
        let reasoning_mode = metadata
            .reasoning_mode
            .value
            .as_ref()
            .and_then(|value| serde_json::from_value(serde_json::Value::String(value.clone())).ok())
            .ok_or_else(|| Error::HostRequest("historical reasoning mode is unsupported".into()))?;
        let request = orvek_harness::ipc::Request::new(Command::ImportLegacy {
            database: crate::tui::session::legacy_database(config.path()),
            source_session: metadata.session_id,
            request: SessionAdmissionRequest::new(
                metadata.workspace.into(),
                ModelSettings {
                    model,
                    thinking,
                    reasoning_mode,
                    fast_mode: metadata.fast_mode,
                },
                config.agent().context_window_tokens(),
                Channel::Stable,
            ),
        });
        let mut last = None;
        for _ in 0..3 {
            match client
                .call(&request, std::time::Duration::from_secs(70))
                .await
            {
                Ok(Response::Session(view)) => return Ok(Self::from_view(config, client, *view)),
                Ok(_) => {
                    return Err(Error::HostRequest(
                        "unexpected historical import response".into(),
                    ));
                }
                Err(error @ Error::HostRequest(_)) => return Err(error),
                Err(error) => last = Some(error),
            }
        }
        Err(Error::HostRequest(format!(
            "historical import operation {} may have committed; retry this operation before importing again: {}",
            request.id,
            last.map(|error| error.to_string()).unwrap_or_default()
        )))
    }

    pub(crate) async fn resume(config: &Config, id: SessionId) -> Result<Self> {
        let client = HostClient::connect(config).await?;
        let Response::Session(session) = client.query(Command::Session { id }).await? else {
            return Err(Error::HostRequest(
                "host did not return the requested session".into(),
            ));
        };
        let catalog = SkillCatalog::load(config.skills());
        let skills = catalog
            .rendered_instructions()
            .map(SkillCatalog::available_in)
            .unwrap_or_default()
            .into();
        Ok(Self {
            client,
            session: *session,
            skills,
            memory_enabled: config.memory().enabled(),
        })
    }

    pub(crate) fn from_view(config: &Config, client: HostClient, session: SessionView) -> Self {
        let catalog = SkillCatalog::load(config.skills());
        let skills = catalog
            .rendered_instructions()
            .map(SkillCatalog::available_in)
            .unwrap_or_default()
            .into();
        Self {
            client,
            session,
            skills,
            memory_enabled: config.memory().enabled(),
        }
    }
}

pub(crate) fn resolve_workspace(path: &Path) -> Result<PathBuf> {
    let workspace = path
        .canonicalize()
        .map_err(|source| RuntimeError::ResolveWorkspace {
            path: path.to_owned(),
            source,
        })?;
    if !workspace.is_dir() {
        return Err(RuntimeError::WorkspaceNotDirectory(workspace).into());
    }
    Ok(workspace)
}

pub(crate) fn configured_memory_store(
    config: &Config,
    workspace: &Path,
) -> Result<Option<SelectedMemoryStore>> {
    if !config.memory().enabled() {
        return Ok(None);
    }
    let store = SelectedMemoryStore::local(config.memory_path());
    let Some(remote) = config.memory().remote() else {
        return Ok(Some(store));
    };
    let canonical_workspace =
        workspace
            .canonicalize()
            .map_err(|source| RuntimeError::ResolveWorkspace {
                path: workspace.to_path_buf(),
                source,
            })?;
    if !remote
        .matches_workspace(&canonical_workspace)
        .map_err(ConfigError::from)?
    {
        return Ok(Some(store));
    }
    let token =
        RemoteToken::new(remote.bearer_token().to_owned()).map_err(RuntimeError::RemoteMemory)?;
    let client = RemoteMemoryClient::new(remote.endpoint(), remote.namespace().to_owned(), token)
        .map_err(RuntimeError::RemoteMemory)?;
    Ok(Some(SelectedMemoryStore::remote(client)))
}
