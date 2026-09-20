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
    let view = orvek_harness::context::project(&session, 4096).unwrap();
    session = store
        .session_command(
            session.id,
            session.revision,
            Uuid::new_v4(),
            SessionCommand::ContextProjected {
                source_revision: session.revision,
                view: Some(view),
                projection: Vec::new(),
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
                view: Some(projection),
                projection: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(session.history, vec![original.clone()]);
    assert!(session.context_view.is_some());
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

#[test]
fn prompt_cache_lineage_is_shared_by_nested_forks_and_survives_reopen() {
    let root = tempfile::tempdir().unwrap();
    let state_path = root.path().join("state");
    let mut store = Store::open(&state_path).unwrap();
    let config = SessionConfig {
        workspace: root.path().into(),
        model: ModelSettings::default(),
        instructions: String::new(),
        context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
    };
    let original = store
        .create_session(SessionId::new(), config.clone(), None)
        .unwrap();
    let child = store
        .create_session(
            SessionId::new(),
            config.clone(),
            Some(original.fork_cursor()),
        )
        .unwrap();
    let grandchild = store
        .create_session(SessionId::new(), config, Some(child.fork_cursor()))
        .unwrap();

    assert_eq!(
        store.prompt_cache_lineage(original.id).unwrap(),
        original.id
    );
    assert_eq!(store.prompt_cache_lineage(child.id).unwrap(), original.id);
    assert_eq!(
        store.prompt_cache_lineage(grandchild.id).unwrap(),
        original.id
    );

    drop(store);
    let reopened = Store::open(&state_path).unwrap();
    assert_eq!(
        reopened.prompt_cache_lineage(grandchild.id).unwrap(),
        original.id
    );
}

#[test]
fn large_tool_results_replay_losslessly_with_bounded_records_and_stable_retry_identity() {
    use orvek_harness::{
        Digest,
        session::SessionEvent,
        trace::{TraceBundle, TraceLimits},
    };
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let mut store = Store::open(&state_root).unwrap();
    let mut state = store
        .create_session(
            SessionId::new(),
            SessionConfig {
                workspace: root.path().into(),
                model: ModelSettings::default(),
                instructions: String::new(),
                context_window_tokens: orvek_harness::context::MAX_WINDOW_TOKENS,
            },
            None,
        )
        .unwrap();
    let request = Uuid::new_v4();
    state = store
        .session_command(
            state.id,
            state.revision,
            request,
            SessionCommand::Input {
                kind: RequestKind::Conversation,
                content: vec![json!({"role":"user","content":"inspect"})],
            },
        )
        .unwrap();
    state = store.session_command(state.id, state.revision, Uuid::new_v4(),
        SessionCommand::Response { request, items: vec![json!({"type":"function_call","call_id":"large","name":"read_file","arguments":"{}"})] }).unwrap();
    let output = json!({"value":"\u{1}💎".repeat(400000)}).to_string();
    let command = SessionCommand::ToolResult {
        request,
        call_id: "large".into(),
        output: output.clone(),
    };
    assert!(serde_json::to_vec(&command).unwrap().len() > 512 * 1024);
    let operation = Uuid::new_v4();
    let revision = state.revision;
    let connection = rusqlite::Connection::open(state_root.join("v1.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_tool_end BEFORE INSERT ON events WHEN json_extract(CAST(NEW.event AS TEXT), '$.data.command.type') = 'tool_result_end' BEGIN SELECT RAISE(FAIL, 'fixture refuses final record'); END;").unwrap();
    assert!(
        store
            .session_command(state.id, revision, operation, command.clone())
            .is_err()
    );
    assert_eq!(
        store.load_session(state.id).unwrap(),
        state,
        "all fragments must roll back together"
    );
    connection
        .execute_batch("DROP TRIGGER reject_tool_end;")
        .unwrap();
    state = store
        .session_command(state.id, revision, operation, command.clone())
        .unwrap();
    let journal_bytes = std::fs::metadata(state_root.join("v1.sqlite3-wal"))
        .unwrap()
        .len();
    assert!(
        journal_bytes < (output.len() * 8) as u64,
        "framing must not rewrite the growing session for every part: {journal_bytes} WAL bytes for {} result bytes",
        output.len()
    );
    assert_eq!(state.history.last().unwrap()["output"], output);
    assert_eq!(
        state.tool_calls["large"].output,
        Some(Digest::of(output.as_bytes()))
    );
    assert_eq!(
        store
            .session_command(state.id, revision, operation, command.clone())
            .unwrap(),
        state
    );
    let mut records = Vec::new();
    let mut after = 0;
    loop {
        let page = store.journal_page(after, 256).unwrap();
        let Some(last) = page.last() else {
            break;
        };
        assert!(last.sequence > after);
        after = last.sequence;
        records.extend(page);
    }
    eprintln!(
        "framing metrics: {} result bytes, {journal_bytes} WAL bytes, {} records",
        output.len(),
        records.len()
    );
    assert!(
        records
            .iter()
            .all(|r| serde_json::to_vec(&r.event).unwrap().len() <= 512 * 1024)
    );
    assert_eq!(
        records
            .iter()
            .filter(|r| matches!(
                serde_json::from_value::<SessionEvent>(r.event.clone()),
                Ok(SessionEvent::Command {
                    command: SessionCommand::ToolResultEnd { .. },
                    ..
                })
            ))
            .count(),
        1
    );
    drop(store);
    let mut store = Store::open(&state_root).unwrap();
    assert_eq!(store.load_session(state.id).unwrap(), state);
    assert_eq!(
        store
            .session_command(state.id, revision, operation, command)
            .unwrap(),
        state
    );
    let trace = TraceBundle::export(
        &state_root,
        None,
        TraceLimits::default(),
        &Default::default(),
        None,
    )
    .unwrap();
    assert_eq!(trace.replay().unwrap().sessions[&state.id], state);
    let first_part = records
        .iter()
        .find(|record| {
            matches!(
                serde_json::from_value::<SessionEvent>(record.event.clone()),
                Ok(SessionEvent::Command {
                    command: SessionCommand::ToolResultPart { .. },
                    ..
                })
            )
        })
        .unwrap();
    let prefix = TraceBundle::export(
        &state_root,
        Some(first_part.sequence),
        TraceLimits::default(),
        &Default::default(),
        None,
    )
    .unwrap()
    .replay()
    .unwrap();
    let partial = &prefix.sessions[&state.id];
    assert!(partial.tool_calls["large"].output.is_none());
    assert!(
        !partial
            .history
            .iter()
            .any(|item| item["type"] == "function_call_output")
    );
}

#[test]
fn malformed_tool_output_parts_cannot_settle_a_result() {
    use orvek_harness::{Digest, session::ToolOutputBuffers};
    let mut pending = ToolOutputBuffers::default();
    let request = Uuid::new_v4();
    assert!(pending.append(request, "call", 1, "suffix").is_err());
    pending.append(request, "call", 0, "hello ").unwrap();
    assert!(
        pending
            .append(Uuid::new_v4(), "call", 6, "foreign")
            .is_err()
    );
    assert!(pending.append(request, "call", 0, "duplicate").is_err());
    assert!(
        pending
            .finish(request, "call", Digest::of(b"wrong"))
            .is_err()
    );
    pending.append(request, "call", 6, "world").unwrap();
    assert_eq!(
        pending
            .finish(request, "call", Digest::of(b"hello world"))
            .unwrap(),
        "hello world"
    );
    assert!(
        pending
            .finish(request, "call", Digest::of(b"hello world"))
            .is_err()
    );
}
