use orvek_harness::{
    Store,
    context::{self, ContextRepresentation},
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

fn input(store: &mut Store, state: &mut orvek_harness::session::SessionState, request: Uuid) {
    *state = store
        .session_command(
            state.id,
            state.revision,
            request,
            SessionCommand::Input {
                kind: RequestKind::Conversation,
                content: vec![json!({"role":"user","content":"run it"})],
            },
        )
        .unwrap();
}

fn tool_result(
    store: &mut Store,
    state: &mut orvek_harness::session::SessionState,
    request: Uuid,
    call_id: &str,
    output: &str,
) {
    *state = store
        .session_command(
            state.id,
            state.revision,
            Uuid::new_v4(),
            SessionCommand::Response {
                request,
                items: vec![json!({"type":"function_call","call_id":call_id,"name":"run_command","arguments":"{}"})],
            },
        )
        .unwrap();
    *state = store
        .session_command(
            state.id,
            state.revision,
            Uuid::new_v4(),
            SessionCommand::ToolResult {
                request,
                call_id: call_id.into(),
                output: output.into(),
            },
        )
        .unwrap();
}

fn settle(store: &mut Store, state: &mut orvek_harness::session::SessionState, request: Uuid) {
    *state = store
        .session_command(
            state.id,
            state.revision,
            Uuid::new_v4(),
            SessionCommand::TurnSettled {
                request,
                outcome: None,
                error: None,
            },
        )
        .unwrap();
}

fn bitmap_pages(view: &context::ContextView) -> Vec<orvek_harness::Digest> {
    view.manifest
        .segments
        .iter()
        .flat_map(|segment| match &segment.representation {
            ContextRepresentation::Bitmap(bitmap) => bitmap
                .pages
                .iter()
                .map(|page| page.digest)
                .collect::<Vec<_>>(),
            ContextRepresentation::NativeText { .. } => Vec::new(),
        })
        .collect()
}

#[test]
fn settled_successful_output_renders_once_and_reuses_after_append_and_resume() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let session = SessionId::new();
    let mut store = Store::open(&state_root).unwrap();
    let mut state = store
        .create_session(session, config(root.path()), None)
        .unwrap();

    let first = Uuid::new_v4();
    input(&mut store, &mut state, first);
    tool_result(
        &mut store,
        &mut state,
        first,
        "successful",
        &serde_json::to_string(&json!({"stdout":"stable output\n".repeat(80),"exit_code":0}))
            .unwrap(),
    );
    let mut live = context::project(&state, 64 * 1024).unwrap();
    context::render_eligible(&mut live, &state, store.public_artifacts(), || false);
    assert!(
        bitmap_pages(&live).is_empty(),
        "active output must remain native"
    );

    settle(&mut store, &mut state, first);
    let mut rendered = context::project(&state, 64 * 1024).unwrap();
    context::render_eligible(&mut rendered, &state, store.public_artifacts(), || false);
    let original_pages = bitmap_pages(&rendered);
    assert_eq!(original_pages.len(), 1);
    assert!(rendered.valid_for(&state));
    state = store
        .session_command(
            session,
            state.revision,
            Uuid::new_v4(),
            SessionCommand::ContextProjected {
                source_revision: state.revision,
                view: Some(rendered.clone()),
                projection: Vec::new(),
            },
        )
        .unwrap();

    let second = Uuid::new_v4();
    input(&mut store, &mut state, second);
    tool_result(
        &mut store,
        &mut state,
        second,
        "failed",
        r#"{"error":"command failed"}"#,
    );
    settle(&mut store, &mut state, second);
    let mut later = context::project(&state, 64 * 1024).unwrap();
    context::reuse_representations(&mut later, state.context_view.as_ref().unwrap(), &state);
    context::render_eligible(&mut later, &state, store.public_artifacts(), || false);
    assert_eq!(bitmap_pages(&later), original_pages);

    drop(store);
    let store = Store::open(&state_root).unwrap();
    let resumed = store.load_session(session).unwrap();
    let mut after_resume = context::project(&resumed, 64 * 1024).unwrap();
    context::reuse_representations(
        &mut after_resume,
        resumed.context_view.as_ref().unwrap(),
        &resumed,
    );
    context::render_eligible(
        &mut after_resume,
        &resumed,
        store.public_artifacts(),
        || false,
    );
    assert_eq!(bitmap_pages(&after_resume), original_pages);
}
