use orvek_harness::{
    AdaptivePromotionDataset, AdaptiveVerdict, AuditEpochId, CampaignBudget, CampaignEvent,
    CampaignId, CandidateId, CandidateLabel, CandidateParent, CaseIdentity, Channel, CohortId,
    CohortLedger, DecisionCoordinates, Digest, EffectAccounting, EffectBudget, EffectIntent,
    EffectKind, EffectOutcome, EffectOutputId, EffectReceipt, EffectReceiptId,
    EffectReconciliation, EffectUncertaintyId, EffectWorkId, EnvironmentIdentity, EvaluationCase,
    EvaluationCohortSpec, EvaluatorIdentity, FrozenScoringPolicy, GateEvidence, GateName,
    GateStatus, IndependentBlock, IndependentBlockId, LeaseEpoch, LedgerLimit, MetricKind,
    MetricName, MetricPolicy, MetricScore, MiningResultId, ModelIdentity, MultiplicityFamily,
    PartitionCommitment, PartitionCommitments, ProposalId, ProtocolIdentity, ResourceUsage,
    RoundId, RoundVerdict, RoundVerdictId, Store, StoreError, StratumName, StratumPolicy,
    TargetProfile, TaskIdentity, TaskProfileIdentity, TrialLedgerRole, TrialPartition,
    TrialResultId,
};
use rusqlite::{Connection, params};
use std::collections::BTreeSet;

#[test]
fn campaign_replays_after_restart_and_adaptive_score_spend_is_atomic() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let (expected, campaign, cohort) = {
        let mut store = Store::open(&state_root).unwrap();
        let cohort = register_cohort(&mut store, "durable");
        let campaign = CampaignId::new();
        let mut state = store
            .create_campaign(CampaignEvent::Started {
                campaign,
                cohort: cohort.id,
                base_revision: cohort.base_revision,
                policy: cohort.policy,
                budget: CampaignBudget::new(32, ResourceUsage::new(100, 1_000, 10_000)).unwrap(),
            })
            .unwrap();

        let round = round_id("round");
        let candidate = candidate_id("candidate");
        for event in [
            CampaignEvent::RoundStarted { round },
            CampaignEvent::MiningCompleted {
                round,
                result: MiningResultId::from_digest(digest("mining")),
            },
            CampaignEvent::CandidateProposed {
                round,
                candidate,
                parent: CandidateParent::BaseRevision(cohort.base_revision),
                proposal: ProposalId::from_digest(digest("proposal")),
            },
            CampaignEvent::ProposalsCompleted { round },
            CampaignEvent::CandidateTrialRecorded {
                round,
                candidate,
                trial: TrialResultId::from_digest(digest("trial")),
            },
            CampaignEvent::TrialsCompleted { round },
        ] {
            state = store
                .append_campaign_event(campaign, state.revision(), event)
                .unwrap();
        }

        assert!(matches!(
            store.append_campaign_event(
                campaign,
                state.revision(),
                CampaignEvent::CandidateScoreRecorded {
                    round,
                    candidate,
                    score: orvek_harness::ScoreResultId::from_digest(digest("fabricated-score")),
                },
            ),
            Err(StoreError::Invalid(
                "candidate score requires typed adaptive scoring"
            ))
        ));
        let dataset = adaptive_dataset(&cohort, candidate, "durable");
        let (scored, report) = store
            .record_adaptive_score(
                campaign,
                state.revision(),
                round,
                dataset.clone(),
                coordinates(0),
            )
            .unwrap();
        state = scored;
        assert_eq!(report.verdict, AdaptiveVerdict::Inconclusive);
        assert_eq!(
            store
                .load_adaptive_score_report(report.result_id().unwrap())
                .unwrap(),
            report
        );
        let adaptive = store
            .cohort_ledger_status(cohort.id, CohortLedger::AdaptivePromotion)
            .unwrap();
        assert_eq!((adaptive.query_used, adaptive.error_used_nanos), (1, 10));

        assert!(matches!(
            store.append_campaign_event(
                campaign,
                state.revision(),
                CampaignEvent::RoundVerdictRecorded {
                    round,
                    verdict: RoundVerdict::Compose {
                        candidate,
                        basis: RoundVerdictId::from_digest(digest("unverified-composition")),
                    },
                },
            ),
            Err(StoreError::Invalid(
                "composition requires typed verified candidate authority"
            ))
        ));

        assert!(matches!(
            store.record_adaptive_score(
                campaign,
                state.revision() - 1,
                round,
                dataset,
                coordinates(1),
            ),
            Err(StoreError::CampaignRevision { .. })
        ));
        let adaptive = store
            .cohort_ledger_status(cohort.id, CohortLedger::AdaptivePromotion)
            .unwrap();
        assert_eq!((adaptive.query_used, adaptive.error_used_nanos), (1, 10));
        state = store
            .append_campaign_event(
                campaign,
                state.revision(),
                CampaignEvent::RoundVerdictRecorded {
                    round,
                    verdict: RoundVerdict::Inconclusive {
                        basis: RoundVerdictId::from_digest(digest("round-verdict")),
                    },
                },
            )
            .unwrap();
        (state, campaign, cohort.id)
    };

    let reopened = Store::open(&state_root).unwrap();
    assert_eq!(reopened.load_campaign(campaign).unwrap(), expected);
    let adaptive = reopened
        .cohort_ledger_status(cohort, CohortLedger::AdaptivePromotion)
        .unwrap();
    assert_eq!((adaptive.query_used, adaptive.error_used_nanos), (1, 10));
}

#[test]
fn settled_effect_retry_is_idempotent_in_the_durable_journal() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let mut store = Store::open(&state_root).unwrap();
    let cohort = register_cohort(&mut store, "effect");
    let campaign = CampaignId::new();
    let mut state = store.create_campaign(started(campaign, &cohort)).unwrap();
    state = store
        .append_campaign_event(
            campaign,
            state.revision(),
            CampaignEvent::RoundStarted {
                round: round_id("effect-round"),
            },
        )
        .unwrap();
    let intent = EffectIntent::new(
        campaign,
        state.revision() + 1,
        EffectKind::Mine,
        EffectWorkId::from_digest(digest("work")),
        EffectBudget::new(ResourceUsage::new(5, 50, 500)).unwrap(),
    )
    .unwrap();
    let effect = intent.id();
    state = store
        .append_campaign_event(
            campaign,
            state.revision(),
            CampaignEvent::EffectIntended { intent },
        )
        .unwrap();
    state = store
        .append_campaign_event(
            campaign,
            state.revision(),
            CampaignEvent::EffectLeased {
                effect,
                epoch: LeaseEpoch::initial(),
            },
        )
        .unwrap();
    let receipt = EffectReceipt {
        id: EffectReceiptId::from_digest(digest("receipt")),
        outcome: EffectOutcome::Succeeded {
            output: EffectOutputId::from_digest(digest("output")),
        },
        accounting: EffectAccounting::Known(ResourceUsage::new(2, 20, 200)),
    };
    let settled = store
        .append_campaign_event(
            campaign,
            state.revision(),
            CampaignEvent::EffectSettled {
                effect,
                epoch: LeaseEpoch::initial(),
                receipt,
            },
        )
        .unwrap();
    let replayed = store
        .append_campaign_event(
            campaign,
            settled.revision(),
            CampaignEvent::EffectSettled {
                effect,
                epoch: LeaseEpoch::initial(),
                receipt,
            },
        )
        .unwrap();
    assert_eq!(replayed, settled);

    drop(store);
    let reopened = Store::open(&state_root).unwrap();
    assert_eq!(reopened.load_campaign(campaign).unwrap(), settled);
}

#[test]
fn uncertain_effect_crash_prefixes_replay_and_fence_without_retrying() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let (cohort, campaign, round, before_intent) = {
        let mut store = Store::open(&state_root).unwrap();
        let cohort = register_cohort(&mut store, "crash-prefix");
        let campaign = CampaignId::new();
        let mut state = store.create_campaign(started(campaign, &cohort)).unwrap();
        let round = round_id("crash-prefix-round");
        state = store
            .append_campaign_event(
                campaign,
                state.revision(),
                CampaignEvent::RoundStarted { round },
            )
            .unwrap();
        (cohort, campaign, round, state)
    };

    let (intent, effect, after_intent) = {
        let mut store = Store::open(&state_root).unwrap();
        assert_eq!(store.load_campaign(campaign).unwrap(), before_intent);
        let intent = EffectIntent::new(
            campaign,
            before_intent.revision() + 1,
            EffectKind::Mine,
            EffectWorkId::from_digest(digest("crash-prefix-work")),
            EffectBudget::new(ResourceUsage::new(5, 50, 500)).unwrap(),
        )
        .unwrap();
        let effect = intent.id();
        let state = store
            .append_campaign_event(
                campaign,
                before_intent.revision(),
                CampaignEvent::EffectIntended {
                    intent: intent.clone(),
                },
            )
            .unwrap();
        (intent, effect, state)
    };

    let after_lease = {
        let mut store = Store::open(&state_root).unwrap();
        assert_eq!(store.load_campaign(campaign).unwrap(), after_intent);
        assert_eq!(after_intent.usage().reserved, intent.budget().resources);
        store
            .append_campaign_event(
                campaign,
                after_intent.revision(),
                CampaignEvent::EffectLeased {
                    effect,
                    epoch: LeaseEpoch::initial(),
                },
            )
            .unwrap()
    };

    let fenced = CampaignEvent::EffectReconciled {
        effect,
        leased_epoch: LeaseEpoch::initial(),
        next_epoch: LeaseEpoch::new(2).unwrap(),
        outcome: EffectReconciliation::FencedInfrastructureUnknown {
            receipt: EffectReceipt {
                id: EffectReceiptId::from_digest(digest("crash-prefix-unknown-receipt")),
                outcome: EffectOutcome::InfrastructureUnknown {
                    uncertainty: EffectUncertaintyId::from_digest(digest(
                        "crash-prefix-uncertainty",
                    )),
                },
                accounting: EffectAccounting::ReservationCharged,
            },
        },
    };
    let settled = {
        let mut store = Store::open(&state_root).unwrap();
        assert_eq!(store.load_campaign(campaign).unwrap(), after_lease);
        store
            .append_campaign_event(campaign, after_lease.revision(), fenced.clone())
            .unwrap()
    };
    assert_eq!(settled.usage().reserved, ResourceUsage::default());
    assert_eq!(settled.usage().used, intent.budget().resources);

    let mut reopened = Store::open(&state_root).unwrap();
    assert_eq!(reopened.load_campaign(campaign).unwrap(), settled);
    let replayed = reopened
        .append_campaign_event(campaign, settled.revision(), fenced)
        .unwrap();
    assert_eq!(replayed, settled);
    assert_eq!(reopened.load_campaign(campaign).unwrap(), settled);
    assert_eq!(settled.cohort(), cohort.id);
    assert_eq!(settled.rounds()[0].id(), round);
}

#[test]
fn campaigns_share_one_cohort_ledger_across_restart() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let (cohort, campaigns) = {
        let mut store = Store::open(&state_root).unwrap();
        let cohort = register_cohort(&mut store, "shared-ledger");
        let mut campaigns = Vec::new();
        for (coordinate, namespace) in [(0, "first"), (1, "second")] {
            let campaign = CampaignId::new();
            let (state, round) =
                scored_campaign(&mut store, campaign, &cohort, namespace, coordinate);
            let state = store
                .append_campaign_event(
                    campaign,
                    state.revision(),
                    CampaignEvent::RoundVerdictRecorded {
                        round,
                        verdict: RoundVerdict::NoUpdate {
                            basis: RoundVerdictId::from_digest(digest(&format!(
                                "{namespace}:verdict"
                            ))),
                        },
                    },
                )
                .unwrap();
            campaigns.push((campaign, state));
        }
        (cohort.id, campaigns)
    };

    let reopened = Store::open(&state_root).unwrap();
    for (campaign, expected) in campaigns {
        assert_eq!(reopened.load_campaign(campaign).unwrap(), expected);
    }
    let ledger = reopened
        .cohort_ledger_status(cohort, CohortLedger::AdaptivePromotion)
        .unwrap();
    assert_eq!((ledger.query_used, ledger.error_used_nanos), (2, 20));
}

#[test]
fn corrupted_campaign_hash_fails_closed_on_replay() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let campaign = {
        let mut store = Store::open(&state_root).unwrap();
        let cohort = register_cohort(&mut store, "corruption");
        let campaign = CampaignId::new();
        store.create_campaign(started(campaign, &cohort)).unwrap();
        campaign
    };
    let connection = Connection::open(state_root.join("v1.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE events SET hash=?2 WHERE aggregate=?1 AND kind='campaign' AND revision=1",
            params![campaign.to_string(), "0".repeat(64)],
        )
        .unwrap();
    drop(connection);

    let store = Store::open(&state_root).unwrap();
    assert!(matches!(
        store.load_campaign(campaign),
        Err(StoreError::Integrity("campaign journal hash differs"))
    ));
}

fn started(campaign: CampaignId, cohort: &EvaluationCohortSpec) -> CampaignEvent {
    CampaignEvent::Started {
        campaign,
        cohort: cohort.id,
        base_revision: cohort.base_revision,
        policy: cohort.policy,
        budget: CampaignBudget::new(16, ResourceUsage::new(50, 500, 5_000)).unwrap(),
    }
}

fn digest(value: &str) -> Digest {
    Digest::of(value.as_bytes())
}

fn round_id(value: &str) -> RoundId {
    RoundId::from_digest(digest(value))
}

fn candidate_id(value: &str) -> CandidateId {
    CandidateId::from_digest(digest(value))
}

fn scored_campaign(
    store: &mut Store,
    campaign: CampaignId,
    cohort: &EvaluationCohortSpec,
    namespace: &str,
    coordinate: u64,
) -> (orvek_harness::CampaignState, RoundId) {
    let mut state = store.create_campaign(started(campaign, cohort)).unwrap();
    let round = round_id(&format!("{namespace}:round"));
    let candidate = candidate_id(&format!("{namespace}:candidate"));
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
            proposal: ProposalId::from_digest(digest(&format!("{namespace}:proposal"))),
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
    state = store
        .record_adaptive_score(
            campaign,
            state.revision(),
            round,
            adaptive_dataset(cohort, candidate, namespace),
            coordinates(coordinate),
        )
        .unwrap()
        .0;
    (state, round)
}

fn register_cohort(store: &mut Store, namespace: &str) -> EvaluationCohortSpec {
    let target = TargetProfile::new(
        ModelIdentity::from_digest(digest(&format!("{namespace}:model"))),
        ProtocolIdentity::from_digest(digest(&format!("{namespace}:protocol"))),
        EnvironmentIdentity::from_digest(digest(&format!("{namespace}:environment"))),
        TaskProfileIdentity::from_digest(digest(&format!("{namespace}:task"))),
        Channel::Canary,
    );
    let binding = store.register_supported_target(target).unwrap();
    let case = CaseIdentity::from_digest(digest("case"));
    let cohort = EvaluationCohortSpec {
        id: CohortId::new(),
        target,
        base_revision: binding.revision(),
        evaluator: EvaluatorIdentity::from_digest(digest("evaluator")),
        policy: binding.policy(),
        partitions: PartitionCommitments {
            mining: PartitionCommitment::from_digest(digest("mining-partition")),
            adaptive_promotion: PartitionCommitment::from_digest(digest("adaptive-partition")),
            final_audit: PartitionCommitment::from_digest(digest("final-partition")),
        },
        blocks: vec![IndependentBlock {
            id: IndependentBlockId::from_digest(digest("block")),
            cases: vec![EvaluationCase {
                id: case,
                task: TaskIdentity::from_digest(digest("task")),
                repeats: 2,
            }],
        }],
        adaptive_promotion: LedgerLimit::new(10, 100),
        final_audit: LedgerLimit::new(10, 100),
        scoring: fixture_scoring(case),
        audit_epoch: AuditEpochId::new(),
    };
    store.register_evaluation_cohort(&cohort).unwrap();
    cohort
}

fn fixture_scoring(case: CaseIdentity) -> FrozenScoringPolicy {
    FrozenScoringPolicy {
        schema_version: 1,
        estimator_version: "paired-block-sign-v1".into(),
        calibration: "campaign-fixture-v1".into(),
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

fn adaptive_dataset(
    cohort: &EvaluationCohortSpec,
    candidate: CandidateId,
    namespace: &str,
) -> AdaptivePromotionDataset {
    AdaptivePromotionDataset::new(
        cohort.id,
        TrialPartition::new(
            TrialLedgerRole::AdaptivePromotion,
            1,
            cohort.partitions.adaptive_promotion,
        )
        .unwrap(),
        candidate,
        CandidateLabel::new(namespace).unwrap(),
        Vec::new(),
        vec![GateEvidence {
            name: GateName::new("policy").unwrap(),
            status: GateStatus::Verified,
            reason: "fixture policy gate passed".into(),
        }],
    )
    .unwrap()
}

fn coordinates(candidate: u64) -> DecisionCoordinates {
    DecisionCoordinates {
        candidate,
        round: 0,
        composite: 0,
        fallback: 0,
        campaign: 0,
        activation_attempt: 0,
    }
}
