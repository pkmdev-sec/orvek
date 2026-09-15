use orvek_harness::{
    BehavioralFailureReason, BehavioralTrialOutcome, CampaignId, CandidateId, CaseIdentity,
    Channel, Digest, EffectId, EffectKind, EffectWorkId, EnvironmentIdentity, EvaluatorIdentity,
    IndependentBlockId, InfrastructureUnknownReason, InputCommitment, IsolatedRunReceipt,
    LeaseEpoch, MetricName, MetricScore, ModelIdentity, PairedTrialEvidence, PartitionCommitment,
    ProtocolIdentity, TargetProfile, TaskProfileIdentity, TerminalTrialOutcome, TrialContext,
    TrialError, TrialLedgerRole, TrialPairSpec, TrialPartition, TrialRunAssignment,
    TrialRuntimeIdentity, TrialSide, TrialTransportCapability, TrialTransportTerminal,
    classify_paired_trial,
    contract::Limits,
    controller::{
        TrialDispatch, TrialDispatchError, native_trial_transport_capability,
        prepare_trial_dispatch,
    },
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use uuid::Uuid;

fn digest(label: &str) -> Digest {
    Digest::of(label.as_bytes())
}

fn model(label: &str) -> ModelIdentity {
    ModelIdentity::from_digest(digest(label))
}

fn protocol(label: &str) -> ProtocolIdentity {
    ProtocolIdentity::from_digest(digest(label))
}

fn evaluator(label: &str) -> EvaluatorIdentity {
    EvaluatorIdentity::from_digest(digest(label))
}

fn environment(label: &str) -> EnvironmentIdentity {
    EnvironmentIdentity::from_digest(digest(label))
}

fn runtime(
    model_label: &str,
    protocol_label: &str,
    evaluator_label: &str,
    environment_label: &str,
) -> TrialRuntimeIdentity {
    TrialRuntimeIdentity::new(
        model(model_label),
        protocol(protocol_label),
        evaluator(evaluator_label),
        environment(environment_label),
    )
}

fn baseline_runtime() -> TrialRuntimeIdentity {
    runtime(
        "trial model",
        "trial protocol",
        "trial evaluator",
        "trial environment",
    )
}

fn target(runtime: TrialRuntimeIdentity) -> TargetProfile {
    TargetProfile::new(
        runtime.model(),
        runtime.protocol(),
        runtime.environment(),
        TaskProfileIdentity::from_digest(digest("trial task profile")),
        Channel::Canary,
    )
}

fn pair_with_input(runtime: TrialRuntimeIdentity, input_label: &str) -> TrialPairSpec {
    let partition = TrialPartition::new(
        TrialLedgerRole::AdaptivePromotion,
        7,
        PartitionCommitment::from_digest(digest("adaptive partition")),
    )
    .unwrap();
    let context = TrialContext::new(
        CampaignId::from_uuid(Uuid::from_u128(1)),
        CandidateId::from_digest(digest("candidate")),
        partition,
        runtime,
        Limits::default(),
    )
    .unwrap();

    TrialPairSpec::new(
        context,
        IndependentBlockId::from_digest(digest("independent block")),
        CaseIdentity::from_digest(digest("evaluation case")),
        2,
        InputCommitment::from_digest(digest(input_label)),
    )
    .unwrap()
}

fn pair(runtime: TrialRuntimeIdentity) -> TrialPairSpec {
    pair_with_input(runtime, "committed input")
}

fn dispatch(pair: TrialPairSpec) -> TrialDispatch {
    prepare_trial_dispatch(
        target(pair.context().runtime()),
        pair.context().runtime().evaluator(),
        pair,
    )
    .unwrap()
}

fn run(dispatch: &TrialDispatch, side: TrialSide) -> TrialRunAssignment {
    *dispatch
        .runs()
        .iter()
        .find(|run| run.key().side() == side)
        .unwrap()
}

fn effect(pair: TrialPairSpec) -> EffectId {
    EffectId::derive(
        pair.context().campaign(),
        17,
        EffectKind::Trial,
        EffectWorkId::from_digest(digest("paired trial work")),
    )
}

fn passing_outcome() -> TerminalTrialOutcome {
    let metrics = BTreeMap::from([(
        MetricName::new("quality").unwrap(),
        MetricScore::from_millionths(900_000).unwrap(),
    )]);
    TerminalTrialOutcome::pass(metrics)
}

fn collected_receipts(
    pair: TrialPairSpec,
    parent_outcome: TerminalTrialOutcome,
    candidate_outcome: TerminalTrialOutcome,
) -> (IsolatedRunReceipt, IsolatedRunReceipt) {
    let dispatch = dispatch(pair);
    let effect = effect(pair);
    let lease_epoch = LeaseEpoch::new(3).unwrap();
    let parent = IsolatedRunReceipt::collected(
        &run(&dispatch, TrialSide::Parent),
        effect,
        lease_epoch,
        parent_outcome,
        digest("parent runtime receipt"),
    )
    .unwrap();
    let candidate = IsolatedRunReceipt::collected(
        &run(&dispatch, TrialSide::Candidate),
        effect,
        lease_epoch,
        candidate_outcome,
        digest("candidate runtime receipt"),
    )
    .unwrap();
    (parent, candidate)
}

fn replace(value: &mut Value, path: &[&str], replacement: Value) {
    let (field, parents) = path.split_last().unwrap();
    let mut current = value;
    for parent in parents {
        current = current.get_mut(*parent).unwrap();
    }
    current
        .as_object_mut()
        .unwrap()
        .insert((*field).to_owned(), replacement);
}

#[test]
fn pair_identity_binds_every_frozen_field() {
    let runtime = baseline_runtime();
    let pair = pair(runtime);
    let target = target(runtime);
    let evaluator = runtime.evaluator();
    let mutations = [
        (vec!["id"], json!(digest("different pair id"))),
        (
            vec!["context", "campaign"],
            json!(CampaignId::from_uuid(Uuid::from_u128(2))),
        ),
        (
            vec!["context", "candidate"],
            json!(CandidateId::from_digest(digest("different candidate"))),
        ),
        (
            vec!["context", "partition", "role"],
            json!(TrialLedgerRole::FinalAudit),
        ),
        (vec!["context", "partition", "epoch"], json!(8)),
        (
            vec!["context", "partition", "commitment"],
            json!(PartitionCommitment::from_digest(digest(
                "different partition"
            ))),
        ),
        (
            vec!["context", "runtime", "model"],
            json!(model("different model")),
        ),
        (
            vec!["context", "runtime", "protocol"],
            json!(protocol("different protocol")),
        ),
        (
            vec!["context", "runtime", "evaluator"],
            json!(crate::evaluator("different evaluator")),
        ),
        (
            vec!["context", "runtime", "environment"],
            json!(environment("different environment")),
        ),
        (vec!["context", "limits", "tokens"], json!(999_999)),
        (
            vec!["block"],
            json!(IndependentBlockId::from_digest(digest("different block"))),
        ),
        (
            vec!["case"],
            json!(CaseIdentity::from_digest(digest("different case"))),
        ),
        (vec!["repeat"], json!(3)),
        (
            vec!["input"],
            json!(InputCommitment::from_digest(digest("different input"))),
        ),
    ];

    for (path, replacement) in mutations {
        let mut altered = serde_json::to_value(pair).unwrap();
        replace(&mut altered, &path, replacement);
        let altered: TrialPairSpec = serde_json::from_value(altered).unwrap();

        assert_eq!(
            prepare_trial_dispatch(target, evaluator, altered).unwrap_err(),
            TrialDispatchError::InvalidPair,
            "mutation at {path:?} was not rejected"
        );
    }
}

#[test]
fn dispatch_requires_registered_runtime_and_creates_strict_fresh_runs() {
    let runtime = baseline_runtime();
    let pair = pair(runtime);
    let target = target(runtime);
    let dispatch = prepare_trial_dispatch(target, runtime.evaluator(), pair).unwrap();

    assert_eq!(dispatch.target(), target);
    assert_eq!(dispatch.evaluator(), runtime.evaluator());
    assert_eq!(dispatch.pair(), pair);
    assert_eq!(
        native_trial_transport_capability(),
        TrialTransportCapability::FenceUnknownAfterStart
    );

    let parent = run(&dispatch, TrialSide::Parent);
    let candidate = run(&dispatch, TrialSide::Candidate);
    assert_ne!(parent.isolation_instance(), candidate.isolation_instance());
    for run in dispatch.runs() {
        let isolation = run.isolation();
        assert!(isolation.fresh_exclusive_workspace());
        assert!(!isolation.network_enabled());
        assert!(isolation.private_ipc());
        assert!(!isolation.host_control_available());
        assert!(!isolation.store_available());
        assert!(!isolation.credential_access());
        assert!(!isolation.sealed_artifact_access());
        assert!(!isolation.shared_writable_mounts());
        assert_eq!(
            run.transport(),
            TrialTransportCapability::FenceUnknownAfterStart
        );
    }

    let wrong_model = TargetProfile::new(
        model("wrong model"),
        target.protocol,
        target.environment,
        target.task_profile,
        target.channel,
    );
    assert_eq!(
        prepare_trial_dispatch(wrong_model, runtime.evaluator(), pair).unwrap_err(),
        TrialDispatchError::ModelMismatch
    );
    let wrong_protocol = TargetProfile::new(
        target.model,
        protocol("wrong protocol"),
        target.environment,
        target.task_profile,
        target.channel,
    );
    assert_eq!(
        prepare_trial_dispatch(wrong_protocol, runtime.evaluator(), pair).unwrap_err(),
        TrialDispatchError::ProtocolMismatch
    );
    let wrong_environment = TargetProfile::new(
        target.model,
        target.protocol,
        environment("wrong environment"),
        target.task_profile,
        target.channel,
    );
    assert_eq!(
        prepare_trial_dispatch(wrong_environment, runtime.evaluator(), pair).unwrap_err(),
        TrialDispatchError::EnvironmentMismatch
    );
    assert_eq!(
        prepare_trial_dispatch(target, evaluator("wrong evaluator"), pair).unwrap_err(),
        TrialDispatchError::EvaluatorMismatch
    );
}

#[test]
fn deterministic_schedule_uses_both_orders_without_changing_identity() {
    let runtime = baseline_runtime();
    let original = pair(runtime);
    assert_eq!(pair(runtime), original);
    assert_eq!(pair(runtime).ordered_sides(), original.ordered_sides());

    let mut parent_first = false;
    let mut candidate_first = false;
    for index in 0..128 {
        let pair = pair_with_input(runtime, &format!("input {index}"));
        match pair.ordered_sides() {
            [TrialSide::Parent, TrialSide::Candidate] => parent_first = true,
            [TrialSide::Candidate, TrialSide::Parent] => candidate_first = true,
            order => panic!("invalid paired schedule: {order:?}"),
        }
    }

    assert!(parent_first);
    assert!(candidate_first);
}

#[test]
fn matching_terminal_receipts_form_usable_paired_evidence() {
    let pair = pair(baseline_runtime());
    let (parent, candidate) = collected_receipts(pair, passing_outcome(), passing_outcome());
    let evidence = classify_paired_trial(pair, Some(&parent), Some(&candidate)).unwrap();

    let PairedTrialEvidence::Usable(receipt) = evidence else {
        panic!("matching receipts must form usable evidence");
    };
    assert_eq!(receipt.pair(), pair.id());
    assert_eq!(receipt.effect(), effect(pair));
    assert_eq!(receipt.lease_epoch(), LeaseEpoch::new(3).unwrap());
    assert_eq!(receipt.parent_receipt(), parent.id());
    assert_eq!(receipt.candidate_receipt(), candidate.id());
    assert_eq!(
        receipt.parent(),
        &BehavioralTrialOutcome::pass(BTreeMap::from([(
            MetricName::new("quality").unwrap(),
            MetricScore::from_millionths(900_000).unwrap(),
        )]))
    );
    assert_eq!(receipt.parent(), receipt.candidate());
    assert_ne!(parent.isolation_instance(), candidate.isolation_instance());
}

#[test]
fn verified_behavioral_failures_remain_usable_negative_evidence() {
    let reasons = [
        BehavioralFailureReason::AttributableCandidateCrash,
        BehavioralFailureReason::BudgetExhaustion,
        BehavioralFailureReason::EvaluatorRejection,
        BehavioralFailureReason::ProtocolTimeout,
    ];

    for reason in reasons {
        let pair = pair_with_input(baseline_runtime(), &format!("failure {reason:?}"));
        let (parent, candidate) = collected_receipts(
            pair,
            passing_outcome(),
            TerminalTrialOutcome::behavioral_failure(reason),
        );
        let evidence = classify_paired_trial(pair, Some(&parent), Some(&candidate)).unwrap();
        let PairedTrialEvidence::Usable(receipt) = evidence else {
            panic!("verified behavioral failure must remain usable evidence");
        };
        assert_eq!(
            receipt.candidate(),
            &BehavioralTrialOutcome::failure(reason)
        );
    }
}

#[test]
fn unknown_terminal_states_are_inconclusive_and_never_locally_retried() {
    let reasons = [
        InfrastructureUnknownReason::EnvironmentDrift,
        InfrastructureUnknownReason::EvaluatorDrift,
        InfrastructureUnknownReason::MissingReceipt,
        InfrastructureUnknownReason::ModelDrift,
        InfrastructureUnknownReason::TamperedReceipt,
        InfrastructureUnknownReason::TransportLoss,
        InfrastructureUnknownReason::UnreconciledExternalAttempt,
    ];

    for reason in reasons {
        let pair = pair_with_input(baseline_runtime(), &format!("unknown {reason:?}"));
        let dispatch = dispatch(pair);
        let effect = effect(pair);
        let lease_epoch = LeaseEpoch::new(3).unwrap();
        let parent = IsolatedRunReceipt::collected(
            &run(&dispatch, TrialSide::Parent),
            effect,
            lease_epoch,
            passing_outcome(),
            digest("parent runtime receipt"),
        )
        .unwrap();
        let candidate = IsolatedRunReceipt::fenced_unknown(
            &run(&dispatch, TrialSide::Candidate),
            effect,
            lease_epoch,
            reason,
            digest("candidate fence receipt"),
        )
        .unwrap();

        assert!(!candidate.retry_permitted());
        assert!(matches!(
            candidate.terminal(),
            TrialTransportTerminal::Fenced(_)
        ));
        let evidence = classify_paired_trial(pair, Some(&parent), Some(&candidate)).unwrap();
        let PairedTrialEvidence::InfrastructureUnknown(unknown) = evidence else {
            panic!("infrastructure uncertainty must not become scored evidence");
        };
        assert_eq!(unknown.pair(), pair.id());
        assert_eq!(unknown.reason(), reason);
        assert!(!unknown.retry_permitted());
    }
}

#[test]
fn runtime_identity_drift_is_inconclusive() {
    let expected = pair(baseline_runtime());
    let expected_dispatch = dispatch(expected);
    let effect = effect(expected);
    let lease_epoch = LeaseEpoch::new(3).unwrap();
    let parent = IsolatedRunReceipt::collected(
        &run(&expected_dispatch, TrialSide::Parent),
        effect,
        lease_epoch,
        passing_outcome(),
        digest("parent runtime receipt"),
    )
    .unwrap();
    let cases = [
        (
            runtime(
                "drifted model",
                "trial protocol",
                "trial evaluator",
                "trial environment",
            ),
            InfrastructureUnknownReason::ModelDrift,
        ),
        (
            runtime(
                "trial model",
                "drifted protocol",
                "trial evaluator",
                "trial environment",
            ),
            InfrastructureUnknownReason::EnvironmentDrift,
        ),
        (
            runtime(
                "trial model",
                "trial protocol",
                "drifted evaluator",
                "trial environment",
            ),
            InfrastructureUnknownReason::EvaluatorDrift,
        ),
        (
            runtime(
                "trial model",
                "trial protocol",
                "trial evaluator",
                "drifted environment",
            ),
            InfrastructureUnknownReason::EnvironmentDrift,
        ),
    ];

    for (runtime, expected_reason) in cases {
        let drifted_pair = pair(runtime);
        let drifted_dispatch = dispatch(drifted_pair);
        let candidate = IsolatedRunReceipt::collected(
            &run(&drifted_dispatch, TrialSide::Candidate),
            effect,
            lease_epoch,
            passing_outcome(),
            digest("drifted candidate runtime receipt"),
        )
        .unwrap();

        let evidence = classify_paired_trial(expected, Some(&parent), Some(&candidate)).unwrap();
        let PairedTrialEvidence::InfrastructureUnknown(unknown) = evidence else {
            panic!("runtime drift must not become scored evidence");
        };
        assert_eq!(unknown.reason(), expected_reason);
    }
}

#[test]
fn missing_mismatched_and_shared_isolation_receipts_fail_closed() {
    let pair = pair(baseline_runtime());
    let dispatch = dispatch(pair);
    let effect = effect(pair);
    let lease_epoch = LeaseEpoch::new(3).unwrap();
    let parent_run = run(&dispatch, TrialSide::Parent);
    let candidate_run = run(&dispatch, TrialSide::Candidate);
    let parent = IsolatedRunReceipt::collected(
        &parent_run,
        effect,
        lease_epoch,
        passing_outcome(),
        digest("parent runtime receipt"),
    )
    .unwrap();

    let missing = classify_paired_trial(pair, Some(&parent), None).unwrap();
    let PairedTrialEvidence::InfrastructureUnknown(missing) = missing else {
        panic!("missing receipt must be inconclusive");
    };
    assert_eq!(
        missing.reason(),
        InfrastructureUnknownReason::MissingReceipt
    );
    assert!(!missing.retry_permitted());

    let wrong_effect = IsolatedRunReceipt::collected(
        &candidate_run,
        EffectId::derive(
            pair.context().campaign(),
            18,
            EffectKind::Trial,
            EffectWorkId::from_digest(digest("different work")),
        ),
        lease_epoch,
        passing_outcome(),
        digest("candidate runtime receipt"),
    )
    .unwrap();
    let evidence = classify_paired_trial(pair, Some(&parent), Some(&wrong_effect)).unwrap();
    let PairedTrialEvidence::InfrastructureUnknown(unknown) = evidence else {
        panic!("effect mismatch must be inconclusive");
    };
    assert_eq!(
        unknown.reason(),
        InfrastructureUnknownReason::TamperedReceipt
    );

    let wrong_lease = IsolatedRunReceipt::collected(
        &candidate_run,
        effect,
        LeaseEpoch::new(4).unwrap(),
        passing_outcome(),
        digest("candidate runtime receipt"),
    )
    .unwrap();
    let evidence = classify_paired_trial(pair, Some(&parent), Some(&wrong_lease)).unwrap();
    let PairedTrialEvidence::InfrastructureUnknown(unknown) = evidence else {
        panic!("lease mismatch must be inconclusive");
    };
    assert_eq!(
        unknown.reason(),
        InfrastructureUnknownReason::TamperedReceipt
    );

    let mut shared_run = serde_json::to_value(candidate_run).unwrap();
    replace(
        &mut shared_run,
        &["isolation_instance"],
        json!(parent_run.isolation_instance()),
    );
    let shared_run: TrialRunAssignment = serde_json::from_value(shared_run).unwrap();
    let shared_isolation = IsolatedRunReceipt::collected(
        &shared_run,
        effect,
        lease_epoch,
        passing_outcome(),
        digest("candidate runtime receipt"),
    )
    .unwrap();
    let evidence = classify_paired_trial(pair, Some(&parent), Some(&shared_isolation)).unwrap();
    let PairedTrialEvidence::InfrastructureUnknown(unknown) = evidence else {
        panic!("shared isolation must be inconclusive");
    };
    assert_eq!(
        unknown.reason(),
        InfrastructureUnknownReason::EnvironmentDrift
    );
}

#[test]
fn receipt_and_key_tampering_is_rejected_during_deserialization() {
    let pair = pair(baseline_runtime());
    let (parent, _) = collected_receipts(pair, passing_outcome(), passing_outcome());

    let mut altered_receipt = serde_json::to_value(&parent).unwrap();
    replace(&mut altered_receipt, &["lease_epoch"], json!(4));
    assert!(serde_json::from_value::<IsolatedRunReceipt>(altered_receipt).is_err());

    let mut altered_key = serde_json::to_value(&parent).unwrap();
    replace(
        &mut altered_key,
        &["key", "side"],
        json!(TrialSide::Candidate),
    );
    assert!(serde_json::from_value::<IsolatedRunReceipt>(altered_key).is_err());

    let dispatch = dispatch(pair);
    let fenced = IsolatedRunReceipt::fenced_unknown(
        &run(&dispatch, TrialSide::Candidate),
        effect(pair),
        LeaseEpoch::new(3).unwrap(),
        InfrastructureUnknownReason::TransportLoss,
        digest("fence receipt"),
    )
    .unwrap();
    let mut behavioral_fence = serde_json::to_value(fenced).unwrap();
    replace(
        &mut behavioral_fence,
        &["outcome"],
        json!(TerminalTrialOutcome::behavioral_failure(
            BehavioralFailureReason::AttributableCandidateCrash
        )),
    );
    let error = serde_json::from_value::<IsolatedRunReceipt>(behavioral_fence).unwrap_err();
    assert!(error.to_string().contains("fenced attempt"));
}

#[test]
fn metric_partition_and_limit_boundaries_are_enforced() {
    assert!(matches!(
        MetricName::new(""),
        Err(TrialError::InvalidMetricName)
    ));
    assert!(matches!(
        MetricName::new("line\nbreak"),
        Err(TrialError::InvalidMetricName)
    ));
    assert!(matches!(
        MetricName::new("x".repeat(129)),
        Err(TrialError::InvalidMetricName)
    ));
    assert!(MetricName::new("x".repeat(128)).is_ok());
    assert_eq!(
        MetricScore::from_millionths(1_000_000)
            .unwrap()
            .millionths(),
        1_000_000
    );
    assert!(matches!(
        MetricScore::from_millionths(1_000_001),
        Err(TrialError::InvalidMetricScore)
    ));

    let commitment = PartitionCommitment::from_digest(digest("partition"));
    assert!(matches!(
        TrialPartition::new(TrialLedgerRole::AdaptivePromotion, 0, commitment),
        Err(TrialError::InvalidPartitionEpoch)
    ));
    assert!(matches!(
        TrialPartition::new(
            TrialLedgerRole::AdaptivePromotion,
            i64::MAX as u64 + 1,
            commitment
        ),
        Err(TrialError::InvalidPartitionEpoch)
    ));

    let partition = TrialPartition::new(TrialLedgerRole::AdaptivePromotion, 1, commitment).unwrap();
    let invalid_limits = Limits {
        tokens: 0,
        ..Limits::default()
    };
    assert!(matches!(
        TrialContext::new(
            CampaignId::from_uuid(Uuid::from_u128(1)),
            CandidateId::from_digest(digest("candidate")),
            partition,
            baseline_runtime(),
            invalid_limits,
        ),
        Err(TrialError::InvalidLimits)
    ));
}
