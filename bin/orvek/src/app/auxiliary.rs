//! Viewer for durable read-only reports; inference and tools remain host-owned.
use super::{
    artifacts::HostArtifacts,
    error::{Error, Result},
    host::HostClient,
    submission,
};
use orvek_harness::{
    auxiliary::{AuxiliaryKind, AuxiliaryReport, AuxiliarySpec, AuxiliaryStatus},
    ipc::{Command, Request, Response},
    session::SessionId,
    submission::{SubmissionStatus, SubmitIntent},
};
use tokio_util::sync::CancellationToken;

pub(crate) async fn run(
    client: &HostClient,
    session: SessionId,
    content: Vec<serde_json::Value>,
    spec: AuxiliarySpec,
    cancel: &CancellationToken,
) -> Result<AuxiliaryReport> {
    let kind = spec.kind;
    let request = Request::new(Command::Submit {
        session,
        content,
        intent: SubmitIntent::Auxiliary { spec },
    });
    let mut receipt = submission::acknowledge(client, &request)
        .await
        .map_err(|failure| *failure.error)?;
    let mut cancelled = false;
    let mut failures = 0u8;
    while receipt.status.pending() {
        tokio::select! {
            ()=cancel.cancelled(),if !cancelled=>{
                cancelled=true;
                match client.query(Command::CancelSubmission {session,request:request.id}).await? {
                    Response::Submission(next) if next.id==request.id=>receipt=next,
                    _=>return Err(Error::HostRequest("unexpected auxiliary cancellation receipt".into())),
                }
            }
            ()=tokio::time::sleep(std::time::Duration::from_millis(250))=>{
                match client.query(Command::Submission {session,request:request.id}).await {
                    Ok(Response::Submission(next)) if next.id==request.id=>{receipt=next;failures=0;}
                    Ok(_)=>return Err(Error::HostRequest("unexpected auxiliary submission receipt".into())),
                    Err(error)=>{failures+=1;if failures>=3{return Err(Error::HostRequest(format!("report {} in session {} remains recoverable through the host: {error}",request.id,session)));}}
                }
            }
        }
    }
    if let Some(digest) = receipt.result {
        // A cancelled viewer may still retrieve the final cancellation receipt.
        let bytes = HostArtifacts::new(client.clone())
            .read(digest, 2 * 1024 * 1024, &CancellationToken::new())
            .await?;
        let report: AuxiliaryReport = serde_json::from_slice(&bytes)
            .map_err(|_| Error::HostRequest("invalid auxiliary report artifact".into()))?;
        if report.version != 1 || report.kind != kind {
            return Err(Error::HostRequest(
                "auxiliary report identity mismatch".into(),
            ));
        }
        Ok(report)
    } else if receipt.status == SubmissionStatus::Cancelled {
        Err(Error::AuxiliaryCancelled)
    } else {
        Err(Error::HostRequest(format!(
            "report {} ended without a published result: {:?}",
            request.id, receipt.status
        )))
    }
}

pub(crate) fn completed_text(report: AuxiliaryReport) -> Result<String> {
    match report.status {
        AuxiliaryStatus::Completed => Ok(report.text),
        AuxiliaryStatus::Cancelled => Err(Error::AuxiliaryCancelled),
        status => Err(Error::HostRequest(format!(
            "{:?} report ended with {status:?}{}",
            report.kind,
            report
                .error
                .map(|error| format!(": {error}"))
                .unwrap_or_default()
        ))),
    }
}

pub(crate) fn spec(
    kind: AuxiliaryKind,
    context: orvek_harness::auxiliary::AuxiliaryContext,
    review: Option<orvek_harness::Digest>,
) -> AuxiliarySpec {
    AuxiliarySpec {
        kind,
        context,
        review,
        limits: Default::default(),
    }
}
