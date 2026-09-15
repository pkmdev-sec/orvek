"""Executable contract for the frozen self-harness evaluation oracle."""

from __future__ import annotations

import itertools
import unittest
from dataclasses import replace
from decimal import Decimal

from .oracle import evaluate_adaptive, evaluate_final_audit
from .schema import (
    AdaptivePromotionDataset,
    BehavioralFailure,
    DecisionCoordinates,
    FinalAuditDataset,
    FrozenEvaluationSpec,
    GateEvidence,
    GateStatus,
    InfrastructureUnknown,
    LedgerRole,
    LedgerState,
    MetricKind,
    MetricSpec,
    MiningEvaluation,
    Pass,
    SchemaError,
    Side,
    TrialObservation,
    Verdict,
    canonical_json,
)


COMMITMENTS = {
    "mining": "hmac-sha256:" + "1" * 64,
    "adaptive": "hmac-sha256:" + "2" * 64,
    "final": "hmac-sha256:" + "3" * 64,
}
REQUIRED_GATES = (
    "policy",
    "provenance",
    "secrecy",
    "cost",
    "latency",
    "resource_ceilings",
)


def spec_mapping(*, minimum_complete_blocks: int = 7) -> dict[str, object]:
    cases = [
        {"case_id": f"case-{index}", "block_id": f"block-{index}", "critical": index == 0}
        for index in range(7)
    ]
    family = {
        "candidates": 2,
        "rounds": 2,
        "metrics": 2,
        "strata": 1,
        "composites": 2,
        "fallbacks": 2,
        "campaigns": 2,
        "activation_attempts": 2,
    }
    family_size = 128
    return {
        "schema_version": 1,
        "estimator_version": "paired-case-block-exact-sign-v1",
        "calibration": "SYNTHETIC_FIXTURE_ONLY",
        "cohort_id": "synthetic-cohort-v1",
        "model_digest": "model-v1",
        "protocol_digest": "protocol-v1",
        "evaluator_digest": "evaluator-v1",
        "environment_digest": "environment-v1",
        "mining_partition_commitment": COMMITMENTS["mining"],
        "adaptive_partition_commitment": COMMITMENTS["adaptive"],
        "final_partition_commitment": COMMITMENTS["final"],
        "repeats": 2,
        "cases": cases,
        "strata": [{"name": "all", "case_ids": [case["case_id"] for case in cases]}],
        "metrics": [
            {"name": "quality", "kind": "primary", "margin": "0.05"},
            {"name": "safety", "kind": "protected", "margin": "0.10"},
        ],
        "minimum_complete_blocks": minimum_complete_blocks,
        "maximum_exact_blocks": 8,
        "required_hard_gates": list(REQUIRED_GATES),
        "multiplicity_family": family,
        "adaptive_ledger": {
            "role": "adaptive_promotion",
            "query_limit": family_size,
            "error_units_limit": family_size,
            "error_scale": family_size,
            "error_units_per_hypothesis": 1,
        },
        "final_audit_ledger": {
            "role": "final_audit",
            "query_limit": family_size,
            "error_units_limit": family_size,
            "error_scale": family_size,
            "error_units_per_hypothesis": 1,
        },
    }


def frozen_spec(*, minimum_complete_blocks: int = 7) -> FrozenEvaluationSpec:
    return FrozenEvaluationSpec.from_mapping(
        spec_mapping(minimum_complete_blocks=minimum_complete_blocks)
    )


def passing_metrics(quality: str, safety: str = "0.80") -> Pass:
    return Pass(
        (
            ("quality", Decimal(quality)),
            ("safety", Decimal(safety)),
        )
    )


def observations(
    spec: FrozenEvaluationSpec,
    *,
    parent_quality: str = "0.60",
    candidate_quality: str = "0.90",
    parent_safety: str = "0.80",
    candidate_safety: str = "0.80",
) -> tuple[TrialObservation, ...]:
    result = []
    for case in spec.cases:
        for repeat in range(spec.repeats):
            for side in Side:
                if side is Side.PARENT:
                    outcome = passing_metrics(parent_quality, parent_safety)
                else:
                    outcome = passing_metrics(candidate_quality, candidate_safety)
                result.append(
                    TrialObservation(
                        case_id=case.case_id,
                        repeat=repeat,
                        block_id=case.block_id,
                        side=side,
                        model_digest=spec.model_digest,
                        protocol_digest=spec.protocol_digest,
                        evaluator_digest=spec.evaluator_digest,
                        environment_digest=spec.environment_digest,
                        outcome=outcome,
                    )
                )
    return tuple(result)


def gate_evidence() -> tuple[GateEvidence, ...]:
    return tuple(
        GateEvidence(name, GateStatus.VERIFIED, f"synthetic {name} fixture passed")
        for name in REQUIRED_GATES
    )


def adaptive_dataset(
    spec: FrozenEvaluationSpec,
    *,
    trial_observations: tuple[TrialObservation, ...] | None = None,
    candidate_label: str = "candidate-a",
    partition_commitment: str | None = None,
) -> AdaptivePromotionDataset:
    return AdaptivePromotionDataset(
        cohort_id=spec.cohort_id,
        adaptive_partition_commitment=(
            partition_commitment or spec.adaptive_partition_commitment
        ),
        candidate_label=candidate_label,
        observations=(trial_observations or observations(spec)),
        hard_gate_evidence=gate_evidence(),
    )


def final_dataset(
    spec: FrozenEvaluationSpec,
    *,
    trial_observations: tuple[TrialObservation, ...] | None = None,
) -> FinalAuditDataset:
    return FinalAuditDataset(
        cohort_id=spec.cohort_id,
        final_partition_commitment=spec.final_partition_commitment,
        audit_epoch="audit-epoch-1",
        access_receipt="sealed-access-receipt-1",
        candidate_label="candidate-a",
        observations=(trial_observations or observations(spec)),
        hard_gate_evidence=gate_evidence(),
    )


def ledger(spec: FrozenEvaluationSpec, role: LedgerRole) -> LedgerState:
    return LedgerState(spec.cohort_id, role, 0, 0)


def coordinates(**changes: int) -> DecisionCoordinates:
    values = {
        "candidate": 0,
        "round": 0,
        "composite": 0,
        "fallback": 0,
        "campaign": 0,
        "activation_attempt": 0,
    }
    values.update(changes)
    return DecisionCoordinates(**values)


def gate_status(result: object, name: str) -> GateStatus:
    for gate in result.gates:  # type: ignore[attr-defined]
        if gate.name == name:
            return gate.status
    raise AssertionError(f"missing gate {name}")


def replace_observation(
    items: tuple[TrialObservation, ...],
    case_id: str,
    repeat: int,
    side: Side,
    **changes: object,
) -> tuple[TrialObservation, ...]:
    return tuple(
        replace(item, **changes)
        if (item.case_id, item.repeat, item.side) == (case_id, repeat, side)
        else item
        for item in items
    )


class EvidenceRoleSchemaTests(unittest.TestCase):
    def test_role_schemas_are_distinct_and_cross_role_use_is_rejected(self) -> None:
        spec = frozen_spec()
        mining = MiningEvaluation.from_mapping(
            {
                "schema": "MiningEvaluation",
                "cohort_id": spec.cohort_id,
                "mining_partition_commitment": spec.mining_partition_commitment,
                "sanitized_failure_facts": ["fact-a"],
                "pass_anchor_ids": ["anchor-a"],
            }
        )
        adaptive = adaptive_dataset(spec)
        final = final_dataset(spec)

        self.assertIs(type(mining), MiningEvaluation)
        self.assertIs(type(adaptive), AdaptivePromotionDataset)
        self.assertIs(type(final), FinalAuditDataset)
        with self.assertRaises(SchemaError):
            evaluate_adaptive(spec, final, ledger(spec, LedgerRole.ADAPTIVE_PROMOTION), coordinates())  # type: ignore[arg-type]
        with self.assertRaises(SchemaError):
            evaluate_final_audit(spec, adaptive, ledger(spec, LedgerRole.FINAL_AUDIT), coordinates())  # type: ignore[arg-type]
        with self.assertRaises(SchemaError):
            AdaptivePromotionDataset.from_mapping(mining.to_mapping())

    def test_unknown_and_malformed_input_fail_closed(self) -> None:
        malformed_spec = spec_mapping()
        malformed_spec["unknown"] = "authority-smuggling"
        with self.assertRaisesRegex(SchemaError, "unknown fields"):
            FrozenEvaluationSpec.from_mapping(malformed_spec)

        dataset = adaptive_dataset(frozen_spec()).to_mapping()
        dataset["observations"][0]["outcome"] = {  # type: ignore[index]
            "kind": "TransportSuccess",
            "metrics": {},
        }
        with self.assertRaisesRegex(SchemaError, "trial outcome kind"):
            AdaptivePromotionDataset.from_mapping(dataset)

    def test_direct_input_construction_enforces_schema_invariants(self) -> None:
        with self.assertRaises(SchemaError):
            MiningEvaluation("cohort", "public-sha256", ("fact",), ("anchor",))
        with self.assertRaises(SchemaError):
            DecisionCoordinates(-1, 0, 0, 0, 0, 0)
        with self.assertRaises(SchemaError):
            MetricSpec("quality", MetricKind.PRIMARY, Decimal("1.1"))

    def test_hiding_commitments_are_keyed_and_role_distinct(self) -> None:
        spec = frozen_spec()
        self.assertEqual(len(set(COMMITMENTS.values())), 3)
        for commitment in COMMITMENTS.values():
            self.assertRegex(commitment, r"^hmac-sha256:[0-9a-f]{64}$")
        malformed = spec_mapping()
        malformed["adaptive_partition_commitment"] = "sha256:" + "2" * 64
        with self.assertRaisesRegex(SchemaError, "keyed hmac-sha256"):
            FrozenEvaluationSpec.from_mapping(malformed)


class GoldenVerdictTests(unittest.TestCase):
    def evaluate(
        self,
        spec: FrozenEvaluationSpec,
        trial_observations: tuple[TrialObservation, ...],
    ):
        return evaluate_adaptive(
            spec,
            adaptive_dataset(spec, trial_observations=trial_observations),
            ledger(spec, LedgerRole.ADAPTIVE_PROMOTION),
            coordinates(),
        )

    def test_improvement_is_verified_by_case_then_block_aggregation(self) -> None:
        spec = frozen_spec()
        result = self.evaluate(spec, observations(spec))

        self.assertEqual(result.verdict, Verdict.VERIFIED)
        quality = next(item for item in result.statistics if item.metric == "quality")
        self.assertEqual(len(quality.case_effects), 7)
        self.assertEqual(len(quality.block_effects), 7)
        self.assertEqual(quality.observed_effect, Decimal("0.3"))
        self.assertEqual(quality.adjusted_effect, Decimal("0.25"))
        self.assertEqual(quality.p_value, "1/128")
        self.assertEqual(quality.alpha, "1/128")

    def test_cases_share_declared_blocks_without_attempt_pseudoreplication(self) -> None:
        mapping = spec_mapping()
        cases = [
            {
                "case_id": f"case-{block}-{member}",
                "block_id": f"block-{block}",
                "critical": block == 0 and member == 0,
            }
            for block in range(7)
            for member in range(2)
        ]
        mapping["cases"] = cases
        mapping["strata"] = [
            {"name": "all", "case_ids": [case["case_id"] for case in cases]}
        ]
        spec = FrozenEvaluationSpec.from_mapping(mapping)
        result = self.evaluate(spec, observations(spec))

        quality = next(item for item in result.statistics if item.metric == "quality")
        self.assertEqual(result.verdict, Verdict.VERIFIED)
        self.assertEqual(len(quality.case_effects), 14)
        self.assertEqual(len(quality.block_effects), 7)
        self.assertEqual(quality.p_value, "1/128")

    def test_regression_and_equality_are_not_verified(self) -> None:
        spec = frozen_spec()
        regression = self.evaluate(
            spec,
            observations(spec, candidate_quality="0.50"),
        )
        equality = self.evaluate(
            spec,
            observations(spec, candidate_quality="0.60"),
        )

        self.assertEqual(regression.verdict, Verdict.NOT_VERIFIED)
        self.assertEqual(equality.verdict, Verdict.NOT_VERIFIED)
        self.assertEqual(
            gate_status(equality, "useful_effect:quality:all"),
            GateStatus.NOT_VERIFIED,
        )

    def test_protected_regression_is_an_independent_failure(self) -> None:
        spec = frozen_spec()
        result = self.evaluate(
            spec,
            observations(spec, candidate_safety="0.50"),
        )
        self.assertEqual(result.verdict, Verdict.NOT_VERIFIED)
        self.assertEqual(
            gate_status(result, "protected_noninferiority:safety:all"),
            GateStatus.NOT_VERIFIED,
        )

    def test_attributable_crash_is_negative_behavioral_evidence(self) -> None:
        spec = frozen_spec()
        items = replace_observation(
            observations(spec),
            "case-0",
            0,
            Side.CANDIDATE,
            outcome=BehavioralFailure("attributable_candidate_crash"),
        )
        result = self.evaluate(spec, items)

        self.assertEqual(result.verdict, Verdict.NOT_VERIFIED)
        self.assertEqual(gate_status(result, "correctness"), GateStatus.NOT_VERIFIED)
        self.assertEqual(gate_status(result, "critical_cases"), GateStatus.NOT_VERIFIED)

    def test_protocol_timeout_is_negative_behavioral_evidence(self) -> None:
        spec = frozen_spec()
        items = replace_observation(
            observations(spec),
            "case-1",
            1,
            Side.CANDIDATE,
            outcome=BehavioralFailure("protocol_timeout"),
        )
        result = self.evaluate(spec, items)

        self.assertEqual(result.verdict, Verdict.NOT_VERIFIED)
        self.assertEqual(gate_status(result, "correctness"), GateStatus.NOT_VERIFIED)

    def test_missing_pair_and_environment_drift_are_inconclusive(self) -> None:
        spec = frozen_spec()
        complete = observations(spec)
        missing = tuple(
            item
            for item in complete
            if (item.case_id, item.repeat, item.side) != ("case-2", 0, Side.CANDIDATE)
        )
        drift = replace_observation(
            complete,
            "case-2",
            0,
            Side.CANDIDATE,
            environment_digest="environment-v2",
        )

        missing_result = self.evaluate(spec, missing)
        drift_result = self.evaluate(spec, drift)
        self.assertEqual(missing_result.verdict, Verdict.INCONCLUSIVE)
        self.assertEqual(drift_result.verdict, Verdict.INCONCLUSIVE)
        self.assertEqual(
            gate_status(missing_result, "data_completeness"),
            GateStatus.INCONCLUSIVE,
        )
        self.assertEqual(
            gate_status(drift_result, "pairing_integrity"),
            GateStatus.INCONCLUSIVE,
        )

    def test_infrastructure_unknown_cannot_promote(self) -> None:
        spec = frozen_spec()
        items = replace_observation(
            observations(spec),
            "case-3",
            0,
            Side.PARENT,
            outcome=InfrastructureUnknown("missing_receipt"),
        )
        result = self.evaluate(spec, items)
        self.assertEqual(result.verdict, Verdict.INCONCLUSIVE)

    def test_insufficient_complete_blocks_is_inconclusive(self) -> None:
        spec = frozen_spec(minimum_complete_blocks=8)
        result = self.evaluate(spec, observations(spec))
        self.assertEqual(result.verdict, Verdict.INCONCLUSIVE)
        self.assertEqual(gate_status(result, "minimum_power"), GateStatus.INCONCLUSIVE)


class LedgerAndMetamorphicTests(unittest.TestCase):
    def test_exact_debit_and_query_or_error_exhaustion(self) -> None:
        spec = frozen_spec()
        dataset = adaptive_dataset(spec)
        allocation = spec.adaptive_ledger
        result = evaluate_adaptive(
            spec,
            dataset,
            ledger(spec, LedgerRole.ADAPTIVE_PROMOTION),
            coordinates(),
        )
        self.assertEqual(result.ledger.query_debit, 2)
        self.assertEqual(result.ledger.error_units_debit, 2)

        query_exhausted = LedgerState(
            spec.cohort_id,
            LedgerRole.ADAPTIVE_PROMOTION,
            allocation.query_limit - 1,
            0,
        )
        error_exhausted = LedgerState(
            spec.cohort_id,
            LedgerRole.ADAPTIVE_PROMOTION,
            0,
            allocation.error_units_limit - 1,
        )
        for exhausted in (query_exhausted, error_exhausted):
            blocked = evaluate_adaptive(spec, dataset, exhausted, coordinates())
            self.assertEqual(blocked.verdict, Verdict.INCONCLUSIVE)
            self.assertEqual(blocked.ledger.query_debit, 0)
            self.assertEqual(blocked.ledger.error_units_debit, 0)
            self.assertEqual(blocked.ledger.after, exhausted)

    def test_order_and_candidate_label_do_not_change_normalized_result(self) -> None:
        spec = frozen_spec()
        original = adaptive_dataset(spec, candidate_label="candidate-a")
        permuted = replace(
            original,
            candidate_label="renamed-candidate",
            observations=tuple(reversed(original.observations)),
            hard_gate_evidence=tuple(reversed(original.hard_gate_evidence)),
        )
        state = ledger(spec, LedgerRole.ADAPTIVE_PROMOTION)
        first = evaluate_adaptive(spec, original, state, coordinates())
        second = evaluate_adaptive(spec, permuted, state, coordinates())

        self.assertEqual(first.verdict, second.verdict)
        self.assertEqual(first.evidence_root, second.evidence_root)
        self.assertEqual(first.canonical_json(), second.canonical_json())

    def test_two_runs_are_byte_identical_and_forbid_binary_floats(self) -> None:
        spec = frozen_spec()
        dataset = adaptive_dataset(spec)
        state = ledger(spec, LedgerRole.ADAPTIVE_PROMOTION)
        first = evaluate_adaptive(spec, dataset, state, coordinates()).canonical_json()
        second = evaluate_adaptive(spec, dataset, state, coordinates()).canonical_json()

        self.assertEqual(first.encode("utf-8"), second.encode("utf-8"))
        with self.assertRaisesRegex(SchemaError, "binary floating-point"):
            canonical_json({"score": 0.1})

    def test_partition_mismatch_is_inconclusive(self) -> None:
        spec = frozen_spec()
        result = evaluate_adaptive(
            spec,
            adaptive_dataset(spec, partition_commitment=COMMITMENTS["mining"]),
            ledger(spec, LedgerRole.ADAPTIVE_PROMOTION),
            coordinates(),
        )
        self.assertEqual(result.verdict, Verdict.INCONCLUSIVE)
        self.assertEqual(gate_status(result, "pairing_integrity"), GateStatus.INCONCLUSIVE)

    def test_final_audit_access_is_globally_one_use(self) -> None:
        spec = frozen_spec()
        dataset = final_dataset(spec)
        first = evaluate_final_audit(
            spec,
            dataset,
            ledger(spec, LedgerRole.FINAL_AUDIT),
            coordinates(),
        )
        second = evaluate_final_audit(
            spec,
            dataset,
            first.ledger.after,
            coordinates(candidate=1),
        )

        self.assertTrue(first.ledger.after.audit_epoch_burned)
        self.assertEqual(second.verdict, Verdict.INCONCLUSIVE)
        self.assertEqual(second.ledger.query_debit, 0)
        self.assertIn("already burned", second.gates[0].reason)

    def test_synthetic_fixture_never_authorizes_production(self) -> None:
        spec = frozen_spec()
        adaptive = evaluate_adaptive(
            spec,
            adaptive_dataset(spec),
            ledger(spec, LedgerRole.ADAPTIVE_PROMOTION),
            coordinates(),
        )
        final = evaluate_final_audit(
            spec,
            final_dataset(spec),
            ledger(spec, LedgerRole.FINAL_AUDIT),
            coordinates(),
        )
        self.assertEqual(adaptive.verdict, Verdict.VERIFIED)
        self.assertEqual(final.verdict, Verdict.VERIFIED)
        self.assertEqual(adaptive.production_activation, Verdict.INCONCLUSIVE)
        self.assertEqual(final.production_activation, Verdict.INCONCLUSIVE)

    def test_deterministic_whole_loop_null_uses_complete_declared_family(self) -> None:
        spec = frozen_spec()
        null_dataset = adaptive_dataset(
            spec,
            trial_observations=observations(spec, candidate_quality="0.60"),
        )
        state = ledger(spec, LedgerRole.ADAPTIVE_PROMOTION)
        promotions = 0
        decisions = 0
        family = spec.multiplicity_family

        for values in itertools.product(
            range(family.candidates),
            range(family.rounds),
            range(family.composites),
            range(family.fallbacks),
            range(family.campaigns),
            range(family.activation_attempts),
        ):
            result = evaluate_adaptive(
                spec,
                null_dataset,
                state,
                DecisionCoordinates(*values),
            )
            promotions += result.verdict is Verdict.VERIFIED
            decisions += 1
            state = result.ledger.after

        self.assertEqual(decisions, family.decision_count)
        self.assertEqual(promotions, 0)
        self.assertEqual(state.queries_used, family.size)
        self.assertEqual(state.error_units_used, family.size)
        exhausted = evaluate_adaptive(spec, null_dataset, state, coordinates())
        self.assertEqual(exhausted.verdict, Verdict.INCONCLUSIVE)
        self.assertEqual(exhausted.ledger.query_debit, 0)


if __name__ == "__main__":
    unittest.main()
