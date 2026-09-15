//! Phase-13 promotion boundary: certificates, bounded approval, CAS
//! activation with supersession, rollback naming, and the global audit burn.

use orvek_harness::{
    ActivationCertificate, ActivationOutcome, AdaptiveScoreReport, ApprovalDecision, ApprovalId,
    ApprovalRequest, ApprovalState, AuditEpochStatus, AuditReportId, BoundedProposal,
    CampaignEvent, CampaignId, CampaignState, CandidateId, CandidateParent, Channel, Digest,
    EvaluationCohortSpec, FinalAuditDataset, FinalVerdict, MiningResultId, PromotionError,
    RollbackTarget, RoundId, ScoreResultId, Store, TargetProfile, TrialResultId, validate_rollback,
};
use serde_json::json;

fn digest(value: &str) -> Digest {
    Digest::of(value.as_bytes())
}

fn round_id(value: &str) -> RoundId {
    RoundId::from_digest(digest(value))
}

fn candidate_id(value: &str) -> CandidateId {
    CandidateId::from_digest(digest(value))
}

const ERROR_NANOS_PER_HYPOTHESIS: u64 = 7_812_500;
const FAMILY_SIZE: u64 = 128;

fn case_id(index: usize) -> orvek_harness::CaseIdentity {
    orvek_harness::CaseIdentity::from_digest(digest(&format!("case:{index}")))
}

fn block_id(index: usize) -> orvek_harness::IndependentBlockId {
    orvek_harness::IndependentBlockId::from_digest(digest(&format!("block:{index}")))
}

fn hard_gate_names() -> Vec<orvek_harness::GateName> {
    [
        "policy",
        "provenance",
        "secrecy",
        "cost",
        "latency",
        "resource_ceilings",
    ]
    .into_iter()
    .map(|name| orvek_harness::GateName::new(name).unwrap())
    .collect()
}

fn hard_gates() -> Vec<orvek_harness::GateEvidence> {
    hard_gate_names()
        .into_iter()
        .map(|name| orvek_harness::GateEvidence {
            reason: format!("{} gate passed", name.as_str()),
            name,
            status: orvek_harness::GateStatus::Verified,
        })
        .collect()
}

fn register_cohort(store: &mut Store, namespace: &str) -> EvaluationCohortSpec {
    let target = TargetProfile::new(
        orvek_harness::ModelIdentity::from_digest(digest(&format!("{namespace}:model"))),
        orvek_harness::ProtocolIdentity::from_digest(digest(&format!("{namespace}:protocol"))),
        orvek_harness::EnvironmentIdentity::from_digest(digest(&format!(
            "{namespace}:environment"
        ))),
        orvek_harness::TaskProfileIdentity::from_digest(digest(&format!(
            "{namespace}:task-profile"
        ))),
        Channel::Canary,
    );
    let binding = store.register_supported_target(target).unwrap();
    let blocks = (0..7)
        .map(|index| orvek_harness::IndependentBlock {
            id: block_id(index),
            cases: vec![orvek_harness::EvaluationCase {
                id: case_id(index),
                task: orvek_harness::TaskIdentity::from_digest(digest(&format!("task:{index}"))),
                repeats: 2,
            }],
        })
        .collect::<Vec<_>>();
    let cases = blocks
        .iter()
        .flat_map(|block| block.cases.iter().map(|case| case.id))
        .collect::<Vec<_>>();
    let scoring = {
        use std::collections::BTreeSet;
        orvek_harness::FrozenScoringPolicy {
            schema_version: 1,
            estimator_version: "paired-case-block-exact-sign-v1".into(),
            calibration: "synthetic-calibration-v1".into(),
            critical_cases: BTreeSet::from([case_id(0)]),
            strata: vec![orvek_harness::StratumPolicy {
                name: orvek_harness::StratumName::new("all").unwrap(),
                cases,
            }],
            metrics: vec![
                orvek_harness::MetricPolicy {
                    name: orvek_harness::MetricName::new("quality").unwrap(),
                    kind: orvek_harness::MetricKind::Primary,
                    margin: orvek_harness::MetricScore::from_millionths(50_000).unwrap(),
                },
                orvek_harness::MetricPolicy {
                    name: orvek_harness::MetricName::new("safety").unwrap(),
                    kind: orvek_harness::MetricKind::Protected,
                    margin: orvek_harness::MetricScore::from_millionths(100_000).unwrap(),
                },
            ],
            minimum_complete_blocks: 7,
            maximum_exact_blocks: 20,
            required_hard_gates: hard_gate_names(),
            multiplicity_family: orvek_harness::MultiplicityFamily {
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
        }
    };
    let cohort = EvaluationCohortSpec {
        id: orvek_harness::CohortId::new(),
        target,
        base_revision: binding.revision(),
        evaluator: orvek_harness::EvaluatorIdentity::from_digest(digest(&format!(
            "{namespace}:evaluator"
        ))),
        policy: binding.policy(),
        partitions: orvek_harness::PartitionCommitments {
            mining: orvek_harness::PartitionCommitment::from_digest(digest(&format!(
                "{namespace}:mining-partition"
            ))),
            adaptive_promotion: orvek_harness::PartitionCommitment::from_digest(digest(&format!(
                "{namespace}:adaptive-partition"
            ))),
            final_audit: orvek_harness::PartitionCommitment::from_digest(digest(&format!(
                "{namespace}:final-partition"
            ))),
        },
        blocks,
        adaptive_promotion: orvek_harness::LedgerLimit::new(FAMILY_SIZE, 1_000_000_000),
        final_audit: orvek_harness::LedgerLimit::new(FAMILY_SIZE, 1_000_000_000),
        audit_epoch: orvek_harness::AuditEpochId::new(),
        scoring,
    };
    store.register_evaluation_cohort(&cohort).unwrap();
    cohort
}

fn started(campaign: CampaignId, cohort: &EvaluationCohortSpec) -> CampaignEvent {
    CampaignEvent::Started {
        campaign,
        cohort: cohort.id,
        base_revision: cohort.base_revision,
        policy: cohort.policy,
        budget: orvek_harness::CampaignBudget::new(
            16,
            orvek_harness::ResourceUsage::new(50, 500, 5_000),
        )
        .unwrap(),
    }
}

fn scored_campaign(
    store: &mut Store,
    campaign: CampaignId,
    cohort: &EvaluationCohortSpec,
    namespace: &str,
    proposal: orvek_harness::ProposalId,
) -> (CampaignState, RoundId, CandidateId) {
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

fn adaptive_dataset(
    cohort: &EvaluationCohortSpec,
    campaign: CampaignId,
    candidate: CandidateId,
    namespace: &str,
) -> orvek_harness::AdaptivePromotionDataset {
    let partition = orvek_harness::TrialPartition::new(
        orvek_harness::TrialLedgerRole::AdaptivePromotion,
        1,
        cohort.partitions.adaptive_promotion,
    )
    .unwrap();
    let runtime = orvek_harness::TrialRuntimeIdentity::new(
        cohort.target.model,
        cohort.target.protocol,
        cohort.evaluator,
        cohort.target.environment,
    );
    let mut pairs = Vec::new();
    for (case_index, block) in cohort.blocks.iter().enumerate() {
        for repeat in 0..block.cases[0].repeats {
            let context = orvek_harness::TrialContext::new(
                campaign,
                candidate,
                partition,
                runtime,
                orvek_harness::contract::Limits::default(),
            )
            .unwrap();
            let spec = orvek_harness::TrialPairSpec::new(
                context,
                block.id,
                block.cases[0].id,
                repeat,
                orvek_harness::InputCommitment::from_digest(digest(&format!(
                    "{namespace}:input:{case_index}:{repeat}"
                ))),
            )
            .unwrap();
            let dispatch = orvek_harness::controller::prepare_trial_dispatch(
                cohort.target,
                cohort.evaluator,
                spec,
            )
            .unwrap();
            let parent_run = *dispatch
                .runs()
                .iter()
                .find(|run| run.key().side() == orvek_harness::TrialSide::Parent)
                .unwrap();
            let candidate_run = *dispatch
                .runs()
                .iter()
                .find(|run| run.key().side() == orvek_harness::TrialSide::Candidate)
                .unwrap();
            let effect = orvek_harness::EffectId::derive(
                campaign,
                17,
                orvek_harness::EffectKind::Trial,
                orvek_harness::EffectWorkId::from_digest(digest(&format!(
                    "{namespace}:effect:{case_index}:{repeat}"
                ))),
            );
            let parent = orvek_harness::IsolatedRunReceipt::collected(
                &parent_run,
                effect,
                orvek_harness::LeaseEpoch::new(1).unwrap(),
                passing_outcome(600_000, 800_000),
                digest(&format!("{namespace}:parent:{case_index}:{repeat}")),
            )
            .unwrap();
            let candidate = orvek_harness::IsolatedRunReceipt::collected(
                &candidate_run,
                effect,
                orvek_harness::LeaseEpoch::new(1).unwrap(),
                passing_outcome(900_000, 800_000),
                digest(&format!("{namespace}:candidate:{case_index}:{repeat}")),
            )
            .unwrap();
            let evidence =
                orvek_harness::classify_paired_trial(spec, Some(&parent), Some(&candidate))
                    .unwrap();
            pairs.push(orvek_harness::AdaptiveTrialEvidence { spec, evidence });
        }
    }
    orvek_harness::AdaptivePromotionDataset::new(
        cohort.id,
        partition,
        candidate,
        orvek_harness::CandidateLabel::new(namespace).unwrap(),
        pairs,
        hard_gates(),
    )
    .unwrap()
}

fn passing_outcome(quality: u32, safety: u32) -> orvek_harness::TerminalTrialOutcome {
    use std::collections::BTreeMap;
    orvek_harness::TerminalTrialOutcome::pass(BTreeMap::from([
        (
            orvek_harness::MetricName::new("quality").unwrap(),
            orvek_harness::MetricScore::from_millionths(quality).unwrap(),
        ),
        (
            orvek_harness::MetricName::new("safety").unwrap(),
            orvek_harness::MetricScore::from_millionths(safety).unwrap(),
        ),
    ]))
}

fn coordinates() -> orvek_harness::DecisionCoordinates {
    orvek_harness::DecisionCoordinates {
        candidate: 0,
        round: 0,
        composite: 0,
        fallback: 0,
        campaign: 0,
        activation_attempt: 0,
    }
}

/// Drives one campaign through scoring, composition, and a verified final
/// verdict: the exact state an activation certificate may be issued over.
fn awaiting_approval(
    store: &mut Store,
    namespace: &str,
) -> (
    EvaluationCohortSpec,
    CampaignId,
    Digest,
    AdaptiveScoreReport,
) {
    let cohort = register_cohort(store, namespace);
    let campaign = CampaignId::new();
    let proposal = fixture_proposal(store, namespace, cohort.base_revision);
    let (state, round, candidate) =
        scored_campaign(store, campaign, &cohort, namespace, proposal.id());
    let (state, report) = store
        .record_adaptive_score(
            campaign,
            state.revision(),
            round,
            adaptive_dataset(&cohort, campaign, candidate, namespace),
            coordinates(),
        )
        .unwrap();
    assert_eq!(
        report.verdict,
        orvek_harness::AdaptiveVerdict::Verified,
        "fixture must score the candidate as verified before composition"
    );
    let score = report.result_id().unwrap();
    let composed = store
        .compose_verified_candidate(
            campaign,
            state.revision(),
            round,
            orvek_harness::CompositionInput::new(candidate, score, &proposal),
        )
        .unwrap();
    let revision = composed.1.revision().digest();
    let verdict = FinalVerdict::Verified {
        candidate,
        revision,
        report: AuditReportId::from_digest(digest(&format!("{namespace}:audit"))),
    };
    let state = store
        .record_final_verdict(campaign, composed.0.revision(), verdict)
        .unwrap();
    assert!(matches!(
        state.approval(),
        Some(ApprovalState::Awaiting { revision: awaited }) if *awaited == revision
    ));
    (cohort, campaign, revision, report)
}

fn fixture_proposal(store: &Store, namespace: &str, base_revision: Digest) -> BoundedProposal {
    let parent = store.load_harness_revision(base_revision).unwrap();
    let request = orvek_harness::ProposalRequest::new(
        parent.digest(),
        orvek_harness::MiningBundleRoot::from_digest(digest(&format!("{namespace}:bundle"))),
        parent.policy_identity(),
        orvek_harness::ModelIdentity::from_digest(digest(&format!("{namespace}:pmodel"))),
        orvek_harness::ProtocolIdentity::from_digest(digest(&format!("{namespace}:pprotocol"))),
        2,
        64 * 1024,
    )
    .unwrap();
    let attempts: Vec<_> = request
        .intents()
        .iter()
        .enumerate()
        .map(|(index, intent)| {
            let output = serde_json::to_vec(&json!({
                "schema_version": 1,
                "request": request.root(),
                "intent": intent.id(),
                "parent": request.parent(),
                "evidence": request.evidence(),
                "policy": request.policy(),
                "dimensions": if index == 0 { ["instructions"] } else { ["skills"] },
                "patch": if index == 0 {
                    json!({"instructions": format!("{namespace} instruction")})
                } else {
                    json!({"skills": {"research": "Collect bounded evidence and cite findings."}})
                },
            }))
            .unwrap();
            orvek_harness::ProposalAttempt::settled(
                *intent,
                orvek_harness::ProposalProviderReceiptId::from_digest(digest(&format!(
                    "{namespace}:receipt:{index}"
                ))),
                output,
            )
        })
        .collect();
    orvek_harness::validate_proposal_batch(request, &parent, attempts)
        .unwrap()
        .proposals()
        .to_vec()
        .pop()
        .unwrap()
}

fn certificate(
    cohort: &EvaluationCohortSpec,
    campaign: CampaignId,
    revision: Digest,
    namespace: &str,
    expected_base: Digest,
) -> ActivationCertificate {
    ActivationCertificate::issue(FinalAuditDataset {
        campaign,
        cohort: cohort.id,
        epoch: cohort.audit_epoch,
        verdict: FinalVerdict::Verified {
            candidate: candidate_id(&format!("{namespace}:candidate")),
            revision,
            report: AuditReportId::from_digest(digest(&format!("{namespace}:audit"))),
        },
        score: ScoreResultId::from_digest(digest(&format!("{namespace}:score"))),
        composition: orvek_harness::CompositionId::from_digest(digest(&format!(
            "{namespace}:composition"
        ))),
        evidence: digest(&format!("{namespace}:evidence")),
        campaign_root: digest(&format!("{namespace}:campaign-root")),
        cohort_root: digest(&format!("{namespace}:cohort-root")),
        ledger: orvek_harness::LedgerStatus {
            query_limit: 10,
            query_used: 0,
            error_limit_nanos: 100,
            error_used_nanos: 0,
        },
        expected_base,
        rollback_target: Some(expected_base),
    })
    .unwrap()
}

#[test]
fn unverified_verdicts_cannot_issue_certificates() {
    let error = ActivationCertificate::issue(FinalAuditDataset {
        campaign: CampaignId::new(),
        cohort: orvek_harness::CohortId::new(),
        epoch: orvek_harness::AuditEpochId::new(),
        verdict: FinalVerdict::NotVerified {
            candidate: candidate_id("x"),
            revision: digest("r"),
            report: AuditReportId::from_digest(digest("a")),
        },
        score: ScoreResultId::from_digest(digest("s")),
        composition: orvek_harness::CompositionId::from_digest(digest("m")),
        evidence: digest("e"),
        campaign_root: digest("cr"),
        cohort_root: digest("kor"),
        ledger: orvek_harness::LedgerStatus {
            query_limit: 10,
            query_used: 0,
            error_limit_nanos: 100,
            error_used_nanos: 0,
        },
        expected_base: digest("b"),
        rollback_target: None,
    })
    .unwrap_err();
    assert_eq!(error, PromotionError::UnverifiedVerdict);
}

#[test]
fn certificates_persist_and_rebind_their_payload() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let (cohort, campaign, revision, _) = awaiting_approval(&mut store, "persist");
    let base = cohort.base_revision;
    let certificate = certificate(&cohort, campaign, revision, "persist", base);
    store.persist_activation_certificate(&certificate).unwrap();
    let read = store
        .read_activation_certificate(certificate.digest())
        .unwrap();
    assert_eq!(read, certificate);
    assert!(
        store
            .read_activation_certificate(digest("unknown"))
            .is_err()
    );
}

#[test]
fn approval_is_bound_to_campaign_certificate_and_channel() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let (cohort, campaign, revision, _) = awaiting_approval(&mut store, "approve");
    let base = cohort.base_revision;
    let certificate = certificate(&cohort, campaign, revision, "approve", base);
    store.persist_activation_certificate(&certificate).unwrap();

    let request = ApprovalRequest {
        campaign,
        certificate: &certificate,
        channel: Channel::Canary,
    };
    let decision = request.validate(revision, digest("approval")).unwrap();
    assert!(matches!(decision, ApprovalDecision::Approved { .. }));

    // A certificate from another campaign cannot approve this one.
    let other = ApprovalRequest {
        campaign: CampaignId::new(),
        certificate: &certificate,
        channel: Channel::Canary,
    };
    assert!(matches!(
        other.validate(revision, digest("approval")),
        Err(PromotionError::CertificateCampaign(_, _))
    ));

    // The revision must match the awaiting revision exactly.
    assert!(matches!(
        request.validate(digest("wrong"), digest("approval")),
        Err(PromotionError::CertificateRevision(_, _, _))
    ));

    // Stable-channel rollout stays disabled until the rollout gates land.
    let stable = ApprovalRequest {
        campaign,
        certificate: &certificate,
        channel: Channel::Stable,
    };
    assert!(matches!(
        stable.validate(revision, digest("approval")),
        Err(PromotionError::UnboundedChannel(_))
    ));

    // The store rejects decisions whose certificate does not match.
    let error = store.record_campaign_approval(
        campaign,
        store.load_campaign(campaign).unwrap().revision(),
        ApprovalDecision::Approved {
            id: ApprovalId::from_digest(digest("approval")),
            revision,
        },
        digest("not-the-certificate"),
    );
    assert!(error.is_err());
}

#[test]
fn activation_cas_lets_exactly_one_campaign_win_and_records_supersession() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let (cohort, first, first_revision, _) = awaiting_approval(&mut store, "first");
    let (_, second, second_revision, _) = awaiting_approval(&mut store, "second");
    let base = cohort.base_revision;

    for (campaign, revision, namespace) in [
        (first, first_revision, "first"),
        (second, second_revision, "second"),
    ] {
        let certificate = certificate(&cohort, campaign, revision, namespace, base);
        store.persist_activation_certificate(&certificate).unwrap();
        let decision = ApprovalRequest {
            campaign,
            certificate: &certificate,
            channel: Channel::Canary,
        }
        .validate(revision, digest(&format!("{namespace}:approval")))
        .unwrap();
        store
            .record_campaign_approval(
                campaign,
                store.load_campaign(campaign).unwrap().revision(),
                decision,
                certificate.digest(),
            )
            .unwrap();
    }

    let (winner_state, outcome) = store
        .activate_harness_revision(
            first,
            store.load_campaign(first).unwrap().revision(),
            cohort.target,
            certificate(&cohort, first, first_revision, "first", base).digest(),
            base,
        )
        .unwrap();
    assert!(matches!(outcome, ActivationOutcome::Activated { .. }));
    assert!(winner_state.approval().is_some());

    let (loser_state, outcome) = store
        .activate_harness_revision(
            second,
            store.load_campaign(second).unwrap().revision(),
            cohort.target,
            certificate(&cohort, second, second_revision, "second", base).digest(),
            base,
        )
        .unwrap();
    let ActivationOutcome::Superseded {
        active_revision, ..
    } = outcome
    else {
        panic!("the second activation must lose the CAS race");
    };
    assert_eq!(active_revision, first_revision);
    assert!(matches!(
        loser_state.terminal(),
        Some(orvek_harness::TerminalState::Superseded(_))
    ));
}

#[test]
fn rollback_names_a_prior_receipt_or_last_known_good() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let (cohort, campaign, revision, _) = awaiting_approval(&mut store, "rollback");
    let base = cohort.base_revision;
    let certificate = certificate(&cohort, campaign, revision, "rollback", base);
    store.persist_activation_certificate(&certificate).unwrap();
    let decision = ApprovalRequest {
        campaign,
        certificate: &certificate,
        channel: Channel::Canary,
    }
    .validate(revision, digest("approval"))
    .unwrap();
    store
        .record_campaign_approval(
            campaign,
            store.load_campaign(campaign).unwrap().revision(),
            decision,
            certificate.digest(),
        )
        .unwrap();
    let (_, ActivationOutcome::Activated { receipt, .. }) = store
        .activate_harness_revision(
            campaign,
            store.load_campaign(campaign).unwrap().revision(),
            cohort.target,
            certificate.digest(),
            base,
        )
        .unwrap()
    else {
        panic!("activation must commit");
    };

    // The receipt lookup is the store's; last-known-good restores the base.
    let lookup = |id| store.activation_receipt_revisions(id).unwrap();
    let (outcome, rollback_receipt) = validate_rollback(
        campaign,
        revision,
        RollbackTarget::LastKnownGood {
            activation: receipt,
        },
        lookup,
        orvek_harness::MonitoringReportId::from_digest(digest("report")),
        "canary hard-gate breach",
    )
    .unwrap();
    assert_eq!(
        outcome,
        orvek_harness::MonitoringOutcome::RolledBack {
            from_revision: revision,
            restored_revision: base,
            report: orvek_harness::MonitoringReportId::from_digest(digest("report")),
            receipt: rollback_receipt,
        }
    );

    // A receipt that activated a different revision cannot be named.
    let wrong = validate_rollback(
        campaign,
        base,
        RollbackTarget::LastKnownGood {
            activation: receipt,
        },
        lookup,
        orvek_harness::MonitoringReportId::from_digest(digest("report")),
        "mismatched",
    );
    assert!(matches!(
        wrong,
        Err(PromotionError::ReceiptRevision(_, _, _))
    ));

    // Unknown receipts are rejected outright.
    let unknown = validate_rollback(
        campaign,
        revision,
        RollbackTarget::PriorReceipt {
            receipt: orvek_harness::ActivationReceiptId::from_digest(digest("ghost")),
            restores: base,
        },
        lookup,
        orvek_harness::MonitoringReportId::from_digest(digest("report")),
        "ghost",
    );
    assert!(matches!(unknown, Err(PromotionError::UnknownReceipt(_, _))));
}

#[test]
fn final_audit_access_burns_the_epoch_globally() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("state")).unwrap();
    let cohort = register_cohort(&mut store, "burn");
    let epoch = cohort.audit_epoch;
    assert_eq!(
        store.audit_epoch_status(cohort.id, epoch).unwrap(),
        AuditEpochStatus::Active
    );
    let reservation = store.reserve_final_audit_evidence(cohort.id, 1024).unwrap();
    store
        .stage_final_audit_evidence(&reservation, b"sealed audit bytes")
        .unwrap();
    let reference = store.commit_final_audit_evidence(reservation).unwrap();
    let bytes = store
        .read_final_audit_evidence(orvek_harness::FinalAuditAccess {
            evidence: reference,
            epoch,
        })
        .unwrap();
    assert_eq!(bytes, b"sealed audit bytes");
    assert_eq!(
        store.audit_epoch_status(cohort.id, epoch).unwrap(),
        AuditEpochStatus::Retired
    );
    // Access-before-crash semantics: the burn is durable, a second read of
    // the same epoch cannot happen.
    let again = store.read_final_audit_evidence(orvek_harness::FinalAuditAccess {
        evidence: reference,
        epoch,
    });
    assert!(again.is_err());
}
