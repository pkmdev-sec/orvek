//! Native session discovery and context reads through the local host.

use crate::{
    app::{
        config::{ReasoningEffort, ReasoningMode},
        host::HostClient,
    },
    tui::host_projection::{ViewChange, history_items},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use orvek_harness::{
    Digest,
    ipc::{Command, HistoryEntry, Response, SessionView},
    session::{SessionCursor, SessionId},
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

pub(crate) const MAX_RECENT_PROMPTS: usize = 100;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct SessionSummary {
    pub(crate) session_id: String,
    pub(crate) started_at_unix_ms: u64,
    pub(crate) model: String,
    pub(crate) effort: Option<ReasoningEffort>,
    pub(crate) reasoning_mode: Option<ReasoningMode>,
    pub(crate) workspace: PathBuf,
    pub(crate) preview: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct RecentPrompt {
    pub(crate) text: String,
    pub(crate) recorded_at_unix_ms: u64,
    pub(crate) session_id: String,
    pub(crate) workspace: PathBuf,
}

#[derive(Debug, Error)]
pub(crate) enum SessionError {
    #[error("session host request failed: {0}")]
    Host(String),
    #[error("invalid native session ID: {0}")]
    InvalidId(#[from] uuid::Error),
    #[error("invalid session response: {0}")]
    Protocol(&'static str),
    #[error("session view exceeds its display bound; use host history paging")]
    DisplayLimit,
}

impl From<SessionView> for SessionSummary {
    fn from(view: SessionView) -> Self {
        Self {
            session_id: view.id.to_string(),
            started_at_unix_ms: view.started_ms,
            model: view.model.model.as_str().into(),
            effort: Some(view.model.thinking.into()),
            reasoning_mode: Some(view.model.reasoning_mode.into()),
            workspace: view.workspace,
            preview: view.title.unwrap_or_else(|| "Untitled session".into()),
        }
    }
}

pub(crate) async fn list_async(
    config_path: PathBuf,
    workspace: PathBuf,
    resumable_only: bool,
) -> Result<Vec<SessionSummary>, SessionError> {
    let client = HostClient::for_config(&config_path);
    let workspace = workspace
        .canonicalize()
        .map_err(|e| SessionError::Host(e.to_string()))?;
    let mut sessions = Vec::new();
    let mut imported = std::collections::HashSet::new();
    let mut finished = false;
    for offset in (0..512).step_by(64) {
        let Response::Sessions(page) = client
            .query(Command::Sessions { offset, limit: 64 })
            .await
            .map_err(host_error)?
        else {
            return Err(SessionError::Protocol("expected session page"));
        };
        finished = page.len() < 64;
        for view in page {
            if let Some(source) = &view.imported {
                imported.insert(source.source_session.clone());
            }
            if view.workspace == workspace
                && (!resumable_only || view.history_items > 0 || view.active_request.is_some())
            {
                sessions.push(SessionSummary::from(view));
            }
        }
        if finished {
            break;
        }
    }
    if !finished {
        return Err(SessionError::DisplayLimit);
    }
    for legacy in legacy_sessions(&client, &config_path).await? {
        if Path::new(&legacy.workspace) != workspace || imported.contains(&legacy.session_id) {
            continue;
        }
        sessions.push(legacy_summary(legacy));
    }
    sessions.sort_by_key(|session| std::cmp::Reverse(session.started_at_unix_ms));
    Ok(sessions)
}

pub(crate) fn legacy_database(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("sessions/v2.sqlite3")
}
pub(crate) fn legacy_label(id: &str) -> String {
    use base64::Engine;
    format!(
        "legacy:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(id.as_bytes())
    )
}
pub(crate) fn legacy_id(label: &str) -> Result<Option<String>, SessionError> {
    use base64::Engine;
    let Some(encoded) = label.strip_prefix("legacy:") else {
        return Ok(None);
    };
    if encoded.len() > 344 {
        return Err(SessionError::Protocol(
            "historical session identifier exceeds limit",
        ));
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| SessionError::Protocol("invalid historical session identifier"))?;
    let id = String::from_utf8(bytes)
        .map_err(|_| SessionError::Protocol("invalid historical session identifier"))?;
    if id.is_empty() || id.contains('\0') {
        return Err(SessionError::Protocol(
            "invalid historical session identifier",
        ));
    }
    Ok(Some(id))
}
fn legacy_setting<T: serde::de::DeserializeOwned>(
    setting: &orvek_harness::import::LegacySetting,
) -> Option<T> {
    setting
        .value
        .as_ref()
        .and_then(|value| serde_json::from_value(serde_json::Value::String(value.clone())).ok())
}
fn legacy_summary(legacy: orvek_harness::import::LegacySessionMetadata) -> SessionSummary {
    SessionSummary {
        session_id: legacy_label(&legacy.session_id),
        started_at_unix_ms: legacy.started_at_ms,
        model: legacy.model,
        effort: legacy_setting(&legacy.effort),
        reasoning_mode: legacy_setting(&legacy.reasoning_mode),
        workspace: legacy.workspace.into(),
        preview: format!("Historical · {}", legacy.preview),
    }
}
pub(crate) async fn legacy_sessions(
    client: &HostClient,
    config_path: &Path,
) -> Result<Vec<orvek_harness::import::LegacySessionMetadata>, SessionError> {
    let database = legacy_database(config_path);
    if !database.try_exists().map_err(host_error)? {
        return Ok(Vec::new());
    }
    let mut sessions = Vec::new();
    for offset in (0..500).step_by(100) {
        let Response::LegacySessions(value) = client
            .call(
                &orvek_harness::ipc::Request::new(Command::LegacySessions {
                    database: database.clone(),
                    offset,
                    limit: 100,
                }),
                Duration::from_secs(35),
            )
            .await
            .map_err(host_error)?
        else {
            return Err(SessionError::Protocol("expected historical session page"));
        };
        let page: Vec<orvek_harness::import::LegacySessionMetadata> = serde_json::from_value(value)
            .map_err(|_| SessionError::Protocol("invalid historical session page"))?;
        if page.len() > 100 {
            return Err(SessionError::Protocol(
                "historical session page exceeds limit",
            ));
        }
        let finished = page.len() < 100;
        sessions.extend(page);
        if finished {
            return Ok(sessions);
        }
    }
    Err(SessionError::DisplayLimit)
}

pub(crate) async fn view(
    client: &HostClient,
    session: SessionId,
) -> Result<SessionView, SessionError> {
    match client
        .query(Command::Session { id: session })
        .await
        .map_err(host_error)?
    {
        Response::Session(view) => Ok(*view),
        _ => Err(SessionError::Protocol("expected session snapshot")),
    }
}

pub(crate) struct HistoryPage {
    pub(crate) changes: Vec<ViewChange>,
    pub(crate) next: Option<usize>,
}

pub(crate) async fn history_page(
    client: &HostClient,
    cursor: SessionCursor,
    start: usize,
) -> Result<HistoryPage, SessionError> {
    let Response::History(page) = client
        .query(Command::History {
            cursor: cursor.clone(),
            start,
            limit: 64,
        })
        .await
        .map_err(host_error)?
    else {
        return Err(SessionError::Protocol("expected history page"));
    };
    let end = start.saturating_add(page.items.len());
    if page.cursor != cursor
        || page.start != start
        || page.items.len() > 64
        || page.items.len() > page.total.saturating_sub(start)
        || (start < page.total && page.items.is_empty())
        || page.next != (end < page.total).then_some(end)
    {
        return Err(SessionError::Protocol("history cursor mismatch"));
    }
    let mut changes = Vec::new();
    for (offset, entry) in page.items.into_iter().enumerate() {
        match entry {
            HistoryEntry::Inline(item) => changes.extend(history_items(None, &[item])),
            HistoryEntry::ToolOutput {
                call_id,
                bytes,
                digest,
            } => {
                let output = history_output(client, &cursor, start + offset, bytes, digest).await?;
                changes.push(ViewChange::ToolResult {
                    request: None,
                    call_id,
                    output,
                });
            }
        }
    }
    Ok(HistoryPage {
        changes,
        next: page.next,
    })
}

async fn history_output(
    client: &HostClient,
    cursor: &SessionCursor,
    item: usize,
    total: usize,
    digest: Digest,
) -> Result<String, SessionError> {
    let mut output = Vec::new();
    loop {
        let Response::HistoryText {
            cursor: source,
            page,
        } = client
            .query(Command::HistoryText {
                cursor: cursor.clone(),
                item,
                content_index: 0,
                offset: output.len(),
                limit: 24 * 1024,
            })
            .await
            .map_err(host_error)?
        else {
            return Err(SessionError::Protocol("expected history text page"));
        };
        if source != *cursor
            || page.item != item
            || page.content_index != 0
            || page.offset != output.len()
            || page.total != total
            || page.digest != digest
            || page.bytes_base64.len() > 32 * 1024
        {
            return Err(SessionError::Protocol(
                "history text identity or size mismatch",
            ));
        }
        let bytes = STANDARD
            .decode(page.bytes_base64)
            .map_err(|_| SessionError::Protocol("invalid history text encoding"))?;
        let end = output.len().saturating_add(bytes.len());
        if bytes.len() > 24 * 1024
            || end > total
            || page.end != end
            || (bytes.is_empty() && end < total)
            || page.next != (end < total).then_some(end)
        {
            return Err(SessionError::Protocol("history text cursor mismatch"));
        }
        output.extend(bytes);
        if page.next.is_none() {
            break;
        }
    }
    if Digest::of(&output) != digest {
        return Err(SessionError::Protocol("history text digest mismatch"));
    }
    String::from_utf8(output).map_err(|_| SessionError::Protocol("invalid history text UTF-8"))
}

pub(crate) async fn history(
    client: &HostClient,
    view: &SessionView,
) -> Result<Vec<ViewChange>, SessionError> {
    let cursor = SessionCursor {
        version: 1,
        session: view.id,
        revision: view.revision,
    };
    let mut start = 0;
    let mut changes = Vec::new();
    loop {
        let page = history_page(client, cursor.clone(), start).await?;
        changes.extend(page.changes);
        if changes.len() > 2048 {
            return Err(SessionError::DisplayLimit);
        }
        match page.next {
            Some(next) => start = next,
            None => return Ok(changes),
        }
    }
}

fn host_error(error: impl std::fmt::Display) -> SessionError {
    SessionError::Host(error.to_string())
}

pub(crate) fn format_age(started_at_unix_ms: u64) -> String {
    if started_at_unix_ms == 0 {
        return "unknown".into();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let elapsed = now
        .saturating_sub(Duration::from_millis(started_at_unix_ms))
        .as_secs();
    match elapsed {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", elapsed / 60),
        3600..=86399 => format!("{}h", elapsed / 3600),
        _ => format!("{}d", elapsed / 86400),
    }
}

pub(crate) async fn load_recent_prompts_async(
    config_path: PathBuf,
) -> Result<Vec<RecentPrompt>, SessionError> {
    let client = HostClient::for_config(&config_path);
    let Response::RecentInputs(inputs) = client
        .query(Command::RecentInputs {
            limit: MAX_RECENT_PROMPTS,
            before: None,
            workspace: None,
        })
        .await
        .map_err(host_error)?
    else {
        return Err(SessionError::Protocol("expected original-input page"));
    };
    let mut prompts = Vec::with_capacity(inputs.len());
    for input in inputs {
        let text = if input.truncated {
            let cursor = SessionCursor {
                version: 1,
                session: input.session,
                revision: input.revision,
            };
            let Response::History(first) = client
                .query(Command::History {
                    cursor: cursor.clone(),
                    start: 0,
                    limit: 1,
                })
                .await
                .map_err(host_error)?
            else {
                return Err(SessionError::Protocol("expected original input history"));
            };
            let total = first.total;
            let page = history_page(&client, cursor, total.saturating_sub(1)).await?;
            page.changes
                .into_iter()
                .rev()
                .find_map(|change| match change {
                    ViewChange::User { text, .. } => Some(text),
                    _ => None,
                })
                .ok_or(SessionError::Protocol("original user input is unavailable"))?
        } else {
            input.text
        };
        prompts.push(RecentPrompt {
            text,
            recorded_at_unix_ms: input.at_ms,
            session_id: input.session.to_string(),
            workspace: input.workspace,
        });
    }
    Ok(prompts)
}

pub(crate) struct QueueSnapshot {
    pub(crate) sequence: u64,
    pub(crate) active_auxiliary: Option<(uuid::Uuid, bool)>,
    pub(crate) inputs: Vec<super::components::QueuedInput>,
}
pub(crate) async fn queued(
    client: &HostClient,
    session: SessionId,
) -> Result<QueueSnapshot, SessionError> {
    for _ in 0..3 {
        let mut offset = 0;
        let mut sequence = None;
        let mut receipts = Vec::new();
        loop {
            let Response::Submissions(page) = client
                .query(Command::Submissions {
                    session,
                    offset,
                    limit: 64,
                })
                .await
                .map_err(host_error)?
            else {
                return Err(SessionError::Protocol("expected submission roster"));
            };
            if sequence.is_some_and(|head| head != page.journal_sequence) {
                break;
            }
            sequence = Some(page.journal_sequence);
            if page.total > 512 || page.submissions.len() > 64 {
                return Err(SessionError::DisplayLimit);
            }
            receipts.extend(page.submissions);
            if let Some(next) = page.next {
                if next <= offset || next > page.total {
                    return Err(SessionError::Protocol("invalid submission roster cursor"));
                }
                offset = next;
            } else {
                let active_auxiliary = receipts.iter().find_map(|receipt| match &receipt.intent {
                    orvek_harness::submission::WorkIntent::Auxiliary { spec }
                        if receipt.status
                            == orvek_harness::submission::SubmissionStatus::Running =>
                    {
                        Some((receipt.id, spec.visible()))
                    }
                    _ => None,
                });
                let mut inputs = Vec::new();
                for receipt in receipts.into_iter().filter(|receipt| {
                    receipt.status == orvek_harness::submission::SubmissionStatus::Queued
                }) {
                    let parts = crate::app::submission::input_parts(
                        client,
                        receipt.input,
                        false,
                        &tokio_util::sync::CancellationToken::new(),
                    )
                    .await
                    .map_err(host_error)?;
                    let prompt = super::prompt::Submission::from_host_content(parts)
                        .map_err(SessionError::Protocol)?;
                    let preview = prompt.display_text().chars().take(512).collect::<String>();
                    inputs.push(super::components::QueuedInput {
                        id: super::components::QueueId(receipt.id),
                        input: receipt.input,
                        prompt: preview.into(),
                    });
                }
                return Ok(QueueSnapshot {
                    sequence: sequence.unwrap_or(0),
                    active_auxiliary,
                    inputs,
                });
            }
        }
    }
    Err(SessionError::Protocol("queue changed during paging; retry"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use orvek_harness::{
        Channel, Store,
        controller::Host,
        inference::{
            Limits, ModelSettings, ResponsesClient, Route, Transport,
            auth::{Auth, SecretString},
        },
        ipc,
        session::{SessionAdmissionRequest, SessionCommand},
        state::RequestKind,
    };
    use serde_json::json;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    fn host(root: &Path) -> Host {
        let provider = ResponsesClient::new(
            Auth::api_key(SecretString::new("fixture".into())).unwrap(),
            Route::new(Transport::Http, "http://127.0.0.1:1/v1/responses").unwrap(),
            Limits {
                max_attempts: 1,
                ..Limits::default()
            },
        )
        .unwrap();
        Host::open_native(root, provider, Digest::of(b"history-fixture")).unwrap()
    }

    #[tokio::test]
    async fn saved_large_tool_output_reopens_with_exact_tui_text_and_following_feedback() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("state");
        let native = host(&root);
        let state = native
            .create_session(SessionAdmissionRequest::new(
                directory.path().into(),
                ModelSettings::default(),
                orvek_harness::context::DEFAULT_WINDOW_TOKENS,
                Channel::Stable,
            ))
            .await
            .unwrap();
        drop(native);
        let mut store = Store::open(&root).unwrap();
        let request = Uuid::new_v4();
        let mut state = store
            .session_command(
                state.id,
                state.revision,
                request,
                SessionCommand::Input {
                    kind: RequestKind::Conversation,
                    content: vec![json!({"role":"user","content":"inspect"})],
                },
            )
            .unwrap();
        state = store.session_command(state.id, state.revision, Uuid::new_v4(), SessionCommand::Response {
            request,
            items: vec![json!({"type":"function_call","call_id":"large","name":"read_file","arguments":"{}"})],
        }).unwrap();
        let output = "\0雪🦀\\\"".repeat(120000);
        let operation = Uuid::new_v4();
        let result = SessionCommand::ToolResult {
            request,
            call_id: "large".into(),
            output: output.clone(),
        };
        state = store
            .session_command(state.id, state.revision, operation, result.clone())
            .unwrap();
        assert_eq!(
            store
                .session_command(state.id, state.revision, operation, result)
                .unwrap(),
            state
        );
        state = store
            .session_command(
                state.id,
                state.revision,
                Uuid::new_v4(),
                SessionCommand::TurnSettled {
                    request,
                    outcome: None,
                    error: None,
                },
            )
            .unwrap();
        store
            .session_command(
                state.id,
                state.revision,
                Uuid::new_v4(),
                SessionCommand::Feedback {
                    message: "saved feedback after large output".into(),
                },
            )
            .unwrap();
        drop(store);

        let native = Arc::new(host(&root));
        let stop = CancellationToken::new();
        let server = tokio::spawn(ipc::serve(native.clone(), stop.clone()));
        tokio::time::timeout(Duration::from_secs(5), async {
            while !root.join("host.sock").exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let client = HostClient::fixture(&root);
        let view = view(&client, state.id).await.unwrap();
        let restored = history(&client, &view).await;
        stop.cancel();
        server.await.unwrap().unwrap();
        let restored = restored.expect("large results must not prevent opening a session");
        assert!(restored.iter().any(|change| matches!(change,
            ViewChange::ToolResult { call_id, output: actual, .. } if call_id == "large" && actual == &output)));
        assert!(restored.iter().any(|change| matches!(change,
            ViewChange::Status(message) if message == "Feedback: saved feedback after large output")));
        assert_eq!(
            native.session(state.id).await.unwrap().tool_calls["large"].output,
            Some(Digest::of(output.as_bytes()))
        );
    }
}
