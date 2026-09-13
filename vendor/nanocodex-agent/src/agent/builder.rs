use super::*;
#[cfg(not(target_family = "wasm"))]
use crate::rollout::RolloutConfig;

/// Builder for one owned agent lifecycle.
#[derive(Clone)]
pub struct NanocodexBuilder<F = StandardServiceFactory> {
    pub(super) config: ModelConfig,
    pub(super) tools: ToolsConfiguration,
    pub(super) workspace: Option<PathBuf>,
    pub(super) session_id: Option<SessionId>,
    pub(super) prompt_cache: PromptCacheConfig,
    pub(super) codex: CodexCompatibility,
    pub(super) resume: Option<SessionSnapshot>,
    pub(super) factory: F,
    pub(super) context_backend: Option<Arc<dyn crate::session::compaction::ContextBackend>>,
    pub(super) context_upgrade: Option<ContextContract>,
}

#[derive(Clone)]
pub(crate) struct ContextContract {
    pub(crate) tools: Tools,
    pub(crate) instructions: Arc<str>,
}

#[derive(Clone, Default)]
pub(super) struct PromptCacheConfig {
    pub(super) key: Option<String>,
    pub(super) shared: Option<SharedPromptCache>,
}

#[derive(Clone, Default)]
pub(super) struct CodexCompatibility {
    pub(super) context: ContextSourceConfig,
    pub(super) durability: DurabilityConfig,
}

impl<F> NanocodexBuilder<F> {
    /// Enables application-owned archival and local tool-result compaction.
    /// Spawned and forked agents share the backend but receive independent archive branches.
    #[must_use]
    pub fn context_backend(
        mut self,
        backend: Arc<dyn crate::session::compaction::ContextBackend>,
    ) -> Self {
        self.config.context_request_limit = Some(backend.policy().request_bytes);
        self.context_backend = Some(backend);
        self
    }
    /// Overrides the `OpenAi` recipe's model for this agent.
    ///
    /// Without this call the agent inherits the client default. The selected
    /// model is fixed for the lifetime of the agent thread.
    #[must_use]
    pub const fn model(mut self, model: Model) -> Self {
        self.config.model = model;
        self
    }

    /// Replaces the stable system/developer instructions.
    #[must_use]
    pub fn instructions(mut self, instructions: impl Into<Arc<str>>) -> Self {
        self.config.system_prompt = instructions.into();
        self
    }

    /// Overrides the `OpenAi` recipe's model thinking level for this agent.
    ///
    /// Without this call the agent inherits the client default. A later
    /// [`Nanocodex::set_thinking`] call affects subsequently accepted turns.
    #[must_use]
    pub const fn thinking(mut self, thinking: Thinking) -> Self {
        self.config.thinking = thinking;
        self
    }

    /// Overrides the `OpenAi` recipe's priority-processing policy for this
    /// agent.
    ///
    /// Without this call the agent inherits the client default. A later
    /// [`Nanocodex::set_fast_mode`] call affects subsequently accepted turns.
    #[must_use]
    pub const fn fast_mode(mut self, enabled: bool) -> Self {
        self.config.fast_mode = enabled;
        self
    }

    /// Overrides the `OpenAi` recipe's Responses reasoning execution mode for
    /// this agent.
    ///
    /// Without this call the agent inherits the client default.
    #[must_use]
    pub const fn reasoning_mode(mut self, reasoning_mode: ReasoningMode) -> Self {
        self.config.reasoning_mode = reasoning_mode;
        self
    }

    /// Replaces the standard built-in tool selection.
    #[must_use]
    pub fn tools(mut self, tools: Tools) -> Self {
        self.tools = ToolsConfiguration::Shared(tools);
        self
    }

    /// Builds a fresh tool collection for every agent driver.
    ///
    /// The factory receives a weak capability targeting the driver whose tool
    /// runtime is being built. Use this for agent-relative tools such as Code
    /// Mode child-agent tools; stateless tools may continue using
    /// [`Self::tools`].
    #[cfg(not(target_family = "wasm"))]
    #[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
    #[must_use]
    pub fn tools_factory<T>(mut self, factory: T) -> Self
    where
        T: Fn(AgentHandle) -> std::result::Result<Tools, ToolsBuildError> + Send + Sync + 'static,
    {
        self.tools = ToolsConfiguration::PerAgent(Arc::new(factory));
        self
    }

    /// Fixes the workspace used by every prompt in this agent session.
    #[must_use]
    pub fn workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    /// Describes the remote environment where model-visible tools execute.
    ///
    /// This replaces host date/time discovery and host `AGENTS.md` discovery
    /// together, so one agent never mixes context from two machines. The
    /// snapshot remains fixed for this agent lifecycle.
    #[must_use]
    pub fn execution_environment(mut self, environment: ExecutionEnvironment) -> Self {
        self.codex.context.set_execution_environment(environment);
        self
    }

    /// Sets the root agent's `UUIDv7` session identity.
    ///
    /// The root identity also seeds its checkpoint lineage. Spawned siblings
    /// and forks receive fresh session IDs; forks retain the root's opaque
    /// lineage so [`Nanocodex::fork_from`] can reject unrelated results.
    #[must_use]
    pub const fn session_id(mut self, session_id: SessionId) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// Sets a stable cache identity for the immutable request prefix.
    ///
    /// Independent root agents may share this key without sharing their
    /// session, conversation, response chain, tools, or workspace. When
    /// omitted, each independently built root uses its own session lineage.
    /// Clean children and forks inherit their root's cache identity.
    #[must_use]
    pub fn prompt_cache_key(mut self, prompt_cache_key: impl Into<String>) -> Self {
        self.prompt_cache.key = Some(prompt_cache_key.into());
        self
    }

    /// Shares completed immutable-prefix warmups among builders cloned from
    /// this recipe.
    ///
    /// The first agent primes the provider cache. Other agents skip the
    /// redundant warmup and send their first complete generation with the same
    /// prefix cache key. Every clean agent still owns an independent session,
    /// conversation, response chain, service stack, tool runtime, event stream,
    /// and workspace. Entries are fingerprinted from the exact prefix and key.
    #[must_use]
    pub fn shared_prompt_cache(mut self) -> Self {
        self.prompt_cache.shared = Some(SharedPromptCache::default());
        self
    }

    /// Loads global user instructions from `AGENTS.override.md` or `AGENTS.md`
    /// in the supplied Codex state directory.
    #[cfg(not(target_family = "wasm"))]
    #[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
    #[must_use]
    pub fn codex_home(mut self, codex_home: impl Into<PathBuf>) -> Self {
        self.codex.context.set_codex_home(codex_home.into());
        self
    }

    /// Records committed history in Codex's resumable JSONL rollout layout.
    #[cfg(not(target_family = "wasm"))]
    #[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
    #[must_use]
    pub fn rollout(mut self, rollout: RolloutConfig) -> Self {
        if self.codex.context.codex_home().is_none() {
            self.codex
                .context
                .set_codex_home(rollout.codex_home().to_path_buf());
        }
        self.codex.durability.set_rollout(rollout);
        self
    }

    /// Restores a completed session boundary into a fresh driver, WebSocket,
    /// and tool runtime while retaining its typed history and cache lineage.
    ///
    /// An explicitly configured session ID names the new runtime/event stream;
    /// it does not replace the snapshot's prompt-cache lineage. Configure the
    /// same instructions, tool definitions, and custom handlers used by the
    /// original session; incompatible policy is rejected during [`Self::build`].
    #[must_use]
    pub fn resume(mut self, snapshot: SessionSnapshot) -> Self {
        self.resume = Some(snapshot);
        self
    }

    /// Explicitly upgrades a legacy context contract after verifying the original
    /// instructions and tool definitions against its saved request prefix. The new
    /// builder contract is installed only after that verification succeeds.
    #[must_use]
    pub fn resume_with_context_upgrade(
        mut self,
        snapshot: SessionSnapshot,
        previous_tools: Tools,
        previous_instructions: impl Into<Arc<str>>,
    ) -> Self {
        self.context_upgrade = Some(ContextContract {
            tools: previous_tools,
            instructions: previous_instructions.into(),
        });
        self.resume = Some(snapshot);
        self
    }
}

#[cfg(not(target_family = "wasm"))]
impl<F> NanocodexBuilder<F>
where
    F: ResponsesServiceFactory + Send + Sync + 'static,
    F::Service: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    <F::Service as Service<ResponsesAttempt>>::Error: Into<ResponseError> + Send + 'static,
    <F::Service as Service<ResponsesAttempt>>::Future: Send,
{
    /// Builds an agent from the configured [`OpenAi`] client recipe.
    ///
    /// Each root, spawned sibling, and fork receives a fresh concrete Tower
    /// service, tool runtime, event stream, and mutable conversation state.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid agent policy or, on native targets, when
    /// no Tokio runtime is active.
    pub fn build(self) -> Result<(Nanocodex, AgentEvents)> {
        build(self)
    }
}

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
impl<F> NanocodexBuilder<F>
where
    F: ResponsesServiceFactory + 'static,
    F::Service: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + 'static,
    <F::Service as Service<ResponsesAttempt>>::Error: Into<ResponseError> + 'static,
{
    /// Builds an agent from the configured [`OpenAi`] client recipe.
    ///
    /// Each root, spawned sibling, and fork receives a fresh concrete Tower
    /// service, tool runtime, event stream, and mutable conversation state.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid agent policy.
    pub fn build(self) -> Result<(Nanocodex, AgentEvents)> {
        build(self)
    }
}

fn build<F>(builder: NanocodexBuilder<F>) -> Result<(Nanocodex, AgentEvents)>
where
    F: ResponsesServiceFactory + AgentFactory + 'static,
    F::Service:
        Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    <F::Service as Service<ResponsesAttempt>>::Error: Into<ResponseError> + AgentSend + 'static,
    <F::Service as Service<ResponsesAttempt>>::Future: AgentSend,
{
    validate(&builder.config, builder.prompt_cache.key.as_deref())?;
    validate_execution_environment(builder.codex.context.execution_environment())?;
    let config = Arc::new(builder.config);
    let factory = builder.factory;
    let service_factory: ServiceFactory<F::Service> = Arc::new(move |config| factory.make(config));
    build_agent(
        config,
        builder.tools,
        builder.workspace,
        builder.session_id,
        builder.prompt_cache,
        builder.codex,
        builder.resume,
        builder.context_backend,
        builder.context_upgrade,
        service_factory,
    )
}

fn validate_execution_environment(environment: Option<&ExecutionEnvironment>) -> Result<()> {
    let Some(environment) = environment else {
        return Ok(());
    };
    if environment.current_date.trim().is_empty() {
        return Err(NanocodexError::InvalidRequest(
            "execution-environment current date must not be empty".to_owned(),
        ));
    }
    if !is_iso_date(environment.current_date.trim()) {
        return Err(NanocodexError::InvalidRequest(
            "execution-environment current date must use YYYY-MM-DD".to_owned(),
        ));
    }
    if environment.timezone.trim().is_empty() {
        return Err(NanocodexError::InvalidRequest(
            "execution-environment timezone must not be empty".to_owned(),
        ));
    }
    if environment
        .project_instructions
        .as_deref()
        .is_some_and(|instructions| instructions.trim().is_empty())
    {
        return Err(NanocodexError::InvalidRequest(
            "execution-environment project instructions must not be empty".to_owned(),
        ));
    }
    Ok(())
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let Some(year) = decimal(&bytes[..4]) else {
        return false;
    };
    let Some(month) = decimal(&bytes[5..7]) else {
        return false;
    };
    let Some(day) = decimal(&bytes[8..]) else {
        return false;
    };
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day)
}

fn decimal(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0_u32, |value, byte| {
        byte.is_ascii_digit()
            .then(|| value * 10 + u32::from(*byte - b'0'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanocodex_oai_api::{
        auth::OpenAiAuth,
        responses::{ContentItem, MessageRole, ResponseItem},
    };
    use std::{
        future::{Pending, pending},
        sync::Mutex,
        task::{Context, Poll},
    };

    #[test]
    fn execution_environment_requires_complete_model_visible_context() {
        assert!(
            validate_execution_environment(Some(&ExecutionEnvironment::new(
                "2026-07-29",
                "Etc/UTC",
            )))
            .is_ok()
        );
        assert!(
            validate_execution_environment(Some(&ExecutionEnvironment::new("July 29", "Etc/UTC",)))
                .is_err()
        );
        assert!(
            validate_execution_environment(Some(&ExecutionEnvironment::new(
                "2026-02-29",
                "Etc/UTC",
            )))
            .is_err()
        );
        assert!(
            validate_execution_environment(Some(
                &ExecutionEnvironment::new("2026-07-29", "Etc/UTC").project_instructions(" "),
            ))
            .is_err()
        );
    }

    #[derive(Clone)]
    struct ObservingFactory {
        model: Arc<Mutex<Option<Model>>>,
    }

    impl ResponsesServiceFactory for ObservingFactory {
        type Service = PendingService;

        fn make(&self, config: Arc<ModelConfig>) -> Self::Service {
            *self.model.lock().expect("model observation lock") = Some(config.model);
            PendingService
        }
    }

    struct PendingService;

    impl Service<ResponsesAttempt> for PendingService {
        type Response = ResponsesServiceResponse;
        type Error = ResponseError;
        type Future = Pending<std::result::Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _context: &mut Context<'_>,
        ) -> Poll<std::result::Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _request: ResponsesAttempt) -> Self::Future {
            pending()
        }
    }

    #[tokio::test]
    async fn resumed_model_reaches_the_service_factory() {
        let workspace = std::env::current_dir().expect("current workspace");
        let canonical_context = ResponseItem::message(
            MessageRole::User,
            [ContentItem::input_text("resume with the retained model")],
        );
        let snapshot = serde_json::from_value(serde_json::json!({
            "version": 1,
            "model": "gpt-5.6-luna",
            "lineage_id": "019c0d31-c308-7d91-bff4-5dca82d15ac6",
            "prompt_cache_key": "retained-model",
            "workspace": workspace,
            "canonical_context": canonical_context,
            "history": [canonical_context],
        }))
        .expect("valid session snapshot");
        let observed_model = Arc::new(Mutex::new(None));
        let mut config = ModelConfig {
            auth: OpenAiAuth::api_key("test-key"),
            ..ModelConfig::default()
        };
        config.model = Model::Sol;
        let builder = NanocodexBuilder {
            config,
            tools: ToolsConfiguration::Shared(
                Tools::builder()
                    .without_defaults()
                    .build()
                    .expect("empty tools"),
            ),
            workspace: None,
            session_id: None,
            prompt_cache: PromptCacheConfig::default(),
            codex: CodexCompatibility::default(),
            resume: Some(snapshot),
            factory: ObservingFactory {
                model: Arc::clone(&observed_model),
            },
        };

        let (agent, events) = builder.build().expect("resumed agent");

        assert_eq!(
            *observed_model.lock().expect("model observation lock"),
            Some(Model::Luna)
        );
        agent.shutdown().await.expect("agent shutdown");
        drop(events);
    }
}
