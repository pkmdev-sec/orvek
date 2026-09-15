use orvek_harness::{
    BaselineReason, Channel, HarnessProvenance, Store, StoreError,
    inference::ModelSettings,
    session::{SessionAdmissionRequest, SessionCommand, SessionConfig, SessionId},
};
use serde_json::json;
use uuid::Uuid;

fn config(workspace: &std::path::Path) -> SessionConfig {
    SessionConfig {
        workspace: workspace.to_owned(),
        model: ModelSettings::default(),
        instructions: "untrusted legacy fixture text".into(),
        context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
    }
}

#[test]
fn fixture_sessions_are_explicitly_baseline_bound_and_survive_replay() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let mut store = Store::open(&state_root).unwrap();
    let session = store
        .create_session(SessionId::new(), config(root.path()), None)
        .unwrap();
    let profile = session.admission().unwrap().clone();

    assert_eq!(
        profile.provenance(),
        HarnessProvenance::CompiledBaseline {
            reason: BaselineReason::StoreFixture,
        }
    );
    assert_eq!(profile.workspace(), root.path());
    assert_eq!(
        profile.context_window_tokens(),
        orvek_harness::context::DEFAULT_WINDOW_TOKENS
    );
    assert_ne!(
        profile.binding().behavior(),
        orvek_harness::Digest::of(b"untrusted legacy fixture text"),
        "legacy fixture text must not become privileged behavior"
    );

    drop(store);
    let reopened = Store::open(&state_root).unwrap();
    assert_eq!(
        reopened.load_session(session.id).unwrap().admission(),
        Some(&profile)
    );
}

#[test]
fn fork_handoff_and_compaction_keep_the_exact_binding() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let original = store
        .create_session(SessionId::new(), config(root.path()), None)
        .unwrap();
    let profile = original.admission().unwrap().clone();

    let fork = store
        .create_session(
            SessionId::new(),
            config(root.path()),
            Some(original.fork_cursor()),
        )
        .unwrap();
    let handoff = store
        .create_handoff_session(SessionId::new(), original.fork_cursor())
        .unwrap();
    let compacted = store
        .session_command(
            original.id,
            original.revision,
            Uuid::new_v4(),
            SessionCommand::ContextProjected {
                source_revision: original.revision,
                projection: vec![json!({"role":"user","content":"bounded summary"})],
            },
        )
        .unwrap();

    assert_eq!(fork.admission(), Some(&profile));
    assert_eq!(handoff.admission(), Some(&profile));
    assert_eq!(compacted.admission(), Some(&profile));
}

#[test]
fn bound_sessions_reject_settings_mutation_without_partial_progress() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let session = store
        .create_session(SessionId::new(), config(root.path()), None)
        .unwrap();

    let error = store
        .session_command(
            session.id,
            session.revision,
            Uuid::new_v4(),
            SessionCommand::SettingsChanged(ModelSettings {
                reasoning_mode: orvek_harness::inference::ReasoningMode::Pro,
                ..ModelSettings::default()
            }),
        )
        .unwrap_err();

    assert!(matches!(error, StoreError::Json(_)));
    let unchanged = store.load_session(session.id).unwrap();
    assert_eq!(unchanged.revision, session.revision);
    assert_eq!(unchanged.admission(), session.admission());
}

#[test]
fn external_admission_requests_cannot_carry_instruction_text() {
    let root = tempfile::tempdir().unwrap();
    let request = json!({
        "workspace": root.path(),
        "model": ModelSettings::default(),
        "context_window_tokens": orvek_harness::context::DEFAULT_WINDOW_TOKENS,
        "channel": Channel::Stable,
        "instructions": "forged privileged authority",
    });

    assert!(serde_json::from_value::<SessionAdmissionRequest>(request).is_err());
}
