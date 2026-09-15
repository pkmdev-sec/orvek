//! Read-only child and message presentation. These labels never authorize operations.

use super::transcript::TranscriptRecord;
use orvek_harness::{Digest, inference::Model};
use serde::{Deserialize, Serialize};
use std::{fmt, sync::Arc};
use uuid::Uuid;

macro_rules! identity {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        #[serde(transparent)]
        pub(crate) struct $name(pub(crate) Uuid);
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let id = self.0.simple().to_string();
                let compact = id.trim_start_matches('0');
                let compact = if compact.is_empty() { "0" } else { compact };
                f.write_str(&compact[..compact.len().min(8)])
            }
        }
    };
}
identity!(ChildId);
identity!(MessageId);
identity!(ThreadId);

// Only `ChildId` is ever constructed directly (fixtures/tests); message and
// thread identities always arrive already-formed off the wire.
#[cfg(test)]
impl ChildId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(Uuid::from_u128(value as u128))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum MessageOrigin {
    Root,
    Child { child_id: ChildId },
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MessagePriority {
    #[default]
    Deferred,
    Urgent,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MessagePurpose {
    Delegate,
    #[default]
    Coordinate,
    Finding,
    Question,
    Reply,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MessageDisposition {
    Started,
    Queued,
    Steered,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct DirectedMessage {
    pub(crate) id: MessageId,
    pub(crate) thread_id: ThreadId,
    pub(crate) from: MessageOrigin,
    pub(crate) to: ChildId,
    pub(crate) priority: MessagePriority,
    pub(crate) purpose: MessagePurpose,
    pub(crate) in_reply_to: Option<MessageId>,
    pub(crate) body: String,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct MessageThread {
    pub(crate) id: ThreadId,
    pub(crate) participants: [MessageOrigin; 2],
    pub(crate) messages: Vec<DirectedMessage>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum MessageDeliveryState {
    Admitted { disposition: MessageDisposition },
    Delivered { disposition: MessageDisposition },
    Failed { error: String },
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct MessageUpdate {
    pub(crate) message_id: MessageId,
    pub(crate) thread: MessageThread,
    pub(crate) delivery: MessageDeliveryState,
}

// Keep this presentation state aligned with the lifecycle variants emitted by
// `orvek_harness::controller::SubagentEvent`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ChildStatus {
    Running,
    // The child's own output is referenced, never inlined: a schema-valid
    // child result is not evidence, so the UI must not be able to present it
    // as one.
    Returned { output: Digest },
    Failed { error: String },
    Cancelled,
}
impl ChildStatus {
    pub(crate) const fn is_active(&self) -> bool {
        matches!(self, Self::Running)
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ChildView {
    pub(crate) id: ChildId,
    pub(crate) session_id: String,
    pub(crate) model: Model,
    pub(crate) role: String,
    pub(crate) task: String,
    pub(crate) parent: Option<ChildId>,
}
#[derive(Debug)]
pub(crate) enum ChildUpdate {
    Added(ChildView),
    // Lifecycle and directed-message events are live. Full child transcript
    // streaming is retained for the inspector contract but is not emitted yet.
    #[allow(dead_code)]
    Record {
        id: ChildId,
        record: Arc<TranscriptRecord>,
    },
    Status {
        id: ChildId,
        status: ChildStatus,
    },
    // The native host does not expose child-to-child message events yet.
    #[allow(dead_code)]
    Message(MessageUpdate),
}
