use orvek_harness::{
    Store, StoreError,
    contract::*,
    runtime::DockerExecutor,
    state::*,
    verification::{self, CheckProgram, ControlFailure, Expectation, Probe},
    workspace::{Snapshot, SnapshotPolicy},
};
use std::{collections::BTreeMap, fs};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "requires local Docker and pre-pulled debian:bookworm-slim"]
async fn real_bug_requires_baseline_failure_and_candidate_success_before_delivery() {
    let root_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.tact/verification-test-workspaces");
    fs::create_dir_all(&root_path).unwrap();
    let root = tempfile::tempdir_in(root_path).unwrap();
    let baseline_path = root.path().join("baseline");
    let candidate_path = root.path().join("candidate");
    for path in [&baseline_path, &candidate_path] {
        fs::create_dir(path).unwrap();
    }
    fs::write(baseline_path.join("add"), "#!/bin/sh\nprintf '3\\n'\n").unwrap();
    fs::write(
        candidate_path.join("add"),
        "#!/bin/sh\nprintf '%s\\n' \"$(($1 + $2))\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&baseline_path, &candidate_path] {
            fs::set_permissions(path.join("add"), fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let mut store = Store::open(&root.path().join("protected-state")).unwrap();
    let baseline =
        Snapshot::capture(&baseline_path, SnapshotPolicy::default(), store.artifacts()).unwrap();
    let candidate = Snapshot::capture(
        &candidate_path,
        SnapshotPolicy::default(),
        store.artifacts(),
    )
    .unwrap();
    let source = candidate.publish(store.artifacts()).unwrap();
    let program = CheckProgram {
        version: 1,
        probes: vec![
            Probe::Command {
                id: "sum-four".into(),
                command: "./add 2 2".into(),
                exit_code: 0,
                stdout: Some(Expectation::Equals("4\n".into())),
                stderr: None,
            },
            Probe::Command {
                id: "sum-twelve".into(),
                command: "./add 5 7".into(),
                exit_code: 0,
                stdout: Some(Expectation::Equals("12\n".into())),
                stderr: None,
            },
        ],
        control_failure: Some(ControlFailure {
            probe: "sum-four".into(),
            stdout: Some(Expectation::Equals("3\n".into())),
            stderr: None,
        }),
    };
    let verifier = store
        .artifacts()
        .put(&serde_json::to_vec(&program).unwrap())
        .unwrap();
    let contract = Contract {
        request: "Fix addition returning a constant".into(),
        outcome: "Return the sum of both arguments".into(),
        scope: "add command".into(),
        requirements: vec![Requirement {
            id: "addition".into(),
            behavior: "add both arguments".into(),
            origin: Origin::User("Fix addition".into()),
            checks: vec!["addition-probes".into()],
            depends_on: vec![],
        }],
        checks: BTreeMap::from([(
            "addition-probes".into(),
            CheckDefinition {
                purpose: "observe actual process outputs".into(),
                kind: CheckKind::Behavior,
                verifier,
                command: vec!["tact-verify".into()],
                timeout_ms: 60_000,
                minimum_assertions: 4,
                control: ControlRequirement::BaselineFailure,
                control_source: None,
                baseline: BaselinePolicy::MustPass,
                flake: FlakePolicy::RejectAnyFailure,
            },
        )]),
        protected_behavior: vec![],
        assumptions: vec![],
        open_questions: vec![],
        delivery: DeliveryKind::Source,
        limits: Limits::default(),
    };
    let mut task = store.create(contract).unwrap();
    let executor = DockerExecutor::connect("debian:bookworm-slim")
        .await
        .unwrap();
    let environment = store
        .artifacts()
        .put(&serde_json::to_vec(&executor.environment()).unwrap())
        .unwrap();
    let baseline_source = baseline.publish(store.artifacts()).unwrap();
    task = store
        .establish_baseline(
            task.id,
            task.revision,
            Candidate {
                provenance: None,
                source: baseline_source,
                environment,
                artifact: baseline_source,
                frozen: true,
            },
        )
        .unwrap();
    task = store
        .select_candidate(
            task.id,
            task.revision,
            Candidate {
                provenance: None,
                source,
                environment,
                artifact: source,
                frozen: true,
            },
        )
        .unwrap();
    assert!(matches!(
        store.complete(task.id, task.revision),
        Err(StoreError::Incomplete(_))
    ));
    let ticket =
        verification::prepare(&mut store, task.id, task.revision, "addition-probes").unwrap();
    let report = verification::execute(
        &ticket,
        &candidate_path,
        Some((&baseline, &baseline_path)),
        &executor,
        CancellationToken::new(),
    )
    .await;
    assert!(report.probes.iter().all(|probe| probe.passed), "{report:?}");
    assert!(report.control_matched, "{report:?}");
    task = verification::finish(&mut store, ticket, report).unwrap();
    assert_eq!(task.evidence[0].observation.assertions, 4);
    assert!(matches!(
        store.complete(task.id, task.revision),
        Err(StoreError::Incomplete(_))
    ));
    let delivered_path = root.path().join("delivered-source");
    candidate
        .materialize(&delivered_path, store.artifacts(), false)
        .unwrap();
    assert!(candidate.matches(&delivered_path).unwrap());
    let receipt = store
        .artifacts()
        .put(b"delivery source identity checked after materialization")
        .unwrap();
    task = store
        .record_delivery(
            task.id,
            task.revision,
            Delivery {
                kind: DeliveryKind::Source,
                source,
                artifact: source,
                receipt,
            },
        )
        .unwrap();
    task = store.complete(task.id, task.revision).unwrap();
    assert_eq!(task.outcome, Some(Outcome::Complete));
    assert_eq!(task.certificates[0].source, source);
}

#[test]
fn verification_program_rejects_empty_checks_unbounded_probes_and_path_escape() {
    assert!(
        CheckProgram {
            version: 1,
            probes: vec![],
            control_failure: None
        }
        .validate()
        .is_err()
    );
    let program = CheckProgram {
        version: 1,
        probes: vec![Probe::Command {
            id: "fake".into(),
            command: "true".into(),
            exit_code: 0,
            stdout: None,
            stderr: None,
        }],
        control_failure: None,
    };
    assert!(program.validate().is_err());
    let program = CheckProgram {
        version: 1,
        probes: vec![Probe::File {
            id: "escape".into(),
            path: "../controller".into(),
            content: orvek_harness::Digest::of(b"x"),
        }],
        control_failure: None,
    };
    assert!(program.validate().is_err());
}
