use orvek_harness::{
    AuditEpochId, AuditEpochStatus, CaseIdentity, Channel, CohortId, CohortLedger, Digest,
    EnvironmentIdentity, EvaluationCase, EvaluationCohortSpec, EvaluatorIdentity,
    FrozenScoringPolicy, GateName, IndependentBlock, IndependentBlockId, LedgerLimit, MetricKind,
    MetricName, MetricPolicy, MetricScore, ModelIdentity, MultiplicityFamily, PartitionCommitment,
    PartitionCommitments, ProtocolIdentity, Store, StoreError, StratumName, StratumPolicy,
    TargetProfile, TaskIdentity, TaskProfileIdentity,
    artifacts::PublicArtifactRef,
    context,
    contract::{
        BaselinePolicy, CheckDefinition, CheckKind, Contract, ControlRequirement, DeliveryKind,
        FlakePolicy, Limits, Origin, Requirement,
    },
    inference::ModelSettings,
    session::{SessionCommand, SessionConfig, SessionId},
    state::RequestKind,
};
use rusqlite::{Connection, params};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use uuid::Uuid;

const V1_FIXTURE: &str = include_str!("fixtures/store_v1.sql");

type RawEvent = (i64, String, String, i64, Vec<u8>, String);

#[test]
fn version_one_store_migrates_without_changing_aggregates_or_journal_bytes() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let mut store = Store::open(&state).unwrap();
    let verifier = store
        .public_artifacts()
        .write(b"migration verifier")
        .unwrap()
        .digest();
    let task = store.create(test_contract(verifier)).unwrap();
    let artifact = store
        .public_artifacts()
        .write(b"migration artifact")
        .unwrap()
        .digest();
    let mut session = store
        .create_session(SessionId::new(), session_config(root.path()), None)
        .unwrap();
    let request = Uuid::new_v4();
    session = store
        .session_command(
            session.id,
            session.revision,
            request,
            SessionCommand::Input {
                kind: RequestKind::Conversation,
                content: vec![json!({"role":"user","content":"preserve this projection"})],
            },
        )
        .unwrap();
    let projection = projected_input(&session);
    drop(store);

    let database = state.join("v1.sqlite3");
    let connection = Connection::open(&database).unwrap();
    connection.execute_batch(V1_FIXTURE).unwrap();
    assert_eq!(schema_version(&connection), 1);
    let original_events = raw_events(&connection);
    drop(connection);

    let migrated = Store::open(&state).unwrap();
    assert_eq!(migrated.load(task.id).unwrap(), task);
    let recovered_session = migrated.load_session(session.id).unwrap();
    assert_eq!(recovered_session, session);
    assert_eq!(projected_input(&recovered_session), projection);
    assert_eq!(
        migrated
            .public_artifacts()
            .resolve(PublicArtifactRef::from_digest(artifact))
            .unwrap(),
        b"migration artifact"
    );
    drop(migrated);

    let connection = Connection::open(database).unwrap();
    assert_eq!(schema_version(&connection), 6);
    assert_eq!(raw_events(&connection), original_events);
}

#[test]
fn version_three_store_migrates_without_changing_session_or_journal_bytes() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let mut store = Store::open(&state).unwrap();
    let session = store
        .create_session(SessionId::new(), session_config(root.path()), None)
        .unwrap();
    drop(store);

    let database = state.join("v1.sqlite3");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "DROP TABLE adaptive_score_reports;
             DROP TABLE campaigns;
             DROP TABLE harness_target_revisions;
             ALTER TABLE evaluation_cohorts DROP COLUMN cohort_spec_digest;
             ALTER TABLE evaluation_cohorts DROP COLUMN cohort_spec;
             PRAGMA user_version=3;",
        )
        .unwrap();
    let original_events = raw_events(&connection);
    let original_session: (i64, Vec<u8>, String) = connection
        .query_row(
            "SELECT revision,state,head FROM sessions WHERE id=?1",
            [session.id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    drop(connection);

    drop(Store::open(&state).unwrap());

    let connection = Connection::open(database).unwrap();
    assert_eq!(schema_version(&connection), 6);
    assert_eq!(raw_events(&connection), original_events);
    let migrated_session: (i64, Vec<u8>, String) = connection
        .query_row(
            "SELECT revision,state,head FROM sessions WHERE id=?1",
            [session.id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(migrated_session, original_session);
}

#[test]
fn corrupt_version_one_event_rolls_the_migration_back_atomically() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    drop(Store::open(&state).unwrap());
    let database = state.join("v1.sqlite3");
    let connection = Connection::open(&database).unwrap();
    connection.execute_batch(V1_FIXTURE).unwrap();
    connection
        .execute_batch("PRAGMA ignore_check_constraints=ON;")
        .unwrap();
    connection
        .execute(
            "INSERT INTO events(aggregate,kind,revision,event,hash) VALUES (?1,'unknown',1,X'7B7D',?2)",
            params![Uuid::new_v4().to_string(), "0".repeat(64)],
        )
        .unwrap();
    connection
        .execute_batch("PRAGMA ignore_check_constraints=OFF;")
        .unwrap();
    drop(connection);

    assert!(matches!(Store::open(&state), Err(StoreError::Sql(_))));

    let connection = Connection::open(database).unwrap();
    assert_eq!(schema_version(&connection), 1);
    let events_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name='events'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(events_sql.contains("'task','session'"));
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE kind='unknown'",
                [],
                |row| { row.get::<_, i64>(0) }
            )
            .unwrap(),
        1
    );
}

#[test]
fn unsupported_schema_versions_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let connection = Connection::open(state.join("v1.sqlite3")).unwrap();
    connection.pragma_update(None, "user_version", 99).unwrap();
    drop(connection);

    assert!(matches!(Store::open(&state), Err(StoreError::Schema(99))));
}

#[test]
fn version_two_event_constraint_accepts_campaigns_and_rejects_unknown_kinds() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    drop(Store::open(&state).unwrap());
    let connection = Connection::open(state.join("v1.sqlite3")).unwrap();
    let aggregate = Uuid::new_v4().to_string();
    connection
        .execute(
            "INSERT INTO events(aggregate,kind,revision,event,hash) VALUES (?1,'campaign',1,X'7B7D',?2)",
            params![aggregate, "1".repeat(64)],
        )
        .unwrap();
    assert!(
        connection
            .execute(
                "INSERT INTO events(aggregate,kind,revision,event,hash) VALUES (?1,'unknown',1,X'7B7D',?2)",
                params![Uuid::new_v4().to_string(), "2".repeat(64)],
            )
            .is_err()
    );
}

#[test]
fn compiled_baseline_is_registered_for_exact_targets_and_is_immutable() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let first = target("first", Channel::Stable);
    let second = target("second", Channel::Canary);
    let mut store = Store::open(&state).unwrap();
    let first_binding = store.register_supported_target(first).unwrap();
    let repeated = store.register_supported_target(first).unwrap();
    let second_binding = store.register_supported_target(second).unwrap();
    assert_eq!(repeated, first_binding);
    assert_eq!(second_binding.revision(), first_binding.revision());
    assert_eq!(store.resolve_harness(first).unwrap(), Some(first_binding));
    assert_eq!(store.resolve_harness(second).unwrap(), Some(second_binding));
    assert_eq!(
        store
            .resolve_harness(target("missing", Channel::Stable))
            .unwrap(),
        None
    );
    drop(store);

    let connection = Connection::open(state.join("v1.sqlite3")).unwrap();
    assert!(
        connection
            .execute(
                "UPDATE harness_revisions SET manifest=X'00' WHERE digest=?1",
                [first_binding.revision().to_string()],
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "DELETE FROM harness_revisions WHERE digest=?1",
                [first_binding.revision().to_string()],
            )
            .is_err()
    );
    drop(connection);

    let reopened = Store::open(&state).unwrap();
    assert_eq!(
        reopened.resolve_harness(first).unwrap(),
        Some(first_binding)
    );
}

#[test]
fn cohort_ledgers_and_audit_epochs_are_durable_and_cannot_be_reset() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let target = target("ledger", Channel::Canary);
    let mut store = Store::open(&state).unwrap();
    let binding = store.register_supported_target(target).unwrap();
    let spec = cohort_spec(target, binding.revision(), binding.policy());
    store.register_evaluation_cohort(&spec).unwrap();
    assert!(store.register_evaluation_cohort(&spec).is_err());
    assert_eq!(
        store.audit_epoch_status(spec.id, spec.audit_epoch).unwrap(),
        AuditEpochStatus::Active
    );

    let initial_status = store
        .cohort_ledger_status(spec.id, CohortLedger::AdaptivePromotion)
        .unwrap();
    assert_eq!(
        (initial_status.query_used, initial_status.error_used_nanos),
        (0, 0)
    );
    store.retire_audit_epoch(spec.id, spec.audit_epoch).unwrap();
    assert_eq!(
        store.audit_epoch_status(spec.id, spec.audit_epoch).unwrap(),
        AuditEpochStatus::Retired
    );
    assert!(store.retire_audit_epoch(spec.id, spec.audit_epoch).is_err());
    drop(store);

    let mut reopened = Store::open(&state).unwrap();
    assert_eq!(
        reopened
            .cohort_ledger_status(spec.id, CohortLedger::AdaptivePromotion)
            .unwrap(),
        initial_status
    );
    assert_eq!(
        reopened
            .audit_epoch_status(spec.id, spec.audit_epoch)
            .unwrap(),
        AuditEpochStatus::Retired
    );
    let mut replacement = spec.clone();
    replacement.audit_epoch = AuditEpochId::new();
    assert!(reopened.register_evaluation_cohort(&replacement).is_err());
}

fn test_contract(verifier: Digest) -> Contract {
    Contract {
        request: "preserve task state".into(),
        outcome: "the task survives migration".into(),
        scope: "store migration".into(),
        requirements: vec![Requirement {
            id: "preserve".into(),
            behavior: "the aggregate remains byte-equivalent".into(),
            origin: Origin::Repository("version-one store contract".into()),
            checks: vec!["migration".into()],
            depends_on: Vec::new(),
        }],
        checks: BTreeMap::from([(
            "migration".into(),
            CheckDefinition {
                purpose: "load the same state after migration".into(),
                kind: CheckKind::Migration,
                verifier,
                command: vec!["cargo".into(), "test".into()],
                timeout_ms: 1_000,
                minimum_assertions: 1,
                control: ControlRequirement::None,
                control_source: None,
                baseline: BaselinePolicy::MustPass,
                flake: FlakePolicy::RejectAnyFailure,
            },
        )]),
        protected_behavior: Vec::new(),
        assumptions: Vec::new(),
        open_questions: Vec::new(),
        delivery: DeliveryKind::Patch,
        limits: Limits::default(),
    }
}

fn session_config(workspace: &Path) -> SessionConfig {
    SessionConfig {
        workspace: workspace.to_owned(),
        model: ModelSettings::default(),
        instructions: "migration instructions".into(),
        context_window_tokens: context::DEFAULT_WINDOW_TOKENS,
    }
}

fn projected_input(session: &orvek_harness::session::SessionState) -> Vec<serde_json::Value> {
    context::project(
        session,
        context::projection_byte_limit(session.config.context_window_tokens).unwrap(),
    )
    .unwrap()
    .input
}

fn schema_version(connection: &Connection) -> i32 {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn raw_events(connection: &Connection) -> Vec<RawEvent> {
    connection
        .prepare("SELECT sequence,aggregate,kind,revision,event,hash FROM events ORDER BY sequence")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn target(namespace: &str, channel: Channel) -> TargetProfile {
    TargetProfile::new(
        ModelIdentity::from_digest(Digest::of(format!("{namespace}:model").as_bytes())),
        ProtocolIdentity::from_digest(Digest::of(format!("{namespace}:protocol").as_bytes())),
        EnvironmentIdentity::from_digest(Digest::of(format!("{namespace}:environment").as_bytes())),
        TaskProfileIdentity::from_digest(Digest::of(format!("{namespace}:task").as_bytes())),
        channel,
    )
}

fn cohort_spec(
    target: TargetProfile,
    base_revision: Digest,
    policy: orvek_harness::PolicyIdentity,
) -> EvaluationCohortSpec {
    let case = CaseIdentity::from_digest(Digest::of(b"case one"));
    EvaluationCohortSpec {
        id: CohortId::new(),
        target,
        base_revision,
        evaluator: EvaluatorIdentity::from_digest(Digest::of(b"cohort evaluator")),
        policy,
        partitions: PartitionCommitments {
            mining: PartitionCommitment::from_digest(Digest::of(b"mining partition")),
            adaptive_promotion: PartitionCommitment::from_digest(Digest::of(b"adaptive partition")),
            final_audit: PartitionCommitment::from_digest(Digest::of(b"final partition")),
        },
        blocks: vec![IndependentBlock {
            id: IndependentBlockId::from_digest(Digest::of(b"block one")),
            cases: vec![EvaluationCase {
                id: case,
                task: TaskIdentity::from_digest(Digest::of(b"task one")),
                repeats: 2,
            }],
        }],
        adaptive_promotion: LedgerLimit::new(10, 100),
        final_audit: LedgerLimit::new(10, 100),
        scoring: fixture_scoring(case),
        audit_epoch: AuditEpochId::new(),
    }
}

fn fixture_scoring(case: CaseIdentity) -> FrozenScoringPolicy {
    FrozenScoringPolicy {
        schema_version: 1,
        estimator_version: "paired-block-sign-v1".into(),
        calibration: "migration-fixture-v1".into(),
        critical_cases: BTreeSet::new(),
        strata: vec![StratumPolicy {
            name: StratumName::new("all").unwrap(),
            cases: vec![case],
        }],
        metrics: vec![MetricPolicy {
            name: MetricName::new("quality").unwrap(),
            kind: MetricKind::Primary,
            margin: MetricScore::from_millionths(0).unwrap(),
        }],
        minimum_complete_blocks: 1,
        maximum_exact_blocks: 20,
        required_hard_gates: vec![GateName::new("policy").unwrap()],
        multiplicity_family: MultiplicityFamily {
            candidates: 10,
            rounds: 1,
            metrics: 1,
            strata: 1,
            composites: 1,
            fallbacks: 1,
            campaigns: 1,
            activation_attempts: 1,
        },
        adaptive_error_nanos_per_hypothesis: 10,
        final_error_nanos_per_hypothesis: 10,
    }
}
