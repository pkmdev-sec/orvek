//! Provider inference only: callers own context, call admission, tools, and task completion.
//!
//! Wire settings preserve Orvek's pinned model contract. Account entitlement and acceptance
//! of `max`/`pro` remain provider decisions; an error never selects a different setting.
//! Deltas are provisional and never authorize execution. The controller must validate
//! terminal tool proposals against its own admitted schemas and capability policy.
//! Protocol references: <https://developers.openai.com/api/docs/guides/websocket-mode> and
//! <https://developers.openai.com/api/reference/typescript/resources/beta/subresources/responses/methods/create>

pub mod auth;
mod protocol;
mod transport;

pub use protocol::{
    ArgumentValidity, Delta, InferenceRequest, InternalContextMedia, Model, ModelSettings,
    OutputItem, PromptCacheIdentity, PromptInput, ProviderResponse, ReasoningMode, ResponseStatus,
    Thinking, ToolProposal, Usage, UsdCost,
};
pub use transport::{
    AttemptRecord, AttemptStatus, CallOutcome, Failure, FailureKind, Limits, ResponsesClient,
    Route, Transport,
};
