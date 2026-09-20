use super::*;
use crate::tui::transcript::{EntryKind, ToolState, TranscriptModel, TranscriptRecord};
use orvek_harness::{
    Store,
    admission::{RepositoryProfile, RequestPolicy},
    contract::{DeliveryKind, Limits},
    inference::ModelSettings,
    session::{SessionCommand, SessionConfig, SessionId},
    state::{JobInvocation, JobStatus, TaskEvent},
};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn recorded_jobs_replay_commands_and_outputs_without_previews() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let session = store
        .create_session(
            SessionId::new(),
            SessionConfig {
                workspace: root.path().into(),
                model: ModelSettings::default(),
                instructions: String::new(),
                context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            },
            None,
        )
        .unwrap();
    let policy = store
        .public_artifacts()
        .write(
            &serde_json::to_vec(&RequestPolicy {
                version: 1,
                delivery: DeliveryKind::Source,
                profile: RepositoryProfile {
                    version: 1,
                    name: "fixture".into(),
                    checks: Default::default(),
                },
            })
            .unwrap(),
        )
        .unwrap()
        .digest();
    let request = Uuid::new_v4();
    let (_, mut task, _) = store
        .start_request(
            session.id,
            request,
            "Inspect workspace".into(),
            Limits::default(),
            policy,
        )
        .unwrap();
    let mut inputs = Vec::new();
    for (index, command) in ["pwd", "printf second"].into_iter().enumerate() {
        let arguments = if index == 0 {
            json!({"cmd":command})
        } else {
            json!({"command":command})
        };
        let bytes =
            serde_json::to_vec(&json!({"name":"exec_command", "arguments":arguments})).unwrap();
        let input = store.public_artifacts().write(&bytes).unwrap().digest();
        let call_id = format!("call-{index}");
        {
            let current = store.load_session(session.id).unwrap();
            store.session_command(session.id, current.revision, Uuid::new_v4(), SessionCommand::Response {
                request, items: vec![json!({"type":"function_call", "id":"provider-item-0", "call_id":call_id, "name":"exec_command", "arguments":arguments.to_string()})],
            }).unwrap();
        }
        let invocation = JobInvocation {
            session: session.id,
            request,
            call_id: Some(call_id.clone()),
            capability: "exec_command".into(),
            input,
            environment: policy,
        };
        let (started, job) = store
            .start_execution_job(task.id, task.revision, false, 1000, invocation)
            .unwrap();
        task = store
            .settle_execution_job(started.id, job, JobStatus::Succeeded, input)
            .unwrap();
        let current = store.load_session(session.id).unwrap();
        store
            .session_command(
                session.id,
                current.revision,
                Uuid::new_v4(),
                SessionCommand::ToolResult {
                    request,
                    call_id,
                    output: json!({"output":format!("output-{index}")}).to_string(),
                },
            )
            .unwrap();
        inputs.push((input, bytes));
    }
    drop(store);
    let store = Store::open(&root.path().join("state")).unwrap();
    let mut projection = HostProjection::new(session.id, 0);
    let mut model = TranscriptModel::default();
    let mut pending = Vec::new();
    for record in store.journal_page(0, 256).unwrap() {
        // A resumed display may start after the provider proposal. Keep the
        // recorded JobStarted and result to exercise that artifact-only path.
        if let Ok(orvek_harness::session::SessionEvent::Command {
            command: SessionCommand::Response { items, .. },
            ..
        }) = serde_json::from_value(record.event.clone())
            && items.iter().any(|item| item["call_id"] == "call-1")
        {
            continue;
        }
        for change in projection.apply(WatchFrame::Journal(record)) {
            if let ViewChange::Task {
                event: TaskEvent::JobStarted(job),
                ..
            } = &change
            {
                pending.push((job.id, job.invocation.clone().unwrap()));
            }
            model.apply(&TranscriptRecord::from_host(
                projection.sequence(),
                projection.recorded_ms(),
                projection.cursor(),
                change,
            ));
        }
    }
    assert_eq!(pending.len(), 2);
    // Exercise HostClient's authenticated same-user IPC path and public artifact command.
    let socket = root.path().join("host.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    let server = tokio::spawn(async move {
        use base64::Engine as _;
        for (expected, bytes) in inputs {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: Request = orvek_harness::ipc::read_frame(&mut stream).await.unwrap();
            assert!(
                matches!(request.command, Command::ReadArtifact { digest, offset: 0, limit: 65536 } if digest == expected)
            );
            orvek_harness::ipc::write_frame(&mut stream, &Response::Artifact(json!({
                "bytes":bytes.len(), "data":base64::engine::general_purpose::STANDARD.encode(bytes), "next":null,
            }))).await.unwrap();
        }
    });
    let client = HostClient::fixture(root.path());
    for (job, invocation) in pending {
        let arguments = task_input(&client, &invocation).await.unwrap();
        model.apply(&TranscriptRecord::from_host(
            projection.sequence(),
            projection.recorded_ms(),
            projection.cursor(),
            ViewChange::TaskInput { job, arguments },
        ));
    }
    server.await.unwrap();
    let tools = model
        .entries()
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Tool(tool) => Some(tool),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 2);
    for (index, command) in ["pwd", "printf second"].into_iter().enumerate() {
        let key = if index == 0 { "cmd" } else { "command" };
        assert_eq!(tools[index].arguments[key], command);
        assert_eq!(
            tools[index].result.as_ref().unwrap()["output"],
            format!("output-{index}")
        );
        assert_eq!(tools[index].state, ToolState::Succeeded);
    }
}

#[test]
fn malformed_or_mismatched_invocation_input_is_an_error() {
    assert!(decode_task_input(b"not json", "exec_command").is_err());
    assert!(decode_task_input(br#"{"name":"other","arguments":{}}"#, "exec_command").is_err());
    assert!(
        decode_task_input(
            br#"{"name":"exec_command","arguments":null}"#,
            "exec_command"
        )
        .is_err()
    );
}

#[test]
fn session_replacement_routes_fork_failures_to_fork_cleanup() {
    let event = SessionReplacement::Fork.failure_event(PaneId::Fork(7), "failed".into());
    assert!(matches!(
        event,
        AppEvent::ForkFailed {
            pane: PaneId::Fork(7),
            error
        } if error == "failed"
    ));

    let event =
        SessionReplacement::New(DraftReset::Clear).failure_event(PaneId::Main, "failed".into());
    assert!(matches!(
        event,
        AppEvent::NewSessionFailed {
            pane: PaneId::Main,
            error
        } if error == "failed"
    ));
}

#[test]
fn subagent_spawn_preserves_session_and_parent_identity() {
    let session = SessionId::new();
    let parent = Uuid::new_v4();
    let agent = Uuid::new_v4();
    let event = orvek_harness::controller::SubagentEvent::Spawned {
        session,
        request: Uuid::new_v4(),
        agent,
        parent: Some(parent),
        role: "reviewer".into(),
        task: "Review lifecycle".into(),
        model: "luna".into(),
    };

    let Some(ChildUpdate::Added(child)) = subagent_update(&event) else {
        panic!("spawn should map to a child");
    };
    assert_eq!(child.session_id, session.to_string());
    assert_eq!(child.parent, Some(ChildId(parent)));
}

/// The host error and the terminal hint must not both state draft retention.
#[tokio::test]
async fn submission_notices_state_each_recovery_hint_once() {
    let unreachable = tempfile::tempdir().unwrap();
    let rejected = SubmitFailure {
        uncertain: false,
        error: Box::new(
            submissions::intent(&HostClient::fixture(unreachable.path()), SessionId::new())
                .await
                .unwrap_err(),
        ),
    };
    let notice = submission_notice(&rejected);
    assert!(notice.ends_with(" · Draft retained"), "notice: {notice}");
    assert_eq!(
        notice.to_lowercase().matches("draft retained").count(),
        1,
        "notice repeated draft retention: {notice}"
    );

    let uncertain = SubmitFailure {
        uncertain: true,
        error: Box::new(Error::HostRequest("lost reply".into())),
    };
    assert_eq!(
        submission_notice(&uncertain),
        "host request: lost reply · Enter retries the same request"
    );
}
