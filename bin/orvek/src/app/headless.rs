//! Versioned headless output from the same durable host used by the terminal.

use crate::{
    app::{
        config::Config,
        error::{Error, Result},
        host::HostClient,
    },
    core::ConfiguredSession,
};
use orvek_harness::{
    contract::Limits,
    inference::Model,
    ipc::{Command, Request, Response, SessionView, WatchFrame},
    session::{SessionCommand, SessionCursor, SessionEvent, SessionId},
    state::{Outcome, TaskId},
    submission::{SubmissionStatus, SubmitIntent},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeSet, io::Write, time::Duration};
use tokio_util::sync::CancellationToken;

struct Output;
impl Output {
    fn emit(&mut self, kind: &str, value: &impl Serialize) -> Result<()> {
        let mut bytes = serde_json::to_vec(
            &serde_json::json!({"protocol":"orvek.host","version":1,"type":kind,"data":value}),
        )
        .map_err(|error| Error::HostRequest(error.to_string()))?;
        bytes.push(b'\n');
        std::io::stdout().write_all(&bytes)?;
        Ok(())
    }

    async fn session_feedback(&mut self, client: &HostClient, view: &SessionView) -> Result<()> {
        #[derive(Deserialize)]
        struct Page {
            items: Vec<Value>,
            next: Option<usize>,
        }
        let cursor = SessionCursor {
            version: 1,
            session: view.id,
            revision: view.revision,
        };
        let mut start = 0;
        loop {
            let Response::History(page) = client
                .query(Command::History {
                    cursor: cursor.clone(),
                    start,
                    limit: 64,
                })
                .await?
            else {
                return Err(Error::HostRequest("expected session history page".into()));
            };
            let page: Page = serde_json::from_value(page)
                .map_err(|_| Error::HostRequest("invalid session history page".into()))?;
            for item in page.items {
                if item["role"] == "developer"
                    && let Some(message) = item["content"].as_str()
                {
                    self.emit(
                        "session_feedback",
                        &json!({"session":view.id,"revision":view.revision,"message":message}),
                    )?;
                }
            }
            match page.next {
                None => return Ok(()),
                Some(next) if next > start && next <= view.history_items => start = next,
                Some(_) => return Err(Error::HostRequest("invalid session history cursor".into())),
            }
        }
    }
}

const MAX_CONSECUTIVE_WATCH_RECONNECT_FAILURES: u32 = 3;

#[derive(Default)]
struct WatchReconnects {
    consecutive_failures: u32,
}

impl WatchReconnects {
    fn can_attempt(&self) -> bool {
        self.consecutive_failures < MAX_CONSECUTIVE_WATCH_RECONNECT_FAILURES
    }

    fn connected(&mut self) {
        self.consecutive_failures = 0;
    }

    fn failed(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
    }
}

pub(crate) async fn run(
    config: &Config,
    model: Model,
    prompt: String,
    resume: Option<String>,
    cancellation: CancellationToken,
) -> Result<Outcome> {
    let configured = match resume {
        Some(label) => ConfiguredSession::resume_label(config, &label).await?,
        None => {
            ConfiguredSession::create(
                config,
                config.agent().thinking(),
                config.agent().reasoning_mode(),
                model,
            )
            .await?
        }
    };
    let client = configured.client;
    let session = configured.session.id;
    let limits = Limits::default();
    let request = Request::new(Command::Submit {
        session,
        content: vec![serde_json::json!({"type":"input_text","text":prompt})],
        intent: SubmitIntent::NewTask {
            limits,
            policy: super::submission::default_policy(),
        },
    });
    let mut output = Output;
    output.emit("session", &configured.session)?;
    // Admission feedback predates this snapshot's watch cursor; resume also restores saved notices.
    if output
        .session_feedback(&client, &configured.session)
        .await
        .is_err()
    {
        output.emit("view_gap", &json!({"session":session,"view":"session_feedback","message":"Saved session feedback is unavailable."}))?;
    }
    output.emit(
        "submission_pending",
        &serde_json::json!({"session":session,"request":request.id}),
    )?;
    let mut receipt = super::submission::acknowledge(&client, &request)
        .await
        .map_err(|failure| *failure.error)?;
    output.emit("submission", &receipt)?;
    let mut after = configured.session.journal_sequence;
    let mut watch = client.subscribe(after, session).await.ok();
    let mut tasks = BTreeSet::new();
    let mut cancel_sent = false;
    let mut reconnects = WatchReconnects::default();
    let mut poll_errors = 0u32;
    let mut poll = tokio::time::interval(Duration::from_secs(1));
    loop {
        if !receipt.status.pending() {
            // Drain the authoritative journal through the host head observed after
            // the terminal receipt. Preview availability cannot change the result.
            if let Ok(info) = client.info().await {
                output.emit(
                    "event",
                    &WatchFrame::Warnings {
                        warnings: info.warnings,
                    },
                )?;
                let target = info.journal_sequence;
                let drain = async {
                    if watch.is_none() {
                        watch = Some(client.subscribe(after, session).await?);
                    }
                    while after < target {
                        let frame = watch.as_mut().expect("connected watch").next().await?;
                        after = watch.as_ref().expect("connected watch").cursor();
                        if visible(session, &mut tasks, &frame) {
                            output.emit("event", &frame)?;
                        }
                    }
                    Ok::<(), Error>(())
                };
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(5), drain).await,
                    Ok(Ok(()))
                ) {
                    output.emit(
                        "view_gap",
                        &serde_json::json!({"after":after,"through":target}),
                    )?;
                }
            }
            output.emit("submission_result", &receipt)?;
            return match receipt.status {
                SubmissionStatus::Finished {
                    outcome: Some(outcome),
                    ..
                } => Ok(outcome),
                SubmissionStatus::Cancelled => Ok(Outcome::Cancelled),
                SubmissionStatus::Finished { error, .. } => {
                    Err(Error::HostRequest(error.unwrap_or_else(|| {
                        "submission ended without a task outcome".into()
                    })))
                }
                SubmissionStatus::Interrupted => Err(Error::HostRequest(format!(
                    "submission {} was interrupted; inspect session {} before continuing",
                    request.id, session
                ))),
                _ => unreachable!("pending receipts continue watching"),
            };
        }
        tokio::select! {
            () = cancellation.cancelled(), if !cancel_sent => {
                cancel_sent = true;
                let result=client.query(Command::CancelSubmission { session,request:request.id }).await?;
                output.emit("cancellation", &result)?;
                if let Response::Submission(next)=result { receipt=next; }
                // Cancelling `next` may interrupt a frame; reopen from its complete cursor.
                watch=None;
            }
            _=poll.tick()=>{
                match client.query(Command::Submission {session,request:request.id}).await {
                    Ok(Response::Submission(next)) if next.id==request.id=>{
                        poll_errors=0;
                        if next!=receipt { output.emit("submission",&next)?; receipt=next; }
                    }
                    Ok(_)=>return Err(Error::HostRequest("unexpected submission status response".into())),
                    Err(error)=>{poll_errors+=1;if poll_errors>3 {return Err(Error::HostRequest(format!("could not recover submission {} in session {}: {error}",request.id,session)));}}
                }
                if watch.is_none() && reconnects.can_attempt() {
                    match client.subscribe(after, session).await {
                        Ok(next) => {
                            reconnects.connected();
                            watch = Some(next);
                        }
                        Err(error) => {
                            reconnects.failed();
                            output.emit("view_reconnecting", &serde_json::json!({"after":after,"error":error.to_string()}))?;
                        }
                    }
                }
            }
            frame=async {match &mut watch {Some(watch)=>watch.next().await,None=>std::future::pending().await}}=>match frame {
                Ok(frame)=>{
                    after=watch.as_ref().expect("watch received a frame").cursor();
                    if visible(session,&mut tasks,&frame) {output.emit("event",&frame)?;}
                }
                Err(error)=>{
                    output.emit("view_reconnecting",&serde_json::json!({"after":after,"error":error.to_string()}))?;
                    watch=None;
                }
            }
        }
    }
}

fn visible(session: SessionId, tasks: &mut BTreeSet<TaskId>, frame: &WatchFrame) -> bool {
    match frame {
        WatchFrame::Journal(record)
            if record.kind == "session" && record.aggregate == session.to_string() =>
        {
            if let Ok(SessionEvent::Command {
                command: SessionCommand::TaskLinked { task, .. },
                ..
            }) = serde_json::from_value(record.event.clone())
            {
                tasks.insert(task);
            }
            true
        }
        WatchFrame::Journal(record) if record.kind == "task" => {
            tasks.iter().any(|id| id.to_string() == record.aggregate)
        }
        WatchFrame::Preview { session: owner, .. } => *owner == session,
        WatchFrame::PreviewGap { .. } | WatchFrame::Warnings { .. } => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::WatchReconnects;

    #[test]
    fn successful_reconnects_do_not_exhaust_retry_budget() {
        let mut reconnects = WatchReconnects::default();

        for _ in 0..10 {
            assert!(reconnects.can_attempt());
            reconnects.failed();
            reconnects.connected();
        }

        assert!(reconnects.can_attempt());
    }

    #[test]
    fn three_consecutive_failures_exhaust_retry_budget() {
        let mut reconnects = WatchReconnects::default();
        for _ in 0..3 {
            reconnects.failed();
        }
        assert!(!reconnects.can_attempt());

        reconnects.connected();
        assert!(reconnects.can_attempt());
    }

    #[test]
    fn diagnostic_headless_visibility_includes_global_warnings_and_only_own_feedback() {
        use orvek_harness::{
            controller::HostWarning,
            ipc::WatchFrame,
            session::{JournalRecord, SessionCommand, SessionEvent, SessionId},
        };
        let session = SessionId::new();
        let mut tasks = std::collections::BTreeSet::new();
        let warning = WatchFrame::Warnings {
            warnings: vec![HostWarning::EventIntakeStopped],
        };
        assert!(super::visible(session, &mut tasks, &warning));
        assert!(super::visible(
            session,
            &mut tasks,
            &WatchFrame::Warnings { warnings: vec![] }
        ));
        let feedback = |owner: SessionId| {
            WatchFrame::Journal(JournalRecord {
                sequence: 1,
                aggregate: owner.to_string(),
                kind: "session".into(),
                revision: 1,
                event: serde_json::to_value(SessionEvent::Command {
                    operation: uuid::Uuid::new_v4(),
                    command: SessionCommand::Feedback {
                        message: "session warning".into(),
                    },
                    at_ms: 0,
                })
                .unwrap(),
            })
        };
        assert!(super::visible(session, &mut tasks, &feedback(session)));
        assert!(!super::visible(
            session,
            &mut tasks,
            &feedback(SessionId::new())
        ));
        assert!(tasks.is_empty());
    }
}
