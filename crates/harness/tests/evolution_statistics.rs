use orvek_harness::{
    AdaptivePromotionDataset, AdaptiveTrialEvidence, AdaptiveVerdict, AuditEpochId,
    BehavioralFailureReason, BoundedProposal, CampaignBudget, CampaignEvent, CampaignId,
    CampaignPhase, CampaignState, CandidateId, CandidateLabel, CandidateParent, CaseIdentity,
    Channel, CohortId, CohortLedger, CompositeField, CompositeOutcome, CompositeStage,
    CompositionFailureReason, CompositionFallback, CompositionInput, DecisionCoordinates, Digest,
    EffectId, EffectKind, EffectWorkId, EnvironmentIdentity, EvaluationCase, EvaluationCohortSpec,
    EvaluatorIdentity, ExactEffect, ExactRatio, FrozenScoringPolicy, GateEvidence, GateName,
    GateStatus, IndependentBlock, IndependentBlockId, InputCommitment, IsolatedRunReceipt,
    LeaseEpoch, LedgerLimit, MetricKind, MetricName, MetricPolicy, MetricScore, MiningBundleRoot,
    MiningResultId, ModelIdentity, MultiplicityFamily, PartitionCommitment, PartitionCommitments,
    ProposalAttempt, ProposalId, ProposalProviderReceiptId, ProposalRequest, ProtocolIdentity,
    ResourceUsage, RoundId, RoundVerdict, RoundVerdictId, ScoreResultId, ScoringError, Store,
    StoreError, StratumName, StratumPolicy, TargetProfile, TaskIdentity, TaskProfileIdentity,
    TerminalState, TerminalTrialOutcome, TrialContext, TrialLedgerRole, TrialPairSpec,
    TrialPartition, TrialResultId, TrialRuntimeIdentity, TrialSide, ValidatedHarnessRevision,
    VerifiedComposition, classify_paired_trial, contract::Limits,
    controller::prepare_trial_dispatch, validate_proposal_batch,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const ERROR_NANOS_PER_HYPOTHESIS: u64 = 7_812_500;
const FAMILY_SIZE: u64 = 128;

#[derive(Clone, Copy, Default)]
struct EvidenceVariation {
    omit: Option<(usize, u32)>,
    unknown: Option<(usize, u32)>,
    drift: Option<(usize, u32)>,
    candidate_failure_case: Option<usize>,
}

#[derive(Clone, Copy)]
struct Scores {
    parent_quality: u32,
    candidate_quality: u32,
    parent_safety: u32,
    candidate_safety: u32,
}

impl Scores {
    const fn canonical() -> Self {
        Self {
            parent_quality: 600_000,
            candidate_quality: 900_000,
            parent_safety: 800_000,
            candidate_safety: 800_000,
        }
    }
}

#[test]
fn canonical_vector_matches_the_frozen_exact_sign_oracle_and_authorizes_composition() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let cohort = register_canonical_cohort(&mut store, "canonical", 7);
    let parent = store.load_harness_revision(cohort.base_revision).unwrap();
    let proposal = proposal_batch(
        &parent,
        "canonical",
        vec![
            (
                vec!["instructions"],
                json!({"instructions": "Inspect evidence and verify the selected candidate."}),
            ),
            (
                vec!["skills"],
                json!({"skills": {"review": "Review the candidate evidence."}}),
            ),
        ],
    )
    .remove(0);
    let campaign = CampaignId::new();
    let (state, round, candidate) = prepare_scoring_campaign_with_proposal(
        &mut store,
        campaign,
        &cohort,
        "canonical",
        proposal.id(),
    );
    let dataset = dataset(
        &cohort,
        campaign,
        candidate,
        "candidate label",
        Scores::canonical(),
        EvidenceVariation::default(),
    );

    let (state, report) = store
        .record_adaptive_score(campaign, state.revision(), round, dataset, coordinates())
        .unwrap();

    assert_eq!(report.verdict, AdaptiveVerdict::Verified);
    assert_eq!(report.production_activation, AdaptiveVerdict::Inconclusive);
    assert_eq!(report.ledger.query_debit, 2);
    assert_eq!(report.ledger.error_nanos_debit, 15_625_000);
    assert_eq!(
        (
            report.ledger.after.query_used,
            report.ledger.after.error_used_nanos
        ),
        (2, 15_625_000)
    );
    let quality = report
        .statistics
        .iter()
        .find(|statistic| statistic.metric.as_str() == "quality")
        .unwrap();
    assert_eq!(quality.case_effects.len(), 7);
    assert_eq!(quality.block_effects.len(), 7);
    assert_eq!(
        quality.observed_effect,
        ExactEffect {
            numerator: 3,
            denominator: 10,
        }
    );
    assert_eq!(
        quality.adjusted_effect,
        ExactEffect {
            numerator: 1,
            denominator: 4,
        }
    );
    assert_eq!(
        quality.p_value,
        ExactRatio {
            numerator: 1,
            denominator: 128,
        }
    );
    assert_eq!(quality.alpha, quality.p_value);
    assert_eq!(
        store
            .load_adaptive_score_report(report.result_id().unwrap())
            .unwrap(),
        report
    );

    let (composed, selected) = store
        .compose_verified_candidate(
            campaign,
            state.revision(),
            round,
            CompositionInput::new(candidate, report.result_id().unwrap(), &proposal),
        )
        .unwrap();
    assert_eq!(composed.phase(), CampaignPhase::Auditing);
    assert_eq!(selected.candidate(), candidate);
    assert_eq!(selected.revision().parent(), cohort.base_revision);
    assert_eq!(
        store
            .load_harness_revision(selected.revision().digest())
            .unwrap(),
        *selected.revision()
    );
    assert!(matches!(
        store.append_campaign_event(
            campaign,
            composed.revision(),
            CampaignEvent::CompositionRecorded {
                round,
                candidate: selected.candidate(),
                composition: selected.composition(),
                revision: selected.revision().digest(),
            },
        ),
        Err(StoreError::Invalid(
            "composition requires typed verified candidate authority"
        ))
    ));
    drop(store);

    let reopened = Store::open(root.path()).unwrap();
    assert_eq!(reopened.load_campaign(campaign).unwrap(), composed);
    assert_eq!(
        reopened
            .load_harness_revision(selected.revision().digest())
            .unwrap(),
        *selected.revision()
    );
}

#[test]
fn composite_uses_fresh_trials_and_falls_back_to_the_parent_after_a_protected_regression() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let cohort = register_canonical_cohort(&mut store, "composite", 7);
    let parent = store.load_harness_revision(cohort.base_revision).unwrap();
    let proposals = proposal_batch(
        &parent,
        "composite",
        vec![
            (
                vec!["instructions"],
                json!({"instructions": "Inspect evidence, implement, and verify."}),
            ),
            (
                vec!["skills"],
                json!({"skills": {"review": "Check protected evidence before reporting."}}),
            ),
        ],
    );
    let campaign = CampaignId::new();
    let round = RoundId::from_digest(digest("composite:round"));
    let candidates = [
        candidate_id("composite:candidate:a"),
        candidate_id("composite:candidate:b"),
    ];
    let mut state = store
        .create_campaign(CampaignEvent::Started {
            campaign,
            cohort: cohort.id,
            base_revision: cohort.base_revision,
            policy: cohort.policy,
            budget: CampaignBudget::new(32, ResourceUsage::new(100, 1_000, 10_000)).unwrap(),
        })
        .unwrap();
    for event in [
        CampaignEvent::RoundStarted { round },
        CampaignEvent::MiningCompleted {
            round,
            result: MiningResultId::from_digest(digest("composite:mining")),
        },
        CampaignEvent::CandidateProposed {
            round,
            candidate: candidates[0],
            parent: CandidateParent::BaseRevision(cohort.base_revision),
            proposal: proposals[0].id(),
        },
        CampaignEvent::CandidateProposed {
            round,
            candidate: candidates[1],
            parent: CandidateParent::BaseRevision(cohort.base_revision),
            proposal: proposals[1].id(),
        },
        CampaignEvent::ProposalsCompleted { round },
        CampaignEvent::CandidateTrialRecorded {
            round,
            candidate: candidates[0],
            trial: TrialResultId::from_digest(digest("composite:trial:a")),
        },
        CampaignEvent::CandidateTrialRecorded {
            round,
            candidate: candidates[1],
            trial: TrialResultId::from_digest(digest("composite:trial:b")),
        },
        CampaignEvent::TrialsCompleted { round },
    ] {
        state = store
            .append_campaign_event(campaign, state.revision(), event)
            .unwrap();
    }

    let child_a = dataset(
        &cohort,
        campaign,
        candidates[0],
        "candidate a",
        Scores::canonical(),
        EvidenceVariation::default(),
    );
    let child_b = dataset(
        &cohort,
        campaign,
        candidates[1],
        "candidate b",
        Scores::canonical(),
        EvidenceVariation::default(),
    );
    let child_pair_ids = child_a
        .pairs()
        .iter()
        .chain(child_b.pairs())
        .map(|pair| pair.spec.id())
        .collect::<BTreeSet<_>>();
    let (next, report_a) = store
        .record_adaptive_score(
            campaign,
            state.revision(),
            round,
            child_a,
            coordinates_at(0, 0),
        )
        .unwrap();
    state = next;
    let (next, report_b) = store
        .record_adaptive_score(
            campaign,
            state.revision(),
            round,
            child_b,
            coordinates_at(1, 0),
        )
        .unwrap();
    state = next;
    assert_eq!(report_a.verdict, AdaptiveVerdict::Verified);
    assert_eq!(report_b.verdict, AdaptiveVerdict::Verified);
    let scores = [report_a.result_id().unwrap(), report_b.result_id().unwrap()];
    let ledger_after_children = store
        .cohort_ledger_status(cohort.id, CohortLedger::AdaptivePromotion)
        .unwrap();
    assert!(matches!(
        store.append_campaign_event(
            campaign,
            state.revision(),
            CampaignEvent::RoundVerdictRecorded {
                round,
                verdict: RoundVerdict::Compose {
                    candidate: candidates[0],
                    basis: RoundVerdictId::from_digest(digest("composite:single-winner-bypass")),
                },
            },
        ),
        Err(StoreError::Invalid(
            "composition requires typed verified candidate authority"
        ))
    ));

    let inputs = [
        CompositionInput::new(candidates[0], scores[0], &proposals[0]),
        CompositionInput::new(candidates[1], scores[1], &proposals[1]),
    ];
    let (next, composition) = store
        .compose_verified_candidates(campaign, state.revision(), round, &inputs)
        .unwrap();
    state = next;
    let VerifiedComposition::Composed(composed) = composition else {
        panic!("disjoint verified candidates must compose");
    };
    let plan = composed.plan();
    let composite_candidate = plan.candidate();
    let composite_revision = plan.revision();
    assert_eq!(state.phase(), CampaignPhase::Trialing);
    assert_eq!(plan.children().len(), 2);
    assert_eq!(plan.fallback(), CompositionFallback::KeepParent);
    assert!(matches!(
        state.rounds()[0]
            .composite()
            .map(|composite| composite.stage()),
        Some(CompositeStage::AwaitingTrial)
    ));
    assert!(matches!(
        store.append_campaign_event(
            campaign,
            state.revision(),
            CampaignEvent::CompositeCompositionRecorded {
                round,
                plan: plan.clone(),
            },
        ),
        Err(StoreError::Invalid(
            "composition requires typed verified candidate authority"
        ))
    ));
    drop(store);
    store = Store::open(root.path()).unwrap();
    assert_eq!(store.load_campaign(campaign).unwrap(), state);
    assert_eq!(
        store.load_campaign(campaign).unwrap().rounds()[0]
            .composite()
            .unwrap()
            .plan()
            .children(),
        plan.children()
    );

    state = store
        .append_campaign_event(
            campaign,
            state.revision(),
            CampaignEvent::CandidateTrialRecorded {
                round,
                candidate: composite_candidate,
                trial: TrialResultId::from_digest(digest("composite:trial:fresh")),
            },
        )
        .unwrap();
    state = store
        .append_campaign_event(
            campaign,
            state.revision(),
            CampaignEvent::TrialsCompleted { round },
        )
        .unwrap();
    assert_eq!(state.phase(), CampaignPhase::Scoring);
    assert!(matches!(
        store.append_campaign_event(
            campaign,
            state.revision(),
            CampaignEvent::CompositeScoreRecorded {
                round,
                candidate: composite_candidate,
                score: ScoreResultId::from_digest(digest("composite:forged-score")),
                outcome: CompositeOutcome::Verified,
            },
        ),
        Err(StoreError::Invalid(
            "composite score requires typed adaptive scoring"
        ))
    ));

    let composite_dataset = dataset(
        &cohort,
        campaign,
        composite_candidate,
        "composite candidate",
        Scores {
            parent_quality: 600_000,
            candidate_quality: 900_000,
            parent_safety: 800_000,
            candidate_safety: 600_000,
        },
        EvidenceVariation::default(),
    );
    assert!(
        composite_dataset
            .pairs()
            .iter()
            .all(|pair| !child_pair_ids.contains(&pair.spec.id()))
    );
    let (terminal, composite_report) = store
        .record_adaptive_score(
            campaign,
            state.revision(),
            round,
            composite_dataset,
            coordinates_at(0, 1),
        )
        .unwrap();
    state = terminal;

    assert_eq!(composite_report.verdict, AdaptiveVerdict::NotVerified);
    assert_eq!(state.phase(), CampaignPhase::Terminal);
    assert!(matches!(
        state.terminal(),
        Some(TerminalState::CompositeFallback {
            candidate,
            revision,
            outcome: CompositeOutcome::NotVerified,
            fallback: CompositionFallback::KeepParent,
            ..
        }) if *candidate == composite_candidate && *revision == composite_revision
    ));
    let ledger_after_composite = store
        .cohort_ledger_status(cohort.id, CohortLedger::AdaptivePromotion)
        .unwrap();
    assert_eq!(
        ledger_after_composite.query_used,
        ledger_after_children.query_used + composite_report.ledger.query_debit
    );
    assert_eq!(
        ledger_after_composite.error_used_nanos,
        ledger_after_children.error_used_nanos + composite_report.ledger.error_nanos_debit
    );
    assert_eq!(
        store
            .resolve_harness(cohort.target)
            .unwrap()
            .unwrap()
            .revision(),
        cohort.base_revision
    );
    assert_eq!(
        store
            .load_harness_revision(composite_revision)
            .unwrap()
            .digest(),
        composite_revision
    );

    drop(store);
    let mut reopened = Store::open(root.path()).unwrap();
    assert_eq!(reopened.load_campaign(campaign).unwrap(), state);
    assert_eq!(
        reopened.load_campaign(campaign).unwrap().rounds()[0]
            .composite()
            .unwrap()
            .plan()
            .children(),
        plan.children()
    );
    assert!(
        reopened
            .append_campaign_event(
                campaign,
                state.revision(),
                CampaignEvent::RoundVerdictRecorded {
                    round,
                    verdict: RoundVerdict::NoUpdate {
                        basis: RoundVerdictId::from_digest(digest("composite:subset-search")),
                    },
                },
            )
            .is_err()
    );
    assert_eq!(reopened.load_campaign(campaign).unwrap(), state);
}

#[test]
fn conflicting_verified_candidates_terminally_keep_the_parent_across_restart() {
    let reason = exercise_composition_fallback(
        "composition-conflict",
        vec![
            (
                vec!["instructions"],
                json!({"instructions": "Use the first verified instruction."}),
            ),
            (
                vec!["instructions"],
                json!({"instructions": "Use the second verified instruction."}),
            ),
        ],
    );
    assert!(matches!(
        reason,
        CompositionFailureReason::FieldConflict {
            field: CompositeField::Instructions,
            ..
        }
    ));
}

#[test]
fn combined_manifest_failure_terminally_keeps_the_parent_across_restart() {
    let reason = exercise_composition_fallback(
        "composition-ceiling",
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
    assert_eq!(reason, CompositionFailureReason::CombinedManifestRejected);
}

#[test]
fn campaign_replay_rejects_a_missing_composition_child_score_report() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let (campaign, child_score, _) = create_successful_composition(&state_root, "missing-score");

    let connection = Connection::open(state_root.join("v1.sqlite3")).unwrap();
    connection
        .execute_batch("DROP TRIGGER adaptive_score_reports_no_delete;")
        .unwrap();
    connection
        .execute(
            "DELETE FROM adaptive_score_reports WHERE result_id=?1",
            [child_score.to_string()],
        )
        .unwrap();
    drop(connection);

    let store = Store::open(&state_root).unwrap();
    assert!(store.load_campaign(campaign).is_err());
}

#[test]
fn campaign_replay_rejects_a_missing_composite_harness_revision() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let (campaign, _, composite_revision) =
        create_successful_composition(&state_root, "missing-revision");

    let connection = Connection::open(state_root.join("v1.sqlite3")).unwrap();
    connection
        .execute_batch("DROP TRIGGER harness_revisions_no_delete;")
        .unwrap();
    connection
        .execute(
            "DELETE FROM harness_revisions WHERE digest=?1",
            [composite_revision.to_string()],
        )
        .unwrap();
    drop(connection);

    let store = Store::open(&state_root).unwrap();
    assert!(store.load_campaign(campaign).is_err());
}

#[test]
fn a_protected_metric_regression_is_not_verified_and_cannot_authorize_composition() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let cohort = register_canonical_cohort(&mut store, "protected", 7);
    let parent = store.load_harness_revision(cohort.base_revision).unwrap();
    let proposal = proposal_batch(
        &parent,
        "protected",
        vec![
            (
                vec!["instructions"],
                json!({"instructions": "This candidate has a protected regression."}),
            ),
            (
                vec!["skills"],
                json!({"skills": {"review": "Check protected metrics."}}),
            ),
        ],
    )
    .remove(0);
    let campaign = CampaignId::new();
    let (state, round, candidate) = prepare_scoring_campaign_with_proposal(
        &mut store,
        campaign,
        &cohort,
        "protected",
        proposal.id(),
    );
    let dataset = dataset(
        &cohort,
        campaign,
        candidate,
        "protected regression",
        Scores {
            candidate_safety: 500_000,
            ..Scores::canonical()
        },
        EvidenceVariation::default(),
    );

    let (state, report) = store
        .record_adaptive_score(campaign, state.revision(), round, dataset, coordinates())
        .unwrap();

    assert_eq!(report.verdict, AdaptiveVerdict::NotVerified);
    assert_eq!(report.ledger.query_debit, 2);
    assert_gate(
        &report,
        "protected_noninferiority:safety:all",
        GateStatus::NotVerified,
    );
    assert!(matches!(
        store.compose_verified_candidate(
            campaign,
            state.revision(),
            round,
            CompositionInput::new(candidate, report.result_id().unwrap(), &proposal),
        ),
        Err(StoreError::Invalid(
            "composition must include every and only verified candidate"
        ))
    ));
}

#[test]
fn an_attributable_behavioral_failure_is_negative_evidence_not_an_unknown() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let cohort = register_canonical_cohort(&mut store, "behavioral", 7);
    let campaign = CampaignId::new();
    let (state, round, candidate) =
        prepare_scoring_campaign(&mut store, campaign, &cohort, "behavioral");
    let dataset = dataset(
        &cohort,
        campaign,
        candidate,
        "behavioral failure",
        Scores::canonical(),
        EvidenceVariation {
            candidate_failure_case: Some(0),
            ..EvidenceVariation::default()
        },
    );

    let (_, report) = store
        .record_adaptive_score(campaign, state.revision(), round, dataset, coordinates())
        .unwrap();

    assert_eq!(report.verdict, AdaptiveVerdict::NotVerified);
    assert_gate(&report, "data_completeness", GateStatus::Verified);
    assert_gate(&report, "correctness", GateStatus::NotVerified);
    assert_gate(&report, "critical_cases", GateStatus::NotVerified);
}

#[test]
fn partial_unknown_drifted_and_underpowered_evidence_are_all_inconclusive() {
    let variations = [
        (
            "partial",
            7,
            EvidenceVariation {
                omit: Some((0, 0)),
                ..EvidenceVariation::default()
            },
            "data_completeness",
        ),
        (
            "unknown",
            7,
            EvidenceVariation {
                unknown: Some((0, 0)),
                ..EvidenceVariation::default()
            },
            "data_completeness",
        ),
        (
            "drift",
            7,
            EvidenceVariation {
                drift: Some((0, 0)),
                ..EvidenceVariation::default()
            },
            "pairing_integrity",
        ),
        (
            "underpowered",
            8,
            EvidenceVariation::default(),
            "minimum_power",
        ),
    ];

    for (namespace, minimum_blocks, variation, expected_gate) in variations {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path()).unwrap();
        let cohort = register_canonical_cohort(&mut store, namespace, minimum_blocks);
        let campaign = CampaignId::new();
        let (state, round, candidate) =
            prepare_scoring_campaign(&mut store, campaign, &cohort, namespace);
        let evidence = dataset(
            &cohort,
            campaign,
            candidate,
            namespace,
            Scores::canonical(),
            variation,
        );

        let (_, report) = store
            .record_adaptive_score(campaign, state.revision(), round, evidence, coordinates())
            .unwrap();

        assert_eq!(report.verdict, AdaptiveVerdict::Inconclusive, "{namespace}");
        assert_gate(&report, expected_gate, GateStatus::Inconclusive);
        assert_eq!(report.ledger.query_debit, 2);
    }
}

#[test]
fn dataset_construction_rejects_duplicate_pairs_and_final_audit_evidence() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let cohort = register_canonical_cohort(&mut store, "construction", 7);
    let campaign = CampaignId::new();
    let candidate = candidate_id("construction:candidate");
    let valid = dataset(
        &cohort,
        campaign,
        candidate,
        "construction",
        Scores::canonical(),
        EvidenceVariation::default(),
    );
    let duplicate = valid.pairs()[0].clone();

    assert!(matches!(
        AdaptivePromotionDataset::new(
            cohort.id,
            valid.partition(),
            candidate,
            CandidateLabel::new("duplicate").unwrap(),
            vec![duplicate.clone(), duplicate],
            hard_gates(),
        ),
        Err(ScoringError::DuplicatePair)
    ));
    assert!(matches!(
        AdaptivePromotionDataset::new(
            cohort.id,
            TrialPartition::new(
                TrialLedgerRole::FinalAudit,
                1,
                cohort.partitions.final_audit,
            )
            .unwrap(),
            candidate,
            CandidateLabel::new("wrong role").unwrap(),
            valid.pairs().to_vec(),
            hard_gates(),
        ),
        Err(ScoringError::WrongEvidenceRole)
    ));
}

#[test]
fn scoring_errors_roll_back_the_report_coordinate_ledger_and_campaign_state() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let cohort = register_canonical_cohort(&mut store, "rollback", 7);
    let campaign = CampaignId::new();
    let (state, round, candidate) =
        prepare_scoring_campaign(&mut store, campaign, &cohort, "rollback");
    let valid = dataset(
        &cohort,
        campaign,
        candidate,
        "rollback",
        Scores::canonical(),
        EvidenceVariation::default(),
    );
    let mut gates = valid.hard_gate_evidence().to_vec();
    gates.push(GateEvidence {
        name: GateName::new("undeclared").unwrap(),
        status: GateStatus::Verified,
        reason: "must fail closed".into(),
    });
    let invalid = AdaptivePromotionDataset::new(
        cohort.id,
        valid.partition(),
        candidate,
        CandidateLabel::new("rollback invalid").unwrap(),
        valid.pairs().to_vec(),
        gates,
    )
    .unwrap();
    let ledger_before = store
        .cohort_ledger_status(cohort.id, CohortLedger::AdaptivePromotion)
        .unwrap();

    assert!(matches!(
        store.record_adaptive_score(campaign, state.revision(), round, invalid, coordinates(),),
        Err(StoreError::Scoring(ScoringError::UndeclaredGate))
    ));
    assert_eq!(store.load_campaign(campaign).unwrap(), state);
    assert_eq!(
        store
            .cohort_ledger_status(cohort.id, CohortLedger::AdaptivePromotion)
            .unwrap(),
        ledger_before
    );

    let (scored, report) = store
        .record_adaptive_score(campaign, state.revision(), round, valid, coordinates())
        .unwrap();
    assert_eq!(scored.revision(), state.revision() + 1);
    assert_eq!(report.verdict, AdaptiveVerdict::Verified);
}

#[test]
fn decision_coordinates_are_unique_across_campaigns_in_the_same_cohort() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let cohort = register_canonical_cohort(&mut store, "coordinate", 7);
    let first_campaign = CampaignId::new();
    let second_campaign = CampaignId::new();
    let (first_state, first_round, first_candidate) =
        prepare_scoring_campaign(&mut store, first_campaign, &cohort, "coordinate:first");
    let (second_state, second_round, second_candidate) =
        prepare_scoring_campaign(&mut store, second_campaign, &cohort, "coordinate:second");
    let first_dataset = dataset(
        &cohort,
        first_campaign,
        first_candidate,
        "first campaign",
        Scores::canonical(),
        EvidenceVariation::default(),
    );
    let second_dataset = dataset(
        &cohort,
        second_campaign,
        second_candidate,
        "second campaign",
        Scores::canonical(),
        EvidenceVariation::default(),
    );
    store
        .record_adaptive_score(
            first_campaign,
            first_state.revision(),
            first_round,
            first_dataset,
            coordinates(),
        )
        .unwrap();
    let ledger_after_first = store
        .cohort_ledger_status(cohort.id, CohortLedger::AdaptivePromotion)
        .unwrap();

    assert!(matches!(
        store.record_adaptive_score(
            second_campaign,
            second_state.revision(),
            second_round,
            second_dataset,
            coordinates(),
        ),
        Err(StoreError::Invalid(
            "adaptive scoring decision coordinates were already used"
        ))
    ));
    assert_eq!(store.load_campaign(second_campaign).unwrap(), second_state);
    assert_eq!(
        store
            .cohort_ledger_status(cohort.id, CohortLedger::AdaptivePromotion)
            .unwrap(),
        ledger_after_first
    );
}

#[test]
fn an_exhausted_store_global_ledger_records_an_inconclusive_zero_debit_report() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let (cohort, campaign, state, round, evidence) = {
        let mut store = Store::open(&state_root).unwrap();
        let cohort = register_canonical_cohort(&mut store, "exhausted", 7);
        let campaign = CampaignId::new();
        let (state, round, candidate) =
            prepare_scoring_campaign(&mut store, campaign, &cohort, "exhausted");
        let evidence = dataset(
            &cohort,
            campaign,
            candidate,
            "exhausted",
            Scores::canonical(),
            EvidenceVariation::default(),
        );
        (cohort, campaign, state, round, evidence)
    };
    let connection = Connection::open(state_root.join("v1.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE cohort_ledgers
             SET query_used=query_limit,error_used_nanos=error_limit_nanos
             WHERE cohort=?1 AND role='adaptive_promotion'",
            params![cohort.id.to_string()],
        )
        .unwrap();
    drop(connection);

    let mut store = Store::open(&state_root).unwrap();
    let (scored, report) = store
        .record_adaptive_score(campaign, state.revision(), round, evidence, coordinates())
        .unwrap();

    assert_eq!(scored.revision(), state.revision() + 1);
    assert_eq!(report.verdict, AdaptiveVerdict::Inconclusive);
    assert_eq!(report.ledger.query_debit, 0);
    assert_eq!(report.ledger.error_nanos_debit, 0);
    assert_eq!(report.ledger.before, report.ledger.after);
    assert_gate(&report, "ledger_capacity", GateStatus::Inconclusive);
    assert_eq!(report.ledger.after.query_used, FAMILY_SIZE);
    assert_eq!(report.ledger.after.error_used_nanos, 1_000_000_000);
    assert_eq!(
        store
            .load_adaptive_score_report(report.result_id().unwrap())
            .unwrap(),
        report
    );
}

#[test]
fn candidate_labels_and_input_order_do_not_change_the_content_addressed_score() {
    let first_root = tempfile::tempdir().unwrap();
    let second_root = tempfile::tempdir().unwrap();
    let mut first = Store::open(first_root.path()).unwrap();
    let mut second = Store::open(second_root.path()).unwrap();
    let cohort = register_canonical_cohort(&mut first, "canonical-order", 7);
    let second_binding = second.register_supported_target(cohort.target).unwrap();
    assert_eq!(second_binding.revision(), cohort.base_revision);
    second.register_evaluation_cohort(&cohort).unwrap();
    let campaign = CampaignId::new();
    let (first_state, round, candidate) =
        prepare_scoring_campaign(&mut first, campaign, &cohort, "canonical-order");
    let (second_state, second_round, second_candidate) =
        prepare_scoring_campaign(&mut second, campaign, &cohort, "canonical-order");
    assert_eq!((second_round, second_candidate), (round, candidate));
    let original = dataset(
        &cohort,
        campaign,
        candidate,
        "human label one",
        Scores::canonical(),
        EvidenceVariation::default(),
    );
    let mut reversed_pairs = original.pairs().to_vec();
    reversed_pairs.reverse();
    let mut reversed_gates = original.hard_gate_evidence().to_vec();
    reversed_gates.reverse();
    let reordered = AdaptivePromotionDataset::new(
        cohort.id,
        original.partition(),
        candidate,
        CandidateLabel::new("a completely different human label").unwrap(),
        reversed_pairs,
        reversed_gates,
    )
    .unwrap();

    let (_, first_report) = first
        .record_adaptive_score(
            campaign,
            first_state.revision(),
            round,
            original,
            coordinates(),
        )
        .unwrap();
    let (_, second_report) = second
        .record_adaptive_score(
            campaign,
            second_state.revision(),
            round,
            reordered,
            coordinates(),
        )
        .unwrap();

    assert_eq!(first_report.evidence_root, second_report.evidence_root);
    assert_eq!(first_report, second_report);
    assert_eq!(
        first_report.result_id().unwrap(),
        second_report.result_id().unwrap()
    );
}

fn register_canonical_cohort(
    store: &mut Store,
    namespace: &str,
    minimum_complete_blocks: u32,
) -> EvaluationCohortSpec {
    let target = TargetProfile::new(
        ModelIdentity::from_digest(digest(&format!("{namespace}:model"))),
        ProtocolIdentity::from_digest(digest(&format!("{namespace}:protocol"))),
        EnvironmentIdentity::from_digest(digest(&format!("{namespace}:environment"))),
        TaskProfileIdentity::from_digest(digest(&format!("{namespace}:task-profile"))),
        Channel::Canary,
    );
    let binding = store.register_supported_target(target).unwrap();
    let blocks = (0..7)
        .map(|index| IndependentBlock {
            id: block_id(index),
            cases: vec![EvaluationCase {
                id: case_id(index),
                task: TaskIdentity::from_digest(digest(&format!("task:{index}"))),
                repeats: 2,
            }],
        })
        .collect::<Vec<_>>();
    let cases = blocks
        .iter()
        .flat_map(|block| block.cases.iter().map(|case| case.id))
        .collect::<Vec<_>>();
    let scoring = FrozenScoringPolicy {
        schema_version: 1,
        estimator_version: "paired-case-block-exact-sign-v1".into(),
        calibration: "synthetic-calibration-v1".into(),
        critical_cases: BTreeSet::from([case_id(0)]),
        strata: vec![StratumPolicy {
            name: StratumName::new("all").unwrap(),
            cases,
        }],
        metrics: vec![
            MetricPolicy {
                name: MetricName::new("quality").unwrap(),
                kind: MetricKind::Primary,
                margin: MetricScore::from_millionths(50_000).unwrap(),
            },
            MetricPolicy {
                name: MetricName::new("safety").unwrap(),
                kind: MetricKind::Protected,
                margin: MetricScore::from_millionths(100_000).unwrap(),
            },
        ],
        minimum_complete_blocks,
        maximum_exact_blocks: 20,
        required_hard_gates: hard_gate_names(),
        multiplicity_family: MultiplicityFamily {
            candidates: 2,
            rounds: 2,
            metrics: 2,
            strata: 1,
            composites: 2,
            fallbacks: 2,
            campaigns: 2,
            activation_attempts: 2,
        },
        adaptive_error_nanos_per_hypothesis: ERROR_NANOS_PER_HYPOTHESIS,
        final_error_nanos_per_hypothesis: ERROR_NANOS_PER_HYPOTHESIS,
    };
    let cohort = EvaluationCohortSpec {
        id: CohortId::new(),
        target,
        base_revision: binding.revision(),
        evaluator: EvaluatorIdentity::from_digest(digest(&format!("{namespace}:evaluator"))),
        policy: binding.policy(),
        partitions: PartitionCommitments {
            mining: PartitionCommitment::from_digest(digest(&format!(
                "{namespace}:mining-partition"
            ))),
            adaptive_promotion: PartitionCommitment::from_digest(digest(&format!(
                "{namespace}:adaptive-partition"
            ))),
            final_audit: PartitionCommitment::from_digest(digest(&format!(
                "{namespace}:final-partition"
            ))),
        },
        blocks,
        adaptive_promotion: LedgerLimit::new(FAMILY_SIZE, 1_000_000_000),
        final_audit: LedgerLimit::new(FAMILY_SIZE, 1_000_000_000),
        audit_epoch: AuditEpochId::new(),
        scoring,
    };
    store.register_evaluation_cohort(&cohort).unwrap();
    cohort
}

fn proposal_batch(
    parent: &ValidatedHarnessRevision,
    namespace: &str,
    patches: Vec<(Vec<&str>, Value)>,
) -> Vec<BoundedProposal> {
    let request = ProposalRequest::new(
        parent.digest(),
        MiningBundleRoot::from_digest(digest(&format!("{namespace}:proposal:mining"))),
        parent.policy_identity(),
        ModelIdentity::from_digest(digest(&format!("{namespace}:proposal:model"))),
        ProtocolIdentity::from_digest(digest(&format!("{namespace}:proposal:protocol"))),
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
                    "{namespace}:proposal:receipt:{index}"
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

fn prepare_verified_composition_round(
    store: &mut Store,
    cohort: &EvaluationCohortSpec,
    namespace: &str,
    patches: Vec<(Vec<&str>, Value)>,
) -> (
    CampaignId,
    CampaignState,
    RoundId,
    [CandidateId; 2],
    Vec<BoundedProposal>,
    [ScoreResultId; 2],
) {
    let parent = store.load_harness_revision(cohort.base_revision).unwrap();
    let proposals = proposal_batch(&parent, namespace, patches);
    let campaign = CampaignId::new();
    let round = RoundId::from_digest(digest(&format!("{namespace}:round")));
    let candidates = [
        candidate_id(&format!("{namespace}:candidate:a")),
        candidate_id(&format!("{namespace}:candidate:b")),
    ];
    let mut state = store
        .create_campaign(CampaignEvent::Started {
            campaign,
            cohort: cohort.id,
            base_revision: cohort.base_revision,
            policy: cohort.policy,
            budget: CampaignBudget::new(32, ResourceUsage::new(100, 1_000, 10_000)).unwrap(),
        })
        .unwrap();
    for event in [
        CampaignEvent::RoundStarted { round },
        CampaignEvent::MiningCompleted {
            round,
            result: MiningResultId::from_digest(digest(&format!("{namespace}:mining"))),
        },
        CampaignEvent::CandidateProposed {
            round,
            candidate: candidates[0],
            parent: CandidateParent::BaseRevision(cohort.base_revision),
            proposal: proposals[0].id(),
        },
        CampaignEvent::CandidateProposed {
            round,
            candidate: candidates[1],
            parent: CandidateParent::BaseRevision(cohort.base_revision),
            proposal: proposals[1].id(),
        },
        CampaignEvent::ProposalsCompleted { round },
        CampaignEvent::CandidateTrialRecorded {
            round,
            candidate: candidates[0],
            trial: TrialResultId::from_digest(digest(&format!("{namespace}:trial:a"))),
        },
        CampaignEvent::CandidateTrialRecorded {
            round,
            candidate: candidates[1],
            trial: TrialResultId::from_digest(digest(&format!("{namespace}:trial:b"))),
        },
        CampaignEvent::TrialsCompleted { round },
    ] {
        state = store
            .append_campaign_event(campaign, state.revision(), event)
            .unwrap();
    }

    let mut scores = [ScoreResultId::from_digest(digest("unset")); 2];
    for index in 0..2 {
        let (next, report) = store
            .record_adaptive_score(
                campaign,
                state.revision(),
                round,
                dataset(
                    cohort,
                    campaign,
                    candidates[index],
                    &format!("{namespace} candidate {index}"),
                    Scores::canonical(),
                    EvidenceVariation::default(),
                ),
                coordinates_at(index as u64, 0),
            )
            .unwrap();
        assert_eq!(report.verdict, AdaptiveVerdict::Verified);
        scores[index] = report.result_id().unwrap();
        state = next;
    }
    (campaign, state, round, candidates, proposals, scores)
}

fn exercise_composition_fallback(
    namespace: &str,
    patches: Vec<(Vec<&str>, Value)>,
) -> CompositionFailureReason {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let mut store = Store::open(&state_root).unwrap();
    let cohort = register_canonical_cohort(&mut store, namespace, 7);
    let (campaign, state, round, candidates, proposals, scores) =
        prepare_verified_composition_round(&mut store, &cohort, namespace, patches);
    let inputs = [
        CompositionInput::new(candidates[0], scores[0], &proposals[0]),
        CompositionInput::new(candidates[1], scores[1], &proposals[1]),
    ];
    let (terminal, resolution) = store
        .compose_verified_candidates(campaign, state.revision(), round, &inputs)
        .unwrap();
    let VerifiedComposition::FellBack(failure) = resolution else {
        panic!("deterministic invalid merge must keep the parent");
    };
    assert_eq!(terminal.phase(), CampaignPhase::Terminal);
    assert_eq!(failure.parent(), cohort.base_revision);
    assert_eq!(failure.fallback(), CompositionFallback::KeepParent);
    assert_eq!(
        failure
            .children()
            .iter()
            .map(|child| child.candidate())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(candidates)
    );
    assert!(matches!(
        terminal.terminal(),
        Some(TerminalState::CompositionFallback {
            round: failed_round,
            failure: stored,
        }) if *failed_round == round && stored == &failure
    ));
    assert_eq!(
        store
            .resolve_harness(cohort.target)
            .unwrap()
            .unwrap()
            .revision(),
        cohort.base_revision
    );

    drop(store);
    let mut reopened = Store::open(&state_root).unwrap();
    assert_eq!(reopened.load_campaign(campaign).unwrap(), terminal);
    assert!(
        reopened
            .append_campaign_event(
                campaign,
                terminal.revision(),
                CampaignEvent::RoundVerdictRecorded {
                    round,
                    verdict: RoundVerdict::NoUpdate {
                        basis: RoundVerdictId::from_digest(digest(&format!(
                            "{namespace}:terminal"
                        ))),
                    },
                },
            )
            .is_err()
    );
    failure.reason()
}

fn create_successful_composition(
    state_root: &std::path::Path,
    namespace: &str,
) -> (CampaignId, ScoreResultId, Digest) {
    let mut store = Store::open(state_root).unwrap();
    let cohort = register_canonical_cohort(&mut store, namespace, 7);
    let (campaign, state, round, candidates, proposals, scores) =
        prepare_verified_composition_round(
            &mut store,
            &cohort,
            namespace,
            vec![
                (
                    vec!["instructions"],
                    json!({"instructions": "Compose this verified instruction."}),
                ),
                (
                    vec!["skills"],
                    json!({"skills": {"review": "Compose this verified skill."}}),
                ),
            ],
        );
    let inputs = [
        CompositionInput::new(candidates[0], scores[0], &proposals[0]),
        CompositionInput::new(candidates[1], scores[1], &proposals[1]),
    ];
    let (_, resolution) = store
        .compose_verified_candidates(campaign, state.revision(), round, &inputs)
        .unwrap();
    let VerifiedComposition::Composed(composed) = resolution else {
        panic!("disjoint verified candidates must compose");
    };
    (campaign, scores[0], composed.plan().revision())
}

fn prepare_scoring_campaign(
    store: &mut Store,
    campaign: CampaignId,
    cohort: &EvaluationCohortSpec,
    namespace: &str,
) -> (CampaignState, RoundId, CandidateId) {
    prepare_scoring_campaign_with_proposal(
        store,
        campaign,
        cohort,
        namespace,
        ProposalId::from_digest(digest(&format!("{namespace}:proposal"))),
    )
}

fn prepare_scoring_campaign_with_proposal(
    store: &mut Store,
    campaign: CampaignId,
    cohort: &EvaluationCohortSpec,
    namespace: &str,
    proposal: ProposalId,
) -> (CampaignState, RoundId, CandidateId) {
    let round = RoundId::from_digest(digest(&format!("{namespace}:round")));
    let candidate = candidate_id(&format!("{namespace}:candidate"));
    let mut state = store
        .create_campaign(CampaignEvent::Started {
            campaign,
            cohort: cohort.id,
            base_revision: cohort.base_revision,
            policy: cohort.policy,
            budget: CampaignBudget::new(32, ResourceUsage::new(100, 1_000, 10_000)).unwrap(),
        })
        .unwrap();
    for event in [
        CampaignEvent::RoundStarted { round },
        CampaignEvent::MiningCompleted {
            round,
            result: MiningResultId::from_digest(digest(&format!("{namespace}:mining"))),
        },
        CampaignEvent::CandidateProposed {
            round,
            candidate,
            parent: CandidateParent::BaseRevision(cohort.base_revision),
            proposal,
        },
        CampaignEvent::ProposalsCompleted { round },
        CampaignEvent::CandidateTrialRecorded {
            round,
            candidate,
            trial: TrialResultId::from_digest(digest(&format!("{namespace}:trial"))),
        },
        CampaignEvent::TrialsCompleted { round },
    ] {
        state = store
            .append_campaign_event(campaign, state.revision(), event)
            .unwrap();
    }
    (state, round, candidate)
}

fn dataset(
    cohort: &EvaluationCohortSpec,
    campaign: CampaignId,
    candidate: CandidateId,
    label: &str,
    scores: Scores,
    variation: EvidenceVariation,
) -> AdaptivePromotionDataset {
    let partition = TrialPartition::new(
        TrialLedgerRole::AdaptivePromotion,
        1,
        cohort.partitions.adaptive_promotion,
    )
    .unwrap();
    let runtime = TrialRuntimeIdentity::new(
        cohort.target.model,
        cohort.target.protocol,
        cohort.evaluator,
        cohort.target.environment,
    );
    let mut pairs = Vec::new();
    for (case_index, block) in cohort.blocks.iter().enumerate() {
        for repeat in 0..block.cases[0].repeats {
            if variation.omit == Some((case_index, repeat)) {
                continue;
            }
            let pair_block = if variation.drift == Some((case_index, repeat)) {
                IndependentBlockId::from_digest(digest("drifted:block"))
            } else {
                block.id
            };
            let context =
                TrialContext::new(campaign, candidate, partition, runtime, Limits::default())
                    .unwrap();
            let spec = TrialPairSpec::new(
                context,
                pair_block,
                block.cases[0].id,
                repeat,
                InputCommitment::from_digest(digest(&format!("input:{case_index}:{repeat}"))),
            )
            .unwrap();
            let dispatch = prepare_trial_dispatch(cohort.target, cohort.evaluator, spec).unwrap();
            let parent_run = *dispatch
                .runs()
                .iter()
                .find(|run| run.key().side() == TrialSide::Parent)
                .unwrap();
            let candidate_run = *dispatch
                .runs()
                .iter()
                .find(|run| run.key().side() == TrialSide::Candidate)
                .unwrap();
            let effect = EffectId::derive(
                campaign,
                17,
                EffectKind::Trial,
                EffectWorkId::from_digest(digest(&format!("effect:{case_index}:{repeat}"))),
            );
            let parent = IsolatedRunReceipt::collected(
                &parent_run,
                effect,
                LeaseEpoch::new(1).unwrap(),
                passing_outcome(scores.parent_quality, scores.parent_safety),
                digest(&format!("parent-receipt:{case_index}:{repeat}")),
            )
            .unwrap();
            let candidate_outcome = if variation.candidate_failure_case == Some(case_index) {
                TerminalTrialOutcome::behavioral_failure(
                    BehavioralFailureReason::AttributableCandidateCrash,
                )
            } else {
                passing_outcome(scores.candidate_quality, scores.candidate_safety)
            };
            let candidate_receipt = IsolatedRunReceipt::collected(
                &candidate_run,
                effect,
                LeaseEpoch::new(1).unwrap(),
                candidate_outcome,
                digest(&format!("candidate-receipt:{case_index}:{repeat}")),
            )
            .unwrap();
            let evidence = if variation.unknown == Some((case_index, repeat)) {
                classify_paired_trial(spec, Some(&parent), None).unwrap()
            } else {
                classify_paired_trial(spec, Some(&parent), Some(&candidate_receipt)).unwrap()
            };
            pairs.push(AdaptiveTrialEvidence { spec, evidence });
        }
    }
    AdaptivePromotionDataset::new(
        cohort.id,
        partition,
        candidate,
        CandidateLabel::new(label).unwrap(),
        pairs,
        hard_gates(),
    )
    .unwrap()
}

fn passing_outcome(quality: u32, safety: u32) -> TerminalTrialOutcome {
    TerminalTrialOutcome::pass(BTreeMap::from([
        (
            MetricName::new("quality").unwrap(),
            MetricScore::from_millionths(quality).unwrap(),
        ),
        (
            MetricName::new("safety").unwrap(),
            MetricScore::from_millionths(safety).unwrap(),
        ),
    ]))
}

fn hard_gate_names() -> Vec<GateName> {
    [
        "policy",
        "provenance",
        "secrecy",
        "cost",
        "latency",
        "resource_ceilings",
    ]
    .into_iter()
    .map(|name| GateName::new(name).unwrap())
    .collect()
}

fn hard_gates() -> Vec<GateEvidence> {
    hard_gate_names()
        .into_iter()
        .map(|name| GateEvidence {
            reason: format!("{} gate passed", name.as_str()),
            name,
            status: GateStatus::Verified,
        })
        .collect()
}

fn coordinates() -> DecisionCoordinates {
    coordinates_at(0, 0)
}

fn coordinates_at(candidate: u64, composite: u64) -> DecisionCoordinates {
    DecisionCoordinates {
        candidate,
        round: 0,
        composite,
        fallback: 0,
        campaign: 0,
        activation_attempt: 0,
    }
}

fn assert_gate(report: &orvek_harness::AdaptiveScoreReport, name: &str, status: GateStatus) {
    assert_eq!(
        report
            .gates
            .iter()
            .find(|gate| gate.name == name)
            .map(|gate| gate.status),
        Some(status),
        "gate {name}"
    );
}

fn digest(value: &str) -> Digest {
    Digest::of(value.as_bytes())
}

fn case_id(index: usize) -> CaseIdentity {
    CaseIdentity::from_digest(digest(&format!("case:{index}")))
}

fn block_id(index: usize) -> IndependentBlockId {
    IndependentBlockId::from_digest(digest(&format!("block:{index}")))
}

fn candidate_id(value: &str) -> CandidateId {
    CandidateId::from_digest(digest(value))
}
