use orvek_harness::{Digest, Store, StoreError, contract::*, state::*};
use std::collections::BTreeMap;
use tempfile::TempDir;
use uuid::Uuid;

struct Fixture {
    directory: TempDir,
    store: Store,
    task: TaskState,
}

#[test]
fn provider_attempts_cannot_disappear_across_restart_or_erase_spend() {
    let mut f = Fixture::new();
    f.candidate(b"fixed source");
    f.pass();
    f.deliver();
    let operation = Uuid::new_v4();
    f.task = f.store.reserve_model_call(f.task.id, operation).unwrap();
    assert!(matches!(
        f.store.complete(f.task.id, f.task.revision),
        Err(StoreError::Incomplete(_))
    ));
    let report = f
        .store
        .artifacts()
        .put(b"response interrupted after dispatch")
        .unwrap();
    f.task = f
        .store
        .record_model_call(
            f.task.id,
            operation,
            ModelCallReceipt {
                status: ModelCallStatus::Unknown,
                tokens: None,
                report,
            },
        )
        .unwrap();
    assert_eq!(f.task.usage.model_calls, 1);
    let path = f.directory.path().to_owned();
    drop(f.store);
    f.store = Store::open(&path).unwrap();
    f.refresh();
    assert!(matches!(
        f.store.complete(f.task.id, f.task.revision),
        Err(StoreError::Incomplete(_))
    ));
    let receipt = ModelCallReceipt {
        status: ModelCallStatus::Failed,
        tokens: Some(123),
        report: f
            .store
            .artifacts()
            .put(b"provider reconciled charge:123")
            .unwrap(),
    };
    f.task = f
        .store
        .record_model_call(f.task.id, operation, receipt.clone())
        .unwrap();
    assert_eq!(f.task.usage.tokens, 123);
    assert_eq!(
        f.store
            .record_model_call(f.task.id, operation, receipt.clone())
            .unwrap(),
        f.task
    );
    assert!(
        f.store
            .record_model_call(
                f.task.id,
                operation,
                ModelCallReceipt {
                    tokens: Some(0),
                    ..receipt
                }
            )
            .is_err()
    );
    assert_eq!(
        f.store
            .complete(f.task.id, f.task.revision)
            .unwrap()
            .outcome,
        Some(Outcome::Complete)
    );
}

#[test]
fn restart_revokes_leases_without_claiming_process_or_external_effect_termination() {
    let mut f = Fixture::new();
    let (_, job) = f
        .store
        .start_job(f.task.id, f.task.revision, true, 60_000)
        .unwrap();
    f.refresh();
    f.store
        .settle_job(f.task.id, job, JobStatus::Failed)
        .unwrap();
    f.refresh();
    f.candidate(b"interrupted candidate");
    let lease = f
        .store
        .begin_check(f.task.id, f.task.revision, "restart-test")
        .unwrap();
    f.refresh();
    let effect = Uuid::new_v4();
    f.task = f
        .store
        .record_effect(
            f.task.id,
            f.task.revision,
            Effect {
                operation_id: effect,
                description: "remote intent awaiting acknowledgement".into(),
                status: EffectStatus::Intended,
                idempotent: false,
            },
        )
        .unwrap();
    let generation = f.task.generation;
    let observation = f.observation(CheckStatus::Passed);
    let path = f.directory.path().to_owned();
    drop(f.store);
    f.store = Store::open(&path).unwrap();
    assert_eq!(f.store.recover_interrupted().unwrap(), vec![f.task.id]);
    f.refresh();
    assert_eq!(f.task.outcome, Some(Outcome::Blocked));
    assert_eq!(f.task.generation, generation + 1);
    assert_eq!(f.task.jobs[&lease.job_id()].status, JobStatus::Unknown);
    assert_eq!(f.task.effects[&effect].status, EffectStatus::Unknown);
    assert!(f.store.finish_check(lease, observation).is_err());
    assert!(f.store.recover_interrupted().unwrap().is_empty());
    assert_eq!(f.store.load(f.task.id).unwrap(), f.task);
    f.task = f
        .store
        .reopen(
            f.task.id,
            f.task.revision,
            "operator requested recovery".into(),
        )
        .unwrap();
    f.pass();
    f.deliver();
    assert!(matches!(
        f.store.complete(f.task.id, f.task.revision),
        Err(StoreError::Incomplete(_))
    ));
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(directory.path()).unwrap();
        let verifier = store
            .artifacts()
            .put(b"protected acceptance runner version 1")
            .unwrap();
        let contract = Contract {
            request: "Fix uploads that lose bytes after restart".into(),
            outcome: "Interrupted uploads resume with identical bytes".into(),
            scope: "upload service".into(),
            requirements: vec![Requirement {
                id: "restart".into(),
                behavior: "resume after service restart without changing content".into(),
                origin: Origin::User("Fix uploads that lose bytes after restart".into()),
                checks: vec!["restart-test".into()],
                depends_on: vec![],
            }],
            checks: BTreeMap::from([(
                "restart-test".into(),
                CheckDefinition {
                    purpose: "interrupt service and compare recovered bytes".into(),
                    kind: CheckKind::Behavior,
                    verifier,
                    command: vec!["protected-restart-test".into()],
                    timeout_ms: 60_000,
                    minimum_assertions: 1,
                    control: ControlRequirement::BaselineFailure,
                    control_source: None,
                    baseline: BaselinePolicy::MustPass,
                    flake: FlakePolicy::RejectAnyFailure,
                },
            )]),
            protected_behavior: vec!["ordinary uploads".into()],
            assumptions: vec![],
            open_questions: vec![],
            delivery: DeliveryKind::Source,
            limits: Limits::default(),
        };
        let task = store.create(contract).unwrap();
        let task = store
            .establish_baseline(
                task.id,
                task.revision,
                Candidate {
                    provenance: None,
                    source: verifier,
                    environment: verifier,
                    artifact: verifier,
                    frozen: true,
                },
            )
            .unwrap();
        Self {
            directory,
            store,
            task,
        }
    }

    fn refresh(&mut self) {
        self.task = self.store.load(self.task.id).unwrap();
    }

    fn candidate(&mut self, bytes: &[u8]) {
        let source = self.store.artifacts().put(bytes).unwrap();
        let environment = self
            .store
            .artifacts()
            .put(b"test image, toolchain and fixtures v1")
            .unwrap();
        let artifact = source;
        self.task = self
            .store
            .select_candidate(
                self.task.id,
                self.task.revision,
                Candidate {
                    provenance: None,
                    source,
                    environment,
                    artifact,
                    frozen: true,
                },
            )
            .unwrap();
    }

    fn observation(&self, status: CheckStatus) -> Observation {
        let report = self
            .store
            .artifacts()
            .put(b"protected runner observed recovered content bytes")
            .unwrap();
        let control = self
            .store
            .artifacts()
            .put(b"baseline lost bytes after restart, before patch")
            .unwrap();
        Observation {
            status,
            report,
            assertions: 1,
            discovered: Some(1),
            skipped: 0,
            exit_code: Some(if status == CheckStatus::Passed { 0 } else { 1 }),
            signal: None,
            control: Some(ControlObservation {
                kind: ControlRequirement::BaselineFailure,
                source: self.task.baseline.as_ref().unwrap().source,
                rejected: true,
                intended_reason: true,
                report: control,
            }),
            baseline_unchanged: false,
            limitations: vec![],
            unreconciled_jobs: vec![],
        }
    }

    fn observe(&mut self, observation: Observation) {
        let lease = self
            .store
            .begin_check(self.task.id, self.task.revision, "restart-test")
            .unwrap();
        self.task = self.store.finish_check(lease, observation).unwrap();
    }

    fn pass(&mut self) {
        self.observe(self.observation(CheckStatus::Passed));
    }

    fn deliver(&mut self) {
        let candidate = self.task.candidate.as_ref().unwrap();
        let receipt = self
            .store
            .artifacts()
            .put(b"verified patch delivered against the named base")
            .unwrap();
        self.task = self
            .store
            .record_delivery(
                self.task.id,
                self.task.revision,
                Delivery {
                    kind: DeliveryKind::Source,
                    source: candidate.source,
                    artifact: candidate.artifact,
                    receipt,
                },
            )
            .unwrap();
    }

    fn complete(&mut self) -> Result<TaskState, StoreError> {
        self.store.complete(self.task.id, self.task.revision)
    }
}

#[test]
fn h01_prose_and_terminal_outcome_cannot_replace_evidence() {
    let mut f = Fixture::new();
    assert!(matches!(f.complete(), Err(StoreError::Incomplete(_))));
    assert!(matches!(
        f.store
            .stop(f.task.id, f.task.revision, Outcome::Complete, "done".into()),
        Err(StoreError::Invalid(_))
    ));
    f.candidate(b"candidate");
    f.deliver();
    assert!(matches!(f.complete(), Err(StoreError::Incomplete(_))));
}

#[test]
fn h02_workspace_report_without_runner_lease_is_not_evidence() {
    let mut f = Fixture::new();
    f.candidate(b"candidate");
    f.store
        .artifacts()
        .put(br#"{"status":"passed","outcome":"complete"}"#)
        .unwrap();
    f.deliver();
    assert!(matches!(f.complete(), Err(StoreError::Incomplete(_))));
    assert!(f.store.load(f.task.id).unwrap().evidence.is_empty());
}

#[test]
fn complete_requires_current_evidence_control_and_delivery_then_replays_exactly() {
    let mut f = Fixture::new();
    f.candidate(b"patched source");
    f.pass();
    assert!(matches!(f.complete(), Err(StoreError::Incomplete(_))));
    f.deliver();
    let complete = f.complete().unwrap();
    assert_eq!(complete.outcome, Some(Outcome::Complete));
    assert_eq!(complete.certificates[0].evidence.len(), 1);
    assert!(matches!(
        f.store.complete(f.task.id, complete.revision),
        Err(StoreError::Terminal)
    ));
    let path = f.directory.path().to_owned();
    drop(f.store);
    let recovered = Store::open(&path).unwrap();
    assert_eq!(recovered.load(complete.id).unwrap(), complete);
}

#[test]
fn h03_empty_or_cyclic_contracts_and_unmapped_quality_checks_are_rejected() {
    let f = Fixture::new();
    let mut contract = f.task.accepted_contract().unwrap().clone();
    contract.requirements.clear();
    assert!(contract.validate().is_err());
    let mut contract = f.task.accepted_contract().unwrap().clone();
    contract.requirements[0].depends_on.push("restart".into());
    assert!(matches!(contract.validate(), Err(ContractError::Cycle)));
    let mut contract = f.task.accepted_contract().unwrap().clone();
    contract.checks.insert(
        "hidden-quality-gate".into(),
        contract.checks["restart-test"].clone(),
    );
    assert!(contract.validate().is_err());
}

#[test]
fn h03_amendment_requires_basis_and_invalidates_old_evidence() {
    let mut f = Fixture::new();
    f.candidate(b"candidate");
    f.pass();
    let mut amended = f.task.accepted_contract().unwrap().clone();
    amended.requirements[0]
        .behavior
        .push_str(" on a second supported backend");
    assert!(
        f.store
            .amend_contract(f.task.id, f.task.revision, amended.clone(), String::new())
            .is_err()
    );
    f.task = f
        .store
        .amend_contract(
            f.task.id,
            f.task.revision,
            amended,
            "User adds second backend".into(),
        )
        .unwrap();
    f.deliver();
    assert!(matches!(f.complete(), Err(StoreError::Incomplete(_))));
    assert_eq!(f.task.contract_history.len(), 1);
}

#[test]
fn h04_zero_tests_skips_cancelled_inconclusive_and_missing_controls_never_pass() {
    for case in 0..6 {
        let mut f = Fixture::new();
        f.candidate(b"candidate");
        let mut observation = f.observation(CheckStatus::Passed);
        match case {
            0 => observation.discovered = Some(0),
            1 => observation.skipped = 1,
            2 => observation.status = CheckStatus::Cancelled,
            3 => observation.status = CheckStatus::Inconclusive,
            4 => observation.control = None,
            5 => observation.control.as_mut().unwrap().intended_reason = false,
            _ => unreachable!(),
        }
        f.observe(observation);
        f.deliver();
        assert!(
            matches!(f.complete(), Err(StoreError::Incomplete(_))),
            "case {case}"
        );
    }
}

#[test]
fn h05_edit_after_pass_invalidates_evidence_even_if_delivery_is_updated() {
    let mut f = Fixture::new();
    f.candidate(b"source version one");
    f.pass();
    f.candidate(b"source version two");
    f.deliver();
    assert!(matches!(f.complete(), Err(StoreError::Incomplete(_))));
}

#[test]
fn h06_retry_until_green_and_reselecting_same_candidate_cannot_hide_failure() {
    let mut f = Fixture::new();
    f.candidate(b"candidate");
    f.observe(f.observation(CheckStatus::Failed));
    f.candidate(b"candidate");
    f.pass();
    f.deliver();
    assert!(matches!(f.complete(), Err(StoreError::Incomplete(_))));
    assert_eq!(f.task.evidence.len(), 2);
}

#[test]
fn h07_materialized_state_corruption_is_detected_against_journal() {
    let f = Fixture::new();
    let id = f.task.id;
    let path = f.directory.path().to_owned();
    drop(f.store);
    let database = rusqlite::Connection::open(path.join("v1.sqlite3")).unwrap();
    database
        .execute("UPDATE tasks SET state='{}'", [])
        .unwrap_err();
    database
        .execute("UPDATE tasks SET state=?1", [b"{}".as_slice()])
        .unwrap();
    drop(database);
    let recovered = Store::open(&path).unwrap();
    assert!(matches!(recovered.load(id), Err(StoreError::Integrity(_))));
}

#[test]
fn h08_unknown_effect_blocks_completion_until_reconciled_and_survives_reopen() {
    let mut f = Fixture::new();
    f.candidate(b"candidate");
    f.pass();
    let mut effect = Effect {
        operation_id: Uuid::new_v4(),
        description: "publish package".into(),
        status: EffectStatus::Intended,
        idempotent: false,
    };
    f.task = f
        .store
        .record_effect(f.task.id, f.task.revision, effect.clone())
        .unwrap();
    effect.status = EffectStatus::Unknown;
    f.task = f
        .store
        .record_effect(f.task.id, f.task.revision, effect.clone())
        .unwrap();
    f.deliver();
    assert!(matches!(f.complete(), Err(StoreError::Incomplete(_))));
    effect.status = EffectStatus::Reconciled;
    f.task = f
        .store
        .record_effect(f.task.id, f.task.revision, effect.clone())
        .unwrap();
    let complete = f.complete().unwrap();
    let reopened = f
        .store
        .reopen(
            f.task.id,
            complete.revision,
            "user requests another change".into(),
        )
        .unwrap();
    assert_eq!(
        reopened.effects[&effect.operation_id].status,
        EffectStatus::Reconciled
    );
    assert_eq!(reopened.certificates.len(), 1);
    assert_eq!(reopened.outcome, None);
}

#[test]
fn h09_late_result_cannot_certify_a_new_generation() {
    let mut f = Fixture::new();
    f.candidate(b"candidate one");
    let lease = f
        .store
        .begin_check(f.task.id, f.task.revision, "restart-test")
        .unwrap();
    let job = lease.job_id();
    f.refresh();
    f.candidate(b"candidate two");
    assert!(matches!(
        f.store
            .finish_check(lease, f.observation(CheckStatus::Passed)),
        Err(StoreError::Lease)
    ));
    assert_eq!(
        f.store.load(f.task.id).unwrap().jobs[&job].status,
        JobStatus::Running
    );
    let receipt = f
        .store
        .artifacts()
        .put(b"trusted test runner fenced the previous execution unit")
        .unwrap();
    f.task = f.store.fence_job(f.task.id, job, receipt).unwrap();
    f.pass();
    f.deliver();
    assert!(f.complete().is_ok());
}

#[test]
fn h11_concurrent_revision_and_live_writer_prevent_freeze() {
    let mut f = Fixture::new();
    let original_revision = f.task.revision;
    let (state, job) = f
        .store
        .start_job(f.task.id, f.task.revision, true, 60_000)
        .unwrap();
    f.task = state;
    let digest = f.store.artifacts().put(b"candidate").unwrap();
    let candidate = Candidate {
        provenance: None,
        source: digest,
        environment: digest,
        artifact: digest,
        frozen: true,
    };
    assert!(matches!(
        f.store
            .select_candidate(f.task.id, original_revision, candidate.clone()),
        Err(StoreError::Revision { .. })
    ));
    assert!(
        f.store
            .select_candidate(f.task.id, f.task.revision, candidate.clone())
            .is_err()
    );
    f.task = f
        .store
        .settle_job(f.task.id, job, JobStatus::Succeeded)
        .unwrap();
    assert!(
        f.store
            .select_candidate(f.task.id, f.task.revision, candidate)
            .is_ok()
    );
}

#[test]
fn h13_budget_reservations_are_atomic_and_survive_restart_and_reopen() {
    let mut f = Fixture::new();
    let mut contract = f.task.accepted_contract().unwrap().clone();
    contract.limits.model_calls = 1;
    f.task = f
        .store
        .amend_contract(
            f.task.id,
            f.task.revision,
            contract,
            "user sets one-call budget".into(),
        )
        .unwrap();
    f.task = f
        .store
        .reserve_model_call(f.task.id, Uuid::new_v4())
        .unwrap();
    assert!(matches!(
        f.store.reserve_model_call(f.task.id, Uuid::new_v4()),
        Err(StoreError::Budget)
    ));
    f.task = f
        .store
        .stop(
            f.task.id,
            f.task.revision,
            Outcome::BudgetExhausted,
            "call allowance spent".into(),
        )
        .unwrap();
    f.task = f
        .store
        .reopen(
            f.task.id,
            f.task.revision,
            "resume without resetting allowance".into(),
        )
        .unwrap();
    assert!(matches!(
        f.store.reserve_model_call(f.task.id, Uuid::new_v4()),
        Err(StoreError::Budget)
    ));
}

#[test]
fn h14_corrupted_evidence_cannot_keep_a_passing_result() {
    let mut f = Fixture::new();
    f.candidate(b"candidate");
    f.pass();
    f.deliver();
    let report = f.task.evidence[0].observation.report;
    std::fs::write(f.store.artifacts().path(report), b"tampered").unwrap();
    assert!(matches!(f.complete(), Err(StoreError::Artifact(_))));
    assert_eq!(f.store.load(f.task.id).unwrap().outcome, None);
}

#[test]
fn unresolved_blocking_finding_prevents_success() {
    let mut f = Fixture::new();
    f.candidate(b"candidate");
    f.pass();
    f.deliver();
    f.task = f
        .store
        .record_finding(
            f.task.id,
            f.task.revision,
            Finding {
                id: "authorization".into(),
                description: "cross-user resume remains possible".into(),
                blocking: true,
                resolved: false,
                resolution: None,
            },
        )
        .unwrap();
    assert!(matches!(f.complete(), Err(StoreError::Incomplete(_))));
}

#[test]
fn one_database_owner_and_digest_validation_are_enforced() {
    let f = Fixture::new();
    assert!(Store::open(f.directory.path()).is_err());
    assert!(serde_json::from_str::<Digest>(r#""../../outside""#).is_err());
    assert!(serde_json::from_str::<Digest>(r#""not-a-digest""#).is_err());
}

#[test]
fn required_evidence_limitations_cannot_be_hidden_behind_passed_status() {
    let mut f = Fixture::new();
    f.candidate(b"candidate");
    let mut observation = f.observation(CheckStatus::Passed);
    observation
        .limitations
        .push("required output truncated".into());
    f.observe(observation);
    f.deliver();
    assert!(matches!(f.complete(), Err(StoreError::Incomplete(_))));
}

#[test]
fn task_deadline_is_checked_even_when_all_evidence_was_already_collected() {
    let mut f = Fixture::new();
    f.candidate(b"candidate");
    f.pass();
    f.deliver();
    let deadline = f.task.started_ms + f.task.limits().elapsed_ms;
    assert!(orvek_harness::completion::evaluate(&f.task, deadline - 1).is_ok());
    assert!(orvek_harness::completion::evaluate(&f.task, deadline).is_err());
}

#[test]
fn artifact_limit_is_cumulative_across_blobs_and_abandoned_candidates() {
    let mut f = Fixture::new();
    let mut contract = f.task.accepted_contract().unwrap().clone();
    contract.limits.artifact_bytes = 1000;
    f.task = f
        .store
        .amend_contract(
            f.task.id,
            f.task.revision,
            contract,
            "user sets storage budget".into(),
        )
        .unwrap();
    let first = f.store.artifacts().put(&[1; 600]).unwrap();
    f.task = f
        .store
        .select_candidate(
            f.task.id,
            f.task.revision,
            Candidate {
                provenance: None,
                source: first,
                environment: first,
                artifact: first,
                frozen: true,
            },
        )
        .unwrap();
    let second = f.store.artifacts().put(&[2; 600]).unwrap();
    assert!(matches!(
        f.store.select_candidate(
            f.task.id,
            f.task.revision,
            Candidate {
                provenance: None,
                source: second,
                environment: second,
                artifact: second,
                frozen: true
            }
        ),
        Err(StoreError::Budget)
    ));
}

#[test]
fn late_accounting_preserves_history_but_revokes_current_completion_when_over_budget() {
    let mut f = Fixture::new();
    f.candidate(b"candidate");
    f.pass();
    f.deliver();
    let complete = f.complete().unwrap();
    let operation = Uuid::new_v4();
    let accounted = f
        .store
        .charge_usage(
            f.task.id,
            operation,
            Usage {
                model_calls: 0,
                tokens: f.task.limits().tokens + 1,
            },
        )
        .unwrap();
    assert_eq!(accounted.outcome, Some(Outcome::BudgetExhausted));
    assert_eq!(accounted.certificates, complete.certificates);
    assert_eq!(f.store.load(f.task.id).unwrap(), accounted);
}

#[test]
fn accounting_replay_after_lost_ack_is_idempotent_and_rejects_changed_payload() {
    let mut f = Fixture::new();
    let reservation = Uuid::new_v4();
    f.task = f.store.reserve_model_call(f.task.id, reservation).unwrap();
    assert_eq!(
        f.store.reserve_model_call(f.task.id, reservation).unwrap(),
        f.task
    );
    let receipt = Uuid::new_v4();
    f.task = f
        .store
        .charge_usage(
            f.task.id,
            receipt,
            Usage {
                model_calls: 0,
                tokens: 123,
            },
        )
        .unwrap();
    let path = f.directory.path().to_owned();
    drop(f.store);
    let mut recovered = Store::open(&path).unwrap();
    assert_eq!(
        recovered
            .charge_usage(
                f.task.id,
                receipt,
                Usage {
                    model_calls: 0,
                    tokens: 123
                }
            )
            .unwrap(),
        f.task
    );
    assert!(
        recovered
            .charge_usage(
                f.task.id,
                receipt,
                Usage {
                    model_calls: 0,
                    tokens: 124
                }
            )
            .is_err()
    );
    assert_eq!(
        recovered
            .reserve_model_call(f.task.id, reservation)
            .unwrap()
            .usage
            .model_calls,
        1
    );
}

#[test]
fn loss_of_evidence_after_certification_revokes_current_eligibility() {
    let mut f = Fixture::new();
    f.candidate(b"candidate");
    f.pass();
    f.deliver();
    let completed = f.complete().unwrap();
    let report = completed.evidence[0].observation.report;
    std::fs::remove_file(f.store.artifacts().path(report)).unwrap();
    let audited = f.store.audit_evidence(f.task.id).unwrap();
    assert_eq!(audited.outcome, Some(Outcome::Blocked));
    assert_eq!(audited.certificates, completed.certificates);
    assert_eq!(f.store.load(f.task.id).unwrap(), audited);
}

#[test]
fn admitted_cancellation_prevents_completion_even_after_all_checks_passed() {
    let mut f = Fixture::new();
    f.candidate(b"candidate");
    f.pass();
    f.deliver();
    f.task = f.store.request_cancellation(f.task.id).unwrap();
    assert!(matches!(f.complete(), Err(StoreError::Incomplete(_))));
    assert!(matches!(
        f.store.reserve_model_call(f.task.id, Uuid::new_v4()),
        Err(StoreError::Cancelled)
    ));
    assert_eq!(f.store.request_cancellation(f.task.id).unwrap(), f.task);
}

#[test]
fn bounded_completion_model_exhausts_independent_failure_dimensions() {
    let mut f = Fixture::new();
    f.candidate(b"candidate");
    f.pass();
    f.deliver();
    for mask in 0u32..1024 {
        let mut state = f.task.clone();
        let mut at = state.started_ms;
        if mask & 1 != 0 {
            state.candidate.as_mut().unwrap().frozen = false;
        }
        if mask & 2 != 0 {
            state.generation += 1;
        }
        if mask & 4 != 0 {
            state.evidence[0].observation.status = CheckStatus::Inconclusive;
        }
        if mask & 8 != 0 {
            state.evidence[0].observation.discovered = Some(0);
        }
        if mask & 16 != 0 {
            state.evidence[0].observation.control = None;
        }
        if mask & 32 != 0 {
            state.delivery = None;
        }
        if mask & 64 != 0 {
            state
                .contract
                .as_mut()
                .unwrap()
                .open_questions
                .push("unresolved required outcome".into());
        }
        if mask & 128 != 0 {
            at += state.limits().elapsed_ms;
        }
        if mask & 256 != 0 {
            state.evidence[0]
                .observation
                .limitations
                .push("required output unavailable".into());
        }
        if mask & 512 != 0 {
            state.jobs.values_mut().next().unwrap().status = JobStatus::Unknown;
        }
        assert_eq!(
            orvek_harness::completion::evaluate(&state, at).is_ok(),
            mask == 0,
            "counterexample mask {mask:010b}"
        );
    }
}
