//! Durable operator input, distinct from a model turn or a completed task.
use crate::{
    Digest,
    contract::Limits,
    state::{Outcome, TaskId},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Schedule {
    #[default]
    Queue,
    Steer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum OrdinaryKind {
    Information,
    Action,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubmitIntent {
    Ordinary {
        limits: Limits,
        policy: crate::admission::RequestPolicy,
        #[serde(default)]
        schedule: Schedule,
    },
    Shell {
        spec: crate::manual::ShellSpec,
    },
    Auxiliary {
        spec: crate::auxiliary::AuxiliarySpec,
    },
    NewTask {
        limits: Limits,
        policy: crate::admission::RequestPolicy,
    },
    Continue {
        task: TaskId,
        scope_revision: u64,
        #[serde(default)]
        schedule: Schedule,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkIntent {
    Ordinary {
        limits: Limits,
        policy: Digest,
        #[serde(default)]
        schedule: Schedule,
    },
    Shell {
        spec: crate::manual::ShellSpec,
    },
    Auxiliary {
        spec: crate::auxiliary::AuxiliarySpec,
    },
    NewTask {
        limits: Limits,
        policy: Digest,
    },
    Continue {
        task: TaskId,
        scope_revision: u64,
        #[serde(default)]
        schedule: Schedule,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SubmissionStatus {
    Queued,
    Running,
    Finished {
        task: Option<TaskId>,
        outcome: Option<Outcome>,
        error: Option<String>,
    },
    Cancelled,
    Interrupted,
}

impl SubmissionStatus {
    pub fn pending(&self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Submission {
    pub manual_job: Option<crate::manual::ManualJob>,
    pub id: Uuid,
    pub input: Digest,
    pub initial_input: Digest,
    pub records: Vec<Digest>,
    pub result: Option<Digest>,
    pub intent: WorkIntent,
    pub status: SubmissionStatus,
    pub submitted_revision: u64,
    pub submitted_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmissionPage {
    pub submissions: Vec<Submission>,
    pub total: usize,
    pub next: Option<usize>,
    pub journal_sequence: u64,
}
