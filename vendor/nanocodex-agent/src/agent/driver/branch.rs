use super::*;

pub(in crate::agent) struct BranchSpawner<S> {
    pub(in crate::agent) config: Arc<ModelConfig>,
    pub(in crate::agent) tools: ToolsConfiguration,
    pub(in crate::agent) lineage_id: Arc<str>,
    pub(in crate::agent) prompt_cache_key: Option<Arc<str>>,
    pub(in crate::agent) shared_prompt_cache: Option<SharedPromptCache>,
    pub(in crate::agent) context_config: ContextSourceConfig,
    pub(in crate::agent) context_source: ContextSource,
    pub(in crate::agent) depth: u32,
    pub(in crate::agent) durability: DurabilityConfig,
    pub(in crate::agent) context_backend:
        Option<Arc<dyn crate::session::compaction::ContextBackend>>,
    pub(in crate::agent) service_factory: ServiceFactory<S>,
}

#[derive(Clone)]
pub(in crate::agent) struct AgentOrigin {
    pub(in crate::agent) kind: &'static str,
    pub(in crate::agent) depth: u32,
    pub(in crate::agent) parent_session_id: Option<Arc<str>>,
}

impl<S> Clone for BranchSpawner<S> {
    fn clone(&self) -> Self {
        Self {
            config: Arc::clone(&self.config),
            tools: self.tools.clone(),
            lineage_id: Arc::clone(&self.lineage_id),
            prompt_cache_key: self.prompt_cache_key.as_ref().map(Arc::clone),
            shared_prompt_cache: self.shared_prompt_cache.clone(),
            context_config: self.context_config.clone(),
            context_source: self.context_source.clone(),
            depth: self.depth,
            durability: self.durability.for_new_thread(),
            context_backend: self.context_backend.clone(),
            service_factory: Arc::clone(&self.service_factory),
        }
    }
}

impl<S> BranchSpawner<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<ResponseError> + AgentSend + 'static,
    S::Future: AgentSend,
{
    pub(super) fn spawn_fork(
        &self,
        checkpoint: &CommittedSession,
        parent_session_id: &str,
        model: Model,
        thinking: Thinking,
        fast_mode: bool,
    ) -> Result<(Nanocodex, AgentEvents)> {
        let session_id = SessionId::new();
        let workspace = Some(Arc::<str>::from(checkpoint.model().workspace()));
        let mut spawner = self.clone();
        spawner.context_source = spawner.context_config.build();
        let mut config = (*spawner.config).clone();
        config.model = model;
        config.thinking = thinking;
        config.fast_mode = fast_mode;
        spawner.config = Arc::new(config);
        spawner.depth = self.depth.saturating_add(1);
        let service = (spawner.service_factory)(Arc::clone(&spawner.config));
        spawn_agent_driver(
            spawner,
            session_id,
            workspace,
            service,
            Some(InitialResume::Exact(Box::new(checkpoint.model().clone()))),
            AgentOrigin {
                kind: "fork",
                depth: self.depth.saturating_add(1),
                parent_session_id: Some(Arc::from(parent_session_id)),
            },
            None,
        )
    }

    pub(super) fn spawn_clean(
        &self,
        workspace: Option<Arc<str>>,
        parent_session_id: &str,
        model: Model,
        thinking: Thinking,
        fast_mode: bool,
    ) -> Result<(Nanocodex, AgentEvents)> {
        let session_id = SessionId::new();
        let session_id_text = session_id.to_string();
        let depth = self.depth.saturating_add(1);
        let mut config = (*self.config).clone();
        config.model = model;
        config.thinking = thinking;
        config.fast_mode = fast_mode;
        let prompt_cache_key = self
            .prompt_cache_key
            .as_ref()
            .map_or_else(|| Arc::clone(&self.lineage_id), Arc::clone);
        let spawner = Self {
            config: Arc::new(config),
            tools: self.tools.clone(),
            lineage_id: Arc::from(session_id_text.as_str()),
            prompt_cache_key: Some(prompt_cache_key),
            shared_prompt_cache: self.shared_prompt_cache.clone(),
            context_config: self.context_config.clone(),
            context_source: self.context_config.build(),
            depth,
            durability: self.durability.for_new_thread(),
            context_backend: self.context_backend.clone(),
            service_factory: Arc::clone(&self.service_factory),
        };
        let service = (spawner.service_factory)(Arc::clone(&spawner.config));
        spawn_agent_driver(
            spawner,
            session_id,
            workspace,
            service,
            None,
            AgentOrigin {
                kind: "spawn",
                depth,
                parent_session_id: Some(Arc::from(parent_session_id)),
            },
            None,
        )
    }
}
