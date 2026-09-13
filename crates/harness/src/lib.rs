//! Authoritative task state and evidence-based completion, independent of inference and UI.

pub mod admission;
pub mod artifacts;
pub mod auxiliary;
pub mod capabilities;
pub mod completion;
pub mod context;
pub mod contract;
pub mod controller;
pub mod delivery;
pub mod digest;
pub mod feedback;
pub mod import;
pub mod inference;
pub mod input;
#[cfg(unix)]
pub mod ipc;
pub mod manual;
pub mod review;
pub mod runtime;
pub mod session;
pub mod state;
pub mod store;
pub mod submission;
pub mod verification;
pub mod workspace;

pub use digest::Digest;
pub use store::{Store, StoreError};
