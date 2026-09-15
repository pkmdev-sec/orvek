//! Versioned headless output from the same durable host used by the terminal.

use crate::{
    app::{
        config::Config,
        error::{Error, Result},
    },
    core::ConfiguredSession,
};
use orvek_harness::{
    contract::Limits,
    inference::Model,
    ipc::{Command, Request, Response, WatchFrame},
    session::{SessionCommand, SessionEvent, SessionId},
    state::{Outcome, TaskId},
    submission::{SubmissionStatus, SubmitIntent},
};
use serde::Serialize;
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
}

pub(crate) async fn run(
    config: &Config,
    model: Model,
    prompt: String,
    cancellation: CancellationToken,
) -> Result<Outcome> {
    let configured = ConfiguredSession::create(
        config,
        config.agent().thinking(),
        config.agent().reasoning_mode(),
        model,
    )
    .await?;
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
    output.emit(
        "submission_pending",
        &serde_json::json!({"session":session,"request":request.id}),
    )?;
    let mut receipt = super::submission::acknowledge(&client, &request)
        .await
        .map_err(|failure| *failure.error)?;
    output.emit("submission", &receipt)?;
    let mut after = configured.session.journal_sequence;
    let mut watch = client.subscribe(after).await.ok();
    let mut tasks = BTreeSet::new();
    let mut cancel_sent = false;
    let mut reconnects = 0u32;
    let mut poll_errors = 0u32;
    let mut poll = tokio::time::interval(Duration::from_secs(1));
    loop {
        if !receipt.status.pending() {
            // Drain the authoritative journal through the host head observed after
            // the terminal receipt. Preview availability cannot change the result.
            if let Ok(info) = client.info().await {
                let target = info.journal_sequence;
                let drain = async {
                    if watch.is_none() {
                        watch = Some(client.subscribe(after).await?);
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
                if watch.is_none() && reconnects<3 {
                    reconnects+=1;
                    match client.subscribe(after).await {Ok(next)=>watch=Some(next),Err(error)=>output.emit("view_reconnecting",&serde_json::json!({"after":after,"error":error.to_string()}))?}
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
        WatchFrame::PreviewGap { .. } => true,
        _ => false,
    }
}
