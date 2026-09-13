//! Complete typed lifecycle events emitted around Responses operations.

mod data;
pub(crate) mod stream;

#[doc(inline)]
pub use data::{
    AgentEventData, AssistantDelta, AssistantEvent, AssistantMessage, CompactionCompleted,
    CompactionFailed, CompactionStarted, ContextEvent, EventUsage, ModelCallCompleted,
    ModelCallFailed, ModelCallStarted, ModelEvent, ModelWarmupCompleted, ModelWarmupFailed,
    ModelWarmupStarted, OpenAiEvent, ReasoningEvent, ReasoningSummaryDelta, RunError, RunEvent,
    RunMetrics, RunStarted, RunStatus, RunSteered, RunTerminal, ToolCall, ToolEvent,
    ToolResultEvent, ToolStatus, TransportEvent,
};
#[doc(inline)]
pub use stream::{AgentEvent, AgentEventKind, AgentEvents, EventError};
#[doc(hidden)]
pub use stream::{AgentEventTiming, TimedAgentEvent, monotonic_now_ns};
