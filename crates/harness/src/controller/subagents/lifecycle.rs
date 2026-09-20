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
            // Filtering changes source indices. Forks start from original history,
            // not a parent's derived summaries or cached representations.
            source.context_transitions.clear();
            source.context_view = None;
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

#[cfg(test)]
mod transition_tests {
    use super::*;
    use crate::{
        context::{
            HistoryRange,
            transitions::{ContextTransition, TransitionProposal},
        },
        contract::Limits,
        inference::ModelSettings,
        session::SessionConfig,
    };
    use serde_json::json;

    #[test]
    fn filtered_child_fork_drops_derived_views_and_keeps_pinned_original_source() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("state")).unwrap();
        let session = store
            .create_session(
                SessionId::new(),
                SessionConfig {
                    workspace: root.path().into(),
                    model: ModelSettings::default(),
                    instructions: String::new(),
                    context_window_tokens: crate::context::DEFAULT_WINDOW_TOKENS,
                },
                None,
            )
            .unwrap();
        let intake = store
            .artifacts()
            .put(
                &serde_json::to_vec(&crate::admission::RequestPolicy {
                    version: 1,
                    delivery: crate::contract::DeliveryKind::Source,
                    profile: crate::admission::RepositoryProfile {
                        version: 1,
                        name: "fixture".into(),
                        checks: Default::default(),
                    },
                })
                .unwrap(),
            )
            .unwrap();
        let (_, task, _) = store
            .start_request(
                session.id,
                Uuid::new_v4(),
                "goal".into(),
                Limits::default(),
                intake,
            )
            .unwrap();
        let mut parent = store.load_session(session.id).unwrap();
        parent.history = vec![
            json!({"role":"user","content":"original goal"}),
            json!({"role":"assistant","content":"original exact source 雪"}),
            json!({"type":"function_call","call_id":"pending","name":"spawn_agent","arguments":"{}"}),
        ];
        parent.settled_history_items = 2;
        parent.context_transitions.push(ContextTransition {
            source: parent.cursor(),
            source_history_items: 3,
            source_history: Digest::of_value(&parent.history).unwrap(),
            source_digest: Digest::of_value(&parent.history[..2]).unwrap(),
            request: parent.active_request.unwrap(),
            call_id: "prior".into(),
            proposal: TransitionProposal {
                range: HistoryRange { start: 0, end: 2 },
                purpose: "phase".into(),
                summary: "incorrect conclusion".into(),
                pending_obligations: vec![],
            },
        });
        let (manifest, _, input) =
            prepare_context(&store, &parent, task.id, ContextMode::ForkAtCursor).unwrap();
        assert_eq!(input, parent.history[..2]);
        assert_eq!(manifest.parent, parent.cursor());
        assert_eq!(
            manifest.source_history,
            Digest::of_value(&parent.history).unwrap()
        );
        assert_eq!(manifest.excluded_calls, vec!["pending"]);
        assert_ne!(
            manifest.projection.unwrap().original_history,
            manifest.source_history
        );
        let (manifest, _, input) =
            prepare_context(&store, &parent, task.id, ContextMode::Isolated).unwrap();
        assert!(input.is_empty());
        assert!(manifest.projection.is_none());
    }
}
