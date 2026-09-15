use orvek_harness::{
    BoundedProposal, CandidateId, CompositeField, CompositionError, CompositionInput, Digest,
    DiversityDimension, ManifestError, MiningBundleRoot, ModelIdentity, ProposalAttempt,
    ProposalProviderReceiptId, ProposalRequest, ProtocolIdentity, ScoreResultId,
    ValidatedHarnessRevision, compose_candidates, validate_proposal_batch,
};
use serde_json::{Value, json};

#[test]
fn disjoint_composition_is_canonical_for_every_input_order() {
    let parent = parent();
    let proposals = proposals(
        &parent,
        "commutative",
        vec![
            (
                vec!["instructions"],
                json!({"instructions": "Inspect evidence, implement, and verify."}),
            ),
            (
                vec!["skills"],
                json!({"skills": {"review": "Check the protected evidence before reporting."}}),
            ),
            (
                vec!["subagent_roles"],
                json!({"subagent_roles": {
                    "reader": {
                        "instructions": "Collect bounded evidence and cite each finding.",
                        "tools": ["read_file", "search"]
                    }
                }}),
            ),
        ],
    );
    let candidates = [candidate("a"), candidate("b"), candidate("c")];
    let scores = [score("a"), score("b"), score("c")];
    let permutations = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];

    let expected = compose_candidates(
        &parent,
        inputs(&proposals, candidates, scores, permutations[0]),
    )
    .unwrap();
    for order in permutations.into_iter().skip(1) {
        let actual =
            compose_candidates(&parent, inputs(&proposals, candidates, scores, order)).unwrap();
        assert_eq!(actual.plan(), expected.plan());
        assert_eq!(
            actual.canonical_patch_bytes(),
            expected.canonical_patch_bytes()
        );
        assert_eq!(actual.revision().digest(), expected.revision().digest());
    }
}

#[test]
fn identical_replacements_are_idempotent_but_different_replacements_conflict() {
    let parent = parent();
    let first = proposals(
        &parent,
        "identical-a",
        vec![
            (
                vec!["instructions"],
                json!({"instructions": "Use the same verified instruction."}),
            ),
            (
                vec!["verifier"],
                json!({"verifier": {"after_tool_calls": 16, "before_completion": true}}),
            ),
        ],
    );
    let second = proposals(
        &parent,
        "identical-b",
        vec![
            (
                vec!["instructions"],
                json!({"instructions": "Use the same verified instruction."}),
            ),
            (
                vec!["verifier"],
                json!({"verifier": {"after_tool_calls": 32, "before_completion": true}}),
            ),
        ],
    );
    let first_instruction = proposal_for(&first, DiversityDimension::Instructions);
    let second_instruction = proposal_for(&second, DiversityDimension::Instructions);
    let composed = compose_candidates(
        &parent,
        [
            CompositionInput::new(candidate("same-a"), score("same-a"), first_instruction),
            CompositionInput::new(candidate("same-b"), score("same-b"), second_instruction),
        ],
    )
    .unwrap();
    assert_eq!(
        composed.revision().digest(),
        first_instruction.revision().digest()
    );

    let conflicting = proposals(
        &parent,
        "conflict",
        vec![
            (
                vec!["instructions"],
                json!({"instructions": "First replacement."}),
            ),
            (
                vec!["instructions"],
                json!({"instructions": "Second replacement."}),
            ),
        ],
    );
    assert!(matches!(
        compose_candidates(
            &parent,
            [
                CompositionInput::new(
                    candidate("conflict-a"),
                    score("conflict-a"),
                    &conflicting[0]
                ),
                CompositionInput::new(
                    candidate("conflict-b"),
                    score("conflict-b"),
                    &conflicting[1]
                ),
            ],
        ),
        Err(CompositionError::FieldConflict {
            field: CompositeField::Instructions,
            ..
        })
    ));
}

#[test]
fn combined_envelope_constraints_are_revalidated_after_merge() {
    let parent = parent();
    let proposals = proposals(
        &parent,
        "combined-ceiling",
        vec![
            (
                vec!["budgets"],
                json!({"budgets": {
                    "tool_calls": 32,
                    "subagents": 4,
                    "verifier_runs": 5,
                    "output_bytes": 1048576,
                    "tokens": 200000,
                    "elapsed_ms": 600000
                }}),
            ),
            (
                vec!["verifier"],
                json!({"verifier": {"after_tool_calls": 64, "before_completion": true}}),
            ),
        ],
    );
    assert!(matches!(
        compose_candidates(
            &parent,
            [
                CompositionInput::new(candidate("budget"), score("budget"), &proposals[0]),
                CompositionInput::new(candidate("verifier"), score("verifier"), &proposals[1]),
            ],
        ),
        Err(CompositionError::Manifest(
            ManifestError::VerifierIntervalExceedsBudget
        ))
    ));
}

fn parent() -> ValidatedHarnessRevision {
    let behavior = json!({
        "instructions": "Work carefully and verify externally meaningful behavior.",
        "skills": {},
        "recovery_reminders": ["inspect_status_before_retry", "preserve_pinned_revision"],
        "subagent_roles": {},
        "verifier": {"after_tool_calls": 8, "before_completion": true},
        "budgets": {
            "tool_calls": 64,
            "subagents": 4,
            "verifier_runs": 16,
            "output_bytes": 1048576,
            "tokens": 200000,
            "elapsed_ms": 600000
        }
    });
    ValidatedHarnessRevision::from_behavior_json(
        Digest::of(b"composition parent"),
        &serde_json::to_vec(&behavior).unwrap(),
    )
    .unwrap()
}

fn proposals(
    parent: &ValidatedHarnessRevision,
    namespace: &str,
    patches: Vec<(Vec<&str>, Value)>,
) -> Vec<BoundedProposal> {
    let request = ProposalRequest::new(
        parent.digest(),
        MiningBundleRoot::from_digest(digest(&format!("{namespace}:mining"))),
        parent.policy_identity(),
        ModelIdentity::from_digest(digest(&format!("{namespace}:model"))),
        ProtocolIdentity::from_digest(digest(&format!("{namespace}:protocol"))),
        patches.len().try_into().unwrap(),
        64 * 1024,
    )
    .unwrap();
    let attempts: Vec<_> = request
        .intents()
        .iter()
        .copied()
        .zip(patches)
        .enumerate()
        .map(|(index, (intent, (dimensions, patch)))| {
            let output = serde_json::to_vec(&json!({
                "schema_version": 1,
                "request": request.root(),
                "intent": intent.id(),
                "parent": request.parent(),
                "evidence": request.evidence(),
                "policy": request.policy(),
                "dimensions": dimensions,
                "patch": patch,
            }))
            .unwrap();
            ProposalAttempt::settled(
                intent,
                ProposalProviderReceiptId::from_digest(digest(&format!(
                    "{namespace}:receipt:{index}"
                ))),
                output,
            )
        })
        .collect();
    validate_proposal_batch(request, parent, attempts)
        .unwrap()
        .proposals()
        .to_vec()
}

fn inputs<'a>(
    proposals: &'a [BoundedProposal],
    candidates: [CandidateId; 3],
    scores: [ScoreResultId; 3],
    order: [usize; 3],
) -> Vec<CompositionInput<'a>> {
    order
        .into_iter()
        .map(|index| CompositionInput::new(candidates[index], scores[index], &proposals[index]))
        .collect()
}

fn proposal_for(proposals: &[BoundedProposal], dimension: DiversityDimension) -> &BoundedProposal {
    proposals
        .iter()
        .find(|proposal| proposal.dimensions().contains(&dimension))
        .unwrap()
}

fn candidate(label: &str) -> CandidateId {
    CandidateId::from_digest(digest(&format!("candidate:{label}")))
}

fn score(label: &str) -> ScoreResultId {
    ScoreResultId::from_digest(digest(&format!("score:{label}")))
}

fn digest(value: &str) -> Digest {
    Digest::of(value.as_bytes())
}
