use base64::{Engine as _, engine::general_purpose::STANDARD};
use orvek_harness::{
    Store,
    context::{self, TextReadError},
    inference::ModelSettings,
    session::{SessionCommand, SessionConfig, SessionId},
    state::RequestKind,
};
use serde_json::json;
use uuid::Uuid;

fn config(workspace: &std::path::Path) -> SessionConfig {
    SessionConfig {
        workspace: workspace.to_owned(),
        model: ModelSettings::default(),
        instructions: String::new(),
        context_window_tokens: context::DEFAULT_WINDOW_TOKENS,
    }
}

#[test]
fn exact_text_pages_resume_and_obey_branch_cutoffs_without_rerunning_tools() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let parent = SessionId::new();
    let child = SessionId::new();
    let unrelated = SessionId::new();
    let output = format!("{}needle-at-the-end☃", "0123456789abcdef\n".repeat(4096));
    let output_index;
    let child_cursor;
    {
        let mut store = Store::open(&state_root).unwrap();
        let mut state = store
            .create_session(parent, config(root.path()), None)
            .unwrap();
        let request = Uuid::new_v4();
        state = store
            .session_command(
                parent,
                state.revision,
                request,
                SessionCommand::Input {
                    kind: RequestKind::Conversation,
                    content: vec![json!({"role":"user","content":"produce exact output"})],
                },
            )
            .unwrap();
        state = store
            .session_command(
                parent,
                state.revision,
                Uuid::new_v4(),
                SessionCommand::Response {
                    request,
                    items: vec![json!({
                        "type":"function_call",
                        "call_id":"exact-call",
                        "name":"run_command",
                        "arguments":"{}"
                    })],
                },
            )
            .unwrap();
        state = store
            .session_command(
                parent,
                state.revision,
                Uuid::new_v4(),
                SessionCommand::ToolResult {
                    request,
                    call_id: "exact-call".into(),
                    output: output.clone(),
                },
            )
            .unwrap();
        state = store
            .session_command(
                parent,
                state.revision,
                Uuid::new_v4(),
                SessionCommand::TurnSettled {
                    request,
                    outcome: None,
                    error: None,
                },
            )
            .unwrap();
        output_index = state
            .history
            .iter()
            .position(|item| item["type"] == "function_call_output")
            .unwrap();
        let first_projection = context::project(&state, 65_536).unwrap();
        let repeated_projection = context::project(&state, 65_536).unwrap();
        assert_eq!(first_projection, repeated_projection);

        let inherited = state.fork_cursor();
        let child_state = store
            .create_session(child, config(root.path()), Some(inherited.clone()))
            .unwrap();
        child_cursor = child_state.cursor();
        store
            .create_session(unrelated, config(root.path()), None)
            .unwrap();
        let scoped = store.scoped_history(child, parent, None).unwrap();
        assert_eq!(scoped.cursor(), inherited);
        assert!(store.scoped_history(unrelated, parent, None).is_err());

        let mut recovered = Vec::new();
        let mut offset = 0;
        loop {
            let page =
                context::read_text_page(&scoped.history, output_index, 0, offset, 1024, None)
                    .unwrap();
            recovered.extend(STANDARD.decode(page.bytes_base64).unwrap());
            let Some(next) = page.next else { break };
            offset = next;
        }
        assert_eq!(recovered, output.as_bytes());
        let search = context::read_text_page(
            &scoped.history,
            output_index,
            0,
            0,
            32,
            Some("needle-at-the-end"),
        )
        .unwrap();
        assert_eq!(
            search.matches,
            vec![output.find("needle-at-the-end").unwrap()]
        );
        let many =
            context::read_text_page(&scoped.history, output_index, 0, 0, 1, Some("0123")).unwrap();
        assert_eq!(many.matches.len(), 64);
        let continuation = many.next_search.unwrap();
        let continued = context::read_text_page(
            &scoped.history,
            output_index,
            0,
            continuation,
            1,
            Some("0123"),
        )
        .unwrap();
        assert_eq!(continued.matches[0], continuation);
        assert!(continued.matches[0] > *many.matches.last().unwrap());
        assert_eq!(
            context::read_text_page(&scoped.history, output_index, 1, 0, 1, None),
            Err(TextReadError::Content)
        );
    }

    let store = Store::open(&state_root).unwrap();
    let resumed = store.scoped_history(child, parent, None).unwrap();
    let tail = context::read_text_page(
        &resumed.history,
        output_index,
        0,
        output.len() - 24,
        24,
        None,
    )
    .unwrap();
    assert_eq!(
        STANDARD.decode(tail.bytes_base64).unwrap(),
        output.as_bytes()[output.len() - 24..]
    );
    assert_eq!(store.load_session(child).unwrap().cursor(), child_cursor);
}
