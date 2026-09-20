//! Application-owned archival and local context compaction.
//!
//! The agent retains ownership of typed history and provider continuation. A backend
//! records source items and prepares replacements for eligible tool-result text only.
//! Payloads passed to this boundary are unredacted; implementations must not log them.

use super::{SessionId, SessionSnapshot};
use nanocodex_oai_api::{
    Model,
    responses::{ResponseItem, ResponseItemId},
};
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// An asynchronous operation on the context backend.
pub type ContextFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ContextError>> + Send + 'a>>;

/// Returns Nanocodex's approximate retained-item token count. Image profiles
/// must adjust image estimates to the selected provider's preprocessing rules.
#[must_use]
pub fn estimate_retained_item_tokens(item: &ResponseItem) -> u64 {
    nanocodex_oai_api::__private::compaction::estimate_item_tokens(item)
}

/// Checks that restored provider-ready history contains complete ordered tool
/// pairs and supported items, without executing tools or changing its text.
///
/// # Errors
/// Rejects empty, unsupported, or structurally inconsistent history.
pub fn validate_context_history(history: &[ResponseItem]) -> Result<(), ContextError> {
    nanocodex_oai_api::__private::ManagedSessionState::resume(history.to_vec())
        .map(|_| ())
        .map_err(|_| ContextError::InvalidArchive {
            reason: "restored history has invalid tool relationships or item types",
        })
}

/// Durable identity of the archive associated with one immutable history boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpoint {
    /// Branch whose accepted items are visible at this boundary.
    pub branch: SessionId,
    /// Last archived item sequence, inclusive; zero is the empty archive.
    pub sequence: u64,
    /// Monotonic provider-response group; the newest two groups remain native.
    pub model_generation: u64,
    /// Active immutable bitmap manifest, if any.
    pub manifest: Option<String>,
}

/// Recovery policy when local compaction cannot produce a usable request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextFallback {
    /// Preserve history and stop the current turn with a typed failure.
    Stop,
    /// Reconstruct formerly visible text and attempt provider compaction.
    Provider,
}

/// Selected carrier while retaining access to the same exact archive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextMode {
    /// Replace eligible historical text with bitmap pages.
    LocalImages,
    /// Use provider compaction after recovering native text on restoration.
    Provider,
}

/// Validated limits used by a local compaction backend.
#[derive(Clone, Copy, Debug)]
pub struct ContextPolicy {
    /// Active compaction implementation.
    pub mode: ContextMode,
    /// Maximum complete serialized request bytes, enforced before transport sends.
    pub request_bytes: usize,
    /// Maximum estimated input tokens, distinct from the provider context window.
    pub input_tokens: u64,
    /// Policy for insufficient reduction or unsupported input.
    pub fallback: ContextFallback,
}

/// One source item accepted into the conversation.
pub struct ContextRecord<'a> {
    /// Archive branch and sequence after accepting this item.
    pub checkpoint: &'a ContextCheckpoint,
    /// Item before ordinary tool-output limiting.
    pub original: &'a ResponseItem,
    /// Exact normalized item retained by the conversation.
    pub visible: &'a ResponseItem,
    /// Authoritative tool outcome; absent for non-tools or imported history.
    pub tool_success: Option<bool>,
}

/// Request inputs borrowed at a safe model boundary.
pub struct CompactionInput<'a> {
    /// Selected model. A backend must reject models it does not support.
    pub model: Model,
    /// Current archive identity.
    pub checkpoint: &'a ContextCheckpoint,
    /// History revision that a prepared result must match.
    pub revision: u64,
    /// Immutable instructions and tool definitions.
    pub prefix: &'a [ResponseItem],
    /// Complete current model-visible history, in provider order.
    pub history: &'a [ResponseItem],
    /// Eligible successful result IDs, excluding protected recent groups.
    pub eligible: &'a [ResponseItemId],
    /// Whether the user explicitly requested compaction.
    pub manual: bool,
}

/// A prepared image for one contiguous source span.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct ContextPage {
    /// Historical source locator shown beside the image.
    pub locator: String,
    /// Validated PNG data URL. Never log or include this value in diagnostics.
    pub image_url: String,
}

/// Replaces one text content block while preserving its enclosing tool result.
pub struct TextReplacement {
    /// Existing result item ID.
    pub item_id: ResponseItemId,
    /// Stable provider identity for the changed result body.
    pub projected_item_id: ResponseItemId,
    /// Text block index; zero for a plain-text result.
    pub content_index: usize,
    /// Nonempty pages in source order.
    pub pages: Vec<ContextPage>,
}

/// Durably prepared replacement, not yet installed in the agent.
pub struct PreparedCompaction {
    /// Revision of the history used to prepare this result.
    pub revision: u64,
    /// Immutable manifest persisted before returning this value.
    pub manifest: String,
    /// Bounded replacement of tool-result text blocks.
    pub replacements: Vec<TextReplacement>,
    /// Profile-aware estimate of the complete resulting request.
    pub input_tokens: u64,
    /// Total generated pages in the resulting history.
    pub page_count: u32,
}

/// Builds the provider-ready projection without changing any live state. Backends
/// use the same validation and assembly operation as the agent for budget estimates.
///
/// # Errors
/// Rejects missing or ineligible results, duplicate text spans, nontext replacements,
/// inconsistent projected identities, and missing PNG pages.
pub fn project_tool_result_text(
    history: &[ResponseItem],
    replacements: &[TextReplacement],
    eligible: &[ResponseItemId],
) -> Result<Vec<ResponseItem>, ContextError> {
    crate::model::run::local_compaction::apply_replacements(
        history.to_vec(),
        replacements,
        eligible,
    )
}

/// Application storage and renderer integration shared by independent agents.
pub trait ContextBackend: Send + Sync {
    /// Returns validated compaction policy.
    fn policy(&self) -> ContextPolicy;

    /// Opens a fresh, restored, or forked branch. Different IDs inherit only the
    /// supplied immutable cutoff; no supplied checkpoint means a fresh history.
    fn open(
        &self,
        session: SessionId,
        inherited: Option<&ContextCheckpoint>,
    ) -> Result<ContextCheckpoint, ContextError>;

    /// Queues an accepted item. Errors must preserve the previous archive cursor.
    fn record(&self, record: ContextRecord<'_>) -> Result<(), ContextError>;

    /// Returns successful result IDs from the archive at this exact cutoff.
    fn successful_results(
        &self,
        checkpoint: &ContextCheckpoint,
    ) -> Result<Vec<ResponseItemId>, ContextError>;

    /// Checks the complete request representation and estimates input tokens.
    fn estimate(
        &self,
        model: Model,
        prefix: &[ResponseItem],
        history: &[ResponseItem],
    ) -> Result<u64, ContextError>;

    /// Renders and durably stores a candidate. Dropping this future cancels work;
    /// detached CPU workers must observe an implementation-owned cancellation flag.
    fn prepare<'a>(&'a self, input: CompactionInput<'a>) -> ContextFuture<'a, PreparedCompaction>;

    /// Reconstructs the exact formerly model-visible history for provider fallback.
    fn restore_text(
        &self,
        checkpoint: &ContextCheckpoint,
        history: &[ResponseItem],
    ) -> Result<Vec<ResponseItem>, ContextError>;

    /// Waits for all queued archive writes without depending on UI event processing.
    fn flush(&self) -> ContextFuture<'_, ()>;

    /// Validates referenced archive data before a saved boundary can be restored.
    fn validate(&self, snapshot: &SessionSnapshot) -> Result<(), ContextError>;
}

/// Content-free failure returned by archival or local compaction operations.
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    /// A configured model or request route has no compatible profile.
    #[error("no compatible context image profile for this model or endpoint")]
    UnsupportedProfile,
    /// Native content or request bytes exceed the configured bound.
    #[error("context cannot fit the configured budget: {reason}")]
    Budget {
        /// Content-free explanation of the limiting resource.
        reason: &'static str,
    },
    /// The protected history cannot be reduced sufficiently.
    #[error("no eligible tool-result text provides sufficient context reduction")]
    NoReduction,
    /// An archive, manifest, or source span failed integrity validation.
    #[error("context archive is missing or invalid: {reason}")]
    InvalidArchive {
        /// Content-free invariant description.
        reason: &'static str,
    },
    /// A prepared candidate no longer corresponds to the current history.
    #[error("the context changed while compaction was being prepared")]
    StaleProjection,
    /// Local work was cancelled before installation.
    #[error("context compaction was cancelled")]
    Cancelled,
    /// Storage or rendering failed; implementations must redact error sources.
    #[error("context {operation} failed: {source}")]
    Backend {
        /// Content-free operation name.
        operation: &'static str,
        /// Underlying error with no history or image payloads.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}
