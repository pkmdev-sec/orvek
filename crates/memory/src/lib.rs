#![doc = include_str!("../README.md")]

/// Incompatible remote-memory protocol generation.
///
/// Route paths and session negotiation derive from this single value. Increment it only when
/// clients and servers must intentionally stop interoperating.
pub const VERSION: u32 = 2;

mod evidence;
mod lessons;
mod model;
pub use lessons::{finalize_lessons, propose_lesson};
#[cfg(feature = "local")]
mod archive;
#[cfg(feature = "local")]
mod sources;
#[cfg(feature = "local")]
pub use archive::{ArchiveEntry, MemoryArchive};
pub use evidence::{
    EvidenceState, LessonQuery, LineRange, MemoryKind, MemoryMetadata, MemoryOrigin, MemoryScope,
    OwnershipReference, ProposalState, SourceEvidence, TraceReference,
};
#[cfg(feature = "local")]
pub use sources::WorkspaceSources;
mod retrieval;
#[cfg(any(feature = "client", feature = "local"))]
mod secrets;
pub mod server;
mod store;
#[cfg(feature = "tool")]
mod tool;

pub use model::{
    MemoryAccess, MemoryCandidate, MemoryImportReport, MemoryKey, MemoryLimits, MemoryRecord,
    MemoryScan, MemorySource, normalize_identity,
};
pub use server::protocol::RemoteRole;
#[cfg(feature = "local")]
pub use store::LocalMemoryStore;
#[cfg(all(feature = "client", feature = "local"))]
pub use store::SelectedMemoryStore;
pub use store::{MemoryError, MemoryStore};
#[cfg(feature = "client")]
pub use store::{RemoteClientError, RemoteMemoryClient, RemoteToken};
#[cfg(feature = "tool")]
pub use tool::{
    MemoryOperationError, MemoryPermission, MemorySession, MemoryTool, MutationAuthorizer,
};

#[cfg(test)]
mod tests;
