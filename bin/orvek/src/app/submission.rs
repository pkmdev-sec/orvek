//! Durable input acknowledgements shared by terminal and headless clients.

use super::{
    error::{Error, Result},
    host::HostClient,
};
use orvek_harness::{
    admission::{RepositoryProfile, RequestPolicy},
    contract::{DeliveryKind, Limits},
    ipc::{Command, Request, Response},
    session::SessionId,
    submission::{Schedule, Submission, SubmitIntent},
};
use std::time::Duration;

pub(crate) fn default_policy() -> RequestPolicy {
    RequestPolicy {
        version: 1,
        profile: RepositoryProfile {
            version: 1,
            name: "Orvek default: explicit behavior checks required".into(),
            checks: Default::default(),
        },
        delivery: DeliveryKind::Patch,
    }
}

pub(crate) async fn intent(client: &HostClient, session: SessionId) -> Result<SubmitIntent> {
    tokio::time::timeout(Duration::from_secs(5), async {
        for attempt in 0..20 {
            let Response::Session(view) = client.query(Command::Session { id: session }).await?
            else {
                return Err(Error::HostRequest("unexpected session response".into()));
            };
            if let Some(task) = view.current_task {
                let Response::Task {
                    id,
                    scope_revision,
                    ..
                } = client.query(Command::Task { id: task }).await?
                else {
                    return Err(Error::HostRequest("unexpected task response".into()));
                };
                if id != task {
                    return Err(Error::HostRequest(
                        "task response identity mismatch".into(),
                    ));
                }
                return Ok(SubmitIntent::Continue {
                    task,
                    scope_revision,
                    schedule: Schedule::Queue,
                });
            }
            let Response::Submissions(page) = client
                .query(Command::Submissions {
                    session,
                    offset: 0,
                    limit: 1,
                })
                .await?
            else {
                return Err(Error::HostRequest("unexpected submission roster".into()));
            };
            if page.total == 0 {
                return Ok(SubmitIntent::Ordinary {
                    limits: Limits::default(),
                    policy: default_policy(),
                    schedule: Schedule::Queue,
                });
            }
            if attempt < 19 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        Err(Error::HostRequest(
            "the first request is still being admitted; draft retained until the host assigns its task"
                .into(),
        ))
    })
    .await
    .map_err(|_| Error::HostRequest("task admission lookup timed out; draft retained".into()))?
}

#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub(crate) struct SubmitFailure {
    /// True means the caller must keep this exact request ID for recovery.
    pub(crate) uncertain: bool,
    // Boxed to keep the `Err` variant of `acknowledge` small.
    pub(crate) error: Box<Error>,
}

/// Retry only this immutable request. A lost response never creates another ID.
pub(crate) async fn acknowledge(
    client: &HostClient,
    request: &Request,
) -> std::result::Result<Submission, SubmitFailure> {
    let Command::Submit { session, .. } = request.command else {
        return Err(SubmitFailure {
            uncertain: false,
            error: Box::new(Error::HostRequest("expected a submit command".into())),
        });
    };
    let mut uncertain = false;
    let mut last_error = None;
    for attempt in 0..3 {
        match client.call(request, Duration::from_secs(5)).await {
            Ok(Response::Submission(receipt)) if receipt.id == request.id => return Ok(receipt),
            Ok(_) => {
                uncertain = true;
                last_error = Some(Error::HostRequest(
                    "submission response identity mismatch".into(),
                ));
            }
            Err(error @ Error::HostRequest(_)) if !uncertain => {
                return Err(SubmitFailure {
                    uncertain: false,
                    error: error.into(),
                });
            }
            Err(error) => {
                uncertain = true;
                last_error = Some(error);
            }
        }
        if let Ok(Response::Submission(receipt)) = client
            .query(Command::Submission {
                session,
                request: request.id,
            })
            .await
            && receipt.id == request.id
        {
            return Ok(receipt);
        }
        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    Err(SubmitFailure {
        uncertain: true,
        error: Box::new(Error::HostRequest(format!(
            "submission {} may have been accepted in session {}; reconnect with this request ID before resubmitting ({})",
            request.id,
            session,
            last_error
                .map(|error| error.to_string())
                .unwrap_or_default()
        ))),
    })
}

pub(crate) async fn input_parts(
    client: &HostClient,
    input: orvek_harness::Digest,
    images: bool,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Vec<serde_json::Value>> {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let artifacts = super::artifacts::HostArtifacts::new(client.clone());
    let bytes = artifacts.read(input, 1024 * 1024, cancel).await?;
    let mut messages: Vec<serde_json::Value> = serde_json::from_slice(&bytes)
        .map_err(|_| Error::HostRequest("invalid input artifact".into()))?;
    if messages.len() != 1 || messages[0]["role"] != "user" {
        return Err(Error::HostRequest(
            "input artifact must contain one user message".into(),
        ));
    }
    let mut parts = messages[0]["content"]
        .as_array_mut()
        .ok_or_else(|| Error::HostRequest("input artifact has no content".into()))?
        .clone();
    if parts.len() > 64 {
        return Err(Error::HostRequest(
            "input artifact exceeds part limit".into(),
        ));
    }

    let mut media_bytes = 0usize;
    let mut image_count = 0;
    for part in &mut parts {
        match part["type"].as_str() {
            Some("input_text") if part["text"].is_string() => {}
            Some("tact_image") => {
                image_count += 1;
                if image_count > 8 {
                    return Err(Error::HostRequest(
                        "input artifact exceeds image count".into(),
                    ));
                }
                if images {
                    let digest = serde_json::from_value(part["digest"].clone())
                        .map_err(|_| Error::HostRequest("invalid image digest".into()))?;
                    let mime = part["mime"]
                        .as_str()
                        .filter(|mime| {
                            matches!(
                                *mime,
                                "image/png" | "image/jpeg" | "image/webp" | "image/gif"
                            )
                        })
                        .ok_or_else(|| Error::HostRequest("invalid image media type".into()))?;
                    let bytes = artifacts.read(digest, 4 * 1024 * 1024, cancel).await?;
                    media_bytes += bytes.len();
                    if media_bytes > 4 * 1024 * 1024 {
                        return Err(Error::HostRequest(
                            "input artifact exceeds image bytes".into(),
                        ));
                    }
                    *part = serde_json::json!({
                        "type":"input_image",
                        "image_url":format!("data:{mime};base64,{}", STANDARD.encode(bytes)),
                        "detail":part["detail"],
                    });
                } else {
                    *part = serde_json::json!({
                        "type":"input_text",
                        "text":format!("[Image #{image_count}]"),
                    });
                }
            }
            _ => return Err(Error::HostRequest("unsupported input artifact part".into())),
        }
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use orvek_harness::{
        Digest,
        submission::{SubmissionStatus, WorkIntent},
    };
    use std::{fs, os::unix::fs::PermissionsExt};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn lost_acknowledgement_recovers_the_same_immutable_request() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("host.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let session = orvek_harness::session::SessionId::new();
        let request = Request::new(Command::Submit {
            session,
            content: vec![serde_json::json!({"type":"input_text","text":"keep this ID"})],
            intent: orvek_harness::submission::SubmitIntent::NewTask {
                limits: orvek_harness::contract::Limits::default(),
                policy: default_policy(),
            },
        });
        let receipt = Submission {
            manual_job: None,
            id: request.id,
            input: Digest::of(b"input"),
            initial_input: Digest::of(b"input"),
            records: Vec::new(),
            result: None,
            intent: WorkIntent::NewTask {
                limits: orvek_harness::contract::Limits::default(),
                policy: Digest::of_value(&default_policy()).unwrap(),
            },
            status: SubmissionStatus::Queued,
            submitted_revision: 1,
            submitted_ms: 1,
        };
        let expected = request.id;
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let submitted: Request = orvek_harness::ipc::read_frame(&mut first).await.unwrap();
            assert_eq!(submitted.id, expected);
            drop(first);

            let (mut recovery, _) = listener.accept().await.unwrap();
            let lookup: Request = orvek_harness::ipc::read_frame(&mut recovery).await.unwrap();
            assert!(matches!(
                lookup.command,
                Command::Submission { request, .. } if request == expected
            ));
            orvek_harness::ipc::write_frame(&mut recovery, &Response::Submission(receipt))
                .await
                .unwrap();
        });

        let recovered = acknowledge(&HostClient::fixture(root.path()), &request)
            .await
            .unwrap();

        assert_eq!(recovered.id, request.id);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn non_submission_requests_are_rejected_without_uncertainty() {
        let failure = acknowledge(
            &HostClient::fixture(tempfile::tempdir().unwrap().path()),
            &Request::new(Command::Info),
        )
        .await
        .unwrap_err();

        assert!(!failure.uncertain);
    }
}
