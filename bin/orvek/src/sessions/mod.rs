//! Shared durable sessions, checkpoints, and transcript records.

pub(crate) mod archive;
pub(crate) mod checkpoint;
pub(crate) mod context;
pub(crate) mod error;
pub(crate) mod journal;
pub(crate) mod record;
pub(crate) mod storage;
