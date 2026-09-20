//! Explicit operator commands share the executor but never grant model authority.
use crate::{Digest, runtime::ExecutionStatus, state::TaskId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellSpec {
    pub command: String,
    pub expected_task: Option<TaskId>,
    pub scope_revision: Option<u64>,
    pub timeout_ms: u64,
    pub output_bytes: u64,
}
impl ShellSpec {
    pub fn validate(&self) -> Result<(), crate::StoreError> {
        if self.command.trim().is_empty()
            || self.command.len() > 128 * 1024
            || self.command.contains('\0')
            || self.timeout_ms == 0
            || self.timeout_ms > 300_000
            || self.output_bytes == 0
            || self.output_bytes > 1024 * 1024
        {
            return Err(crate::StoreError::Invalid(
                "shell command or execution bounds are invalid",
            ));
        }
        if self.scope_revision.is_some() && self.expected_task.is_none() {
            return Err(crate::StoreError::Invalid(
                "shell scope requires its task identity",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManualJob {
    pub job: Uuid,
    pub task: Option<TaskId>,
    pub before: Digest,
    pub origin: Digest,
    pub environment: Digest,
    pub started_ms: u64,
    pub scope_revision: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShellReport {
    pub version: u32,
    pub job: ManualJob,
    pub status: ExecutionStatus,
    pub stdout: Digest,
    pub stderr: Digest,
    pub elapsed_ms: u64,
    pub receipt: Digest,
    pub after: Option<Digest>,
    pub adopted: bool,
    pub error: Option<String>,
}
