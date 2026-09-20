//! Host-owned optional context, independent of configuration and provider adapters.
use crate::Digest;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io,
    path::{Path, PathBuf},
};

/// Authority supplied by the controller, not model tool arguments.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextAccess {
    ReadOnly,
    ReadWrite,
}

/// An application resolves backend identity from the admitted workspace.
/// Credentials stay inside this service and must never enter a manifest.
pub trait ContextService: Send + Sync {
    fn open(&self, workspace: &Path) -> io::Result<Box<dyn ContextSession>>;
    /// Optional asynchronous reference-data consolidation after host settlement.
    fn post_run(
        &self,
        _workspace: PathBuf,
        _run: ContextRun,
    ) -> BoxFuture<'static, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

/// Run-local capability state. Snapshots refresh only between provider turns.
pub trait ContextSession: Send {
    /// Host-owned producing identity, never accepted from tool arguments.
    fn bind_run(&mut self, _run: ContextRun) {}
    fn snapshot(&mut self) -> BoxFuture<'_, io::Result<ContextManifest>>;
    fn definitions(&self, access: ContextAccess) -> Vec<Value>;
    fn execute<'a>(
        &'a mut self,
        name: &'a str,
        arguments: Value,
        access: ContextAccess,
    ) -> BoxFuture<'a, io::Result<Value>>;
}

/// Address of the producing task in the portable trace/journal namespace.
#[derive(Clone, Debug)]
pub struct ContextRun {
    pub session: crate::session::SessionId,
    pub request: uuid::Uuid,
    pub task: crate::state::TaskId,
}

/// Exact metadata used in a request, retained as a content-addressed host artifact.
/// Memory records and skill bodies are retrieved explicitly, not copied into the catalog.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextManifest {
    pub version: u32,
    pub skills: String,
    pub memory: Option<MemoryContext>,
    pub diagnostics: Vec<String>,
}

/// The visible bounded discovery window, not a global remote-corpus revision.
/// Each operation still uses current backend state and exact CAS keys.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryContext {
    pub identity: Digest,
    pub backend: Value,
    pub window_limit: usize,
    pub keys: Vec<Value>,
}
