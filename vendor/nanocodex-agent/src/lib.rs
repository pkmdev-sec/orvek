#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(all(target_family = "wasm", not(target_os = "unknown")))]
compile_error!(
    "nanocodex-agent supports browser/JavaScript WebAssembly \
     (`wasm32-unknown-unknown`), not WASI targets"
);

extern crate self as nanocodex_agent;

mod agent;
mod error;
mod model;
mod prompt_cache;
#[cfg(not(target_family = "wasm"))]
#[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
/// Codex-compatible durable rollout recording and restoration.
pub mod rollout;
/// Durable agent session identities and snapshots.
pub mod session;
/// Per-turn token accounting and USD estimates.
pub mod usage;

pub use agent::{
    AgentHandle, AgentSessionContext, ExecutionEnvironment, Nanocodex, NanocodexBuilder,
    PromptRoute, Turn, TurnControl, TurnResult,
};
pub use error::{NanocodexError, Result};
pub use nanocodex_oai_api::{
    Model, OpenAi, ReasoningMode, ResponseError, ResponseErrorKind, Thinking, events::AgentEvents,
};
#[cfg(not(target_family = "wasm"))]
#[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
pub use nanocodex_tools::tool;
pub use nanocodex_tools::{Tool, Tools};
pub use usage::{CostStatus, EstimatedUsdCost, ServiceTier, TurnUsage, UsdAmount};

/// Complete typed lifecycle events emitted by an agent.
pub mod events {
    pub use nanocodex_oai_api::{
        events::{
            AgentEvent, AgentEventData, AgentEventKind, AgentEventTiming, AgentEvents,
            AssistantDelta, AssistantEvent, AssistantMessage, CompactionCompleted,
            CompactionFailed, CompactionStarted, ContextEvent, EventUsage, ModelCallCompleted,
            ModelCallFailed, ModelCallStarted, ModelEvent, ModelWarmupCompleted, ModelWarmupFailed,
            ModelWarmupStarted, OpenAiEvent, ReasoningEvent, ReasoningSummaryDelta, RunError,
            RunEvent, RunMetrics, RunStarted, RunStatus, RunSteered, RunTerminal, TimedAgentEvent,
            ToolCall, ToolEvent, ToolResultEvent, ToolStatus, TransportEvent, monotonic_now_ns,
        },
        responses::AgentMessageContent,
    };
}

/// Prompts and multimodal user input accepted by the agent.
pub mod input {
    pub use nanocodex_oai_api::{
        ImageDetail, Prompt, PromptInput, PromptMessage, PromptMessageRole, UserInput,
        responses::{AgentMessageContent, ContentItem},
    };
}

/// Advanced Responses transport and Tower service configuration.
#[cfg(not(target_family = "wasm"))]
#[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
pub mod transport {
    pub use crate::error::ResponsesError;
    pub use nanocodex_oai_api::{
        responses::RequestProfile,
        tower::{
            DefaultResponsesService, ResponsesAttempt, ResponsesAttemptKind, ResponsesClient,
            ResponsesRetryPolicy, ResponsesServiceError, ResponsesServiceResponse,
        },
        transport::{ResponsesHistory, ResponsesTransport},
    };
}

/// Complete tool contracts, registry, built-ins, Code Mode, and MCP.
pub mod tools {
    #[doc(inline)]
    pub use nanocodex_tools::*;
}

#[cfg(not(target_family = "wasm"))]
#[doc(hidden)]
pub mod __private {
    pub use nanocodex_tools::__private::*;
}
