//! Disposable presentation changes derived from the authenticated host journal.
//! This module cannot execute tools, persist domain state, or certify a task.

use orvek_harness::{
    inference::{Delta, ModelSettings},
    ipc::WatchFrame,
    session::{
        JournalRecord, SessionCommand, SessionConfig, SessionCursor, SessionEvent, SessionId,
    },
    state::{TaskEvent, TaskId},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub(crate) enum ViewChange {
    WorkspaceSaved(orvek_harness::session::WorkspaceSeed),
    QueueChanged,
    Submission(orvek_harness::submission::Submission),
    SubmissionChanged {
        request: Uuid,
        status: orvek_harness::submission::SubmissionStatus,
    },
    HistoricalImport {
        manifest: orvek_harness::Digest,
        source_session: String,
    },
    ProviderUsage {
        usage: orvek_harness::inference::Usage,
    },
    Session {
        config: SessionConfig,
        parent: Option<SessionCursor>,
        started_ms: u64,
    },
    User {
        request: Option<Uuid>,
        text: String,
    },
    RequestStarted {
        request: Uuid,
    },
    Assistant {
        request: Option<Uuid>,
        item: String,
        text: String,
        replace: bool,
        confirmed: bool,
    },
    Reasoning {
        request: Option<Uuid>,
        item: String,
        text: String,
        replace: bool,
    },
    ToolProposed {
        request: Option<Uuid>,
        #[serde(default)]
        item_id: Option<String>,
        call_id: String,
        name: String,
        arguments: String,
    },
    ToolArguments {
        request: Option<Uuid>,
        item_id: String,
        chunk: String,
    },
    ToolResult {
        request: Option<Uuid>,
        call_id: String,
        output: String,
    },
    RequestSettled {
        request: Uuid,
        error: Option<String>,
    },
    Settings(ModelSettings),
    TaskLinked(TaskId),
    ShellStarted {
        request: Uuid,
    },
    ShellPublished {
        request: Uuid,
        report: orvek_harness::Digest,
    },
    ReviewRecorded {
        feedback: orvek_harness::Digest,
    },
    Task {
        id: TaskId,
        event: TaskEvent,
    },
    ContextProjected {
        source_revision: u64,
        items: usize,
    },
    DiscardPreviews,
    Warning(String),
    Status(String),
}

pub(crate) struct HostProjection {
    session: SessionId,
    sequence: u64,
    revision: u64,
    recorded_ms: u64,
    active_request: Option<Uuid>,
    auxiliary: BTreeMap<Uuid, bool>,
    tasks: BTreeSet<TaskId>,
    task_order: VecDeque<TaskId>,
}

impl HostProjection {
    pub(crate) fn new(session: SessionId, after: u64) -> Self {
        Self {
            session,
            sequence: after,
            revision: 0,
            recorded_ms: 0,
            active_request: None,
            auxiliary: BTreeMap::new(),
            tasks: BTreeSet::new(),
            task_order: VecDeque::new(),
        }
    }

    pub(crate) fn classify_auxiliary(&mut self, request: Uuid, visible: bool) {
        self.auxiliary.insert(request, visible);
        if !visible && self.active_request == Some(request) {
            self.active_request = None;
        }
        while self.auxiliary.len() > 128 {
            self.auxiliary.pop_first();
        }
    }
    pub(crate) fn visible_request(&self, request: Uuid) -> bool {
        self.auxiliary.get(&request) != Some(&false)
    }

    pub(crate) fn at_snapshot(view: &orvek_harness::ipc::SessionView) -> Self {
        let mut projection = Self::new(view.id, view.journal_sequence);
        projection.revision = view.revision;
        projection.active_request = view.active_request;
        if let Some(task) = view.current_task {
            projection.tasks.insert(task);
            projection.task_order.push_back(task);
        }
        projection
    }

    pub(crate) const fn recorded_ms(&self) -> u64 {
        self.recorded_ms
    }
    pub(crate) const fn sequence(&self) -> u64 {
        self.sequence
    }
    pub(crate) fn cursor(&self) -> SessionCursor {
        SessionCursor {
            version: 1,
            session: self.session,
            revision: self.revision,
        }
    }

    pub(crate) fn apply(&mut self, frame: WatchFrame) -> Vec<ViewChange> {
        match frame {
            WatchFrame::Journal(record) => self.journal(record),
            // Subagent lifecycle is presentation state, not journal projection;
            // the client intercepts these frames before applying the rest.
            WatchFrame::Subagent { .. } => Vec::new(),
            WatchFrame::Preview {
                session,
                request,
                delta,
            } if session == self.session && self.active_request == Some(request) => {
                match delta {
                    Delta::Text { item_id, text } => vec![ViewChange::Assistant {
                        request: Some(request),
                        item: if self.auxiliary.contains_key(&request) {
                            format!("auxiliary-{request}")
                        } else {
                            item_id
                        },
                        text,
                        replace: false,
                        confirmed: false,
                    }],
                    Delta::ReasoningSummary { item_id, text } => vec![ViewChange::Reasoning {
                        request: Some(request),
                        item: item_id,
                        text,
                        replace: false,
                    }],
                    // Preview item identity is reconciled with execution call identity
                    // only when the journal records the complete proposal.
                    Delta::ToolArguments { item_id, arguments } => {
                        vec![ViewChange::ToolArguments {
                            request: Some(request),
                            item_id,
                            chunk: arguments,
                        }]
                    }
                    Delta::Created { .. } | Delta::ItemDone { .. } => Vec::new(),
                }
            }
            WatchFrame::PreviewGap { .. } => vec![ViewChange::DiscardPreviews],
            WatchFrame::Preview { .. } | WatchFrame::Ready { .. } => Vec::new(),
        }
    }

    fn journal(&mut self, record: JournalRecord) -> Vec<ViewChange> {
        if record.sequence <= self.sequence {
            return Vec::new();
        }
        let changes = if record.kind == "session" && record.aggregate == self.session.to_string() {
            self.revision = record.revision;
            match serde_json::from_value::<SessionEvent>(record.event) {
                Ok(event) => self.session_event(event),
                Err(error) => vec![ViewChange::Warning(format!(
                    "Cannot display host session event: {error}"
                ))],
            }
        } else if record.kind == "task" {
            let task = Uuid::parse_str(&record.aggregate).ok().map(TaskId);
            match task.filter(|id| self.tasks.contains(id)) {
                Some(id) => match serde_json::from_value::<TaskEvent>(record.event) {
                    Ok(event) => vec![ViewChange::Task { id, event }],
                    Err(error) => vec![ViewChange::Warning(format!(
                        "Cannot display host task event: {error}"
                    ))],
                },
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };
        self.sequence = record.sequence;
        changes
    }

    fn session_event(&mut self, event: SessionEvent) -> Vec<ViewChange> {
        match event {
            SessionEvent::Created {
                config,
                parent,
                history,
                at_ms,
                imported,
                ..
            } => {
                self.recorded_ms = at_ms;
                let mut changes = vec![ViewChange::Session {
                    config,
                    parent,
                    started_ms: at_ms,
                }];
                if let Some(imported) = imported {
                    changes.push(ViewChange::HistoricalImport {
                        manifest: imported.manifest,
                        source_session: imported.source_session,
                    });
                }
                changes.extend(history_items(None, &history));
                changes
            }
            SessionEvent::Command {
                operation,
                command,
                at_ms,
            } => {
                self.recorded_ms = at_ms;
                match command {
                    SessionCommand::AdmissionPinned { .. }
                    | SessionCommand::LegacyImportBound { .. } => Vec::new(),
                    SessionCommand::ReviewRecorded { feedback } => {
                        vec![ViewChange::ReviewRecorded { feedback }]
                    }
                    SessionCommand::ShellStarted { .. } => {
                        vec![ViewChange::ShellStarted { request: operation }]
                    }
                    SessionCommand::ShellPublished {
                        request, report, ..
                    } => {
                        vec![ViewChange::ShellPublished { request, report }]
                    }
                    SessionCommand::WorkspaceSaved { seed, .. } => {
                        vec![ViewChange::WorkspaceSaved(seed)]
                    }
                    SessionCommand::AuxiliaryStarted => {
                        if self.auxiliary.get(&operation) == Some(&true) {
                            self.active_request = Some(operation);
                            vec![ViewChange::RequestStarted { request: operation }]
                        } else {
                            Vec::new()
                        }
                    }
                    SessionCommand::AuxiliaryRecorded { .. } => Vec::new(),
                    SessionCommand::AuxiliaryPublished { request, text, .. } => text
                        .map(|text| {
                            vec![ViewChange::Assistant {
                                request: Some(request),
                                item: format!("auxiliary-{request}"),
                                text,
                                replace: true,
                                confirmed: true,
                            }]
                        })
                        .unwrap_or_default(),
                    SessionCommand::QueueEdited { .. } | SessionCommand::QueueMoved { .. } => {
                        vec![ViewChange::QueueChanged]
                    }
                    SessionCommand::Submitted(submission) => {
                        let preview_visible = match &submission.intent {
                            orvek_harness::submission::WorkIntent::Auxiliary { spec } => {
                                Some(spec.visible())
                            }
                            orvek_harness::submission::WorkIntent::Ordinary { .. } => Some(true),
                            _ => None,
                        };
                        if let Some(visible) = preview_visible {
                            self.classify_auxiliary(submission.id, visible);
                        }
                        vec![ViewChange::Submission(*submission)]
                    }
                    SessionCommand::SubmissionChanged { request, status } => {
                        vec![ViewChange::SubmissionChanged { request, status }]
                    }
                    SessionCommand::Input { content, .. } => {
                        self.active_request = Some(operation);
                        let mut changes = vec![ViewChange::RequestStarted { request: operation }];
                        changes.extend(history_items(Some(operation), &content));
                        changes
                    }
                    SessionCommand::Response { request, items } => {
                        history_items(Some(request), &items)
                    }
                    SessionCommand::ProviderUsage { usage, .. } => {
                        vec![ViewChange::ProviderUsage { usage }]
                    }
                    SessionCommand::ToolResult {
                        request,
                        call_id,
                        output,
                    } => vec![ViewChange::ToolResult {
                        request: Some(request),
                        call_id,
                        output,
                    }],
                    SessionCommand::TaskLinked { task, .. } => {
                        if self.tasks.insert(task) {
                            self.task_order.push_back(task);
                            if self.task_order.len() > 256
                                && let Some(oldest) = self.task_order.pop_front()
                            {
                                self.tasks.remove(&oldest);
                            }
                        }
                        vec![ViewChange::TaskLinked(task)]
                    }
                    SessionCommand::TurnSettled { request, error, .. } => {
                        if self.auxiliary.remove(&request) == Some(false) {
                            return Vec::new();
                        }
                        if self.active_request == Some(request) {
                            self.active_request = None;
                        }
                        vec![ViewChange::RequestSettled { request, error }]
                    }
                    SessionCommand::SettingsChanged(settings) => {
                        vec![ViewChange::Settings(settings)]
                    }
                    SessionCommand::ContextProjected {
                        source_revision,
                        projection,
                    } => vec![ViewChange::ContextProjected {
                        source_revision,
                        items: projection.len(),
                    }],
                    SessionCommand::Feedback { message } => {
                        vec![ViewChange::Status(format!("Feedback: {message}"))]
                    }
                }
            }
        }
    }
}

pub(crate) fn history_items(request: Option<Uuid>, items: &[Value]) -> Vec<ViewChange> {
    let mut changes = Vec::new();
    for item in items {
        let kind = item["type"].as_str().unwrap_or("message");
        match kind {
            "message" => match item["role"].as_str() {
                Some("user") => changes.push(ViewChange::User {
                    request,
                    text: content_text(&item["content"], true),
                }),
                Some("assistant") => changes.push(ViewChange::Assistant {
                    request,
                    item: item["id"].as_str().unwrap_or("message").to_owned(),
                    text: content_text(&item["content"], false),
                    replace: true,
                    confirmed: true,
                }),
                _ => {}
            },
            "function_call" => {
                if let (Some(call_id), Some(name), Some(arguments)) = (
                    item["call_id"].as_str(),
                    item["name"].as_str(),
                    item["arguments"].as_str(),
                ) {
                    changes.push(ViewChange::ToolProposed {
                        request,
                        item_id: item["id"].as_str().map(str::to_owned),
                        call_id: call_id.into(),
                        name: name.into(),
                        arguments: arguments.into(),
                    });
                }
            }
            "function_call_output" => {
                if let (Some(call_id), Some(output)) =
                    (item["call_id"].as_str(), item["output"].as_str())
                {
                    changes.push(ViewChange::ToolResult {
                        request,
                        call_id: call_id.into(),
                        output: output.into(),
                    });
                }
            }
            "reasoning" => changes.push(ViewChange::Reasoning {
                request,
                item: item["id"].as_str().unwrap_or("reasoning").into(),
                text: item["summary"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|part| part["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default(),
                replace: true,
            }),
            _ => {}
        }
    }
    changes
}

fn content_text(content: &Value, images: bool) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| match part["type"].as_str() {
                    Some("input_text" | "output_text" | "text") => part["text"].as_str(),
                    Some("refusal") => part["refusal"].as_str(),
                    Some("input_image" | "tact_image") if images => Some("[Image]"),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use orvek_harness::{
        Digest,
        contract::Limits,
        inference::{OutputItem, Usage},
        state::{Outcome, RequestKind},
        submission::{Schedule, Submission, SubmissionStatus, WorkIntent},
    };
    use serde_json::json;

    fn event_at(
        session: SessionId,
        sequence: u64,
        operation: Uuid,
        command: SessionCommand,
    ) -> WatchFrame {
        WatchFrame::Journal(JournalRecord {
            sequence,
            aggregate: session.to_string(),
            kind: "session".into(),
            revision: sequence,
            event: serde_json::to_value(SessionEvent::Command {
                operation,
                command,
                at_ms: sequence,
            })
            .unwrap(),
        })
    }

    fn event(session: SessionId, sequence: u64, command: SessionCommand) -> WatchFrame {
        WatchFrame::Journal(JournalRecord {
            sequence,
            aggregate: session.to_string(),
            kind: "session".into(),
            revision: sequence,
            event: serde_json::to_value(SessionEvent::Command {
                operation: Uuid::nil(),
                command,
                at_ms: sequence,
            })
            .unwrap(),
        })
    }

    #[test]
    fn feedback_reaches_the_transcript_as_a_status_line() {
        let session = SessionId::new();
        let mut projection = HostProjection::new(session, 0);

        let changes = projection.apply(event(
            session,
            1,
            SessionCommand::Feedback {
                message: "keep the patch minimal".to_owned(),
            },
        ));
        assert!(matches!(
            changes.as_slice(),
            [super::ViewChange::Status(text)] if text.contains("keep the patch minimal")
        ));
    }

    #[test]
    fn shell_lifecycle_events_carry_their_request_identity() {
        let session = SessionId::new();
        let mut projection = HostProjection::new(session, 0);
        let request = Uuid::new_v4();
        let digest = orvek_harness::Digest::of(b"shell");

        let changes = projection.apply(event_at(
            session,
            1,
            request,
            SessionCommand::ShellStarted {
                job: orvek_harness::manual::ManualJob {
                    job: Uuid::new_v4(),
                    task: None,
                    before: digest,
                    origin: digest,
                    environment: digest,
                    started_ms: 1,
                    scope_revision: None,
                },
            },
        ));
        assert!(matches!(
            changes.as_slice(),
            [super::ViewChange::ShellStarted { request: observed }] if *observed == request
        ));

        let report = orvek_harness::Digest::of(b"report");
        let changes = projection.apply(event_at(
            session,
            2,
            request,
            SessionCommand::ShellPublished {
                request,
                report,
                seed: None,
                settled: true,
            },
        ));
        assert!(matches!(
            changes.as_slice(),
            [super::ViewChange::ShellPublished {
                request: observed,
                report: observed_report,
            }] if *observed == request && *observed_report == report
        ));
    }

    #[test]
    fn previews_and_turn_settlement_cannot_create_a_completion_certificate() {
        let session = SessionId::new();
        let mut projection = HostProjection::new(session, 0);
        projection.apply(event(
            session,
            1,
            SessionCommand::Input {
                kind: RequestKind::Task,
                content: vec![json!({"role":"user","content":"fix it"})],
            },
        ));
        assert!(
            projection
                .apply(WatchFrame::Preview {
                    session,
                    request: Uuid::nil(),
                    delta: Delta::ItemDone {
                        item: OutputItem::Message {
                            id: "m".into(),
                            text: "all tests passed".into(),
                            refusals: vec![]
                        }
                    }
                })
                .is_empty()
        );
        let changes = projection.apply(event(
            session,
            2,
            SessionCommand::TurnSettled {
                request: Uuid::nil(),
                outcome: Some(Outcome::Complete),
                error: None,
            },
        ));
        assert!(matches!(&changes[..], [ViewChange::RequestSettled { .. }]));
        assert!(
            projection
                .apply(WatchFrame::Preview {
                    session,
                    request: Uuid::nil(),
                    delta: Delta::Text {
                        item_id: "m".into(),
                        text: "late".into()
                    }
                })
                .is_empty()
        );
    }

    #[test]
    fn tool_argument_previews_stream_and_the_proposal_replaces_them() {
        let session = SessionId::new();
        let mut projection = HostProjection::new(session, 0);
        let request = Uuid::new_v4();
        projection.apply(WatchFrame::Preview {
            session,
            request,
            delta: Delta::Created {
                response_id: "resp-1".into(),
            },
        });
        projection.apply(event_at(
            session,
            1,
            request,
            SessionCommand::Input {
                kind: RequestKind::Conversation,
                content: vec![json!({"type":"input_text","text":"run it"})],
            },
        ));

        let changes = projection.apply(WatchFrame::Preview {
            session,
            request,
            delta: Delta::ToolArguments {
                item_id: "fc-1".into(),
                arguments: r#"{"command":"pd"#.into(),
            },
        });
        assert!(matches!(
            changes.as_slice(),
            [super::ViewChange::ToolArguments {
                item_id,
                chunk,
                ..
            }] if item_id == "fc-1" && chunk == r#"{"command":"pd"#
        ));

        // The provider item ID and execution call ID need not match.
        let changes = projection.apply(event_at(
            session,
            2,
            request,
            SessionCommand::Response {
                request,
                items: vec![json!({
                    "type": "function_call",
                    "id": "fc-1",
                    "call_id": "call-1",
                    "name": "exec_command",
                    "arguments": r#"{"command":"pwd"}"#,
                    "status": "completed"
                })],
            },
        ));
        assert!(changes.iter().any(|change| matches!(
            change,
            super::ViewChange::ToolProposed { item_id, call_id, name, .. }
                if item_id.as_deref() == Some("fc-1")
                    && call_id == "call-1" && name == "exec_command"
        )));
    }

    #[test]
    fn ordinary_submission_keeps_answer_previews_visible() {
        let session = SessionId::new();
        let request = Uuid::nil();
        let input = Digest::of(b"input");
        let mut projection = HostProjection::new(session, 0);
        projection.apply(event(
            session,
            1,
            SessionCommand::Submitted(Box::new(Submission {
                manual_job: None,
                id: request,
                input,
                initial_input: input,
                records: Vec::new(),
                result: None,
                intent: WorkIntent::Ordinary {
                    limits: Limits::default(),
                    policy: Digest::of(b"policy"),
                    schedule: Schedule::Queue,
                },
                status: SubmissionStatus::Queued,
                submitted_revision: 1,
                submitted_ms: 1,
            })),
        ));
        projection.apply(event(session, 2, SessionCommand::AuxiliaryStarted));

        let changes = projection.apply(WatchFrame::Preview {
            session,
            request,
            delta: Delta::ReasoningSummary {
                item_id: "reasoning-1".into(),
                text: "visible reasoning".into(),
            },
        });

        assert!(
            matches!(&changes[..], [ViewChange::Reasoning { text, .. }] if text == "visible reasoning")
        );
    }

    #[test]
    fn malformed_tool_arguments_and_model_success_claims_remain_plain_display_data() {
        let items = vec![
            json!({"type":"function_call","call_id":"call","name":"write_file","arguments":"{malformed"}),
            json!({"type":"function_call_output","call_id":"call","output":"{\"success\":true}"}),
            json!({"type":"run.completed","success":true}),
        ];
        let changes = history_items(None, &items);
        assert_eq!(changes.len(), 2);
        assert!(
            matches!(&changes[0], ViewChange::ToolProposed { arguments, .. } if arguments == "{malformed")
        );
        assert!(
            matches!(&changes[1], ViewChange::ToolResult { output, .. } if output == "{\"success\":true}")
        );
    }

    #[test]
    fn durable_provider_usage_reaches_every_client_projection() {
        let session = SessionId::new();
        let usage = Usage {
            input_tokens: Some(100),
            output_tokens: Some(20),
            total_tokens: Some(120),
            cached_input_tokens: Some(40),
            reasoning_tokens: Some(8),
        };
        let changes = HostProjection::new(session, 0).apply(event(
            session,
            1,
            SessionCommand::ProviderUsage {
                request: Uuid::nil(),
                usage: usage.clone(),
            },
        ));

        assert!(matches!(
            &changes[..],
            [ViewChange::ProviderUsage { usage: observed }] if observed == &usage
        ));
    }

    #[test]
    fn durable_replay_is_idempotent_and_other_sessions_are_not_rendered() {
        let session = SessionId::new();
        let mut projection = HostProjection::new(session, 0);
        let record = event(
            session,
            3,
            SessionCommand::Input {
                kind: RequestKind::Conversation,
                content: vec![json!({"role":"user","content":"hello"})],
            },
        );
        assert_eq!(projection.apply(record.clone()).len(), 2);
        assert!(projection.apply(record).is_empty());
        assert!(
            projection
                .apply(event(
                    SessionId::new(),
                    4,
                    SessionCommand::Feedback {
                        message: "elsewhere".into()
                    }
                ))
                .is_empty()
        );
        assert_eq!(projection.sequence(), 4);
        assert_eq!(projection.cursor().revision, 3);
        assert!(matches!(
            &projection.apply(WatchFrame::PreviewGap { dropped: 2 })[..],
            [ViewChange::DiscardPreviews]
        ));
        assert_eq!(projection.sequence(), 4);
    }
}
