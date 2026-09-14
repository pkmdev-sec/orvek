//! Bounded terminal projections of native host records.

mod entry;
mod model;
mod record;

pub(crate) use entry::{
    DirectedMessageEntry, EntryId, EntryKind, MessageDelivery, ToolEntry, ToolState,
    TranscriptEntry, TransientStatus,
};
pub(crate) use model::TranscriptModel;
pub(crate) use record::{LocalEvent, TranscriptRecord};
// Only the test and bench targets construct these directly. Re-exporting them
// unconditionally would be an unused import in an ordinary build.
#[cfg(test)]
pub(crate) use record::{SessionStarted, TurnId};
