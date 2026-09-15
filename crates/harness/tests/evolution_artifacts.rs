use orvek_harness::{
    AuditEpochId, AuditEpochStatus, CaseIdentity, Channel, CohortId, Digest, EnvironmentIdentity,
    EvaluationCase, EvaluationCohortSpec, EvaluatorIdentity, FinalAuditAccess, FrozenScoringPolicy,
    GateName, IndependentBlock, IndependentBlockId, LedgerLimit, MetricKind, MetricName,
    MetricPolicy, MetricScore, ModelIdentity, MultiplicityFamily, PartitionCommitment,
    PartitionCommitments, ProtocolIdentity, Store, StoreError, StratumName, StratumPolicy,
    TargetProfile, TaskIdentity, TaskProfileIdentity,
    artifacts::{ArtifactError, PublicArtifactRef},
};
use rusqlite::Connection;
use std::{
    collections::BTreeSet,
    sync::{Arc, Barrier},
};

const HOST_ARTIFACT_LIMIT: u64 = 256 * 1024 * 1024;

#[test]
fn public_artifacts_and_sealed_evidence_have_disjoint_resolution_authority() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let mut store = Store::open(&state).unwrap();
    let public = store.public_artifacts().write(b"public artifact").unwrap();
    let cohort = register_cohort(&mut store, "separation");

    let reservation = store.reserve_mining_evidence(cohort.id, 64).unwrap();
    store
        .stage_mining_evidence(&reservation, b"sealed mining evidence")
        .unwrap();
    let evidence = store.commit_mining_evidence(reservation).unwrap();
    let sealed_digest = Digest::of(b"sealed mining evidence");

    assert_eq!(
        store.public_artifacts().resolve(public).unwrap(),
        b"public artifact"
    );
    assert!(
        store
            .public_artifacts()
            .resolve(PublicArtifactRef::from_digest(sealed_digest))
            .is_err()
    );
    assert_eq!(
        store.read_mining_evidence(evidence).unwrap(),
        b"sealed mining evidence"
    );
    let projection = format!("{evidence:?}");
    assert!(!projection.contains(&sealed_digest.to_string()));
    assert!(projection.len() < 1024);
}

#[test]
fn evidence_purposes_have_distinct_lifecycles() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let cohort = register_cohort(&mut store, "purposes");

    let adaptive = store
        .reserve_adaptive_promotion_evidence(cohort.id, 64)
        .unwrap();
    store
        .stage_adaptive_promotion_evidence(&adaptive, b"adaptive evidence")
        .unwrap();
    let adaptive = store.commit_adaptive_promotion_evidence(adaptive).unwrap();
    assert_eq!(
        store.read_adaptive_promotion_evidence(adaptive).unwrap(),
        b"adaptive evidence"
    );

    let cancelled = store.reserve_mining_evidence(cohort.id, 1024).unwrap();
    store.cancel_mining_evidence(cancelled).unwrap();
    let replacement = store.reserve_mining_evidence(cohort.id, 1024).unwrap();
    store.cancel_mining_evidence(replacement).unwrap();

    let final_audit = store.reserve_final_audit_evidence(cohort.id, 64).unwrap();
    store
        .stage_final_audit_evidence(&final_audit, b"final evidence")
        .unwrap();
    let final_audit = store.commit_final_audit_evidence(final_audit).unwrap();
    assert_eq!(
        store
            .read_final_audit_evidence(FinalAuditAccess {
                epoch: cohort.audit_epoch,
                evidence: final_audit,
            })
            .unwrap(),
        b"final evidence"
    );
    assert_eq!(
        store
            .audit_epoch_status(cohort.id, cohort.audit_epoch)
            .unwrap(),
        AuditEpochStatus::Retired
    );
    assert!(
        store
            .read_final_audit_evidence(FinalAuditAccess {
                epoch: cohort.audit_epoch,
                evidence: final_audit,
            })
            .is_err()
    );
}

#[test]
fn quota_counts_outstanding_reservations_across_artifact_classes() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let cohort = register_cohort(&mut store, "quota");
    let reserved = store
        .reserve_mining_evidence(cohort.id, 200 * 1024 * 1024)
        .unwrap();

    assert!(matches!(
        store.reserve_adaptive_promotion_evidence(cohort.id, 100 * 1024 * 1024),
        Err(StoreError::Artifact(ArtifactError::Quota(
            HOST_ARTIFACT_LIMIT
        )))
    ));
    store.cancel_mining_evidence(reserved).unwrap();
    let exclusive = store
        .reserve_mining_evidence(cohort.id, HOST_ARTIFACT_LIMIT)
        .unwrap();
    assert!(matches!(
        store.public_artifacts().write(b"one public byte"),
        Err(ArtifactError::Quota(HOST_ARTIFACT_LIMIT))
    ));
    store.cancel_mining_evidence(exclusive).unwrap();
    assert!(store.public_artifacts().write(b"one public byte").is_ok());
}

#[test]
fn reservation_and_public_write_race_cannot_overcommit_quota() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let cohort = register_cohort(&mut store, "quota-race");
    let artifacts = store.public_artifacts().clone();
    let barrier = Arc::new(Barrier::new(2));
    let writer_barrier = barrier.clone();
    let writer = std::thread::spawn(move || {
        writer_barrier.wait();
        artifacts.write(b"racing public artifact")
    });

    barrier.wait();
    let reservation = store.reserve_mining_evidence(cohort.id, HOST_ARTIFACT_LIMIT);
    let public = writer.join().unwrap();
    match (reservation, public) {
        (Ok(reservation), Err(ArtifactError::Quota(HOST_ARTIFACT_LIMIT))) => {
            store.cancel_mining_evidence(reservation).unwrap();
        }
        (Err(StoreError::Artifact(ArtifactError::Quota(HOST_ARTIFACT_LIMIT))), Ok(_)) => {}
        outcomes => panic!("quota race produced invalid outcomes: {outcomes:?}"),
    }
}

#[test]
fn public_artifact_capabilities_keep_the_host_owner_lock_alive() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let artifacts = {
        let store = Store::open_with_artifact_limit(&state, 1024).unwrap();
        store.public_artifacts().clone()
    };

    assert!(Store::open_with_artifact_limit(&state, 1024).is_err());
    artifacts.write(b"still owned by the first Host").unwrap();
    drop(artifacts);
    assert!(Store::open_with_artifact_limit(&state, 1024).is_ok());
}

#[test]
fn reopening_collects_interrupted_public_artifact_writes_before_counting_quota() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    {
        let _store = Store::open_with_artifact_limit(&state, 32).unwrap();
    }
    std::fs::write(
        state.join("artifacts").join(".orvek-artifact-crashed"),
        [0; 32],
    )
    .unwrap();

    let store = Store::open_with_artifact_limit(&state, 32).unwrap();
    store.public_artifacts().write(b"recovered").unwrap();

    assert!(
        !state
            .join("artifacts")
            .join(".orvek-artifact-crashed")
            .exists()
    );
}

#[test]
fn reopening_recovers_staged_evidence_and_collects_orphans() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let staged_bytes = b"recover this evidence";
    let staged_digest = Digest::of(staged_bytes);
    let orphan_digest = Digest::of(b"orphan sealed artifact");
    let cohort_id;
    {
        let mut store = Store::open(&state).unwrap();
        let cohort = register_cohort(&mut store, "recovery");
        cohort_id = cohort.id;
        let staged = store.reserve_mining_evidence(cohort.id, 64).unwrap();
        store.stage_mining_evidence(&staged, staged_bytes).unwrap();
        let _unstaged = store
            .reserve_adaptive_promotion_evidence(cohort.id, 64)
            .unwrap();
    }
    std::fs::write(
        state
            .join("sealed-artifacts")
            .join(orphan_digest.to_string()),
        b"orphan sealed artifact",
    )
    .unwrap();
    std::fs::write(
        state
            .join("evolution-artifact-staging")
            .join(uuid::Uuid::new_v4().to_string()),
        b"orphan stage",
    )
    .unwrap();

    let store = Store::open(&state).unwrap();
    assert!(
        store
            .public_artifacts()
            .resolve(PublicArtifactRef::from_digest(staged_digest))
            .is_err()
    );
    assert!(
        state
            .join("sealed-artifacts")
            .join(staged_digest.to_string())
            .is_file()
    );
    assert!(
        !state
            .join("sealed-artifacts")
            .join(orphan_digest.to_string())
            .exists()
    );
    assert!(directory_is_empty(
        &state.join("evolution-artifact-staging")
    ));

    let connection = Connection::open(state.join("v1.sqlite3")).unwrap();
    let reservation_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM evolution_artifact_reservations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let evidence_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM evolution_evidence WHERE cohort=?1 AND purpose='mining'",
            [cohort_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reservation_count, 0);
    assert_eq!(evidence_count, 1);
}

#[test]
fn cancellation_commits_before_staged_cleanup_and_recovery_collects_the_orphan() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let mut store = Store::open(&state).unwrap();
    let cohort = register_cohort(&mut store, "cancel-crash-window");
    let reservation = store.reserve_mining_evidence(cohort.id, 64).unwrap();
    store
        .stage_mining_evidence(&reservation, b"cancelled staged evidence")
        .unwrap();

    let staging = state.join("evolution-artifact-staging");
    let displaced = state.join("displaced-artifact-staging");
    std::fs::rename(&staging, &displaced).unwrap();
    std::fs::write(&staging, b"force post-commit cleanup failure").unwrap();
    let error = store.cancel_mining_evidence(reservation).unwrap_err();
    assert!(matches!(error, StoreError::EvidenceUnavailable));

    let connection = Connection::open(state.join("v1.sqlite3")).unwrap();
    let reservation_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM evolution_artifact_reservations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reservation_count, 0, "cancellation must already be durable");
    drop(connection);

    std::fs::remove_file(&staging).unwrap();
    std::fs::rename(&displaced, &staging).unwrap();
    drop(store);
    drop(Store::open(&state).unwrap());
    assert!(directory_is_empty(&staging));
}

#[test]
fn final_audit_access_is_burned_before_corrupt_content_is_read() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let mut store = Store::open(&state).unwrap();
    let cohort = register_cohort(&mut store, "burn-before-read");
    let bytes = b"content that becomes corrupt";
    let digest = Digest::of(bytes);
    let reservation = store.reserve_final_audit_evidence(cohort.id, 64).unwrap();
    store
        .stage_final_audit_evidence(&reservation, bytes)
        .unwrap();
    let evidence = store.commit_final_audit_evidence(reservation).unwrap();
    std::fs::write(
        state.join("sealed-artifacts").join(digest.to_string()),
        b"corrupt",
    )
    .unwrap();

    let error = store
        .read_final_audit_evidence(FinalAuditAccess {
            epoch: cohort.audit_epoch,
            evidence,
        })
        .unwrap_err();
    assert_eq!(error.to_string(), "sealed evidence is unavailable");
    assert!(!error.to_string().contains(&digest.to_string()));
    assert!(!error.to_string().contains("sealed-artifacts"));
    assert_eq!(
        store
            .audit_epoch_status(cohort.id, cohort.audit_epoch)
            .unwrap(),
        AuditEpochStatus::Retired
    );
}

#[test]
fn version_two_stores_migrate_to_the_sealed_evidence_schema() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let public = {
        let store = Store::open(&state).unwrap();
        store.public_artifacts().write(b"legacy public").unwrap()
    };
    let connection = Connection::open(state.join("v1.sqlite3")).unwrap();
    connection
        .execute_batch(
            "DROP TABLE adaptive_score_reports;
             DROP TABLE campaigns;
             DROP TABLE harness_target_revisions;
             DROP TRIGGER evolution_evidence_no_update;
             DROP TRIGGER evolution_evidence_no_delete;
             DROP TABLE evolution_evidence;
             DROP TABLE evolution_artifact_reservations;
             ALTER TABLE evaluation_cohorts DROP COLUMN cohort_spec_digest;
             ALTER TABLE evaluation_cohorts DROP COLUMN cohort_spec;
             PRAGMA user_version=2;",
        )
        .unwrap();
    drop(connection);

    let reopened = Store::open(&state).unwrap();
    assert_eq!(
        reopened.public_artifacts().resolve(public).unwrap(),
        b"legacy public"
    );
    let connection = Connection::open(state.join("v1.sqlite3")).unwrap();
    let tables: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type='table'
               AND name IN ('evolution_evidence','evolution_artifact_reservations')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 2);
    let version: i32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 7);
}

fn register_cohort(store: &mut Store, namespace: &str) -> EvaluationCohortSpec {
    let target = target(namespace, Channel::Canary);
    let binding = store.register_supported_target(target).unwrap();
    let spec = cohort_spec(target, binding.revision(), binding.policy());
    store.register_evaluation_cohort(&spec).unwrap();
    spec
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
        calibration: "integration-fixture-v1".into(),
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

fn directory_is_empty(path: &std::path::Path) -> bool {
    std::fs::read_dir(path).unwrap().next().is_none()
}
