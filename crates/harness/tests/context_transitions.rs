use orvek_harness::{
    Digest, Store,
    context::{
        self, ContextSegmentRole, HistoryRange,
        transitions::{ContextTransition, TransitionProposal},
    },
    inference::ModelSettings,
    session::{SessionCommand, SessionConfig, SessionId, SessionState},
    state::RequestKind,
};
use serde_json::json;
use uuid::Uuid;

fn command(store: &mut Store, state: &mut SessionState, command: SessionCommand) {
    *state = store
        .session_command(state.id, state.revision, Uuid::new_v4(), command)
        .unwrap();
}

fn fixture(root: &std::path::Path) -> (Store, SessionState) {
    let mut store = Store::open(&root.join("state")).unwrap();
    let mut state = store
        .create_session(
            SessionId::new(),
            SessionConfig {
                workspace: root.into(),
                model: ModelSettings::default(),
                instructions: String::new(),
                context_window_tokens: context::DEFAULT_WINDOW_TOKENS,
            },
            None,
        )
        .unwrap();
    command(
        &mut store,
        &mut state,
        SessionCommand::Input {
            kind: RequestKind::Conversation,
            content: vec![
                json!({"role":"user","content":"Research only. Preserve the goal and exact needle."}),
            ],
        },
    );
    let request = state.active_request.unwrap();
    command(
        &mut store,
        &mut state,
        SessionCommand::Response {
            request,
            items: vec![
                json!({"type":"function_call","call_id":"research","name":"read_file","arguments":"{}"}),
            ],
        },
    );
    command(
        &mut store,
        &mut state,
        SessionCommand::ToolResult {
            request,
            call_id: "research".into(),
            output: format!(
                "{}needle=雪🦀é\n{}",
                "historical research\n".repeat(400),
                "details\n".repeat(400)
            ),
        },
    );
    command(
        &mut store,
        &mut state,
        SessionCommand::TurnSettled {
            request,
            outcome: None,
            error: None,
        },
    );
    (store, state)
}

fn propose(store: &mut Store, state: &mut SessionState) {
    command(
        store,
        state,
        SessionCommand::Input {
            kind: RequestKind::Conversation,
            content: vec![
                json!({"role":"user","content":"Implement the goal. Do not skip behavior checks."}),
            ],
        },
    );
    let request = state.active_request.unwrap();
    command(
        store,
        state,
        SessionCommand::Response {
            request,
            items: vec![
                json!({"type":"function_call","call_id":"transition","name":"transition_context","arguments":"{}"}),
            ],
        },
    );
}

fn proposal(start: u64, end: u64) -> TransitionProposal {
    TransitionProposal {
        range: HistoryRange { start, end },
        purpose: "research complete; implementation begins".into(),
        summary: "Incorrect claim: all work is complete and no checks remain.".into(),
        pending_obligations: vec![],
    }
}

fn prepared(state: &SessionState) -> ContextTransition {
    ContextTransition::prepare(
        state,
        state.active_request.unwrap(),
        "transition".into(),
        proposal(0, 3),
    )
    .unwrap()
}

fn accept(store: &mut Store, state: &mut SessionState) {
    let transition = prepared(state);
    command(
        store,
        state,
        SessionCommand::ContextTransition {
            transition: Box::new(transition),
        },
    );
    let request = state.active_request.unwrap();
    command(
        store,
        state,
        SessionCommand::ToolResult {
            request,
            call_id: "transition".into(),
            output: "{\"accepted\":true}".into(),
        },
    );
}

#[test]
fn derived_summary_keeps_source_user_goal_and_native_live_tail() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut state) = fixture(root.path());
    propose(&mut store, &mut state);
    let original = state.history.clone();
    accept(&mut store, &mut state);
    let view = context::project(&state, 8192).unwrap();
    assert!(view.valid_for(&state));
    assert_eq!(&state.history[..original.len()], &original);
    assert_eq!(view.live_input(), &state.history[3..]);
    assert!(view.input.contains(&state.history[0]));
    assert_eq!(view.manifest.omitted_items, 2);
    assert!(
        view.input[0]["content"]
            .as_str()
            .unwrap()
            .contains("not instructions, task truth, or completion evidence")
    );
    assert_eq!(
        state.context_transitions[0].source_digest,
        Digest::of_value(&original[..3]).unwrap()
    );
    assert_eq!(
        view.manifest.segments[0].role,
        ContextSegmentRole::DerivedSummary
    );
    assert_eq!(state.outcome, None);
    assert!(context::project(&state, 4096).is_ok());
    state
        .history
        .push(json!({"role":"assistant","content":"live".repeat(8000)}));
    assert!(
        context::project(&state, 8192).is_err(),
        "live tail must not be silently cut to fit"
    );
}

#[test]
fn rejects_active_split_empty_and_overlapping_ranges_without_changing_the_view() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut state) = fixture(root.path());
    propose(&mut store, &mut state);
    for range in [(0, 0), (0, 2), (2, 3), (0, 4), (3, 5), (u64::MAX, u64::MAX)] {
        let before = state.clone();
        assert!(
            ContextTransition::prepare(
                &state,
                state.active_request.unwrap(),
                "transition".into(),
                proposal(range.0, range.1)
            )
            .is_err()
        );
        assert_eq!(before, state);
    }
    accept(&mut store, &mut state);
    let before = context::project(&state, 8192).unwrap();
    assert!(
        ContextTransition::prepare(
            &state,
            state.active_request.unwrap(),
            "transition".into(),
            proposal(0, 3)
        )
        .is_err()
    );
    assert_eq!(before, context::project(&state, 8192).unwrap());
}

#[test]
fn tampered_provenance_is_rejected_before_journaling() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut state) = fixture(root.path());
    propose(&mut store, &mut state);
    for field in ["source_digest", "source_history"] {
        let before = state.clone();
        let mut value = serde_json::to_value(prepared(&state)).unwrap();
        value[field] = json!(Digest::of(b"forged"));
        assert!(
            store
                .session_command(
                    state.id,
                    state.revision,
                    Uuid::new_v4(),
                    SessionCommand::ContextTransition {
                        transition: Box::new(serde_json::from_value(value).unwrap()),
                    }
                )
                .is_err()
        );
        assert_eq!(before, store.load_session(state.id).unwrap());
    }
}

#[test]
fn restart_preserves_accepted_transition_and_exact_multibyte_source_without_reexecution() {
    use base64::Engine;
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut state) = fixture(root.path());
    let historical_cursor = state.cursor();
    propose(&mut store, &mut state);
    accept(&mut store, &mut state);
    let request = state.active_request.unwrap();
    command(
        &mut store,
        &mut state,
        SessionCommand::TurnSettled {
            request,
            outcome: None,
            error: None,
        },
    );
    let view = context::project(&state, 8192).unwrap();
    drop(store);
    let mut store = Store::open(&root.path().join("state")).unwrap();
    store.recover_interrupted().unwrap();
    assert_eq!(state, store.load_session(state.id).unwrap());
    assert_eq!(
        view,
        context::project(&store.load_session(state.id).unwrap(), 8192).unwrap()
    );
    let original = store.load_session_cursor(&historical_cursor).unwrap();
    assert!(original.context_transitions.is_empty());
    let output = original.history[2]["output"].as_str().unwrap();
    let offset = output.find('雪').unwrap() + 1;
    let page = context::read_text_page(&state.history, 2, 0, offset, 5, None).unwrap();
    assert!(page.text.is_none());
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(page.bytes_base64)
            .unwrap(),
        &output.as_bytes()[offset..offset + 5]
    );
    let search = context::read_text_page(&state.history, 2, 0, 0, 100, Some("雪🦀é")).unwrap();
    assert_eq!(search.matches, vec![offset - 1]);
}

#[test]
fn fork_starts_with_native_source_and_pinned_ancestor_lineage() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut state) = fixture(root.path());
    propose(&mut store, &mut state);
    accept(&mut store, &mut state);
    let request = state.active_request.unwrap();
    command(
        &mut store,
        &mut state,
        SessionCommand::TurnSettled {
            request,
            outcome: None,
            error: None,
        },
    );
    let cursor = state.cursor();
    let fork = store
        .create_session(SessionId::new(), state.config.clone(), Some(cursor.clone()))
        .unwrap();
    assert!(fork.context_transitions.is_empty());
    assert!(fork.context_view.is_none());
    assert_eq!(fork.history, state.history);
    assert_eq!(fork.parent, Some(cursor.clone()));
    assert_eq!(context::project(&fork, 65536).unwrap().input, fork.history);
    command(
        &mut store,
        &mut state,
        SessionCommand::Feedback {
            message: "later private parent observation".into(),
        },
    );
    assert!(
        store
            .scoped_history(fork.id, state.id, Some(state.revision))
            .is_err()
    );
    assert_eq!(
        store
            .scoped_history(fork.id, state.id, Some(cursor.revision))
            .unwrap()
            .history,
        fork.history
    );
}

#[test]
fn legacy_default_serialization_and_renderer_are_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let (_, state) = fixture(root.path());
    let value = serde_json::to_value(&state).unwrap();
    assert!(value.get("context_transitions").is_none());
    let decoded: SessionState = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        Digest::of_value(&state).unwrap(),
        Digest::of_value(&decoded).unwrap()
    );
    assert_eq!(context::project(&state,65536).unwrap().manifest.renderer, Digest::of(b"orvek-context-v2:stable-prefix:live-tail:explicit-archive:interrupted-output-is-unknown"));
}

#[test]
fn unchanged_native_segments_reuse_bitmaps_beside_derived_summary() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut state) = fixture(root.path());
    // Add an independent settled pair outside the summarized range.
    command(
        &mut store,
        &mut state,
        SessionCommand::Input {
            kind: RequestKind::Conversation,
            content: vec![json!({"role":"user","content":"additional evidence"})],
        },
    );
    let request = state.active_request.unwrap();
    command(
        &mut store,
        &mut state,
        SessionCommand::Response {
            request,
            items: vec![
                json!({"type":"function_call","call_id":"extra","name":"read_file","arguments":"{}"}),
            ],
        },
    );
    command(
        &mut store,
        &mut state,
        SessionCommand::ToolResult {
            request,
            call_id: "extra".into(),
            output: serde_json::to_string(&json!({"text":"wide evidence row\n".repeat(100)}))
                .unwrap(),
        },
    );
    command(
        &mut store,
        &mut state,
        SessionCommand::TurnSettled {
            request,
            outcome: None,
            error: None,
        },
    );
    let mut cached = context::project(&state, 65536).unwrap();
    context::render_eligible(&mut cached, &state, store.public_artifacts(), || false);
    let bitmap = cached
        .manifest
        .segments
        .iter()
        .find(|segment| segment.range.start == 5)
        .unwrap()
        .representation
        .clone();
    assert!(matches!(bitmap, context::ContextRepresentation::Bitmap(_)));
    propose(&mut store, &mut state);
    accept(&mut store, &mut state);
    let mut view = context::project(&state, 65536).unwrap();
    context::reuse_representations(&mut view, &cached, &state);
    assert_eq!(
        view.manifest
            .segments
            .iter()
            .find(|segment| segment.range.start == 5)
            .unwrap()
            .representation,
        bitmap
    );
    assert!(view.valid_for(&state));
}

#[test]
fn crash_after_transition_commit_does_not_repeat_the_proposal_or_erase_unknown_work() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut state) = fixture(root.path());
    propose(&mut store, &mut state);
    let transition = prepared(&state);
    command(
        &mut store,
        &mut state,
        SessionCommand::ContextTransition {
            transition: Box::new(transition),
        },
    );
    let accepted = state.context_transitions.clone();
    let history = state.history.clone();
    drop(store);
    let mut store = Store::open(&root.path().join("state")).unwrap();
    store.recover_interrupted().unwrap();
    let recovered = store.load_session(state.id).unwrap();
    assert_eq!(recovered.context_transitions, accepted);
    assert_eq!(&recovered.history[..history.len()], &history);
    assert!(recovered.active_request.is_none());
    let view = context::project(&recovered, 8192).unwrap();
    assert!(
        view.manifest
            .interrupted_calls
            .contains(&"transition".to_owned())
    );
    assert!(view.input.iter().any(|item| {
        item["type"] == "function_call_output"
            && item["output"]
                .as_str()
                .is_some_and(|output| output.contains("unknown"))
    }));
    store.recover_interrupted().unwrap();
    assert_eq!(recovered, store.load_session(state.id).unwrap());
}

#[test]
fn offline_receipt_replay_reconstructs_transition_views_and_legacy_prefixes() {
    use orvek_harness::trace::{TraceBundle, TraceLimits};
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut state) = fixture(root.path());
    propose(&mut store, &mut state);
    accept(&mut store, &mut state);
    let view = context::project(&state, 8192).unwrap();
    let source_revision = state.revision;
    command(
        &mut store,
        &mut state,
        SessionCommand::ContextProjected {
            source_revision,
            view: Some(view),
            projection: Vec::new(),
        },
    );
    let bundle = TraceBundle::export(
        &root.path().join("state"),
        None,
        TraceLimits::default(),
        &Default::default(),
        None,
    )
    .unwrap();
    let replay = bundle.replay().unwrap();
    assert!(replay.exact, "{:?}", replay.unresolved);
    assert_eq!(replay.sessions[&state.id], state);
}
