//! A handoff creates a host context only after its durable report is available.
use crate::{
    app::{
        auxiliary,
        config::Config,
        error::{Error, Result},
        host::HostClient,
    },
    core::ConfiguredSession,
    tui::session,
};
use orvek_harness::{
    auxiliary::{AuxiliaryContext, AuxiliaryKind},
    ipc::{Command, Request, Response, SessionView},
    session::{SessionCursor, SessionId},
};
use tokio_util::sync::CancellationToken;

const PROMPT: &str = "Prepare a self-contained continuation prompt for a new coding context. Summarize the user's objective and requirements, decisions and constraints, work completed, relevant files, validation actually performed, unresolved blockers, and concrete next steps. Preserve exact technical facts and distinguish uncertain results. Do not continue the task or claim that this summary grants task completion authority. Return only the editable continuation prompt.";

pub(crate) struct PreparedHandoff {
    pub(crate) prompt: String,
    pub(crate) configured: ConfiguredSession,
}
pub(crate) struct HandoffFailure {
    pub(crate) prompt: Option<String>,
    // Boxed to keep the `Err` variant of `prepare` small.
    pub(crate) error: Box<Error>,
}

pub(crate) async fn prepare(
    config: &Config,
    client: &HostClient,
    id: SessionId,
    cancel: &CancellationToken,
) -> std::result::Result<PreparedHandoff, HandoffFailure> {
    let source = session::view(client, id)
        .await
        .map_err(|error| HandoffFailure {
            prompt: None,
            error: Box::new(error.into()),
        })?;
    if source.branch.pending_task.is_some() {
        return Err(HandoffFailure {
            prompt: None,
            error: Box::new(Error::HostRequest(
                "a settled workspace checkpoint is required for handoff".into(),
            )),
        });
    }
    let result = auxiliary::run(
        client,
        id,
        vec![serde_json::json!({"type":"input_text","text":PROMPT})],
        auxiliary::spec(
            AuxiliaryKind::Handoff,
            AuxiliaryContext::CurrentConversation,
            None,
        ),
        cancel,
    )
    .await
    .and_then(auxiliary::completed_text);
    let prompt = result.map_err(|error| HandoffFailure {
        prompt: None,
        error: error.into(),
    })?;
    if cancel.is_cancelled() {
        return Err(HandoffFailure {
            prompt: Some(prompt),
            error: Box::new(Error::AuxiliaryCancelled),
        });
    }
    let result=async {
        let latest=session::view(client,id).await?;
        if latest.current_task!=source.current_task || latest.branch.workspace!=source.branch.workspace || latest.branch.pending_task.is_some() {return Err(Error::HostRequest("workspace changed during the handoff report; the draft is retained in this session".into()));}
        let view=create(client,source.fork_cursor).await?;
        Ok(ConfiguredSession::from_view(config,client.clone(),view))
    }.await;
    result
        .map(|configured| PreparedHandoff {
            prompt: prompt.clone(),
            configured,
        })
        .map_err(|error| HandoffFailure {
            prompt: Some(prompt),
            error: error.into(),
        })
}
async fn create(client: &HostClient, parent: SessionCursor) -> Result<SessionView> {
    let id = SessionId::new();
    let request = Request::new(Command::HandoffSession {
        id,
        parent: parent.clone(),
    });
    let mut uncertain = false;
    let mut last = None;
    for _ in 0..3 {
        match client
            .call(&request, std::time::Duration::from_secs(5))
            .await
        {
            Ok(Response::Session(view))
                if view.id == id
                    && view.branch.fresh_context
                    && view.parent.as_ref().is_some_and(|cursor| {
                        cursor.session == parent.session && cursor.revision <= parent.revision
                    }) =>
            {
                return Ok(*view);
            }
            Err(error @ Error::HostRequest(_)) if !uncertain => return Err(error),
            Err(error) => {
                uncertain = true;
                last = Some(error);
            }
            Ok(_) => {
                return Err(Error::HostRequest(
                    "handoff response identity mismatch".into(),
                ));
            }
        }
        if let Ok(view) = session::view(client, id).await
            && view.branch.fresh_context
            && view.parent.as_ref().is_some_and(|cursor| {
                cursor.session == parent.session && cursor.revision <= parent.revision
            })
        {
            return Ok(view);
        }
    }
    Err(Error::HostRequest(format!(
        "handoff context {id} may have been created; resume that ID to recover it. The report draft is retained. {}",
        last.map(|error| error.to_string()).unwrap_or_default()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use orvek_harness::{ipc, session::SessionBranch};
    use std::{fs, os::unix::fs::PermissionsExt};
    use tokio::net::UnixListener;
    #[tokio::test]
    async fn lost_creation_ack_recovers_the_same_context_without_another_handoff() {
        let root = tempfile::Builder::new()
            .prefix("orvek-handoff-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = root.path().join("host.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let parent = SessionCursor {
            version: 1,
            session: SessionId::new(),
            revision: 7,
        };
        let expected = parent.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let first: Request = ipc::read_frame(&mut stream).await.unwrap();
            let Command::HandoffSession { id, parent } = first.command else {
                panic!("expected handoff")
            };
            assert_eq!(parent, expected);
            drop(stream);
            let (mut stream, _) = listener.accept().await.unwrap();
            let lookup: Request = ipc::read_frame(&mut stream).await.unwrap();
            assert!(matches!(lookup.command,Command::Session{id:found} if found==id));
            let view = SessionView {
                branch: SessionBranch {
                    fresh_context: true,
                    ..Default::default()
                },
                id,
                revision: 1,
                fork_cursor: SessionCursor {
                    version: 1,
                    session: id,
                    revision: 1,
                },
                workspace: "/fixture".into(),
                model: Default::default(),
                context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
                admission: None,
                parent: Some(parent),
                current_task: None,
                active_request: None,
                history_items: 0,
                outcome: None,
                error: None,
                journal_sequence: 2,
                started_ms: 1,
                title: None,
                imported: None,
                cost_usd: Default::default(),
                cost_uncertain: false,
            };
            ipc::write_frame(&mut stream, &Response::Session(Box::new(view)))
                .await
                .unwrap();
            id
        });
        let view = create(&HostClient::fixture(root.path()), parent)
            .await
            .unwrap();
        assert_eq!(view.id, server.await.unwrap());
        assert!(view.branch.fresh_context);
        assert!(view.current_task.is_none());
        assert!(view.outcome.is_none());
    }
}
