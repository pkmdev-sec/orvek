//! Test operator socket backed by the real read-only artifact adapter.
use crate::app::host::HostClient;
use base64::{Engine, engine::general_purpose::STANDARD};
use orvek_harness::{
    Store,
    artifacts::{ArtifactStore, PublicArtifactRef},
    ipc::{self, Command, Request, Response},
    review,
};
use std::os::unix::fs::PermissionsExt;
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;

pub(super) async fn client() -> HostClient {
    let root = tempfile::Builder::new()
        .prefix("orvek-review-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = root.path().join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    let artifacts = Store::open_with_artifact_limit(root.path(), 128 * 1024 * 1024)
        .unwrap()
        .public_artifacts()
        .clone();
    let client = HostClient::fixture(root.path());
    tokio::spawn(async move {
        let _root = root;
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let artifacts = artifacts.clone();
            tokio::spawn(async move {
                let Ok(request) = ipc::read_frame::<Request>(&mut stream).await else {
                    return;
                };
                let response = match dispatch(request.command, &artifacts).await {
                    Ok(response) => response,
                    Err(error) => Response::Error(ipc::IpcErrorEnvelope::new(
                        ipc::IpcErrorCode::Internal,
                        ipc::IpcErrorDisposition::Reject,
                        error.to_string(),
                    )),
                };
                let _ = ipc::write_frame(&mut stream, &response).await;
            });
        }
    });
    client
}
async fn dispatch(
    command: Command,
    artifacts: &ArtifactStore,
) -> Result<Response, Box<dyn std::error::Error + Send + Sync>> {
    let cancel = CancellationToken::new();
    Ok(match command {
        Command::ReviewCatalog { workspace } => {
            Response::ReviewCatalog(review::catalog(&workspace, &cancel).await?)
        }
        Command::InspectWorkspace { workspace, range } => {
            Response::WorkspaceReview(review::inspect(&workspace, range, artifacts, &cancel).await?)
        }
        Command::ReviewFile {
            manifest,
            side,
            path,
        } => Response::ReviewFile(review::file(artifacts, manifest, side, &path)?),
        Command::ReviewFiles {
            manifest,
            side,
            offset,
            limit,
        } => Response::ReviewFiles(review::page(artifacts, manifest, side, offset, limit)?),
        Command::ReadArtifact {
            digest,
            offset,
            limit,
        } => {
            let bytes = artifacts.resolve(PublicArtifactRef::from_digest(digest))?;
            let end = offset.saturating_add(limit).min(bytes.len());
            if offset > end || limit > 65536 {
                return Err("invalid fixture artifact range".into());
            }
            Response::Artifact(
                serde_json::json!({"digest":digest,"offset":offset,"bytes":bytes.len(),"encoding":"base64","data":STANDARD.encode(&bytes[offset..end]),"next":(end<bytes.len()).then_some(end)}),
            )
        }
        _ => return Err("unexpected fixture operator command".into()),
    })
}
