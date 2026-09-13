mod lifecycle;
pub(crate) mod local_compaction;
mod responses;
mod state;
mod tool_calls;
mod turn;

use super::{
    CompactionCompleted, CompactionFailed, CompactionStarted, ModelCallCompleted, ModelCallFailed,
    ModelCallStarted, RunError, RunStarted, RunStats, RunSteered, ToolCallArguments, ToolCallEvent,
    ToolResultEvent, WarmupCompleted, WarmupFailed, WarmupStarted,
    context::{ContextBaseline, ContextSnapshot, ContextState},
    display_endpoint, elapsed_ns,
    input::{
        custom_tool_notification, custom_tool_output, developer_context, function_tool_output,
        prompt_messages, task_input, tool_search_output, turn_aborted,
    },
    terminal_payload,
};
use crate::{
    NanocodexError, Result,
    agent::{AgentSend, ContextSource},
    prompt_cache::ModelPromptCache,
    session::{
        SessionId,
        compaction::{ContextBackend, ContextCheckpoint},
    },
    usage::TurnUsage,
};
use futures_util::{FutureExt, StreamExt, stream::FuturesOrdered};
use lifecycle::*;
use nanocodex_oai_api::{
    __private::{
        EventSink, ManagedSessionState, ModelConfig, ResponsesAttemptFactory,
        assign_missing_response_item_id, compaction, with_code_mode_tool_names,
    },
    CONTEXT_WINDOW_TOKENS, Model, Prompt, Thinking,
    events::AgentEventKind,
    pricing::{ServiceTier, estimate_for_model},
    responses::{ContentItem, MessageRole, RequestProfile, ResponseItem, ToolDefinition, Usage},
    tower::{
        CodeCall, CodeCallKind, GenerationOutput as TurnResult, ResponsesAttempt, ResponsesClient,
        ResponsesOutput, ResponsesServiceResponse,
    },
    transport::{ResponsesError, ResponsesTransport, TransportStats},
};
use nanocodex_tools::{
    __private::model_contract as model_tool_contract,
    ToolContext, Tools,
    code_mode::{CodeModeExecution, CodeModeObserver, CodeModeUpdate},
    contract::{DEFAULT_TOOL_OUTPUT_TOKENS, ToolInput, ToolOutput, ToolOutputBody},
    image::{prepare_output_images, prepare_user_input},
    runtime::{
        ImageGenerationConfig, OwnedToolContext, ToolRuntime, ToolRuntimeControl, WebSearchConfig,
    },
};
use responses::*;
use serde::Serialize;
use serde_json::{Value, value::RawValue};
use state::*;
use std::{
    any::Any,
    collections::HashMap,
    panic::AssertUnwindSafe,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::{RwLock, watch};
use tool_calls::*;
use tower::Service;
use tracing::{Instrument, info, info_span};
use web_time::Instant;

pub(crate) struct ModelRun<S> {
    events: EventSink,
    config: Arc<ModelConfig>,
    model: Model,
    thinking: Thinking,
    fast_mode: bool,
    client: ResponsesClient<S>,
    transport_stats: Arc<TransportStats>,
    started_at: Instant,
    stats: RunStats,
    session: Option<ModelSessionState>,
    active_tools: Option<ToolRuntimeControl>,
    active_tool_calls: Vec<ActiveToolCall>,
    active_tool_batch_started_at: Option<Instant>,
    tool_call_indices: HashMap<Box<str>, u32>,
    tools: Tools,
    prompt_cache: ModelPromptCache,
    context_source: ContextSource,
    global_instructions: Option<Arc<str>>,
    force_compaction: bool,
    pending_developer_messages: Vec<ResponseItem>,
    context_backend: Option<Arc<dyn ContextBackend>>,
}

pub(crate) enum ModelTurnOutcome {
    Completed(CompletedModelTurn),
    Cancelled(ModelCheckpoint),
    Failed {
        error: NanocodexError,
        checkpoint: ModelCheckpoint,
    },
}

pub(crate) enum ModelCompactOutcome {
    Completed(ModelCheckpoint),
    Cancelled(ModelCheckpoint),
    Failed {
        error: NanocodexError,
        checkpoint: ModelCheckpoint,
    },
}

pub(crate) struct CompletedModelTurn {
    pub(crate) final_message: String,
    pub(crate) usage: TurnUsage,
    pub(crate) checkpoint: ModelCheckpoint,
}

#[derive(Clone)]
pub(crate) struct ModelCheckpoint {
    workspace: String,
    conversation: ConversationState,
    request_prefix: Arc<[ResponseItem]>,
    prompt_cache_key: Arc<str>,
    preserve_inherited_delta: bool,
    global_instructions: Option<Arc<str>>,
    context_baseline: ContextBaseline,
}

pub(crate) struct PreparedCheckpoint {
    pub(crate) checkpoint: ModelCheckpoint,
    pub(crate) runtime: ToolRuntime,
    pub(crate) context_source: ContextSource,
    selected_agents_md: Option<Arc<str>>,
}

pub(crate) struct HistoryCheckpoint {
    pub(crate) workspace: String,
    pub(crate) canonical_context: ResponseItem,
    pub(crate) history: Vec<ResponseItem>,
    pub(crate) prompt_cache_key: Arc<str>,
    pub(crate) context_baseline: Option<ContextBaseline>,
}

impl ModelCheckpoint {
    pub(crate) const fn context_checkpoint(&self) -> Option<&ContextCheckpoint> {
        self.conversation.archive.as_ref()
    }
    pub(crate) fn workspace(&self) -> &str {
        &self.workspace
    }
    pub(crate) fn history(&self) -> nanocodex_oai_api::responses::ResponseHistory {
        self.conversation.shared_history()
    }

    #[allow(dead_code, reason = "consumed by the native durability boundary only")]
    pub(crate) const fn history_revision(&self) -> u64 {
        self.conversation.history_revision()
    }

    pub(crate) fn request_prefix(&self) -> &[ResponseItem] {
        &self.request_prefix
    }

    pub(crate) fn prompt_cache_key(&self) -> &str {
        &self.prompt_cache_key
    }

    pub(crate) fn canonical_context(&self) -> &ResponseItem {
        &self.conversation.canonical_context
    }

    pub(crate) fn snapshot_history(&self) -> Vec<ResponseItem> {
        self.conversation.flattened_history()
    }

    pub(crate) const fn context_baseline(&self) -> &ContextBaseline {
        &self.context_baseline
    }

    pub(crate) fn resume(
        workspace: String,
        mut request_prefix: Vec<ResponseItem>,
        prompt_cache_key: Arc<str>,
        canonical_context: ResponseItem,
        history: Vec<ResponseItem>,
        global_instructions: Option<Arc<str>>,
        context_baseline: Option<ContextBaseline>,
        context_archive: Option<ContextCheckpoint>,
    ) -> Result<Self> {
        assign_request_prefix_ids(&mut request_prefix);
        let context_baseline =
            context_baseline.unwrap_or_else(|| ContextBaseline::reconstruct(&history));
        let mut conversation = ConversationState::resume(canonical_context, history)?;
        conversation.archive = context_archive;
        Ok(Self {
            workspace,
            conversation,
            request_prefix: Arc::from(request_prefix),
            prompt_cache_key,
            preserve_inherited_delta: false,
            global_instructions,
            context_baseline,
        })
    }
}

impl<S> ModelRun<S> {
    pub(crate) fn new(
        events: EventSink,
        config: Arc<ModelConfig>,
        client: ResponsesClient<S>,
        transport_stats: Arc<TransportStats>,
        tools: Tools,
        prompt_cache: ModelPromptCache,
        context_source: ContextSource,
    ) -> Self {
        let model = config.model;
        let thinking = config.thinking;
        let fast_mode = config.fast_mode;
        let global_instructions = context_source.global_instructions();
        Self {
            events,
            config,
            model,
            thinking,
            fast_mode,
            client,
            transport_stats,
            started_at: Instant::now(),
            stats: RunStats::default(),
            session: None,
            active_tools: None,
            active_tool_calls: Vec::new(),
            active_tool_batch_started_at: None,
            tool_call_indices: HashMap::new(),
            tools,
            prompt_cache,
            context_source,
            global_instructions,
            force_compaction: false,
            pending_developer_messages: Vec::new(),
            context_backend: None,
        }
    }

    pub(crate) fn from_checkpoint(
        events: EventSink,
        config: Arc<ModelConfig>,
        client: ResponsesClient<S>,
        transport_stats: Arc<TransportStats>,
        tools: Tools,
        prompt_cache: ModelPromptCache,
        prepared: PreparedCheckpoint,
    ) -> Self {
        let PreparedCheckpoint {
            checkpoint,
            runtime,
            context_source,
            selected_agents_md,
        } = prepared;
        let active_tools = runtime.control();
        let (_, code_mode_tool_names) = model_tool_contract(&runtime, events.request_id());
        let factory = ResponsesAttemptFactory::new(
            with_code_mode_tool_names(
                RequestProfile::new(
                    events.request_id(),
                    checkpoint.prompt_cache_key.to_string(),
                    Arc::clone(&checkpoint.request_prefix),
                ),
                code_mode_tool_names,
            ),
            events.clone(),
            Arc::clone(&transport_stats),
        );
        let model = config.model;
        let thinking = config.thinking;
        let fast_mode = config.fast_mode;
        let context_source =
            context_source.with_fallback_global(checkpoint.global_instructions.clone());
        let global_instructions = context_source.global_instructions();
        Self {
            events,
            config,
            model,
            thinking,
            fast_mode,
            client,
            transport_stats,
            started_at: Instant::now(),
            stats: RunStats::default(),
            session: Some(ModelSessionState {
                workspace: checkpoint.workspace,
                tools: runtime,
                factory,
                conversation: checkpoint.conversation,
                context: ContextState::new(selected_agents_md, checkpoint.context_baseline),
                preserve_inherited_delta: checkpoint.preserve_inherited_delta,
            }),
            active_tools: Some(active_tools),
            active_tool_calls: Vec::new(),
            active_tool_batch_started_at: None,
            tool_call_indices: HashMap::new(),
            tools,
            prompt_cache,
            context_source,
            global_instructions,
            force_compaction: false,
            pending_developer_messages: Vec::new(),
            context_backend: None,
        }
    }

    pub(crate) fn set_events(&mut self, events: EventSink) {
        if let Some(session) = &mut self.session {
            session.factory.set_events(events.clone());
        }
        self.events = events;
    }

    pub(crate) fn replace_client(&mut self, client: ResponsesClient<S>) {
        self.client = client;
    }

    pub(crate) async fn shutdown(&mut self) {
        if let Some(tools) = &self.active_tools {
            tools.cancel().await;
        }
    }

    pub(crate) fn append_developer_message(
        &mut self,
        text: String,
    ) -> Result<Option<ModelCheckpoint>> {
        let item = ResponseItem::message(
            MessageRole::Developer,
            [ContentItem::InputText {
                text: text.into_boxed_str(),
            }],
        );
        let Some(session) = &mut self.session else {
            self.pending_developer_messages.push(item);
            return Ok(None);
        };
        session.conversation.append([item])?;
        session.conversation.commit_tail();
        Ok(Some(ModelCheckpoint {
            workspace: session.workspace.clone(),
            conversation: session.conversation.clone(),
            request_prefix: session.factory.profile().shared_prefix(),
            prompt_cache_key: Arc::from(session.factory.profile().prompt_cache_key()),
            preserve_inherited_delta: false,
            global_instructions: self.global_instructions.clone(),
            context_baseline: session.context.baseline(),
        }))
    }

    pub(crate) fn enable_context(&mut self, backend: Arc<dyn ContextBackend>) -> Result<()> {
        let session_id = self.events.request_id().parse::<SessionId>().map_err(|_| {
            NanocodexError::InvalidRequest("invalid context session identity".to_owned())
        })?;
        if let Some(session) = &mut self.session {
            session
                .conversation
                .open_archive(Arc::clone(&backend), session_id)?;
            if backend.policy().mode == crate::session::compaction::ContextMode::Provider {
                let archive = session
                    .conversation
                    .archive
                    .as_ref()
                    .expect("archive opened above");
                if archive.manifest.is_some() {
                    let text =
                        backend.restore_text(archive, &session.conversation.flattened_history())?;
                    let tokens =
                        backend.estimate(self.model, session.factory.profile().prefix(), &text)?;
                    session
                        .conversation
                        .managed
                        .install_projection(
                            session.conversation.managed.projection_revision(),
                            text,
                            session.factory.profile().prefix(),
                            tokens,
                        )
                        .map_err(
                            |_| crate::session::compaction::ContextError::InvalidArchive {
                                reason: "invalid native recovery history",
                            },
                        )?;
                    session
                        .conversation
                        .archive
                        .as_mut()
                        .expect("archive opened above")
                        .manifest = None;
                }
            }
        }
        self.context_backend = Some(backend);
        Ok(())
    }

    fn local_context_enabled(&self) -> bool {
        self.context_backend.as_ref().is_some_and(|backend| {
            backend.policy().mode == crate::session::compaction::ContextMode::LocalImages
        })
    }

    fn attach_archive(&self, conversation: &mut ConversationState) -> Result<()> {
        if let Some(backend) = &self.context_backend {
            let session_id = self.events.request_id().parse::<SessionId>().map_err(|_| {
                NanocodexError::InvalidRequest("invalid context session identity".to_owned())
            })?;
            conversation.open_archive(Arc::clone(backend), session_id)?;
        }
        Ok(())
    }

    pub(crate) async fn flush_context(&mut self) -> Result<()> {
        if let Some(backend) = &self.context_backend {
            backend.flush().await?;
        }
        Ok(())
    }

    pub(crate) fn current_checkpoint(&self) -> Option<ModelCheckpoint>
    where
        S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
        S::Error: Into<nanocodex_oai_api::ResponseError>,
        S::Future: AgentSend,
    {
        self.session.as_ref().map(|session| {
            Self::checkpoint_from_session(session, false, self.global_instructions.clone())
        })
    }

    fn empty_session(&mut self, requested_workspace: Option<&str>) -> Result<ModelSessionState> {
        let workspace = requested_workspace.map_or_else(
            || self.context_source.resolve_workspace(None),
            |workspace| Ok(workspace.to_owned()),
        )?;
        let selected_agents_md = self
            .context_source
            .project_instructions(&workspace)
            .map(Arc::<str>::from);
        let tools = tool_runtime(&workspace, &self.config, &self.tools);
        let tool_control = tools.control();
        self.active_tools = Some(tool_control);
        let factory = self.attempt_factory(&tools);
        let context = ContextState::new(selected_agents_md, ContextBaseline::Missing);
        let canonical_context = context
            .capture(
                tools.working_directory(),
                tools.default_shell_name(),
                self.context_source.execution_environment(),
            )
            .full_item();
        let mut conversation = ConversationState::empty(canonical_context);
        self.attach_archive(&mut conversation)?;
        Ok(ModelSessionState {
            workspace,
            tools,
            factory,
            conversation,
            context,
            preserve_inherited_delta: false,
        })
    }

    fn attempt_factory(&self, tools: &ToolRuntime) -> ResponsesAttemptFactory {
        attempt_factory(
            &self.events,
            &self.transport_stats,
            self.prompt_cache.key(),
            tools,
            self.config.system_prompt(),
        )
    }

    fn responses_endpoint(&self) -> &str {
        match self.config.responses_transport {
            ResponsesTransport::WebSocket => &self.config.websocket_url,
            ResponsesTransport::Https => &self.config.api_base_url,
        }
    }
}

pub(crate) fn prepare_checkpoint(
    checkpoint: ModelCheckpoint,
    config: &ModelConfig,
    tools: &Tools,
    context_source: ContextSource,
) -> PreparedCheckpoint {
    let runtime = tool_runtime(checkpoint.workspace(), config, tools);
    let selected_agents_md = context_source
        .project_instructions(checkpoint.workspace())
        .map(Arc::from);
    PreparedCheckpoint {
        checkpoint,
        runtime,
        context_source,
        selected_agents_md,
    }
}

pub(crate) fn prepare_resumed_checkpoint(
    mut checkpoint: ModelCheckpoint,
    config: &ModelConfig,
    tools: &Tools,
    session_id: &str,
    context_source: ContextSource,
    previous_contract: Option<&crate::agent::ContextContract>,
) -> Result<PreparedCheckpoint> {
    checkpoint.global_instructions = context_source
        .global_instructions()
        .or(checkpoint.global_instructions);
    let mut prepared = prepare_checkpoint(checkpoint, config, tools, context_source);
    let (tool_specs, code_mode_tool_names) = model_tool_contract(&prepared.runtime, session_id);
    let expected = request_profile(
        "resume-validation",
        "resume-validation",
        tool_specs,
        code_mode_tool_names,
        config.system_prompt(),
    );
    let validation_prefix = if let Some(previous) = previous_contract {
        if prepared.checkpoint.context_checkpoint().is_some() {
            return Err(NanocodexError::InvalidSessionSnapshot(
                "only legacy context contracts may be upgraded".to_owned(),
            ));
        }
        let mut previous_config = config.clone();
        previous_config.system_prompt = Arc::clone(&previous.instructions);
        let previous_runtime = tool_runtime(
            prepared.checkpoint.workspace(),
            &previous_config,
            &previous.tools,
        );
        let (specs, names) = model_tool_contract(&previous_runtime, session_id);
        request_profile(
            "resume-validation",
            "resume-validation",
            specs,
            names,
            &previous.instructions,
        )
    } else {
        expected.clone()
    };
    let encoded_expected = serde_json::to_vec(&without_response_item_ids(
        validation_prefix.prefix(),
    ))
    .map_err(|error| {
        NanocodexError::InvalidSessionSnapshot(format!(
            "failed to validate the request prefix: {error}"
        ))
    })?;
    let stored = serde_json::to_vec(&without_response_item_ids(
        prepared.checkpoint.request_prefix(),
    ))
    .map_err(|error| {
        NanocodexError::InvalidSessionSnapshot(format!(
            "failed to validate the stored request prefix: {error}"
        ))
    })?;
    if encoded_expected != stored {
        return Err(NanocodexError::InvalidSessionSnapshot(
            "instructions or tool definitions do not match the resumed session".to_owned(),
        ));
    }
    if previous_contract.is_some() {
        prepared.checkpoint.request_prefix = expected.shared_prefix();
        prepared.checkpoint.conversation.reset_for_full_request();
    }
    Ok(prepared)
}

pub(crate) fn prepare_history_checkpoint(
    resume: HistoryCheckpoint,
    config: &ModelConfig,
    tools: &Tools,
    session_id: &str,
    context_source: ContextSource,
) -> Result<PreparedCheckpoint> {
    let HistoryCheckpoint {
        workspace,
        canonical_context,
        history,
        prompt_cache_key,
        context_baseline,
    } = resume;
    let selected_agents_md = context_source
        .project_instructions(&workspace)
        .map(Arc::from);
    let runtime = tool_runtime(&workspace, config, tools);
    let (tool_specs, code_mode_tool_names) = model_tool_contract(&runtime, session_id);
    let request_prefix = request_profile(
        "history-resume",
        "history-resume",
        tool_specs,
        code_mode_tool_names,
        config.system_prompt(),
    )
    .prefix()
    .to_vec();
    let checkpoint = ModelCheckpoint::resume(
        workspace,
        request_prefix,
        prompt_cache_key,
        canonical_context,
        history,
        context_source.global_instructions(),
        context_baseline,
        None,
    )?;
    Ok(PreparedCheckpoint {
        checkpoint,
        runtime,
        context_source,
        selected_agents_md,
    })
}

fn without_response_item_ids(items: &[ResponseItem]) -> Vec<ResponseItem> {
    items
        .iter()
        .cloned()
        .map(|mut item| {
            item.strip_id();
            item
        })
        .collect()
}
