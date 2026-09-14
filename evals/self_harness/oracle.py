"""Deterministic reference oracle for frozen self-harness evaluation policy."""

from __future__ import annotations

import itertools
from collections import defaultdict
from dataclasses import replace
from decimal import Decimal
from fractions import Fraction

from .schema import (
    AdaptivePromotionDataset,
    BehavioralFailure,
    DecisionCoordinates,
    FinalAuditDataset,
    FrozenEvaluationSpec,
    GateResult,
    GateStatus,
    InfrastructureUnknown,
    LedgerAllocation,
    LedgerRole,
    LedgerState,
    LedgerTransition,
    MetricKind,
    MetricSpec,
    MetricStatistic,
    Pass,
    ReferenceVerdict,
    SchemaError,
    Side,
    TrialObservation,
    Verdict,
    canonical_digest,
)


Dataset = AdaptivePromotionDataset | FinalAuditDataset


def evaluate_adaptive(
    spec: FrozenEvaluationSpec,
    dataset: AdaptivePromotionDataset,
    ledger: LedgerState,
    coordinates: DecisionCoordinates,
) -> ReferenceVerdict:
    """Score adaptive evidence without accepting mining or final-audit schemas."""

    if type(dataset) is not AdaptivePromotionDataset:
        raise SchemaError("evaluate_adaptive requires AdaptivePromotionDataset exactly")
    return _evaluate(spec, dataset, ledger, coordinates, LedgerRole.ADAPTIVE_PROMOTION)


def evaluate_final_audit(
    spec: FrozenEvaluationSpec,
    dataset: FinalAuditDataset,
    ledger: LedgerState,
    coordinates: DecisionCoordinates,
) -> ReferenceVerdict:
    """Score one-use final evidence on its independent global ledger."""

    if type(dataset) is not FinalAuditDataset:
        raise SchemaError("evaluate_final_audit requires FinalAuditDataset exactly")
    return _evaluate(spec, dataset, ledger, coordinates, LedgerRole.FINAL_AUDIT)


def _evaluate(
    spec: FrozenEvaluationSpec,
    dataset: Dataset,
    ledger: LedgerState,
    coordinates: DecisionCoordinates,
    role: LedgerRole,
) -> ReferenceVerdict:
    _validate_context(spec, dataset, ledger, coordinates, role)
    allocation = spec.allocation(role)
    transition = _reserve_ledger(spec, allocation, ledger, role)
    if transition.query_debit == 0:
        return _ledger_exhausted_verdict(spec, dataset, coordinates, role, transition)

    observations = {observation.key(): observation for observation in dataset.observations}
    expected_keys = {
        (case.case_id, repeat, side.value)
        for case in spec.cases
        for repeat in range(spec.repeats)
        for side in Side
    }
    actual_keys = set(observations)
    unexpected = actual_keys - expected_keys
    if unexpected:
        raise SchemaError(f"dataset contains observations outside the frozen manifest: {sorted(unexpected)!r}")

    missing = expected_keys - actual_keys
    unknowns = [
        observation
        for observation in dataset.observations
        if isinstance(observation.outcome, InfrastructureUnknown)
    ]
    drift = _drift_observations(spec, dataset.observations)
    partition_matches = _partition_commitment(spec, dataset, role) == _expected_partition(spec, role)

    gates: list[GateResult] = []
    if missing or unknowns:
        reasons: list[str] = []
        if missing:
            reasons.append(f"{len(missing)} paired observations are missing")
        if unknowns:
            reasons.append(f"{len(unknowns)} observations are InfrastructureUnknown")
        gates.append(_gate("data_completeness", GateStatus.INCONCLUSIVE, "; ".join(reasons)))
    else:
        gates.append(_gate("data_completeness", GateStatus.VERIFIED, "all frozen pairs are complete"))

    if drift or not partition_matches:
        reasons = []
        if drift:
            reasons.append(f"{len(drift)} observations have identity or block drift")
        if not partition_matches:
            reasons.append("partition commitment does not match the frozen role")
        gates.append(_gate("pairing_integrity", GateStatus.INCONCLUSIVE, "; ".join(reasons)))
    else:
        gates.append(_gate("pairing_integrity", GateStatus.VERIFIED, "all paired identities match the frozen spec"))

    external = {item.name: item for item in dataset.hard_gate_evidence}
    unexpected_gates = set(external) - set(spec.required_hard_gates)
    if unexpected_gates:
        raise SchemaError(f"dataset contains undeclared hard gates: {', '.join(sorted(unexpected_gates))}")
    for name in spec.required_hard_gates:
        item = external.get(name)
        if item is None:
            gates.append(_gate(name, GateStatus.INCONCLUSIVE, "required hard-gate evidence is missing"))
        else:
            gates.append(_gate(name, item.status, item.reason))

    candidate_failures = [
        observation
        for observation in dataset.observations
        if observation.side is Side.CANDIDATE and isinstance(observation.outcome, BehavioralFailure)
    ]
    critical_ids = {case.case_id for case in spec.cases if case.critical}
    critical_failures = [item for item in candidate_failures if item.case_id in critical_ids]
    if unknowns or missing:
        gates.append(_gate("correctness", GateStatus.INCONCLUSIVE, "candidate correctness evidence is incomplete"))
        gates.append(_gate("critical_cases", GateStatus.INCONCLUSIVE, "critical-case evidence is incomplete"))
    else:
        gates.append(
            _gate(
                "correctness",
                GateStatus.NOT_VERIFIED if candidate_failures else GateStatus.VERIFIED,
                f"{len(candidate_failures)} candidate BehavioralFailure outcomes" if candidate_failures else "no candidate BehavioralFailure outcomes",
            )
        )
        gates.append(
            _gate(
                "critical_cases",
                GateStatus.NOT_VERIFIED if critical_failures else GateStatus.VERIFIED,
                f"{len(critical_failures)} critical candidate failures" if critical_failures else "all critical cases avoid BehavioralFailure",
            )
        )

    stratum_block_counts = {
        stratum.name: len(
            {
                case.block_id
                for case in spec.cases
                if case.case_id in set(stratum.case_ids)
            }
        )
        for stratum in spec.strata
    }
    underpowered = {
        name: count for name, count in stratum_block_counts.items() if count < spec.minimum_complete_blocks
    }
    if underpowered:
        detail = ", ".join(f"{name}={count}" for name, count in sorted(underpowered.items()))
        gates.append(
            _gate(
                "minimum_power",
                GateStatus.INCONCLUSIVE,
                f"complete independent blocks below frozen minimum {spec.minimum_complete_blocks}: {detail}",
            )
        )
    else:
        gates.append(
            _gate(
                "minimum_power",
                GateStatus.VERIFIED,
                f"each stratum has at least {spec.minimum_complete_blocks} independent blocks",
            )
        )

    can_score = not missing and not unknowns and not drift and partition_matches and not underpowered
    statistics: list[MetricStatistic] = []
    for metric in sorted(spec.metrics, key=lambda item: item.name):
        for stratum in sorted(spec.strata, key=lambda item: item.name):
            gate_name = _statistical_gate_name(metric, stratum.name)
            if not can_score:
                gates.append(
                    _gate(gate_name, GateStatus.INCONCLUSIVE, "paired block statistic is unavailable")
                )
                continue
            statistic, status, reason = _score_metric(
                spec,
                observations,
                metric,
                stratum.name,
                set(stratum.case_ids),
                allocation,
            )
            statistics.append(statistic)
            gates.append(_gate(gate_name, status, reason))

    verdict = _combine_gate_statuses(gates)
    return ReferenceVerdict(
        verdict=verdict,
        evidence_role=role,
        estimator_version=spec.estimator_version,
        policy_digest=spec.policy_digest,
        coordinates=coordinates,
        gates=tuple(gates),
        statistics=tuple(statistics),
        ledger=transition,
        evidence_root=_evidence_root(dataset, role),
        production_activation=Verdict.INCONCLUSIVE,
        production_activation_reason="synthetic fixture thresholds are not production calibration",
    )


def _validate_context(
    spec: FrozenEvaluationSpec,
    dataset: Dataset,
    ledger: LedgerState,
    coordinates: DecisionCoordinates,
    role: LedgerRole,
) -> None:
    if dataset.cohort_id != spec.cohort_id:
        raise SchemaError("dataset cohort does not match FrozenEvaluationSpec")
    if ledger.cohort_id != spec.cohort_id or ledger.role is not role:
        raise SchemaError("ledger is not the Store-global ledger for this cohort and evidence role")
    limits = {
        "candidate": spec.multiplicity_family.candidates,
        "round": spec.multiplicity_family.rounds,
        "composite": spec.multiplicity_family.composites,
        "fallback": spec.multiplicity_family.fallbacks,
        "campaign": spec.multiplicity_family.campaigns,
        "activation_attempt": spec.multiplicity_family.activation_attempts,
    }
    for field_name, limit in limits.items():
        value = getattr(coordinates, field_name)
        if value < 0 or value >= limit:
            raise SchemaError(f"DecisionCoordinates.{field_name} is outside the frozen family")


def _reserve_ledger(
    spec: FrozenEvaluationSpec,
    allocation: LedgerAllocation,
    ledger: LedgerState,
    role: LedgerRole,
) -> LedgerTransition:
    query_debit = spec.multiplicity_family.hypotheses_per_decision
    error_debit = query_debit * allocation.error_units_per_hypothesis
    has_capacity = (
        ledger.queries_used + query_debit <= allocation.query_limit
        and ledger.error_units_used + error_debit <= allocation.error_units_limit
    )
    if role is LedgerRole.FINAL_AUDIT and ledger.audit_epoch_burned:
        has_capacity = False
    if not has_capacity:
        return LedgerTransition(ledger, 0, 0, ledger)
    after = replace(
        ledger,
        queries_used=ledger.queries_used + query_debit,
        error_units_used=ledger.error_units_used + error_debit,
        audit_epoch_burned=ledger.audit_epoch_burned or role is LedgerRole.FINAL_AUDIT,
    )
    return LedgerTransition(ledger, query_debit, error_debit, after)


def _ledger_exhausted_verdict(
    spec: FrozenEvaluationSpec,
    dataset: Dataset,
    coordinates: DecisionCoordinates,
    role: LedgerRole,
    transition: LedgerTransition,
) -> ReferenceVerdict:
    reason = (
        "final-audit epoch was already burned"
        if role is LedgerRole.FINAL_AUDIT and transition.before.audit_epoch_burned
        else "Store-global query or error ledger has no allocation for this decision"
    )
    return ReferenceVerdict(
        verdict=Verdict.INCONCLUSIVE,
        evidence_role=role,
        estimator_version=spec.estimator_version,
        policy_digest=spec.policy_digest,
        coordinates=coordinates,
        gates=(_gate("ledger_capacity", GateStatus.INCONCLUSIVE, reason),),
        statistics=(),
        ledger=transition,
        evidence_root=_evidence_root(dataset, role),
        production_activation=Verdict.INCONCLUSIVE,
        production_activation_reason="synthetic fixture thresholds are not production calibration",
    )


def _partition_commitment(spec: FrozenEvaluationSpec, dataset: Dataset, role: LedgerRole) -> str:
    if role is LedgerRole.ADAPTIVE_PROMOTION:
        assert isinstance(dataset, AdaptivePromotionDataset)
        return dataset.adaptive_partition_commitment
    assert isinstance(dataset, FinalAuditDataset)
    return dataset.final_partition_commitment


def _expected_partition(spec: FrozenEvaluationSpec, role: LedgerRole) -> str:
    if role is LedgerRole.ADAPTIVE_PROMOTION:
        return spec.adaptive_partition_commitment
    return spec.final_partition_commitment


def _drift_observations(
    spec: FrozenEvaluationSpec, observations: tuple[TrialObservation, ...]
) -> list[TrialObservation]:
    blocks = {case.case_id: case.block_id for case in spec.cases}
    return [
        observation
        for observation in observations
        if observation.model_digest != spec.model_digest
        or observation.protocol_digest != spec.protocol_digest
        or observation.evaluator_digest != spec.evaluator_digest
        or observation.environment_digest != spec.environment_digest
        or blocks.get(observation.case_id) != observation.block_id
    ]


def _score_metric(
    spec: FrozenEvaluationSpec,
    observations: dict[tuple[str, int, str], TrialObservation],
    metric: MetricSpec,
    stratum_name: str,
    case_ids: set[str],
    allocation: LedgerAllocation,
) -> tuple[MetricStatistic, GateStatus, str]:
    cases = {case.case_id: case for case in spec.cases if case.case_id in case_ids}
    case_effects: dict[str, Decimal] = {}
    for case_id in sorted(cases):
        repeat_effects = []
        for repeat in range(spec.repeats):
            parent = observations[(case_id, repeat, Side.PARENT.value)]
            candidate = observations[(case_id, repeat, Side.CANDIDATE.value)]
            repeat_effects.append(_score(candidate, metric.name) - _score(parent, metric.name))
        case_effects[case_id] = sum(repeat_effects, Decimal(0)) / Decimal(spec.repeats)

    by_block: dict[str, list[Decimal]] = defaultdict(list)
    for case_id, effect in case_effects.items():
        by_block[cases[case_id].block_id].append(effect)
    block_effects = {
        block_id: sum(effects, Decimal(0)) / Decimal(len(effects))
        for block_id, effects in sorted(by_block.items())
    }
    values = tuple(block_effects.values())
    observed = sum(values, Decimal(0)) / Decimal(len(values))
    adjusted_values = tuple(
        value - metric.margin if metric.kind is MetricKind.PRIMARY else value + metric.margin
        for value in values
    )
    adjusted = sum(adjusted_values, Decimal(0)) / Decimal(len(adjusted_values))
    p_value = _exact_sign_flip_p_value(adjusted_values)
    alpha = Fraction(allocation.error_units_per_hypothesis, allocation.error_scale)
    verified = adjusted > 0 and p_value <= alpha
    if verified:
        status = GateStatus.VERIFIED
        reason = "adjusted block effect is positive at the frozen exact-sign allocation"
    else:
        status = GateStatus.NOT_VERIFIED
        reason = "effect margin or frozen exact-sign allocation was not satisfied"
    statistic = MetricStatistic(
        metric=metric.name,
        stratum=stratum_name,
        case_effects=tuple(sorted(case_effects.items())),
        block_effects=tuple(sorted(block_effects.items())),
        observed_effect=observed,
        adjusted_effect=adjusted,
        minimum_block_effect=min(values),
        maximum_block_effect=max(values),
        p_value=_fraction_text(p_value),
        alpha=_fraction_text(alpha),
    )
    return statistic, status, reason


def _score(observation: TrialObservation, metric_name: str) -> Decimal:
    if isinstance(observation.outcome, BehavioralFailure):
        return Decimal(0)
    if not isinstance(observation.outcome, Pass):
        raise AssertionError("InfrastructureUnknown must be excluded before scoring")
    scores = dict(observation.outcome.metrics)
    if metric_name not in scores:
        raise SchemaError(f"Pass outcome is missing frozen metric {metric_name}")
    return scores[metric_name]


def _exact_sign_flip_p_value(values: tuple[Decimal, ...]) -> Fraction:
    observed_sum = sum(values, Decimal(0))
    extreme = 0
    total = 1 << len(values)
    for signs in itertools.product((Decimal(-1), Decimal(1)), repeat=len(values)):
        permuted_sum = sum((sign * value for sign, value in zip(signs, values)), Decimal(0))
        if permuted_sum >= observed_sum:
            extreme += 1
    return Fraction(extreme, total)


def _statistical_gate_name(metric: MetricSpec, stratum: str) -> str:
    prefix = "useful_effect" if metric.kind is MetricKind.PRIMARY else "protected_noninferiority"
    return f"{prefix}:{metric.name}:{stratum}"


def _fraction_text(value: Fraction) -> str:
    return f"{value.numerator}/{value.denominator}"


def _gate(name: str, status: GateStatus, reason: str) -> GateResult:
    return GateResult(name, status, reason)


def _combine_gate_statuses(gates: list[GateResult]) -> Verdict:
    statuses = {gate.status for gate in gates}
    if GateStatus.INCONCLUSIVE in statuses:
        return Verdict.INCONCLUSIVE
    if GateStatus.NOT_VERIFIED in statuses:
        return Verdict.NOT_VERIFIED
    return Verdict.VERIFIED


def _evidence_root(dataset: Dataset, role: LedgerRole) -> str:
    if role is LedgerRole.ADAPTIVE_PROMOTION:
        assert isinstance(dataset, AdaptivePromotionDataset)
        partition = dataset.adaptive_partition_commitment
        role_fields: dict[str, object] = {}
    else:
        assert isinstance(dataset, FinalAuditDataset)
        partition = dataset.final_partition_commitment
        role_fields = {"audit_epoch": dataset.audit_epoch, "access_receipt": dataset.access_receipt}
    return canonical_digest(
        {
            "role": role,
            "cohort_id": dataset.cohort_id,
            "partition_commitment": partition,
            "observations": [
                item.to_mapping() for item in sorted(dataset.observations, key=lambda observation: observation.key())
            ],
            "hard_gate_evidence": [
                item.to_mapping() for item in sorted(dataset.hard_gate_evidence, key=lambda gate: gate.name)
            ],
            **role_fields,
        }
    )
