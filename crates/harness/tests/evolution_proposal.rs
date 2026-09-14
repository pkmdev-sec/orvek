use orvek_harness::{
    Channel, Digest, EnvironmentIdentity, MiningBundleRoot, ModelIdentity, PolicyIdentity,
    ProposalAttempt, ProposalError, ProposalIntent, ProposalProviderReceiptId, ProposalRequest,
    ProtocolIdentity, TargetProfile, TaskProfileIdentity, ValidatedHarnessRevision,
    controller::{ProposalDispatchError, prepare_proposal_dispatch},
    validate_proposal_batch,
};
use serde_json::{Value, json};

fn behavior() -> Value {
    json!({
        "instructions": "Work carefully and verify externally meaningful behavior.",
        "skills": {
            "review": "Inspect the relevant evidence before reporting a conclusion."
        },
        "recovery_reminders": [
            "inspect_status_before_retry",
            "preserve_pinned_revision"
        ],
        "subagent_roles": {
            "reader": {
                "instructions": "Collect bounded evidence and return cited findings.",
                "tools": ["read_file", "search"]
            }
        },
        "verifier": {
            "after_tool_calls": 8,
            "before_completion": true
        },
        "budgets": {
            "tool_calls": 64,
            "subagents": 4,
            "verifier_runs": 16,
            "output_bytes": 1048576,
            "tokens": 200000,
            "elapsed_ms": 600000
        }
    })
}

fn parent() -> ValidatedHarnessRevision {
    ValidatedHarnessRevision::from_behavior_json(
        Digest::of(b"proposal parent"),
        &serde_json::to_vec(&behavior()).unwrap(),
    )
    .unwrap()
}

fn model(label: &str) -> ModelIdentity {
    ModelIdentity::from_digest(Digest::of(label.as_bytes()))
}

fn protocol(label: &str) -> ProtocolIdentity {
    ProtocolIdentity::from_digest(Digest::of(label.as_bytes()))
}

fn request_for(
    parent: &ValidatedHarnessRevision,
    candidate_count: u16,
    max_patch_bytes: usize,
) -> ProposalRequest {
    ProposalRequest::new(
        parent.digest(),
        MiningBundleRoot::from_digest(Digest::of(b"mining bundle")),
        parent.policy_identity(),
        model("target model"),
        protocol("frozen protocol"),
        candidate_count,
        max_patch_bytes,
    )
    .unwrap()
}

fn receipt(label: &str) -> ProposalProviderReceiptId {
    ProposalProviderReceiptId::from_digest(Digest::of(label.as_bytes()))
}

fn output(
    request: ProposalRequest,
    intent: ProposalIntent,
    dimensions: Value,
    patch: Value,
) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema_version": 1,
        "request": request.root(),
        "intent": intent.id(),
        "parent": request.parent(),
        "evidence": request.evidence(),
        "policy": request.policy(),
        "dimensions": dimensions,
        "patch": patch
    }))
    .unwrap()
}

fn diverse_attempts(request: ProposalRequest) -> Vec<ProposalAttempt> {
    let intents = request.intents();
    vec![
        ProposalAttempt::settled(
            intents[0],
            receipt("first receipt"),
            output(
                request,
                intents[0],
                json!(["instructions"]),
                json!({"instructions": "Inspect the evidence, then verify the result."}),
            ),
        ),
        ProposalAttempt::settled(
            intents[1],
            receipt("second receipt"),
            output(
                request,
                intents[1],
                json!(["verifier"]),
                json!({
                    "verifier": {
                        "after_tool_calls": 16,
                        "before_completion": true
                    }
                }),
            ),
        ),
    ]
}

#[test]
fn valid_diverse_outputs_are_canonical_across_completion_order() {
    let parent = parent();
    let request = request_for(&parent, 2, 4096);
    let first = validate_proposal_batch(request, &parent, diverse_attempts(request)).unwrap();

    let mut reversed = diverse_attempts(request);
    reversed.reverse();
    let second = validate_proposal_batch(request, &parent, reversed).unwrap();

    assert_eq!(first.root(), second.root());
    assert_eq!(first.canonical_bytes(), second.canonical_bytes());
    assert_eq!(first.proposals(), second.proposals());
    assert_eq!(first.proposals().len(), 2);
    assert_ne!(
        first.proposals()[0].revision().behavior_digest(),
        first.proposals()[1].revision().behavior_digest()
    );
}

#[test]
fn malformed_and_out_of_envelope_outputs_fail_before_trial() {
    let parent = parent();
    let request = request_for(&parent, 2, 4096);
    let intents = request.intents();
    let malformed = vec![
        ProposalAttempt::settled(intents[0], receipt("malformed"), b"not json".to_vec()),
        diverse_attempts(request).remove(1),
    ];
    assert!(matches!(
        validate_proposal_batch(request, &parent, malformed),
        Err(ProposalError::InvalidProviderOutput(_))
    ));

    let forbidden = vec![
        ProposalAttempt::settled(
            intents[0],
            receipt("forbidden"),
            output(
                request,
                intents[0],
                json!(["instructions"]),
                json!({"permissions": "all"}),
            ),
        ),
        diverse_attempts(request).remove(1),
    ];
    assert!(matches!(
        validate_proposal_batch(request, &parent, forbidden),
        Err(ProposalError::InvalidProviderOutput(_))
    ));
}

#[test]
fn redundant_no_op_and_duplicate_behaviors_are_rejected() {
    let parent = parent();
    let request = request_for(&parent, 2, 4096);
    let intents = request.intents();
    let redundant = vec![
        ProposalAttempt::settled(
            intents[0],
            receipt("redundant"),
            output(
                request,
                intents[0],
                json!(["instructions"]),
                json!({"instructions": behavior()["instructions"].clone()}),
            ),
        ),
        diverse_attempts(request).remove(1),
    ];
    assert!(matches!(
        validate_proposal_batch(request, &parent, redundant),
        Err(ProposalError::RedundantPatchField { .. })
    ));

    let patch = json!({"instructions": "Use the same distinct behavior."});
    let duplicate = vec![
        ProposalAttempt::settled(
            intents[0],
            receipt("duplicate one"),
            output(request, intents[0], json!(["instructions"]), patch.clone()),
        ),
        ProposalAttempt::settled(
            intents[1],
            receipt("duplicate two"),
            output(request, intents[1], json!(["instructions"]), patch),
        ),
    ];
    assert!(matches!(
        validate_proposal_batch(request, &parent, duplicate),
        Err(ProposalError::DuplicateCandidate)
    ));
}

#[test]
fn stale_bindings_and_wrong_intents_are_rejected() {
    let parent = parent();
    let request = request_for(&parent, 2, 4096);
    let intents = request.intents();
    for field in ["request", "parent", "evidence", "policy", "intent"] {
        let mut payload: Value = serde_json::from_slice(&output(
            request,
            intents[0],
            json!(["instructions"]),
            json!({"instructions": "A valid changed instruction."}),
        ))
        .unwrap();
        payload[field] = json!(Digest::of(field.as_bytes()));
        let attempts = vec![
            ProposalAttempt::settled(
                intents[0],
                receipt(field),
                serde_json::to_vec(&payload).unwrap(),
            ),
            diverse_attempts(request).remove(1),
        ];
        assert!(matches!(
            validate_proposal_batch(request, &parent, attempts),
            Err(ProposalError::OutputBindingMismatch { field: actual }) if actual == field
        ));
    }

    let duplicate_intent = vec![
        ProposalAttempt::settled(
            intents[0],
            receipt("one"),
            output(
                request,
                intents[0],
                json!(["instructions"]),
                json!({"instructions": "First."}),
            ),
        ),
        ProposalAttempt::settled(
            intents[0],
            receipt("two"),
            output(
                request,
                intents[0],
                json!(["instructions"]),
                json!({"instructions": "Second."}),
            ),
        ),
    ];
    assert!(matches!(
        validate_proposal_batch(request, &parent, duplicate_intent),
        Err(ProposalError::DuplicateIntent)
    ));
}

#[test]
fn patch_budget_and_declared_diversity_are_enforced() {
    let parent = parent();
    let request = request_for(&parent, 2, 8);
    let intents = request.intents();
    let attempts = vec![
        ProposalAttempt::settled(
            intents[0],
            receipt("large"),
            output(
                request,
                intents[0],
                json!(["instructions"]),
                json!({"instructions": "This patch is larger than eight bytes."}),
            ),
        ),
        diverse_attempts(request).remove(1),
    ];
    assert!(matches!(
        validate_proposal_batch(request, &parent, attempts),
        Err(ProposalError::PatchTooLarge { maximum: 8 })
    ));

    let request = request_for(&parent, 2, 4096);
    let intents = request.intents();
    let mismatch = vec![
        ProposalAttempt::settled(
            intents[0],
            receipt("dimension mismatch"),
            output(
                request,
                intents[0],
                json!(["verifier"]),
                json!({"instructions": "A valid changed instruction."}),
            ),
        ),
        diverse_attempts(request).remove(1),
    ];
    assert!(matches!(
        validate_proposal_batch(request, &parent, mismatch),
        Err(ProposalError::DiversityMismatch)
    ));
}

#[test]
fn provider_unknown_is_conservatively_charged_and_never_retried() {
    let parent = parent();
    let request = request_for(&parent, 2, 4096);
    let intents = request.intents();
    let unknown =
        ProposalAttempt::infrastructure_unknown(intents[0], receipt("unknown receipt"), 50_000)
            .unwrap();
    assert!(!unknown.retry_permitted());
    assert_eq!(
        unknown
            .infrastructure_unknown_receipt()
            .unwrap()
            .charged_tokens(),
        50_000
    );
    let attempts = vec![unknown, diverse_attempts(request).remove(1)];
    assert!(matches!(
        validate_proposal_batch(request, &parent, attempts),
        Err(ProposalError::ProviderUnknown {
            charged_tokens: 50_000,
            ..
        })
    ));
}

#[test]
fn request_exposes_only_mining_identity_not_cross_role_evidence() {
    let parent = parent();
    let request = request_for(&parent, 2, 4096);
    let serialized = serde_json::to_string(&request).unwrap();

    assert!(serialized.contains("evidence"));
    for forbidden in [
        "adaptive",
        "final_audit",
        "raw_trace",
        "evaluator",
        "credential",
        "active_pointer",
    ] {
        assert!(!serialized.contains(forbidden), "leaked {forbidden}");
    }
}

#[test]
fn host_dispatch_requires_frozen_model_protocol_and_registered_policy() {
    let parent = parent();
    let request = request_for(&parent, 2, 4096);
    let target = TargetProfile::new(
        request.model(),
        request.protocol(),
        EnvironmentIdentity::from_digest(Digest::of(b"environment")),
        TaskProfileIdentity::from_digest(Digest::of(b"tasks")),
        Channel::Stable,
    );
    let dispatch = prepare_proposal_dispatch(target, request.policy(), request).unwrap();
    assert_eq!(dispatch.intents(), request.intents());

    let wrong_model = TargetProfile::new(
        model("other"),
        request.protocol(),
        target.environment,
        target.task_profile,
        target.channel,
    );
    assert!(matches!(
        prepare_proposal_dispatch(wrong_model, request.policy(), request),
        Err(ProposalDispatchError::ModelMismatch)
    ));

    let wrong_protocol = TargetProfile::new(
        request.model(),
        protocol("other"),
        target.environment,
        target.task_profile,
        target.channel,
    );
    assert!(matches!(
        prepare_proposal_dispatch(wrong_protocol, request.policy(), request),
        Err(ProposalDispatchError::ProtocolMismatch)
    ));
    assert!(matches!(
        prepare_proposal_dispatch(
            target,
            PolicyIdentity::from_digest(Digest::of(b"other policy")),
            request,
        ),
        Err(ProposalDispatchError::PolicyMismatch)
    ));
}

#[test]
fn request_limits_and_attempt_count_fail_closed() {
    let parent = parent();
    assert!(matches!(
        ProposalRequest::new(
            parent.digest(),
            MiningBundleRoot::from_digest(Digest::of(b"mining")),
            parent.policy_identity(),
            model("model"),
            protocol("protocol"),
            1,
            1024,
        ),
        Err(ProposalError::InvalidCandidateCount { .. })
    ));

    let request = request_for(&parent, 2, 4096);
    let mut attempts = diverse_attempts(request);
    attempts.pop();
    assert!(matches!(
        validate_proposal_batch(request, &parent, attempts),
        Err(ProposalError::WrongAttemptCount {
            expected: 2,
            actual: 1,
        })
    ));
}
