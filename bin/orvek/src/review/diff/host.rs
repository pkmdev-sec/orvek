use super::{DiffError, HostClient, HostRange, Path, TaskId};
use crate::app::artifacts::HostArtifacts;
use orvek_harness::{
    Digest,
    ipc::{Command, Request, Response},
    review::{ReviewCatalog, ReviewInspection},
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
fn host_error(error: impl std::fmt::Display) -> DiffError {
    DiffError::Host(error.to_string())
}
pub(super) async fn catalog(
    client: &HostClient,
    workspace: &Path,
) -> Result<ReviewCatalog, DiffError> {
    match client
        .call(
            &Request::new(Command::ReviewCatalog {
                workspace: workspace.to_owned(),
            }),
            Duration::from_secs(45),
        )
        .await
        .map_err(host_error)?
    {
        Response::ReviewCatalog(view) => Ok(view),
        _ => Err(DiffError::Protocol),
    }
}
pub(super) async fn inspect_workspace(
    client: &HostClient,
    workspace: &Path,
    range: HostRange,
) -> Result<ReviewInspection, DiffError> {
    match client
        .call(
            &Request::new(Command::InspectWorkspace {
                workspace: workspace.to_owned(),
                range,
            }),
            Duration::from_secs(45),
        )
        .await
        .map_err(host_error)?
    {
        Response::WorkspaceReview(view) => Ok(view),
        _ => Err(DiffError::Protocol),
    }
}
pub(super) async fn inspect_task(
    client: &HostClient,
    task: TaskId,
) -> Result<ReviewInspection, DiffError> {
    match client
        .call(
            &Request::new(Command::InspectTaskReview { task }),
            Duration::from_secs(45),
        )
        .await
        .map_err(host_error)?
    {
        Response::TaskReview { view, review } if view.task == task && !view.pending_writes => {
            Ok(review)
        }
        Response::TaskReview { .. } => Err(DiffError::WorkspaceChangedDuringSnapshot),
        _ => Err(DiffError::Protocol),
    }
}
pub(super) async fn patch(client: &HostClient, digest: Digest) -> Result<String, DiffError> {
    let bytes = HostArtifacts::new(client.clone())
        .read(digest, 32 * 1024 * 1024, &CancellationToken::new())
        .await
        .map_err(host_error)?;
    Ok(String::from_utf8(bytes)?)
}
