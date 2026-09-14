use crate::{
    app::config::{ReasoningEffort, ReasoningMode},
    tui::host_projection::ViewChange,
};
use orvek_harness::session::SessionCursor;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub(crate) const SCHEMA_VERSION: u32 = 3;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub(crate) struct TurnId(u64);

impl TurnId {
    // Turn identifiers arrive from the host; only tests mint their own.
    #[cfg(test)]
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct ShellId(u64);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct SessionStarted {
    pub(crate) session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent_sequence: Option<u64>,
    pub(crate) model: String,
    pub(crate) effort: ReasoningEffort,
    pub(crate) reasoning_mode: ReasoningMode,
    pub(crate) fast_mode: bool,
    pub(crate) workspace: PathBuf,
    pub(crate) application_version: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionOutcome {
    Closed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct SessionEnded {
    pub(crate) outcome: SessionOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub(crate) enum LocalEvent {
    SessionStarted(SessionStarted),
    UserSubmitted {
        id: TurnId,
        text: String,
    },
    UserSteered {
        text: String,
    },
    ReflectionStarted {
        id: TurnId,
    },
    ShellStarted {
        id: ShellId,
        command: String,
        workspace: PathBuf,
    },
    ShellFinished {
        id: ShellId,
        output: String,
        exit_code: Option<i32>,
        duration_ns: u64,
        truncated: bool,
        error: Option<String>,
    },
    EffortChanged {
        from: ReasoningEffort,
        to: ReasoningEffort,
    },
    FastModeChanged {
        from: bool,
        to: bool,
    },
    ContextObserved {
        prompt_cache: bool,
        previous_response: bool,
    },
    WorkerTurnAccepted {
        id: TurnId,
    },
    WorkerTurnFinished {
        id: TurnId,
        error: Option<String>,
    },
    WorkerTurnsInterrupted {
        count: usize,
        error: Option<String>,
    },
    WorkerSteerFailed {
        error: String,
    },
    WorkerStopped {
        error: Option<String>,
    },
    SessionEnded(SessionEnded),
}

/// A disposable view record. Durable source identity is always a native host cursor.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct TranscriptRecord {
    schema_version: u32,
    sequence: u64,
    recorded_at_unix_ms: u64,
    cursor: Option<SessionCursor>,
    host: Vec<ViewChange>,
    local: Option<LocalEvent>,
}

impl TranscriptRecord {
    pub(crate) fn from_host(
        sequence: u64,
        recorded_at_unix_ms: u64,
        cursor: SessionCursor,
        change: ViewChange,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            sequence,
            recorded_at_unix_ms,
            cursor: Some(cursor),
            host: vec![change],
            local: None,
        }
    }
    // Batched multi-change records are only assembled by fixtures.
    #[cfg(test)]
    pub(crate) fn from_host_batch(
        sequence: u64,
        recorded_at_unix_ms: u64,
        cursor: SessionCursor,
        changes: Vec<ViewChange>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            sequence,
            recorded_at_unix_ms,
            cursor: Some(cursor),
            host: changes,
            local: None,
        }
    }
    pub(crate) fn host_changes(&self) -> &[ViewChange] {
        &self.host
    }
    // Local-echo records predate the host's own `ViewChange::User`; only tests
    // still construct them.
    #[cfg(test)]
    pub(crate) fn from_local(
        sequence: u64,
        recorded_at_unix_ms: u64,
        event: LocalEvent,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            sequence,
            recorded_at_unix_ms,
            cursor: None,
            host: Vec::new(),
            local: Some(event),
        })
    }
    pub(crate) const fn recorded_at_unix_ms(&self) -> u64 {
        self.recorded_at_unix_ms
    }
    pub(crate) fn host(&self) -> Option<&ViewChange> {
        self.host.first()
    }
    pub(crate) fn local(&self) -> Option<&LocalEvent> {
        self.local.as_ref()
    }
}
