#![cfg(unix)]

use orvek_harness::ipc::{self, Command, Request, Response};
use serde_json::json;
use tokio::io::{AsyncWriteExt, duplex};

#[tokio::test]
async fn bounded_frames_preserve_request_identity_and_reject_oversized_input() {
    let request = Request::new(Command::Journal {
        after: 12,
        limit: 3,
    });
    let (mut writer, mut reader) = duplex(4096);
    ipc::write_frame(&mut writer, &request).await.unwrap();
    let decoded: Request = ipc::read_frame(&mut reader).await.unwrap();
    assert_eq!(decoded.id, request.id);
    assert!(matches!(
        decoded.command,
        Command::Journal {
            after: 12,
            limit: 3
        }
    ));
    writer
        .write_u32(ipc::MAX_FRAME_BYTES as u32 + 1)
        .await
        .unwrap();
    assert!(ipc::read_frame::<Request>(&mut reader).await.is_err());
}

#[tokio::test]
async fn operator_protocol_exposes_no_arbitrary_state_or_evidence_mutation() {
    for kind in [
        "complete",
        "record_evidence",
        "patch_state",
        "mark_done",
        "grant_capability",
    ] {
        let (mut writer, mut reader) = duplex(4096);
        let value = json!({
            "version": ipc::PROTOCOL_VERSION,
            "id": uuid::Uuid::new_v4(),
            "command": {"type": kind, "data": {}}
        });
        ipc::write_frame(&mut writer, &value).await.unwrap();
        assert!(
            ipc::read_frame::<Request>(&mut reader).await.is_err(),
            "{kind}"
        );
    }
    let (mut writer, mut reader) = duplex(4096);
    writer.write_u32(100).await.unwrap();
    writer.write_all(b"partial").await.unwrap();
    drop(writer);
    assert!(ipc::read_frame::<Request>(&mut reader).await.is_err());
}

#[tokio::test]
async fn responses_round_trip_without_turn_completion_becoming_task_completion() {
    let (mut writer, mut reader) = duplex(4096);
    let response = Response::Cancelled { requested: false };
    ipc::write_frame(&mut writer, &response).await.unwrap();
    assert!(matches!(
        ipc::read_frame::<Response>(&mut reader).await.unwrap(),
        Response::Cancelled { requested: false }
    ));
}

#[tokio::test]
#[ignore = "requires local Docker and pre-pulled debian:bookworm-slim"]
async fn real_operator_socket_uses_the_host_owner_and_survives_client_reconnect() {
    use orvek_harness::{
        Channel,
        controller::Host,
        inference::{
            Limits, ModelSettings, ResponsesClient, Route, Transport,
            auth::{Auth, SecretString},
        },
        runtime::DockerExecutor,
        session::{SessionAdmissionRequest, SessionId},
    };
    use std::{sync::Arc, time::Duration};
    use tokio_util::sync::CancellationToken;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let state = root.path().join("host");
    let client = ResponsesClient::new(
        Auth::api_key(SecretString::new("fixture".into())).unwrap(),
        Route::new(Transport::Http, "http://127.0.0.1:1/v1/responses").unwrap(),
        Limits {
            max_attempts: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    let host = Arc::new(
        Host::open(
            &state,
            client,
            DockerExecutor::connect("debian:bookworm-slim")
                .await
                .unwrap(),
        )
        .unwrap(),
    );
    let stop = CancellationToken::new();
    let server = tokio::spawn(ipc::serve(host, stop.clone()));
    let socket = state.join("host.sock");
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let mut watch = ipc::subscribe(&socket, 0).await.unwrap();
    assert!(matches!(
        ipc::read_frame::<ipc::WatchFrame>(&mut watch)
            .await
            .unwrap(),
        ipc::WatchFrame::Ready {
            after: 0,
            through: 0
        }
    ));
    assert!(
        matches!(ipc::read_frame::<ipc::WatchFrame>(&mut watch).await.unwrap(), ipc::WatchFrame::Warnings { warnings } if warnings.is_empty())
    );
    let id = SessionId::new();
    let create = Request::new(Command::CreateSession {
        id,
        request: SessionAdmissionRequest::new(
            source,
            ModelSettings::default(),
            orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            Channel::Stable,
        ),
    });
    assert!(
        matches!(ipc::call(&socket,&create,Duration::from_secs(5)).await.unwrap(),Response::Session(view) if view.id==id)
    );
    let observed = tokio::time::timeout(
        Duration::from_secs(5),
        ipc::read_frame::<ipc::WatchFrame>(&mut watch),
    )
    .await
    .unwrap()
    .unwrap();
    let ipc::WatchFrame::Journal(record) = observed else {
        panic!("creation must be a durable journal event");
    };
    assert_eq!(record.aggregate, id.to_string());
    let cursor = record.sequence;
    drop(watch);
    let mut watch = ipc::subscribe(&socket, cursor).await.unwrap();
    assert!(
        matches!(ipc::read_frame::<ipc::WatchFrame>(&mut watch).await.unwrap(), ipc::WatchFrame::Ready { after, through } if after == cursor && through == cursor)
    );
    assert!(
        matches!(ipc::read_frame::<ipc::WatchFrame>(&mut watch).await.unwrap(), ipc::WatchFrame::Warnings { warnings } if warnings.is_empty())
    );
    assert!(
        matches!(ipc::call(&socket,&create,Duration::from_secs(5)).await.unwrap(),Response::Session(view) if view.id==id && view.revision==1)
    );
    let query = Request::new(Command::Session { id });
    assert!(
        matches!(ipc::call(&socket,&query,Duration::from_secs(5)).await.unwrap(),Response::Session(view) if view.active_request.is_none())
    );
    let mut invalid = query;
    invalid.version = 99;
    assert!(matches!(
        ipc::call(&socket, &invalid, Duration::from_secs(5))
            .await
            .unwrap(),
        Response::Error(envelope) if envelope.code == ipc::IpcErrorCode::UnsupportedProtocol && envelope.disposition == ipc::IpcErrorDisposition::Reject
    ));
    let mut second = create.clone();
    let Command::CreateSession { id: second_id, .. } = &mut second.command else {
        unreachable!()
    };
    *second_id = SessionId::new();
    let second_id = *second_id;
    assert!(
        matches!(ipc::call(&socket, &second, Duration::from_secs(5)).await.unwrap(), Response::Session(view) if view.id == second_id)
    );
    let observed = tokio::time::timeout(
        Duration::from_secs(5),
        ipc::read_frame::<ipc::WatchFrame>(&mut watch),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(observed, ipc::WatchFrame::Journal(record) if record.sequence > cursor && record.aggregate == second_id.to_string())
    );
    drop(watch);
    stop.cancel();
    server.await.unwrap().unwrap();
    assert!(!socket.exists());
}
