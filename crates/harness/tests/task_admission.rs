use orvek_harness::{
    Digest, Store, StoreError,
    admission::{self, Proposal, ProposedCheck, RepositoryProfile},
    artifacts::PublicArtifactRef,
    contract::*,
    inference::ModelSettings,
    session::{SessionConfig, SessionId},
    state::{JobStatus, Phase},
    verification::{CheckProgram, ControlFailure, Expectation, Probe},
    workspace::{Snapshot, SnapshotPolicy},
};
use std::collections::BTreeMap;
use uuid::Uuid;

fn proposal() -> Proposal {
    Proposal {
        outcome: "add both operands".into(),
        scope: "addition command".into(),
        requirements: vec![Requirement {
            id: "sum".into(),
            behavior: "2 plus 2 yields 4".into(),
            origin: Origin::User("Fix addition".into()),
            checks: vec!["sum".into()],
            depends_on: vec![],
        }],
        checks: BTreeMap::from([(
            "sum".into(),
            ProposedCheck {
                purpose: "observe exact arithmetic output".into(),
                kind: CheckKind::Behavior,
                program: CheckProgram {
                    version: 1,
                    probes: vec![Probe::Command {
                        id: "sum".into(),
                        command: "./add 2 2".into(),
                        exit_code: 0,
                        stdout: Some(Expectation::Equals("4\n".into())),
                        stderr: None,
                    }],
                    control_failure: Some(ControlFailure {
                        probe: "sum".into(),
                        stdout: Some(Expectation::Equals("3\n".into())),
                        stderr: None,
                    }),
                },
                baseline_failure: true,
                control_omission: None,
            },
        )]),
        protected_behavior: vec![],
        assumptions: vec![],
        open_questions: vec![],
    }
}

#[test]
fn queue_edits_reordering_and_promotion_are_atomic_and_distinct_from_normal_queueing() {
    use orvek_harness::{
        input,
        submission::{Schedule, SubmissionStatus, WorkIntent},
    };
    use serde_json::json;
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
            &serde_json::to_vec(&admission::RequestPolicy {
                version: 1,
                delivery: DeliveryKind::Source,
                profile: RepositoryProfile {
                    version: 1,
                    name: "fixture".into(),
                    checks: BTreeMap::new(),
                },
            })
            .unwrap(),
        )
        .unwrap()
        .digest();
    let (_, task, _) = store
        .start_request(
            session.id,
            Uuid::new_v4(),
            "Fix addition".into(),
            Limits::default(),
            policy,
        )
        .unwrap();
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let input = input::prepare(
        vec![json!({"type":"input_text","text":"also handle zero"})],
        store.public_artifacts(),
    )
    .unwrap()
    .artifact;
    let revised = input::prepare(
        vec![json!({"type":"input_text","text":"also handle negative operands"})],
        store.public_artifacts(),
    )
    .unwrap()
    .artifact;
    let intent = WorkIntent::Continue {
        task: task.id,
        scope_revision: task.scope_revision,
        schedule: Schedule::Queue,
    };
    store
        .submit(session.id, first, input, intent.clone())
        .unwrap();
    store
        .submit(session.id, second, input, intent.clone())
        .unwrap();
    assert_eq!(
        store.load(task.id).unwrap(),
        task,
        "plain queueing does not change active task authority"
    );
    let edit = Uuid::new_v4();
    let edited = store
        .edit_submission(session.id, edit, second, input, Some(revised))
        .unwrap();
    assert_eq!(edited.input, revised);
    assert_eq!(edited.initial_input, input);
    assert_eq!(
        store.submit(session.id, second, input, intent).unwrap(),
        edited,
        "lost initial acknowledgement can still be retried after a later edit"
    );
    assert_eq!(
        store
            .edit_submission(session.id, edit, second, input, Some(revised))
            .unwrap(),
        edited
    );
    assert!(
        store
            .edit_submission(session.id, Uuid::new_v4(), second, input, Some(revised))
            .is_err()
    );
    store
        .move_submission(session.id, Uuid::new_v4(), second, revised, Some(first))
        .unwrap();
    assert_eq!(
        store.next_submission(session.id).unwrap().unwrap().id,
        second
    );
    assert_eq!(store.load(task.id).unwrap(), task, "move does not steer");
    store
        .edit_submission(session.id, Uuid::new_v4(), first, input, None)
        .unwrap();
    let steered = store.load(task.id).unwrap();
    assert_eq!(steered.scope_revision, task.scope_revision + 1);
    assert!(steered.amendment_pending);
    assert_eq!(
        store.next_submission(session.id).unwrap().unwrap().id,
        first
    );
    let page = store.submissions(session.id, 0, 1).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.next, Some(1));
    assert_eq!(page.submissions[0].id, first);
    assert_eq!(page.journal_sequence, store.journal_head().unwrap());
    store
        .set_submission_status(session.id, first, SubmissionStatus::Running)
        .unwrap();
    assert!(
        store
            .edit_submission(session.id, Uuid::new_v4(), first, input, Some(revised))
            .is_err()
    );
    assert!(
        store
            .move_submission(session.id, Uuid::new_v4(), first, input, None)
            .is_err()
    );
    drop(store);
    let store = Store::open(&root.path().join("state")).unwrap();
    assert_eq!(
        store
            .submissions(session.id, 0, 64)
            .unwrap()
            .submissions
            .iter()
            .map(|s| s.id)
            .collect::<Vec<_>>(),
        vec![first, second]
    );
    assert_eq!(store.load(task.id).unwrap(), steered);
}

#[test]
fn queued_followups_revoke_old_authority_and_keep_obligations_workspace_and_spend() {
    use orvek_harness::{
        input,
        session::SessionCommand,
        state::{Candidate, Outcome, Usage},
        submission::{SubmissionStatus, WorkIntent},
    };
    use serde_json::json;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("add"), "original source").unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let session = store
        .create_session(
            SessionId::new(),
            SessionConfig {
                workspace: source.clone(),
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
            &serde_json::to_vec(&admission::RequestPolicy {
                version: 1,
                delivery: DeliveryKind::Source,
                profile: RepositoryProfile {
                    version: 1,
                    name: "fixture".into(),
                    checks: BTreeMap::new(),
                },
            })
            .unwrap(),
        )
        .unwrap()
        .digest();
    let initial = input::prepare(
        vec![json!({"type":"input_text","text":"Fix addition"})],
        store.public_artifacts(),
    )
    .unwrap();
    let request = Uuid::new_v4();
    let accepted = store
        .submit(
            session.id,
            request,
            initial.artifact,
            WorkIntent::NewTask {
                limits: Limits::default(),
                policy,
            },
        )
        .unwrap();
    assert_eq!(
        store
            .submit(
                session.id,
                request,
                initial.artifact,
                accepted.intent.clone()
            )
            .unwrap(),
        accepted
    );
    assert_eq!(store.pending_submissions().unwrap(), 1);
    store
        .set_submission_status(session.id, request, SubmissionStatus::Running)
        .unwrap();
    let (_, mut task, _) = store
        .start_prepared_request(session.id, request, initial, Limits::default(), policy)
        .unwrap();
    let baseline =
        Snapshot::capture(&source, SnapshotPolicy::default(), store.public_artifacts()).unwrap();
    let compiled =
        admission::compile(&task, proposal(), &baseline, store.public_artifacts()).unwrap();
    task = store
        .admit_contract(
            task.id,
            task.revision,
            compiled.contract,
            "initial request".into(),
            compiled.receipt,
        )
        .unwrap();
    let source_id = baseline.publish(store.public_artifacts()).unwrap();
    let environment = store
        .public_artifacts()
        .write(b"fixture environment")
        .unwrap()
        .digest();
    task = store
        .establish_baseline(
            task.id,
            task.revision,
            Candidate {
                source: source_id,
                artifact: source_id,
                environment,
                frozen: true,
                provenance: None,
            },
        )
        .unwrap();
    task = store
        .charge_usage(
            task.id,
            Uuid::new_v4(),
            Usage {
                tokens: 7,
                model_calls: 0,
            },
        )
        .unwrap();
    let before = task.clone();
    let followup = Uuid::new_v4();
    let input = input::prepare(
        vec![json!({"type":"input_text","text":"Also preserve zero addition"})],
        store.public_artifacts(),
    )
    .unwrap();
    let intent = WorkIntent::Continue {
        task: task.id,
        scope_revision: task.scope_revision,
        schedule: orvek_harness::submission::Schedule::Steer,
    };
    let accepted = store
        .submit(session.id, followup, input.artifact, intent.clone())
        .unwrap();
    task = store.load(task.id).unwrap();
    assert!(task.amendment_pending);
    assert!(task.generation > before.generation);
    assert_eq!(task.scope_revision, before.scope_revision + 1);
    assert_eq!(
        store
            .submit(session.id, followup, input.artifact, intent.clone())
            .unwrap(),
        accepted
    );
    assert_eq!(
        store.load(task.id).unwrap().scope_revision,
        task.scope_revision
    );
    assert!(
        store
            .submit(session.id, Uuid::new_v4(), input.artifact, intent)
            .is_err(),
        "stale scope is not another accepted user instruction"
    );
    assert!(store.complete(task.id, task.revision).is_err());
    assert!(store.start_job(task.id, task.revision, true, 1000).is_err());
    let mut weakened = before.contract.clone().unwrap();
    weakened.requirements[0].behavior = "anything is acceptable".into();
    assert!(
        store
            .accept_additive_contract(task.id, task.revision, weakened, compiled.receipt)
            .is_err()
    );
    task = store
        .stop(
            task.id,
            task.revision,
            Outcome::Blocked,
            "superseded turn".into(),
        )
        .unwrap();
    let active = store.load_session(session.id).unwrap();
    store
        .session_command(
            session.id,
            active.revision,
            Uuid::new_v4(),
            SessionCommand::TurnSettled {
                request,
                outcome: task.outcome,
                error: None,
            },
        )
        .unwrap();
    store
        .set_submission_status(
            session.id,
            request,
            SubmissionStatus::Finished {
                task: Some(task.id),
                outcome: task.outcome,
                error: None,
            },
        )
        .unwrap();
    drop(store);
    let mut store = Store::open(&root.path().join("state")).unwrap();
    store.recover_interrupted().unwrap();
    store.recover_submissions().unwrap();
    assert_eq!(
        store.next_submission(session.id).unwrap().unwrap().id,
        followup
    );
    store
        .set_submission_status(session.id, followup, SubmissionStatus::Running)
        .unwrap();
    let (_, task, created) = store.continue_submission(session.id, followup).unwrap();
    assert!(created);
    assert_eq!(task.id, before.id);
    assert_eq!(task.request, before.request);
    assert_eq!(task.baseline, before.baseline);
    assert_eq!(task.started_ms, before.started_ms);
    assert_eq!(task.usage, before.usage);
    let mut added = proposal();
    added.requirements[0].id = "zero".into();
    added.requirements[0].behavior = "zero remains an additive identity".into();
    added.requirements[0].origin = Origin::User("Also preserve zero addition".into());
    added.requirements[0].checks = vec!["zero".into()];
    let mut check = added.checks.remove("sum").unwrap();
    check.purpose = "observe zero identity".into();
    added.checks.insert("zero".into(), check);
    let compiled = admission::compile(&task, added, &baseline, store.public_artifacts()).unwrap();
    assert_eq!(compiled.contract.requirements.len(), 2);
    assert_eq!(
        compiled.contract.checks.get("sum"),
        before.contract.as_ref().unwrap().checks.get("sum")
    );
    let task = store
        .accept_additive_contract(task.id, task.revision, compiled.contract, compiled.receipt)
        .unwrap();
    assert!(!task.amendment_pending);
    assert_eq!(task.usage.tokens, 7);
    assert_eq!(task.contract_history.len(), 1);
}

#[test]
fn cancelled_and_interrupted_submissions_are_not_replayed_as_new_tasks() {
    use orvek_harness::{
        input,
        submission::{SubmissionStatus, WorkIntent},
    };
    use serde_json::json;
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
            &serde_json::to_vec(&admission::RequestPolicy {
                version: 1,
                delivery: DeliveryKind::Source,
                profile: RepositoryProfile {
                    version: 1,
                    name: "fixture".into(),
                    checks: BTreeMap::new(),
                },
            })
            .unwrap(),
        )
        .unwrap()
        .digest();
    let input = input::prepare(
        vec![json!({"type":"input_text","text":"Fix addition"})],
        store.public_artifacts(),
    )
    .unwrap();
    let cancelled = Uuid::new_v4();
    let uncertain = Uuid::new_v4();
    let unstarted = Uuid::new_v4();
    for request in [cancelled, uncertain, unstarted] {
        store
            .submit(
                session.id,
                request,
                input.artifact,
                WorkIntent::NewTask {
                    limits: Limits::default(),
                    policy,
                },
            )
            .unwrap();
    }
    store
        .set_submission_status(session.id, cancelled, SubmissionStatus::Cancelled)
        .unwrap();
    store
        .set_submission_status(session.id, unstarted, SubmissionStatus::Running)
        .unwrap();
    store
        .set_submission_status(session.id, uncertain, SubmissionStatus::Running)
        .unwrap();
    store
        .start_prepared_request(session.id, uncertain, input, Limits::default(), policy)
        .unwrap();
    drop(store);
    let mut store = Store::open(&root.path().join("state")).unwrap();
    store.recover_interrupted().unwrap();
    store.recover_submissions().unwrap();
    assert_eq!(
        store.submission(session.id, cancelled).unwrap().status,
        SubmissionStatus::Cancelled
    );
    assert_eq!(
        store.submission(session.id, uncertain).unwrap().status,
        SubmissionStatus::Interrupted
    );
    assert_eq!(
        store.next_submission(session.id).unwrap().unwrap().id,
        unstarted
    );
    assert_eq!(store.list().unwrap().len(), 1);
}

/// A session plus a stored request policy, shared by the ordinary
/// cancellation/recovery cases below.
fn ordinary_fixture() -> (tempfile::TempDir, Store, SessionId, Digest) {
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
            &serde_json::to_vec(&admission::RequestPolicy {
                version: 1,
                delivery: DeliveryKind::Source,
                profile: RepositoryProfile {
                    version: 1,
                    name: "fixture".into(),
                    checks: BTreeMap::new(),
                },
            })
            .unwrap(),
        )
        .unwrap()
        .digest();
    (root, store, session.id, policy)
}

/// A fixed, nonzero dispatch timestamp. The accounting projection has to be
/// rebuildable, so the record carries its own time rather than reading the
/// clock while folding.
const DISPATCHED_MS: u64 = 1_700_000_000_000;

#[test]
fn ordinary_cancellation_before_classification_is_never_dispatched() {
    use orvek_harness::{
        input,
        submission::{Schedule, SubmissionStatus, WorkIntent},
    };
    use serde_json::json;
    let (_root, mut store, session, policy) = ordinary_fixture();
    let input = input::prepare(
        vec![json!({"type":"input_text","text":"Fix addition"})],
        store.public_artifacts(),
    )
    .unwrap();
    let while_queued = Uuid::new_v4();
    let after_claim = Uuid::new_v4();
    for request in [while_queued, after_claim] {
        store
            .submit(
                session,
                request,
                input.artifact,
                WorkIntent::Ordinary {
                    limits: Limits::default(),
                    policy,
                    schedule: Schedule::Queue,
                },
            )
            .unwrap();
    }
    // Cancelled before the queue ever claims it.
    store
        .set_submission_status(session, while_queued, SubmissionStatus::Cancelled)
        .unwrap();
    // Claimed, then cancelled in the window before `begin_classification`.
    store
        .set_submission_status(session, after_claim, SubmissionStatus::Running)
        .unwrap();
    store
        .set_submission_status(session, after_claim, SubmissionStatus::Cancelled)
        .unwrap();

    for request in [while_queued, after_claim] {
        let error = store.begin_classification(session, request).unwrap_err();
        assert!(matches!(error, StoreError::Invalid(_)), "{error:?}");
        let submission = store.submission(session, request).unwrap();
        assert_eq!(submission.status, SubmissionStatus::Cancelled);
        assert!(
            submission.records.is_empty(),
            "a cancelled request never admitted a classifier call, so there is nothing to reconcile"
        );
    }
    assert!(store.next_submission(session).unwrap().is_none());
    let state = store.load_session(session).unwrap();
    assert_eq!(state.active_request, None);
    assert_eq!(state.current_task, None);
    assert!(state.tasks_by_request.is_empty());
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn ordinary_classification_interrupted_before_observation_keeps_the_pending_call() {
    use orvek_harness::{
        auxiliary::AuxiliaryRecord,
        input,
        submission::{Schedule, SubmissionStatus, WorkIntent},
    };
    use serde_json::json;
    let (root, mut store, session, policy) = ordinary_fixture();
    let input = input::prepare(
        vec![json!({"type":"input_text","text":"Fix addition"})],
        store.public_artifacts(),
    )
    .unwrap();
    let request = Uuid::new_v4();
    store
        .submit(
            session,
            request,
            input.artifact,
            WorkIntent::Ordinary {
                limits: Limits::default(),
                policy,
                schedule: Schedule::Queue,
            },
        )
        .unwrap();
    store
        .set_submission_status(session, request, SubmissionStatus::Running)
        .unwrap();
    store.begin_classification(session, request).unwrap();
    let call = Uuid::new_v4();
    let invocation = store.public_artifacts().write(b"{}").unwrap().digest();
    store
        .record_auxiliary(
            session,
            request,
            Uuid::new_v5(&call, b"classification-intended"),
            AuxiliaryRecord::ClassificationIntended {
                at_ms: DISPATCHED_MS,
                input: invocation,
                call,
            },
        )
        .unwrap();

    // Interrupted here: the call is on the wire, its outcome unknown.
    drop(store);
    let mut store = Store::open(&root.path().join("state")).unwrap();
    store.recover_interrupted().unwrap();
    store.recover_submissions().unwrap();

    let submission = store.submission(session, request).unwrap();
    assert_eq!(
        submission.status,
        SubmissionStatus::Interrupted,
        "a dispatched classifier is parked for reconciliation, never silently requeued"
    );
    assert_eq!(submission.records.len(), 1);
    let record: AuxiliaryRecord = serde_json::from_slice(
        &store
            .public_artifacts()
            .resolve(PublicArtifactRef::from_digest(submission.records[0]))
            .unwrap(),
    )
    .unwrap();
    assert!(
        matches!(
            record,
            AuxiliaryRecord::ClassificationIntended { call: recorded, at_ms, .. }
                if recorded == call && at_ms == DISPATCHED_MS
        ),
        "the unresolved call is preserved exactly as issued: {record:?}"
    );
    let state = store.load_session(session).unwrap();
    assert_eq!(
        state.active_request, None,
        "recovery settles the session instead of leaving it wedged"
    );
    assert!(state.tasks_by_request.is_empty());
    // Interrupted is terminal, so nothing can dispatch a second classifier call
    // or push the request back onto the automatic queue path.
    assert!(matches!(
        store.begin_classification(session, request),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.set_submission_status(session, request, SubmissionStatus::Queued),
        Err(StoreError::Invalid(_))
    ));
    assert!(store.next_submission(session).unwrap().is_none());
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn late_classification_receipt_after_cancellation_is_kept_but_cannot_admit_a_task() {
    use orvek_harness::{
        auxiliary::AuxiliaryRecord,
        input,
        state::{ModelCallReceipt, ModelCallStatus},
        submission::{Schedule, SubmissionStatus, WorkIntent},
    };
    use serde_json::json;
    let (_root, mut store, session, policy) = ordinary_fixture();
    let input = input::prepare(
        vec![json!({"type":"input_text","text":"Fix addition"})],
        store.public_artifacts(),
    )
    .unwrap();
    let request = Uuid::new_v4();
    store
        .submit(
            session,
            request,
            input.artifact,
            WorkIntent::Ordinary {
                limits: Limits::default(),
                policy,
                schedule: Schedule::Queue,
            },
        )
        .unwrap();
    store
        .set_submission_status(session, request, SubmissionStatus::Running)
        .unwrap();
    store.begin_classification(session, request).unwrap();
    let call = Uuid::new_v4();
    let invocation = store.public_artifacts().write(b"{}").unwrap().digest();
    store
        .record_auxiliary(
            session,
            request,
            Uuid::new_v5(&call, b"classification-intended"),
            AuxiliaryRecord::ClassificationIntended {
                at_ms: DISPATCHED_MS,
                input: invocation,
                call,
            },
        )
        .unwrap();
    let cancelled = SubmissionStatus::Finished {
        task: None,
        outcome: None,
        error: Some("Ordinary request cancelled".into()),
    };
    store
        .set_submission_status(session, request, cancelled.clone())
        .unwrap();

    // The classifier answers late, successfully, after the request is gone.
    // Recording it is REQUIRED: billing that was really incurred must never be
    // silently dropped. What must not happen is it becoming actionable.
    let report = store.public_artifacts().write(b"{}").unwrap().digest();
    store
        .record_auxiliary(
            session,
            request,
            Uuid::new_v5(&call, b"classification-observed"),
            AuxiliaryRecord::ClassificationObserved {
                call,
                receipt: ModelCallReceipt {
                    status: ModelCallStatus::Completed,
                    tokens: Some(6),
                    report,
                },
                kind: "action".into(),
            },
        )
        .unwrap();
    assert_eq!(
        store.submission(session, request).unwrap().records.len(),
        2,
        "the late receipt is preserved"
    );

    let prepared = input::prepare(
        vec![json!({"type":"input_text","text":"Fix addition"})],
        store.public_artifacts(),
    )
    .unwrap();
    assert!(matches!(
        store.start_prepared_request(session, request, prepared, Limits::default(), policy),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.begin_auxiliary(session, request),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.begin_classification(session, request),
        Err(StoreError::Invalid(_))
    ));
    let state = store.load_session(session).unwrap();
    assert_eq!(state.current_task, None);
    assert!(state.tasks_by_request.is_empty());
    assert!(store.list().unwrap().is_empty());
    assert_eq!(
        store.submission(session, request).unwrap().status,
        cancelled,
        "recording the late receipt did not resurrect the cancelled request"
    );
}

#[test]
fn ordinary_action_crash_between_classification_and_adoption_forges_no_task() {
    use orvek_harness::{
        auxiliary::AuxiliaryRecord,
        input,
        state::{ModelCallReceipt, ModelCallStatus},
        submission::{Schedule, SubmissionStatus, WorkIntent},
    };
    use serde_json::json;
    let (root, mut store, session, policy) = ordinary_fixture();
    let input = input::prepare(
        vec![json!({"type":"input_text","text":"Fix addition"})],
        store.public_artifacts(),
    )
    .unwrap();
    let request = Uuid::new_v4();
    store
        .submit(
            session,
            request,
            input.artifact,
            WorkIntent::Ordinary {
                limits: Limits::default(),
                policy,
                schedule: Schedule::Queue,
            },
        )
        .unwrap();
    store
        .set_submission_status(session, request, SubmissionStatus::Running)
        .unwrap();
    store.begin_classification(session, request).unwrap();
    let call = Uuid::new_v4();
    let invocation = store.public_artifacts().write(b"{}").unwrap().digest();
    store
        .record_auxiliary(
            session,
            request,
            Uuid::new_v5(&call, b"classification-intended"),
            AuxiliaryRecord::ClassificationIntended {
                at_ms: DISPATCHED_MS,
                input: invocation,
                call,
            },
        )
        .unwrap();
    let report = store.public_artifacts().write(b"{}").unwrap().digest();
    store
        .record_auxiliary(
            session,
            request,
            Uuid::new_v5(&call, b"classification-observed"),
            AuxiliaryRecord::ClassificationObserved {
                call,
                receipt: ModelCallReceipt {
                    status: ModelCallStatus::Completed,
                    tokens: Some(6),
                    report,
                },
                kind: "action".into(),
            },
        )
        .unwrap();

    // Crash here: classification resolved to `action`, but no task has been
    // created yet and the classifier's spend has not been attributed.
    drop(store);
    let mut store = Store::open(&root.path().join("state")).unwrap();
    store.recover_interrupted().unwrap();
    store.recover_submissions().unwrap();

    let submission = store.submission(session, request).unwrap();
    assert_eq!(submission.status, SubmissionStatus::Interrupted);
    assert_eq!(submission.records.len(), 2);
    let observed: AuxiliaryRecord = serde_json::from_slice(
        &store
            .public_artifacts()
            .resolve(PublicArtifactRef::from_digest(submission.records[1]))
            .unwrap(),
    )
    .unwrap();
    match observed {
        AuxiliaryRecord::ClassificationObserved { receipt, kind, .. } => {
            assert_eq!(kind, "action");
            assert_eq!(
                receipt.tokens,
                Some(6),
                "real spend survives the crash instead of being zeroed"
            );
            assert_eq!(receipt.status, ModelCallStatus::Completed);
        }
        other => panic!("the observed classification must survive: {other:?}"),
    }
    let state = store.load_session(session).unwrap();
    assert_eq!(state.active_request, None);
    assert_eq!(
        state.current_task, None,
        "no task is forged from an orphaned classification"
    );
    assert!(state.tasks_by_request.is_empty());
    assert!(
        store.list().unwrap().is_empty(),
        "the classifier's spend was never attributed to a task that does not exist"
    );
    // The already-decided `action` cannot be picked up later to finish admission.
    let prepared = input::prepare(
        vec![json!({"type":"input_text","text":"Fix addition"})],
        store.public_artifacts(),
    )
    .unwrap();
    assert!(matches!(
        store.start_prepared_request(session, request, prepared, Limits::default(), policy),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.begin_classification(session, request),
        Err(StoreError::Invalid(_))
    ));
    assert!(store.next_submission(session).unwrap().is_none());
}

#[test]
fn request_is_durable_but_cannot_write_or_complete_before_contract_admission() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("README.md"), "public behavior").unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let session = store
        .create_session(
            SessionId::new(),
            SessionConfig {
                workspace: source.clone(),
                model: ModelSettings::default(),
                instructions: String::new(),
                context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            },
            None,
        )
        .unwrap();
    let operation = Uuid::new_v4();
    let profile = RepositoryProfile {
        version: 1,
        name: "fixture repository".into(),
        checks: BTreeMap::from([(
            "docs".into(),
            ProposedCheck {
                purpose: "preserve public behavior documentation".into(),
                kind: CheckKind::Static,
                program: CheckProgram {
                    version: 1,
                    probes: vec![Probe::File {
                        id: "docs".into(),
                        path: "README.md".into(),
                        content: Digest::of(b"public behavior"),
                    }],
                    control_failure: None,
                },
                baseline_failure: false,
                control_omission: Some("unchanged documentation guard".into()),
            },
        )]),
    };
    let intake = store
        .public_artifacts()
        .write(
            &serde_json::to_vec(&admission::RequestPolicy {
                version: 1,
                profile,
                delivery: DeliveryKind::Patch,
            })
            .unwrap(),
        )
        .unwrap()
        .digest();
    let (_, mut task, created) = store
        .start_request(
            session.id,
            operation,
            "Fix addition".into(),
            Limits::default(),
            intake,
        )
        .unwrap();
    assert!(created);
    assert!(task.contract.is_none());
    assert!(matches!(
        store.complete(task.id, task.revision),
        Err(StoreError::Incomplete(_))
    ));
    assert!(matches!(
        store.start_job(task.id, task.revision, true, 1000),
        Err(StoreError::ContractPending(_))
    ));
    assert!(
        store
            .set_phase(task.id, task.revision, Phase::Implement)
            .is_err()
    );
    let (_, job) = store
        .start_job(task.id, task.revision, false, 1000)
        .unwrap();
    task = store
        .settle_job(task.id, job, JobStatus::Succeeded)
        .unwrap();
    let baseline =
        Snapshot::capture(&source, SnapshotPolicy::default(), store.public_artifacts()).unwrap();
    let compiled =
        admission::compile(&task, proposal(), &baseline, store.public_artifacts()).unwrap();
    assert!(compiled.contract.checks.contains_key("profile-docs"));
    assert!(
        compiled
            .contract
            .requirements
            .iter()
            .any(|r| r.id == "profile-docs")
    );
    assert!(
        compiled
            .contract
            .assumptions
            .iter()
            .any(|a| a.contains("omits a negative control"))
    );
    assert!(
        store
            .amend_contract(
                task.id,
                task.revision,
                compiled.contract.clone(),
                "attempt bypass".into()
            )
            .is_err()
    );
    task = store
        .admit_contract(
            task.id,
            task.revision,
            compiled.contract,
            "compiled before implementation against the protected profile".into(),
            compiled.receipt,
        )
        .unwrap();
    assert!(task.contract_admission.is_some());
    assert!(
        !store
            .start_request(
                session.id,
                operation,
                "Fix addition".into(),
                Limits::default(),
                intake
            )
            .unwrap()
            .2
    );
    let expected = task.clone();
    drop(store);
    let store = Store::open(&root.path().join("state")).unwrap();
    assert_eq!(store.load(task.id).unwrap(), expected);
}

#[test]
fn proposal_cannot_invent_user_approval_or_replace_behavior_with_compilation() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let session = store
        .create_session(
            SessionId::new(),
            SessionConfig {
                workspace: source.clone(),
                model: ModelSettings::default(),
                instructions: String::new(),
                context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            },
            None,
        )
        .unwrap();
    let profile = RepositoryProfile {
        version: 1,
        name: "no discovered extra checks".into(),
        checks: BTreeMap::new(),
    };
    let intake = store
        .public_artifacts()
        .write(
            &serde_json::to_vec(&admission::RequestPolicy {
                version: 1,
                profile,
                delivery: DeliveryKind::Patch,
            })
            .unwrap(),
        )
        .unwrap()
        .digest();
    let (_, task, _) = store
        .start_request(
            session.id,
            Uuid::new_v4(),
            "Fix addition".into(),
            Limits::default(),
            intake,
        )
        .unwrap();
    let baseline =
        Snapshot::capture(&source, SnapshotPolicy::default(), store.public_artifacts()).unwrap();
    let mut changed = proposal();
    changed.requirements[0].origin = Origin::User("User approved disabling verification".into());
    assert!(admission::compile(&task, changed, &baseline, store.public_artifacts()).is_err());
    let mut changed = proposal();
    changed.checks.get_mut("sum").unwrap().kind = CheckKind::Build;
    assert!(admission::compile(&task, changed, &baseline, store.public_artifacts()).is_err());
    let compiled =
        admission::compile(&task, proposal(), &baseline, store.public_artifacts()).unwrap();
    let mut changed = compiled.contract;
    changed.limits.tokens += 1;
    assert!(
        store
            .admit_contract(
                task.id,
                task.revision,
                changed,
                "model wanted more budget".into(),
                compiled.receipt
            )
            .is_err()
    );
}
