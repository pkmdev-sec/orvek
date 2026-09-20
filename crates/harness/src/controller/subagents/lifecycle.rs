//! Durable child receipts and pinned, non-authoritative inherited context.

use crate::{
    Digest, Store,
    session::{SessionCommand, SessionCursor, SessionId, SessionState},
    state::TaskId,
    store::StoreError,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMode {
    #[default]
    Isolated,
    ForkAtCursor,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextManifest {
    pub version: u32,
    pub mode: ContextMode,
    pub parent: SessionCursor,
    pub source_history: Digest,
    pub projection: Option<crate::context::Manifest>,
    pub excluded_calls: Vec<String>,
    pub input: Digest,
    pub task: TaskId,
    pub observed_generation: u64,
    pub frozen_workspace: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Spawn {
    pub agent: Uuid,
    pub request: Uuid,
    pub task: TaskId,
    pub role: String,
    pub task_text: String,
    pub model: String,
    pub output_schema: Value,
    pub context: Digest,
    pub sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub text: String,
    pub priority: String,
    pub purpose: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    SchemaValid { result: Digest },
    Unsubmitted { answer: Digest, diagnostic: String },
    Interrupted { reason: String },
    Failed { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Spawned(Spawn),
    MessageAccepted { agent: Uuid, message: Message },
    MessageConsumed { agent: Uuid, message: Uuid },
    Terminal { agent: Uuid, outcome: Outcome },
}

impl Event {
    fn operation(&self) -> Uuid {
        match self {
            Self::Spawned(spawn) => Uuid::new_v5(&spawn.agent, b"child-spawn"),
            Self::MessageAccepted { message, .. } => {
                Uuid::new_v5(&message.id, b"child-message-accepted")
            }
            Self::MessageConsumed { message, .. } => {
                Uuid::new_v5(message, b"child-message-consumed")
            }
            Self::Terminal { agent, .. } => Uuid::new_v5(agent, b"child-terminal"),
        }
    }

    pub(super) fn record(&self, store: &mut Store, session: SessionId) -> Result<(), StoreError> {
        let revision = store.load_session(session)?.revision;
        store.session_command(
            session,
            revision,
            self.operation(),
            SessionCommand::ChildLifecycle(Box::new(self.clone())),
        )?;
        Ok(())
    }
}

pub(super) fn prepare_context(
    store: &Store,
    parent: &SessionState,
    task: TaskId,
    mode: ContextMode,
) -> Result<(ContextManifest, Digest, Vec<Value>), StoreError> {
    let generation = store.load(task)?.generation;
    let (projection, input, excluded_calls) = match mode {
        ContextMode::Isolated => (None, Vec::new(), Vec::new()),
        ContextMode::ForkAtCursor => {
            // The cursor identifies the unmodified journal. The projection describes
            // the filtered fork source; source_history and excluded_calls bind the filter.
            let outputs = parent
                .history
                .iter()
                .filter(|item| item["type"] == "function_call_output")
                .filter_map(|item| item["call_id"].as_str())
                .collect::<BTreeSet<_>>();
            let excluded = parent
                .history
                .iter()
                .filter(|item| item["type"] == "function_call")
                .filter_map(|item| item["call_id"].as_str())
                .filter(|id| !outputs.contains(id))
                .map(str::to_owned)
                .collect::<BTreeSet<_>>();
            let mut source = parent.clone();
            source.settled_history_items = parent
                .history
                .iter()
                .take(parent.settled_history_items)
                .filter(|item| {
                    !item["call_id"]
                        .as_str()
                        .is_some_and(|id| excluded.contains(id))
                })
                .count();
            source.history.retain(|item| {
                !item["call_id"]
                    .as_str()
                    .is_some_and(|id| excluded.contains(id))
            });
            let limit = crate::context::projection_byte_limit(parent.context_window_tokens())
                .map_err(|_| StoreError::Invalid("child context window is invalid"))?;
            let view = crate::context::project(&source, limit)
                .map_err(|_| StoreError::Invalid("child context projection failed"))?;
            (
                Some(view.manifest),
                view.input,
                excluded.into_iter().collect(),
            )
        }
    };
    let input_digest = store.artifacts().put(&serde_json::to_vec(&input)?)?;
    let manifest = ContextManifest {
        version: 1,
        mode,
        parent: parent.cursor(),
        source_history: Digest::of_value(&parent.history)?,
        projection,
        excluded_calls,
        input: input_digest,
        task,
        observed_generation: generation,
        frozen_workspace: false,
    };
    let digest = store.artifacts().put(&serde_json::to_vec(&manifest)?)?;
    Ok((manifest, digest, input))
}
