use crate::{
    NanocodexError, Result,
    model::run::{
        CompletedModelTurn, HistoryCheckpoint, ModelCheckpoint, ModelCompactOutcome, ModelRun,
        ModelTurnOutcome, PreparedCheckpoint, prepare_history_checkpoint,
        prepare_resumed_checkpoint,
    },
    prompt_cache::{ModelPromptCache, SharedPromptCache},
    session::{CommittedSession, SessionResume, SessionSnapshot},
    usage::TurnUsage,
};
use futures_util::Stream;
use nanocodex_oai_api::{
    __private::{EventSink, ModelConfig, ResponsesServiceFactory, into_openai_parts},
    Model, OpenAi, Prompt, ReasoningMode, ResponseError, Thinking,
    auth::OpenAiAuthMode,
    events::{AgentEvent, AgentEvents},
    session::SessionId,
    tower::{ResponsesAttempt, ResponsesClient, ResponsesServiceResponse, StandardServiceFactory},
    transport::{ResponsesHistory, ResponsesTransport, TransportStats},
};
use nanocodex_tools::Tools;
#[cfg(not(target_family = "wasm"))]
use nanocodex_tools::ToolsBuildError;
use std::{
    collections::VecDeque,
    fmt,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
};
use tokio::sync::{mpsc, oneshot, watch};
use tower::Service;
use tracing::{Instrument, info, info_span};

const COMMAND_CAPACITY: usize = 8;
const STEER_CAPACITY: usize = 8;

#[cfg(not(target_family = "wasm"))]
type ToolsFactory =
    Arc<dyn Fn(AgentHandle) -> std::result::Result<Tools, ToolsBuildError> + Send + Sync>;

enum InitialResume {
    Exact(Box<ModelCheckpoint>),
    History(Box<HistoryCheckpoint>),
}

impl InitialResume {
    fn workspace(&self) -> &str {
        match self {
            Self::Exact(checkpoint) => checkpoint.workspace(),
            Self::History(resume) => &resume.workspace,
        }
    }

    fn history_len(&self) -> usize {
        match self {
            Self::Exact(checkpoint) => checkpoint.history().len(),
            Self::History(resume) => resume.history.len(),
        }
    }
}

#[derive(Clone)]
enum ToolsConfiguration {
    Shared(Tools),
    #[cfg(not(target_family = "wasm"))]
    PerAgent(ToolsFactory),
}

impl ToolsConfiguration {
    fn materialize(&self, agent_handle: AgentHandle) -> Result<Tools> {
        #[cfg(all(target_family = "wasm", target_os = "unknown"))]
        let _ = agent_handle;
        match self {
            Self::Shared(tools) => Ok(tools.clone()),
            #[cfg(not(target_family = "wasm"))]
            Self::PerAgent(factory) => factory(agent_handle).map_err(Into::into),
        }
    }
}

mod builder;
mod context_source;
mod driver;
mod durability;
mod executor;
mod handle;
mod session_context;
mod spawn;
mod turn;

pub(crate) use builder::ContextContract;
pub use builder::NanocodexBuilder;
use builder::{CodexCompatibility, PromptCacheConfig};
pub(crate) use context_source::ContextSource;
use context_source::ContextSourceConfig;
pub use context_source::ExecutionEnvironment;
use driver::{AgentDriver, AgentOrigin, BranchSpawner, DriverShutdown};
use durability::{Durability, DurabilityConfig};
pub(crate) use executor::{AgentFactory, AgentSend};
use executor::{ServiceFactory, spawn_driver};
use handle::request_command;
pub use handle::{AgentHandle, Nanocodex};
pub use session_context::AgentSessionContext;
use spawn::{build_agent, spawn_agent_driver, validate};
use turn::{Command, PromptRouteKind, QueuedTurn, TurnKey};
pub use turn::{PromptRoute, Turn, TurnControl, TurnResult};
