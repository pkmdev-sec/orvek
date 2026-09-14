use orvek_harness::{
    Store, StoreError,
    inference::ModelSettings,
    session::{SessionCommand, SessionConfig, SessionId},
    state::{Outcome, RequestKind},
};
use serde_json::json;
use uuid::Uuid;

#[test]
fn reused_tool_ids_and_orphan_results_are_rejected_before_execution() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let mut session = store
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
    let request = Uuid::new_v4();
    session = store
        .session_command(
            session.id,
            session.revision,
            request,
            SessionCommand::Input {
                kind: RequestKind::Conversation,
                content: vec![json!({"role":"user","content":"inspect"})],
            },
        )
        .unwrap();
    let response = SessionCommand::Response {
        request,
        items: vec![
            json!({"type":"function_call","call_id":"once","name":"read_file","arguments":"{}"}),
        ],
    };
    session = store
        .session_command(
            session.id,
            session.revision,
            Uuid::new_v4(),
            response.clone(),
        )
        .unwrap();
    assert!(
        store
            .session_command(
                session.id,
                session.revision,
                Uuid::new_v4(),
                response.clone()
            )
            .is_err()
    );
    let result = SessionCommand::ToolResult {
        request,
        call_id: "once".into(),
        output: "observed bytes".into(),
    };
    let operation = Uuid::new_v4();
    session = store
        .session_command(session.id, session.revision, operation, result.clone())
        .unwrap();
    assert_eq!(
        store
            .session_command(session.id, session.revision, operation, result.clone())
            .unwrap(),
        session
    );
    assert!(
        store
            .session_command(session.id, session.revision, Uuid::new_v4(), result)
            .is_err()
    );
    session = store
        .session_command(
            session.id,
            session.revision,
            Uuid::new_v4(),
            SessionCommand::ContextProjected {
                source_revision: session.revision,
                projection: vec![json!({"role":"user","content":"bounded summary"})],
            },
        )
        .unwrap();
    assert!(
        store
            .session_command(session.id, session.revision, Uuid::new_v4(), response)
            .is_err(),
        "a context projection cannot erase execution identities"
    );
}

#[test]
fn startup_settles_interrupted_conversations_and_rejects_their_late_results() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let mut session = store
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
    let request = Uuid::new_v4();
    session = store
        .session_command(
            session.id,
            session.revision,
            request,
            SessionCommand::Input {
                kind: RequestKind::Conversation,
                content: vec![json!({"role":"user","content":"explain"})],
            },
        )
        .unwrap();
    drop(store);
    let mut store = Store::open(&root.path().join("state")).unwrap();
    store.recover_interrupted().unwrap();
    session = store.load_session(session.id).unwrap();
    assert_eq!(session.active_request, None);
    assert_eq!(session.outcome, None);
    assert!(session.error.is_some());
    assert!(
        store
            .session_command(
                session.id,
                session.revision,
                Uuid::new_v4(),
                SessionCommand::Response {
                    request,
                    items: vec![json!({"role":"assistant","content":"late answer"})]
                }
            )
            .is_err()
    );
    let revision = session.revision;
    store.recover_interrupted().unwrap();
    assert_eq!(store.load_session(session.id).unwrap().revision, revision);
}

#[test]
fn session_commands_replay_once_and_fork_only_persisted_prefix() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let config = SessionConfig {
        workspace: root.path().to_owned(),
        model: ModelSettings::default(),
        instructions: "explicit task protocol".into(),
        context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
    };
    let mut session = store
        .create_session(SessionId::new(), config.clone(), None)
        .unwrap();
    let request = Uuid::new_v4();
    let input = SessionCommand::Input {
        kind: RequestKind::Conversation,
        content: vec![json!({"role":"user","content":"first"})],
    };
    session = store
        .session_command(session.id, session.revision, request, input.clone())
        .unwrap();
    let unsafe_cursor = session.cursor();
    let fork_cursor = session.fork_cursor();
    assert!(
        store
            .create_session(SessionId::new(), config.clone(), Some(unsafe_cursor))
            .is_err()
    );
    assert_eq!(
        store
            .session_command(session.id, 1, request, input)
            .unwrap(),
        session
    );
    let response = Uuid::new_v4();
    session = store
        .session_command(
            session.id,
            session.revision,
            response,
            SessionCommand::Response {
                request,
                items: vec![json!({"role":"assistant","content":"reply"})],
            },
        )
        .unwrap();
    session = store
        .session_command(
            session.id,
            session.revision,
            Uuid::new_v4(),
            SessionCommand::TurnSettled {
                request,
                outcome: None,
                error: None,
            },
        )
        .unwrap();
    let child = store
        .create_session(SessionId::new(), config, Some(fork_cursor))
        .unwrap();
    assert_eq!(child.history.len(), 0);
    assert_eq!(session.history.len(), 2);
    assert_eq!(child.active_request, None);
    assert_eq!(
        store
            .scoped_history(child.id, session.id, None)
            .unwrap()
            .history,
        child.history
    );
    assert!(
        store
            .scoped_history(child.id, session.id, Some(session.revision))
            .is_err()
    );
    assert!(store.scoped_history(session.id, child.id, None).is_err());
    let events = store.journal_page(0, 256).unwrap();
    assert_eq!(events.len(), 5);
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    drop(store);
    let recovered = Store::open(&root.path().join("state")).unwrap();
    assert_eq!(recovered.load_session(session.id).unwrap(), session);
    assert_eq!(recovered.load_session(child.id).unwrap(), child);
}

#[test]
fn installed_projection_keeps_exact_archives_and_fork_cutoffs() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let config = SessionConfig {
        workspace: root.path().into(),
        model: ModelSettings::default(),
        instructions: String::new(),
        context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
    };
    let mut session = store
        .create_session(SessionId::new(), config.clone(), None)
        .unwrap();
    let request = Uuid::new_v4();
    let original = json!({"role":"user","content":"exact original data ".repeat(10_000)});
    session = store
        .session_command(
            session.id,
            session.revision,
            request,
            SessionCommand::Input {
                kind: RequestKind::Conversation,
                content: vec![original.clone()],
            },
        )
        .unwrap();
    let archived = session.cursor();
    session = store
        .session_command(
            session.id,
            session.revision,
            Uuid::new_v4(),
            SessionCommand::TurnSettled {
                request,
                outcome: None,
                error: None,
            },
        )
        .unwrap();
    let projection = orvek_harness::context::project(&session, 4096).unwrap();
    session = store
        .session_command(
            session.id,
            session.revision,
            Uuid::new_v4(),
            SessionCommand::ContextProjected {
                source_revision: session.revision,
                projection: projection.input,
            },
        )
        .unwrap();
    assert!(serde_json::to_vec(&session.history).unwrap().len() < 4096);
    let child = store
        .create_session(SessionId::new(), config, Some(session.cursor()))
        .unwrap();
    assert_eq!(
        store
            .scoped_history(child.id, session.id, Some(archived.revision))
            .unwrap()
            .history,
        vec![original]
    );
    session = store
        .session_command(
            session.id,
            session.revision,
            Uuid::new_v4(),
            SessionCommand::Input {
                kind: RequestKind::Conversation,
                content: vec![json!({"role":"user","content":"future question"})],
            },
        )
        .unwrap();
    assert!(
        store
            .scoped_history(child.id, session.id, Some(session.revision))
            .is_err()
    );
    let recent = store
        .recent_inputs(100, None, Some(root.path()))
        .unwrap()
        .into_iter()
        .filter(|input| input.revision == archived.revision)
        .collect::<Vec<_>>();
    assert_eq!(
        recent.len(),
        1,
        "forked or compacted history cannot duplicate original submissions"
    );
    assert_eq!(recent[0].session, session.id);
    assert_eq!(recent[0].revision, archived.revision);
    assert!(recent[0].text.starts_with("exact original data"));
    assert!(recent[0].truncated);
    assert!(recent[0].at_ms > 0);
    assert!(
        store
            .recent_inputs(100, Some(recent[0].sequence), None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn journal_pages_are_bounded_before_ipc_serialization() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    for _ in 0..12 {
        store
            .create_session(
                SessionId::new(),
                SessionConfig {
                    workspace: root.path().into(),
                    model: ModelSettings::default(),
                    instructions: "x".repeat(450 * 1024),
                    context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
                },
                None,
            )
            .unwrap();
    }

    let page = store.journal_page(0, 256).unwrap();
    assert!(!page.is_empty());
    assert!(page.len() < 12, "the encoded-byte bound must stop the page");
    let response = orvek_harness::ipc::Response::Journal(page);
    assert!(serde_json::to_vec(&response).unwrap().len() < orvek_harness::ipc::MAX_FRAME_BYTES);
}

#[test]
fn session_cannot_invent_completion_or_accept_late_response() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let config = SessionConfig {
        workspace: root.path().to_owned(),
        model: ModelSettings::default(),
        instructions: String::new(),
        context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
    };
    let mut session = store
        .create_session(SessionId::new(), config, None)
        .unwrap();
    let request = Uuid::new_v4();
    session = store
        .session_command(
            session.id,
            session.revision,
            request,
            SessionCommand::Input {
                kind: RequestKind::Task,
                content: vec![json!({"role":"user","content":"fix it"})],
            },
        )
        .unwrap();
    assert!(matches!(
        store.session_command(
            session.id,
            session.revision,
            Uuid::new_v4(),
            SessionCommand::TurnSettled {
                request,
                outcome: Some(Outcome::Complete),
                error: None
            }
        ),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.session_command(
            session.id,
            session.revision,
            Uuid::new_v4(),
            SessionCommand::Response {
                request: Uuid::new_v4(),
                items: vec![]
            }
        ),
        Err(StoreError::Invalid(_))
    ));
}
