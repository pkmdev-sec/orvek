use crate::{
    app::config::ReasoningEffort,
    tui::children::{MessageDeliveryState, MessageId, MessageOrigin, MessageThread},
};
use orvek_harness::state::TaskId;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct EntryId(usize);

impl EntryId {
    pub(super) const fn from_index(index: usize) -> Self {
        Self(index)
    }

    pub(crate) const fn index(self) -> usize {
        self.0
    }
}

// Warming/Compacting/Retrying/Connecting were states the old local worker
// loop produced. The host now reports context trimming as one atomic
// `ViewChange::ContextProjected` fact (see `tui/context.rs`) rather than a
// live in-progress phase, never surfaces provider retry delay over the wire
// (see the `absent_host_retry_timing_remains_unknown` test below), and has no
// "warming"/"connecting" concept at all (`crates/harness` protocol has
// neither) — the TUI's own IPC connection lifecycle is handled below this
// layer, in `client.rs`. `Reconnecting` is the one connection state the
// current protocol actually surfaces (a disconnected live stream).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TransientStatus {
    Thinking,
    Responding,
    WaitingForBackgroundWork,
    Tool(String),
    Reconnecting,
    Error(String),
}

#[derive(Clone, Debug)]
pub(crate) struct TranscriptEntry {
    pub(crate) id: EntryId,
    pub(crate) revision: u64,
    pub(crate) kind: EntryKind,
    pub(crate) hidden: bool,
    pub(crate) parent: Option<EntryId>,
    pub(crate) trailing_spacer: bool,
}

// `Interrupted`/`ContextCompacted`/`ContextCompactionFailed` markers from the
// old worker loop were dropped: `SessionCommand::TurnSettled`'s `outcome`
// (which would carry `Outcome::Cancelled`) is discarded by
// `host_projection.rs` before it ever reaches a `ViewChange`, so an
// interrupted turn is indistinguishable from any other settled turn today;
// and context trimming lands as one already-done `ContextProjected` fact
// (rendered via `HostStatus`), never as a live start/duration/failure phase.
#[derive(Clone, Debug)]
pub(crate) enum EntryKind {
    User {
        text: String,
    },
    Assistant {
        text: String,
        complete: bool,
        final_answer: bool,
    },
    Reasoning {
        text: String,
    },
    Tool(ToolEntry),
    DirectedMessage(DirectedMessageEntry),
    ForkedFrom {
        session_id: String,
    },
    EffortChanged {
        to: ReasoningEffort,
    },
    FastModeChanged {
        enabled: bool,
    },
    ReflectionStarted,
    TurnCompleted {
        duration_ns: u64,
    },
    Error {
        message: String,
    },
    HostStatus {
        text: String,
        verified: bool,
    },
    TaskStatus {
        id: TaskId,
        text: String,
        verified: bool,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct DirectedMessageEntry {
    pub(crate) perspective: MessageOrigin,
    pub(crate) thread: MessageThread,
    pub(crate) deliveries: Vec<MessageDelivery>,
}

impl DirectedMessageEntry {
    pub(crate) fn delivery(&self, message_id: MessageId) -> Option<&MessageDeliveryState> {
        self.deliveries
            .iter()
            .find(|delivery| delivery.message_id == message_id)
            .map(|delivery| &delivery.state)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MessageDelivery {
    pub(crate) message_id: MessageId,
    pub(crate) state: MessageDeliveryState,
}

#[derive(Clone, Debug)]
pub(crate) struct ToolEntry {
    pub(crate) name: String,
    pub(crate) arguments: Value,
    pub(crate) started_at_unix_ms: u64,
    pub(crate) state: ToolState,
    pub(crate) duration_ns: Option<u64>,
    pub(crate) result: Option<Value>,
    pub(crate) metadata: Option<Value>,
    pub(crate) substeps: Vec<String>,
    pub(crate) child_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolState {
    Proposed,
    Received,
    Unknown,
    Cancelled,
    Fenced,
    Running,
    Succeeded,
    Failed,
}
