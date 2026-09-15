//! Bounded visual projection. All task and execution facts originate in host records.

use super::{
    DirectedMessageEntry, EntryId, EntryKind, LocalEvent, MessageDelivery, ToolEntry, ToolState,
    TranscriptEntry, TranscriptRecord, TransientStatus,
};
use crate::{
    app::config::ReasoningEffort,
    tui::{
        children::{
            MessageDeliveryState, MessageDisposition, MessageOrigin, MessageUpdate, ThreadId,
        },
        host_projection::ViewChange,
    },
};
use orvek_harness::{
    inference::Thinking,
    state::{JobStatus, Outcome, TaskEvent},
};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

const MAX_RETAINED_MESSAGE_THREADS: usize = 256;
const MAX_ENTRIES: usize = 2048;
const MAX_TEXT_BYTES: usize = 64 * 1024;

type MessageKey = (Option<Uuid>, String);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ModelChange {
    pub(crate) changed: bool,
    pub(crate) removed: Vec<EntryId>,
}

#[derive(Default)]
pub(crate) struct TranscriptModel {
    entries: Vec<TranscriptEntry>,
    entry_indices: HashMap<EntryId, usize>,
    next_entry_id: usize,
    assistants: HashMap<MessageKey, EntryId>,
    reasoning: HashMap<MessageKey, EntryId>,
    users: HashMap<Uuid, EntryId>,
    tools: HashMap<String, EntryId>,
    tool_items: HashMap<MessageKey, EntryId>,
    pending_tool_previews: HashSet<EntryId>,
    jobs: HashMap<Uuid, EntryId>,
    active_requests: HashMap<Uuid, u64>,
    running_tools: HashSet<EntryId>,
    transient: Option<TransientStatus>,
    message_threads: HashMap<ThreadId, EntryId>,
    message_order: VecDeque<ThreadId>,
    evicted: Vec<EntryId>,
    discarded_previews: Vec<EntryId>,
    history_truncated: bool,
}

impl TranscriptModel {
    pub(crate) fn entries(&self) -> &[TranscriptEntry] {
        &self.entries
    }
    pub(crate) fn entry(&self, id: EntryId) -> Option<&TranscriptEntry> {
        self.index_of(id).and_then(|index| self.entries.get(index))
    }
    pub(crate) fn index_of(&self, id: EntryId) -> Option<usize> {
        self.entry_indices.get(&id).copied()
    }
    pub(crate) fn transient(&self) -> Option<&TransientStatus> {
        self.transient.as_ref()
    }
    pub(crate) fn is_active(&self) -> bool {
        !self.active_requests.is_empty() || !self.running_tools.is_empty()
    }
    pub(crate) fn has_running_tools(&self) -> bool {
        !self.running_tools.is_empty()
    }
    pub(crate) fn running_tool_ids(&self) -> impl Iterator<Item = EntryId> + '_ {
        self.running_tools.iter().copied()
    }

    pub(crate) fn fork_snapshot(&self) -> Self {
        let end = if self.is_active() {
            self.entries
                .iter()
                .rposition(|entry| matches!(entry.kind, EntryKind::User { .. }))
                .unwrap_or(self.entries.len())
        } else {
            self.entries.len()
        };
        let entries = self.entries[..end]
            .iter()
            .filter(|entry| match &entry.kind {
                EntryKind::Assistant { complete, .. } => *complete,
                EntryKind::Tool(tool) => {
                    !matches!(tool.state, ToolState::Running | ToolState::Proposed)
                }
                _ => true,
            })
            .cloned()
            .collect::<Vec<_>>();
        Self {
            entry_indices: entries
                .iter()
                .enumerate()
                .map(|(i, entry)| (entry.id, i))
                .collect(),
            entries,
            next_entry_id: self.next_entry_id,
            ..Self::default()
        }
    }

    pub(crate) fn apply(&mut self, record: &TranscriptRecord) -> ModelChange {
        let mut changed = false;
        for change in record.host_changes() {
            changed |= self.apply_host(change, record.recorded_at_unix_ms());
        }
        if record.host_changes().is_empty() {
            changed |= record.local().is_some_and(|event| self.apply_local(event));
        }
        if !self.evicted.is_empty() && !self.history_truncated {
            self.history_truncated = true;
            self.push(EntryKind::HostStatus {
                text: "Earlier display rows were unloaded; their originals remain in host history."
                    .into(),
                verified: false,
            });
        }
        self.evicted.append(&mut self.discarded_previews);
        ModelChange {
            changed,
            removed: std::mem::take(&mut self.evicted),
        }
    }

    fn apply_host(&mut self, change: &ViewChange, at_ms: u64) -> bool {
        match change {
            ViewChange::ProviderUsage { .. }
            | ViewChange::WorkspaceSaved(_)
            | ViewChange::QueueChanged
            | ViewChange::Submission(_)
            | ViewChange::SubmissionChanged { .. }
            | ViewChange::ShellStarted { .. }
            | ViewChange::ShellPublished { .. }
            | ViewChange::ReviewRecorded { .. } => return false,
            ViewChange::HistoricalImport {
                manifest,
                source_session,
            } => {
                self.push(EntryKind::HostStatus {text:format!("Historical context from {source_session} · manifest {manifest} · previous results are unverified"),verified:false});
            }
            ViewChange::Session { parent, .. } => {
                if let Some(parent) = parent {
                    self.push(EntryKind::ForkedFrom {
                        session_id: parent.session.to_string(),
                    });
                }
            }
            ViewChange::User { request, text } => {
                if request.is_some_and(|id| self.users.contains_key(&id)) {
                    return false;
                }
                let id = self.push(EntryKind::User {
                    text: bounded(text),
                });
                if let Some(request) = request {
                    self.users.insert(*request, id);
                }
            }
            ViewChange::RequestStarted { request } => {
                self.active_requests.insert(*request, at_ms);
                self.transient = Some(TransientStatus::Thinking);
            }
            ViewChange::Assistant {
                request,
                item,
                text,
                replace,
                confirmed,
            } => {
                let key = (*request, item.clone());
                let id = match self.assistants.get(&key).copied() {
                    Some(id) => id,
                    None => {
                        let id = self.push(EntryKind::Assistant {
                            text: String::new(),
                            complete: false,
                        });
                        self.assistants.insert(key, id);
                        id
                    }
                };
                self.update(id, |kind| {
                    if let EntryKind::Assistant {
                        text: current,
                        complete,
                    } = kind
                    {
                        if *replace {
                            *current = bounded(text);
                        } else {
                            append_bounded(current, text);
                        }
                        *complete = *confirmed;
                    }
                });
                self.transient = self.is_active().then_some(if *confirmed {
                    TransientStatus::Thinking
                } else {
                    TransientStatus::Responding
                });
            }
            ViewChange::Reasoning {
                request,
                item,
                text,
                replace,
            } => {
                if text.is_empty() {
                    return false;
                }
                let key = (*request, item.clone());
                let id = match self.reasoning.get(&key).copied() {
                    Some(id) => id,
                    None => {
                        let id = self.push(EntryKind::Reasoning {
                            text: String::new(),
                        });
                        self.reasoning.insert(key, id);
                        id
                    }
                };
                self.update(id, |kind| {
                    if let EntryKind::Reasoning { text: current } = kind {
                        if *replace {
                            *current = bounded(text);
                        } else {
                            append_bounded(current, text);
                        }
                    }
                });
            }
            ViewChange::ToolProposed {
                request,
                item_id,
                call_id,
                name,
                arguments,
            } => {
                if let Some(id) = self.tools.get(call_id).copied() {
                    // Replay can deliver the execution before its provider proposal.
                    // Fill missing input without resetting the execution or its output.
                    self.update(id, |kind| {
                        if let EntryKind::Tool(tool) = kind
                            && tool.arguments.is_null()
                        {
                            tool.arguments = serde_json::from_str(arguments)
                                .unwrap_or_else(|_| Value::String(bounded(arguments)));
                        }
                    });
                    return true;
                }
                let tool = ToolEntry {
                    name: name.clone(),
                    arguments: serde_json::from_str(&bounded(arguments))
                        .unwrap_or_else(|_| Value::String(bounded(arguments))),
                    started_at_unix_ms: at_ms,
                    state: ToolState::Proposed,
                    duration_ns: None,
                    result: None,
                    metadata: None,
                    substeps: Vec::new(),
                    child_count: 0,
                };
                // Provider item IDs identify preview streams, not execution calls.
                let key = item_id.as_ref().map(|item| (*request, item.clone()));
                let preview = key
                    .as_ref()
                    .and_then(|key| self.tool_items.get(key))
                    .copied();
                let id = if let Some(id) = preview {
                    self.pending_tool_previews.remove(&id);
                    self.update(id, |kind| *kind = EntryKind::Tool(tool));
                    id
                } else {
                    self.push(EntryKind::Tool(tool))
                };
                if let Some(key) = key {
                    self.tool_items.insert(key, id);
                }
                self.tools.insert(call_id.clone(), id);
            }
            ViewChange::ToolArguments {
                request,
                item_id,
                chunk,
            } => {
                if chunk.is_empty() {
                    return false;
                }
                let key = (*request, item_id.clone());
                let id = match self.tool_items.get(&key).copied() {
                    Some(id) if !self.pending_tool_previews.contains(&id) => return false,
                    Some(id) => id,
                    None => {
                        let id = self.push(EntryKind::Tool(ToolEntry {
                            name: "tool".to_owned(),
                            arguments: Value::String(String::new()),
                            started_at_unix_ms: at_ms,
                            state: ToolState::Proposed,
                            duration_ns: None,
                            result: None,
                            metadata: None,
                            substeps: Vec::new(),
                            child_count: 0,
                        }));
                        self.tool_items.insert(key, id);
                        self.pending_tool_previews.insert(id);
                        id
                    }
                };
                self.update(id, |kind| {
                    if let EntryKind::Tool(tool) = kind
                        && let Value::String(current) = &mut tool.arguments
                    {
                        current.push_str(chunk);
                        *current = bounded(current);
                    }
                });
            }
            ViewChange::ToolResult {
                call_id, output, ..
            } => {
                if let Some(id) = self.tools.get(call_id).copied() {
                    self.update(id, |kind| {
                        if let EntryKind::Tool(tool) = kind {
                            if tool.state == ToolState::Proposed {
                                tool.state = ToolState::Received;
                            }
                            let parsed = serde_json::from_str(&bounded(output))
                                .unwrap_or_else(|_| Value::String(bounded(output)));
                            let (result, elapsed) = display_tool_result(&tool.name, parsed);
                            tool.result = Some(result);
                            tool.duration_ns = elapsed;
                        }
                    });
                }
            }
            ViewChange::RequestSettled { request, error } => {
                self.discard_tool_previews(Some(*request));
                let started = self.active_requests.remove(request);
                if let Some(error) = error {
                    self.push(EntryKind::Error {
                        message: bounded(error),
                    });
                } else {
                    self.push(EntryKind::TurnCompleted {
                        duration_ns: started
                            .filter(|started| *started > 0)
                            .map(|started| at_ms.saturating_sub(started).saturating_mul(1_000_000))
                            .unwrap_or(0),
                    });
                }
                self.transient = self.is_active().then_some(TransientStatus::Thinking);
            }
            ViewChange::Settings(settings) => {
                self.push(EntryKind::EffortChanged {
                    to: effort(settings.thinking),
                });
                self.push(EntryKind::FastModeChanged {
                    enabled: settings.fast_mode,
                });
            }
            ViewChange::TaskLinked(task) => {
                self.push(EntryKind::HostStatus {
                    text: format!("Task {task} registered with the host"),
                    verified: false,
                });
            }
            ViewChange::TaskInput { job, arguments } => {
                if let Some(entry) = self.jobs.get(job).copied() {
                    self.update(entry, |kind| {
                        if let EntryKind::Tool(tool) = kind {
                            tool.arguments = arguments.clone();
                        }
                    });
                }
            }
            ViewChange::Task { id, event } => {
                if let TaskEvent::JobStarted(job) = event {
                    if let Some(invocation) = &job.invocation
                        && let Some(call_id) = &invocation.call_id
                    {
                        let entry = self.tools.get(call_id).copied().unwrap_or_else(|| {
                            let entry = self.push(EntryKind::Tool(ToolEntry {
                                name: invocation.capability.clone(),
                                arguments: Value::Null,
                                started_at_unix_ms: job.started_ms,
                                state: ToolState::Running,
                                duration_ns: None,
                                result: None,
                                metadata: Some(
                                    serde_json::json!({"input_artifact":invocation.input}),
                                ),
                                substeps: Vec::new(),
                                child_count: 0,
                            }));
                            self.tools.insert(call_id.clone(), entry);
                            entry
                        });
                        self.jobs.insert(job.id, entry);
                        self.running_tools.insert(entry);
                        self.transient = Some(if invocation.capability == "wait" {
                            TransientStatus::WaitingForBackgroundWork
                        } else {
                            TransientStatus::Tool(crate::tui::format::humanize_tool(
                                &invocation.capability,
                            ))
                        });
                        self.update(entry, |kind| {
                            if let EntryKind::Tool(tool) = kind {
                                tool.state = ToolState::Running;
                                tool.started_at_unix_ms = job.started_ms;
                            }
                        });
                    }
                    return true;
                }
                if let TaskEvent::JobSettled { id, status, .. } = event {
                    if let Some(entry) = self.jobs.get(id).copied() {
                        let state = match status {
                            JobStatus::Running => ToolState::Running,
                            JobStatus::Succeeded => ToolState::Succeeded,
                            JobStatus::Failed => ToolState::Failed,
                            JobStatus::Cancelled => ToolState::Cancelled,
                            JobStatus::Unknown => ToolState::Unknown,
                            JobStatus::Fenced => ToolState::Fenced,
                        };
                        self.update(entry, |kind| {
                            if let EntryKind::Tool(tool) = kind {
                                tool.state = state;
                            }
                        });
                        if !status.unresolved() {
                            self.running_tools.remove(&entry);
                        }
                        self.transient = if *status == JobStatus::Unknown {
                            Some(TransientStatus::Error(
                                "Execution outcome unknown; host reconciliation required".into(),
                            ))
                        } else {
                            self.is_active().then_some(TransientStatus::Thinking)
                        };
                    }
                    return true;
                }
                if let TaskEvent::JobFenced { id, receipt } = event {
                    if let Some(entry) = self.jobs.get(id).copied() {
                        self.update(entry, |kind| {
                            if let EntryKind::Tool(tool) = kind {
                                tool.state = ToolState::Fenced;
                                tool.metadata = Some(serde_json::json!({"fence_receipt":receipt}));
                            }
                        });
                        self.running_tools.remove(&entry);
                    }
                    return true;
                }
                let (text, verified) = match event {
                    TaskEvent::ContractAdmitted { contract, .. }
                    | TaskEvent::ContractAmended { contract, .. } => (
                        format!(
                            "Task {id}: {} required outcomes; {} acceptance checks",
                            contract.requirements.len(),
                            contract.checks.len()
                        ),
                        false,
                    ),
                    TaskEvent::PhaseChanged(phase) => (format!("Task {id}: {phase:?}"), false),
                    TaskEvent::Completed(certificate) => (
                        format!(
                            "Task {id} complete · artifact {} · {} accepted evidence records",
                            certificate.artifact,
                            certificate.evidence.len()
                        ),
                        true,
                    ),
                    TaskEvent::Stopped {
                        outcome: Outcome::FinishedUnverified,
                        ..
                    } => return false,
                    TaskEvent::Stopped { outcome, reason } => (
                        format!("Task {id}: {} · {reason}", outcome_name(*outcome)),
                        false,
                    ),
                    TaskEvent::EvidenceInvalidated { reason }
                    | TaskEvent::WorkspaceChanged { reason } => {
                        (format!("Task {id}: evidence stale · {reason}"), false)
                    }
                    TaskEvent::Delivered(delivery) => (
                        format!(
                            "Task {id}: {:?} artifact prepared · {}",
                            delivery.kind, delivery.artifact
                        ),
                        false,
                    ),
                    TaskEvent::Interrupted { reason } | TaskEvent::Reopened { reason } => {
                        (format!("Task {id}: {reason}"), false)
                    }
                    _ => return false,
                };
                self.push(EntryKind::HostStatus {
                    text: bounded(&text),
                    verified,
                });
            }
            ViewChange::ContextProjected {
                source_revision,
                items,
            } => {
                self.push(EntryKind::HostStatus {text:format!("Context projected from revision {source_revision} · {items} items · original history retained"),verified:false});
            }
            ViewChange::DiscardPreviews => {
                self.discard_tool_previews(None);
                let pending = self.assistants.values().copied().collect::<Vec<_>>();
                for id in pending {
                    self.update(id, |kind| {
                        if let EntryKind::Assistant {
                            text,
                            complete: false,
                        } = kind
                        {
                            *text =
                                "[Preview interrupted; waiting for the recorded response]".into();
                        }
                    });
                }
            }
            ViewChange::Warning(message) => {
                self.push(EntryKind::Error {
                    message: bounded(message),
                });
            }
            ViewChange::Status(text) => {
                self.push(EntryKind::HostStatus {
                    text: bounded(text),
                    verified: false,
                });
            }
        }
        true
    }

    pub(crate) fn apply_message(
        &mut self,
        perspective: MessageOrigin,
        update: MessageUpdate,
    ) -> ModelChange {
        let Some(id) = self.message_threads.get(&update.thread.id).copied() else {
            let thread_id = update.thread.id;
            let id = self.push(EntryKind::DirectedMessage(DirectedMessageEntry {
                perspective,
                thread: update.thread,
                deliveries: vec![MessageDelivery {
                    message_id: update.message_id,
                    state: update.delivery,
                }],
            }));
            self.message_threads.insert(thread_id, id);
            self.message_order.push_back(thread_id);
            return ModelChange {
                changed: true,
                removed: self.trim_message_history().into_iter().collect(),
            };
        };

        let Some(index) = self.index_of(id) else {
            return ModelChange::default();
        };
        let EntryKind::DirectedMessage(message) = &self.entries[index].kind else {
            return ModelChange::default();
        };
        let previous_delivery = message
            .deliveries
            .iter()
            .find(|delivery| delivery.message_id == update.message_id);
        let changed = message.thread != update.thread
            || previous_delivery
                .is_none_or(|delivery| delivery_advances(&delivery.state, &update.delivery));
        if !changed {
            return ModelChange::default();
        }

        self.reasoning.clear();
        let EntryKind::DirectedMessage(message) = &mut self.entries[index].kind else {
            return ModelChange::default();
        };
        message.thread = update.thread;
        message.deliveries.retain(|delivery| {
            message
                .thread
                .messages
                .iter()
                .any(|retained| retained.id == delivery.message_id)
        });
        let delivery = message
            .deliveries
            .iter_mut()
            .find(|delivery| delivery.message_id == update.message_id);
        match delivery {
            Some(delivery) if delivery_advances(&delivery.state, &update.delivery) => {
                delivery.state = update.delivery;
            }
            None => message.deliveries.push(MessageDelivery {
                message_id: update.message_id,
                state: update.delivery,
            }),
            Some(_) => {}
        }
        self.entries[index].revision = self.entries[index].revision.saturating_add(1);
        ModelChange {
            changed: true,
            removed: self.trim_message_history().into_iter().collect(),
        }
    }

    fn trim_message_history(&mut self) -> Option<EntryId> {
        if self.message_order.len() <= MAX_RETAINED_MESSAGE_THREADS {
            return None;
        }
        let position = self.message_order.iter().position(|thread_id| {
            let Some(id) = self.message_threads.get(thread_id) else {
                return true;
            };
            let Some(entry) = self.entry(*id) else {
                return true;
            };
            let EntryKind::DirectedMessage(message) = &entry.kind else {
                return true;
            };
            !message.deliveries.iter().any(|delivery| {
                matches!(
                    delivery.state,
                    MessageDeliveryState::Admitted {
                        disposition: MessageDisposition::Queued
                    }
                )
            })
        })?;
        let thread_id = self
            .message_order
            .remove(position)
            .expect("the retained message thread should still exist");
        let id = self.message_threads.remove(&thread_id)?;
        let removed_index = self.entry_indices.remove(&id)?;
        self.entries.remove(removed_index);
        for (index, entry) in self.entries.iter().enumerate().skip(removed_index) {
            self.entry_indices.insert(entry.id, index);
        }
        Some(id)
    }

    fn apply_local(&mut self, event: &LocalEvent) -> bool {
        match event {
            LocalEvent::UserSubmitted { text, .. } | LocalEvent::UserSteered { text } => {
                self.push(EntryKind::User {
                    text: bounded(text),
                });
                true
            }
            LocalEvent::SessionStarted(start) => {
                if let Some(parent) = &start.parent_session_id {
                    *self = self.fork_snapshot();
                    self.push(EntryKind::ForkedFrom {
                        session_id: parent.clone(),
                    });
                    true
                } else {
                    false
                }
            }
            LocalEvent::EffortChanged { to, .. } => {
                self.push(EntryKind::EffortChanged { to: *to });
                true
            }
            LocalEvent::FastModeChanged { to, .. } => {
                self.push(EntryKind::FastModeChanged { enabled: *to });
                true
            }
            LocalEvent::ReflectionStarted { .. } => {
                self.push(EntryKind::ReflectionStarted);
                true
            }

            LocalEvent::WorkerSteerFailed { error }
            | LocalEvent::WorkerStopped { error: Some(error) } => {
                self.push(EntryKind::Error {
                    message: bounded(error),
                });
                true
            }
            LocalEvent::SessionEnded(_) => {
                self.transient = Some(TransientStatus::Reconnecting);
                true
            }
            // Local notices only affect presentation. Execution and task outcomes require host records.
            _ => false,
        }
    }

    pub(crate) fn view_disconnected(&mut self) -> bool {
        self.transient = Some(TransientStatus::Reconnecting);
        true
    }

    fn push(&mut self, kind: EntryKind) -> EntryId {
        if self.entries.len() >= MAX_ENTRIES {
            let removed = self.entries.remove(0).id;
            self.evicted.push(removed);
            self.users.retain(|_, id| *id != removed);
            self.assistants.retain(|_, id| *id != removed);
            self.reasoning.retain(|_, id| *id != removed);
            self.tools.retain(|_, id| *id != removed);
            self.tool_items.retain(|_, id| *id != removed);
            self.pending_tool_previews.remove(&removed);
            self.jobs.retain(|_, id| *id != removed);
            self.running_tools.remove(&removed);
            self.entry_indices = self
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| (e.id, i))
                .collect();
        }
        let id = EntryId::from_index(self.next_entry_id);
        self.next_entry_id = self.next_entry_id.saturating_add(1);
        self.entry_indices.insert(id, self.entries.len());
        self.entries.push(TranscriptEntry {
            id,
            revision: 0,
            kind,
            hidden: false,
            parent: None,
            trailing_spacer: true,
        });
        id
    }

    fn discard_tool_previews(&mut self, request: Option<Uuid>) {
        let removed = self
            .tool_items
            .iter()
            .filter(|((owner, _), id)| {
                request.is_none_or(|request| *owner == Some(request))
                    && self.pending_tool_previews.contains(id)
            })
            .map(|(_, id)| *id)
            .collect::<HashSet<_>>();
        if removed.is_empty() {
            return;
        }
        self.entries.retain(|entry| !removed.contains(&entry.id));
        self.tool_items.retain(|_, id| !removed.contains(id));
        self.pending_tool_previews
            .retain(|id| !removed.contains(id));
        self.entry_indices = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id, index))
            .collect();
        self.discarded_previews.extend(removed);
    }

    fn update(&mut self, id: EntryId, change: impl FnOnce(&mut EntryKind)) {
        if let Some(index) = self.index_of(id) {
            change(&mut self.entries[index].kind);
            self.entries[index].revision = self.entries[index].revision.saturating_add(1);
        }
    }
}

fn bounded(text: &str) -> String {
    if text.len() <= MAX_TEXT_BYTES {
        return text.into();
    }
    let mut end = MAX_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[Display truncated; full data remains in host history]",
        &text[..end]
    )
}
fn append_bounded(current: &mut String, text: &str) {
    if current.len() > MAX_TEXT_BYTES {
        return;
    }
    current.push_str(text);
    if current.len() > MAX_TEXT_BYTES {
        *current = bounded(current);
    }
}
fn effort(value: Thinking) -> ReasoningEffort {
    match value {
        Thinking::Low => ReasoningEffort::Low,
        Thinking::Medium => ReasoningEffort::Medium,
        Thinking::High => ReasoningEffort::High,
        Thinking::Xhigh => ReasoningEffort::Xhigh,
        Thinking::Max => ReasoningEffort::Max,
    }
}
fn outcome_name(value: Outcome) -> &'static str {
    match value {
        Outcome::Complete => "complete",
        Outcome::DeliveredWithExceptions => "delivered with exceptions",
        Outcome::Blocked => "blocked",
        Outcome::BudgetExhausted => "budget exhausted",
        Outcome::Cancelled => "cancelled",
        Outcome::Failed => "failed",
        Outcome::FinishedUnverified => "finished unverified",
    }
}
fn delivery_advances(current: &MessageDeliveryState, next: &MessageDeliveryState) -> bool {
    current != next && matches!(current, MessageDeliveryState::Admitted { .. })
}

fn display_tool_result(name: &str, value: Value) -> (Value, Option<u64>) {
    let value = if value.get("task_id").is_some()
        && value.get("job_id").is_some()
        && value.get("generation").is_some()
    {
        value.get("result").cloned().unwrap_or(value)
    } else {
        value
    };
    if name != "exec_command" || value.get("status").and_then(|v| v.get("kind")).is_none() {
        return (value, None);
    }
    let decode = |part: &Value| match part["encoding"].as_str() {
        Some("utf8") => part["data"].as_str().unwrap_or_default().to_owned(),
        Some("base64") => format!(
            "[Binary output: {} bytes; original data is in the host receipt]",
            part["bytes"]
        ),
        _ => String::new(),
    };
    let output = [decode(&value["stdout"]), decode(&value["stderr"])]
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let elapsed = value["elapsed_ms"].as_u64();
    let code = (value["status"]["kind"] == "exited")
        .then(|| value["status"]["code"].as_i64())
        .flatten();
    let rendered = serde_json::json!({"output":output,"exit_code":code,"truncated":value["output_truncated"],"status":value["status"],"detail":value["detail"],"wall_time_seconds":elapsed.map(|value|value as f64/1000.0)});
    (
        rendered,
        elapsed.map(|value| value.saturating_mul(1_000_000)),
    )
}

#[cfg(test)]
mod tool_preview_tests {
    use super::*;
    use crate::tui::host_projection::history_items;
    use orvek_harness::session::{SessionCursor, SessionId};
    use serde_json::json;

    fn apply(model: &mut TranscriptModel, changes: Vec<ViewChange>) -> ModelChange {
        model.apply(&TranscriptRecord::from_host_batch(
            1,
            1,
            SessionCursor {
                version: 1,
                session: SessionId::new(),
                revision: 1,
            },
            changes,
        ))
    }

    fn preview(request: Uuid, item: &str, chunk: &str) -> ViewChange {
        ViewChange::ToolArguments {
            request: Some(request),
            item_id: item.into(),
            chunk: chunk.into(),
        }
    }

    #[test]
    fn streamed_arguments_reconcile_provider_item_with_execution_call() {
        let mut model = TranscriptModel::default();
        let request = Uuid::new_v4();
        apply(&mut model, vec![preview(request, "fc-1", "{\"command\":")]);
        let preview_id = model.entries()[0].id;
        apply(&mut model, vec![preview(request, "fc-1", "\"pwd\"}")]);
        let EntryKind::Tool(tool) = &model.entries()[0].kind else {
            panic!("expected tool")
        };
        assert_eq!(tool.arguments, json!("{\"command\":\"pwd\"}"));
        apply(
            &mut model,
            history_items(
                Some(request),
                &[json!({
                    "type": "function_call", "id": "fc-1", "call_id": "call-1",
                    "name": "exec_command", "arguments": "{\"command\":\"pwd\"}"
                })],
            ),
        );
        assert_eq!(model.entries().len(), 1);
        assert_eq!(model.entries()[0].id, preview_id);
        assert!(!apply(&mut model, vec![preview(request, "fc-1", "late")]).changed);
        apply(
            &mut model,
            vec![ViewChange::ToolResult {
                request: Some(request),
                call_id: "call-1".into(),
                output: "done".into(),
            }],
        );
        let EntryKind::Tool(tool) = &model.entries()[0].kind else {
            panic!("expected tool")
        };
        assert_eq!(tool.name, "exec_command");
        assert_eq!(tool.arguments, json!({"command":"pwd"}));
        assert!(tool.result.is_some());
    }

    #[test]
    fn preview_gap_removes_only_unconfirmed_tools_and_replay_restores_them() {
        let mut model = TranscriptModel::default();
        let request = Uuid::new_v4();
        let recorded = history_items(
            Some(request),
            &[json!({
                "type":"function_call", "id":"fc-recorded", "call_id":"call-recorded",
                "name":"read_file", "arguments":"{}"
            })],
        );
        apply(&mut model, recorded);
        apply(&mut model, vec![preview(request, "fc-pending", "partial")]);
        let preview_id = model.entries()[1].id;
        let change = apply(&mut model, vec![ViewChange::DiscardPreviews]);
        assert_eq!(change.removed, vec![preview_id]);
        assert_eq!(model.entries().len(), 1);
        apply(
            &mut model,
            history_items(
                Some(request),
                &[json!({
                    "type":"function_call", "id":"fc-pending", "call_id":"call-pending",
                    "name":"read_file", "arguments":"{}"
                })],
            ),
        );
        assert_eq!(model.entries().len(), 2);
        assert!(
            model
                .entries()
                .iter()
                .all(|entry| matches!(entry.kind, EntryKind::Tool(_)))
        );
    }

    #[test]
    fn finished_unverified_is_not_rendered() {
        let mut model = TranscriptModel::default();

        let change = apply(
            &mut model,
            vec![ViewChange::Task {
                id: orvek_harness::state::TaskId::new(),
                event: TaskEvent::Stopped {
                    outcome: Outcome::FinishedUnverified,
                    reason: "Finished on the native host without verification evidence".into(),
                },
            }],
        );

        assert!(!change.changed);
        assert!(model.entries().is_empty());
    }

    #[test]
    fn settling_request_discards_its_previews_without_touching_other_requests() {
        let mut model = TranscriptModel::default();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        apply(
            &mut model,
            vec![
                preview(first, "same-item", "one"),
                preview(second, "same-item", "two"),
            ],
        );
        let second_id = model.entries()[1].id;
        apply(
            &mut model,
            vec![ViewChange::RequestSettled {
                request: first,
                error: None,
            }],
        );
        let tools = model
            .entries()
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::Tool(_)))
            .collect::<Vec<_>>();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].id, second_id);
    }
}
