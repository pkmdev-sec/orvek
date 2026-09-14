"""Strict, implementation-independent schemas for Phase 1 evaluation evidence."""

from __future__ import annotations

import hashlib
import json
import re
import unicodedata
from dataclasses import dataclass
from decimal import Decimal, InvalidOperation
from enum import Enum
from typing import Any, Mapping, Sequence


class SchemaError(ValueError):
    """Raised when reference-oracle input is not canonical or well formed."""


class Verdict(str, Enum):
    VERIFIED = "VERIFIED"
    NOT_VERIFIED = "NOT_VERIFIED"
    INCONCLUSIVE = "INCONCLUSIVE"


class GateStatus(str, Enum):
    VERIFIED = "VERIFIED"
    NOT_VERIFIED = "NOT_VERIFIED"
    INCONCLUSIVE = "INCONCLUSIVE"


class Side(str, Enum):
    PARENT = "parent"
    CANDIDATE = "candidate"


class MetricKind(str, Enum):
    PRIMARY = "primary"
    PROTECTED = "protected"


class LedgerRole(str, Enum):
    ADAPTIVE_PROMOTION = "adaptive_promotion"
    FINAL_AUDIT = "final_audit"


BEHAVIORAL_FAILURE_REASONS = frozenset(
    {
        "attributable_candidate_crash",
        "budget_exhaustion",
        "evaluator_rejection",
        "protocol_timeout",
    }
)
INFRASTRUCTURE_UNKNOWN_REASONS = frozenset(
    {
        "environment_drift",
        "evaluator_drift",
        "missing_receipt",
        "model_drift",
        "tampered_receipt",
        "transport_loss",
        "unreconciled_external_attempt",
    }
)
SYNTHETIC_CALIBRATION = "SYNTHETIC_FIXTURE_ONLY"
ESTIMATOR_VERSION = "paired-case-block-exact-sign-v1"
_COMMITMENT_RE = re.compile(r"^hmac-sha256:[0-9a-f]{64}$")


def _mapping(value: Any, context: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise SchemaError(f"{context} must be an object")
    return value


def _sequence(value: Any, context: str) -> Sequence[Any]:
    if isinstance(value, (str, bytes)) or not isinstance(value, Sequence):
        raise SchemaError(f"{context} must be an array")
    return value


def _keys(
    value: Mapping[str, Any], required: set[str], context: str, optional: set[str] | None = None
) -> None:
    optional = optional or set()
    actual = set(value)
    missing = required - actual
    unknown = actual - required - optional
    if missing:
        raise SchemaError(f"{context} is missing fields: {', '.join(sorted(missing))}")
    if unknown:
        raise SchemaError(f"{context} has unknown fields: {', '.join(sorted(unknown))}")


def _text(value: Any, context: str) -> str:
    if not isinstance(value, str) or not value or len(value) > 512:
        raise SchemaError(f"{context} must be a non-empty string of at most 512 characters")
    if any(unicodedata.category(character) == "Cc" for character in value):
        raise SchemaError(f"{context} must not contain control characters")
    return unicodedata.normalize("NFC", value)


def _positive_int(value: Any, context: str, *, allow_zero: bool = False) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise SchemaError(f"{context} must be an integer")
    minimum = 0 if allow_zero else 1
    if value < minimum:
        raise SchemaError(f"{context} must be at least {minimum}")
    return value


def _decimal(value: Any, context: str) -> Decimal:
    if isinstance(value, bool) or not isinstance(value, (str, int, Decimal)):
        raise SchemaError(f"{context} must be an exact decimal string or integer")
    try:
        result = Decimal(value)
    except (InvalidOperation, ValueError) as error:
        raise SchemaError(f"{context} is not a valid decimal") from error
    if not result.is_finite():
        raise SchemaError(f"{context} must be finite")
    return result


def _enum(enum_type: type[Enum], value: Any, context: str) -> Any:
    try:
        return enum_type(value)
    except (TypeError, ValueError) as error:
        choices = ", ".join(member.value for member in enum_type)
        raise SchemaError(f"{context} must be one of: {choices}") from error


def _commitment(value: Any, context: str) -> str:
    result = _text(value, context)
    if not _COMMITMENT_RE.fullmatch(result):
        raise SchemaError(f"{context} must be a keyed hmac-sha256 commitment")
    return result


def decimal_text(value: Decimal) -> str:
    if value == 0:
        return "0"
    normalized = value.normalize()
    return format(normalized, "f")


def _normalize(value: Any) -> Any:
    if hasattr(value, "to_mapping"):
        return _normalize(value.to_mapping())
    if isinstance(value, Enum):
        return value.value
    if isinstance(value, Decimal):
        return decimal_text(value)
    if isinstance(value, Mapping):
        normalized: dict[str, Any] = {}
        for raw_key, raw_value in value.items():
            key = unicodedata.normalize("NFC", str(raw_key))
            if key in normalized:
                raise SchemaError(f"canonical object contains duplicate normalized key {key!r}")
            normalized[key] = _normalize(raw_value)
        return {key: normalized[key] for key in sorted(normalized)}
    if isinstance(value, (list, tuple)):
        return [_normalize(item) for item in value]
    if isinstance(value, str):
        return unicodedata.normalize("NFC", value)
    if value is None or isinstance(value, (bool, int)):
        return value
    if isinstance(value, float):
        raise SchemaError("canonical JSON forbids binary floating-point values")
    raise SchemaError(f"cannot canonicalize {type(value).__name__}")


def canonical_json(value: Any) -> str:
    """Return normalized UTF-8 JSON with sorted keys and no insignificant whitespace."""

    return json.dumps(
        _normalize(value),
        ensure_ascii=False,
        allow_nan=False,
        separators=(",", ":"),
        sort_keys=True,
    )


def canonical_digest(value: Any) -> str:
    return hashlib.sha256(canonical_json(value).encode("utf-8")).hexdigest()


@dataclass(frozen=True)
class Pass:
    metrics: tuple[tuple[str, Decimal], ...]

    def __post_init__(self) -> None:
        names = [name for name, _ in self.metrics]
        if len(names) != len(set(names)):
            raise SchemaError("Pass.metrics contains duplicate metric names")
        for name, score in self.metrics:
            _text(name, "Pass metric name")
            if not isinstance(score, Decimal) or not score.is_finite() or not Decimal(0) <= score <= Decimal(1):
                raise SchemaError("Pass metric scores must be finite Decimals in [0, 1]")

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> Pass:
        value = _mapping(value, "Pass")
        _keys(value, {"kind", "metrics"}, "Pass")
        if value["kind"] != "Pass":
            raise SchemaError("Pass.kind must be 'Pass'")
        metrics = _mapping(value["metrics"], "Pass.metrics")
        parsed = tuple(
            sorted(
                (_text(name, "Pass metric name"), _decimal(score, f"Pass.metrics.{name}"))
                for name, score in metrics.items()
            )
        )
        return cls(parsed)

    def to_mapping(self) -> dict[str, Any]:
        return {"kind": "Pass", "metrics": {name: score for name, score in sorted(self.metrics)}}


@dataclass(frozen=True)
class BehavioralFailure:
    reason: str

    def __post_init__(self) -> None:
        if self.reason not in BEHAVIORAL_FAILURE_REASONS:
            raise SchemaError(f"unknown BehavioralFailure reason: {self.reason}")

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> BehavioralFailure:
        value = _mapping(value, "BehavioralFailure")
        _keys(value, {"kind", "reason"}, "BehavioralFailure")
        if value["kind"] != "BehavioralFailure":
            raise SchemaError("BehavioralFailure.kind must be 'BehavioralFailure'")
        return cls(_text(value["reason"], "BehavioralFailure.reason"))

    def to_mapping(self) -> dict[str, Any]:
        return {"kind": "BehavioralFailure", "reason": self.reason}


@dataclass(frozen=True)
class InfrastructureUnknown:
    reason: str

    def __post_init__(self) -> None:
        if self.reason not in INFRASTRUCTURE_UNKNOWN_REASONS:
            raise SchemaError(f"unknown InfrastructureUnknown reason: {self.reason}")

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> InfrastructureUnknown:
        value = _mapping(value, "InfrastructureUnknown")
        _keys(value, {"kind", "reason"}, "InfrastructureUnknown")
        if value["kind"] != "InfrastructureUnknown":
            raise SchemaError("InfrastructureUnknown.kind must be 'InfrastructureUnknown'")
        return cls(_text(value["reason"], "InfrastructureUnknown.reason"))

    def to_mapping(self) -> dict[str, Any]:
        return {"kind": "InfrastructureUnknown", "reason": self.reason}


TrialOutcome = Pass | BehavioralFailure | InfrastructureUnknown


def parse_outcome(value: Mapping[str, Any]) -> TrialOutcome:
    value = _mapping(value, "trial outcome")
    kind = value.get("kind")
    if kind == "Pass":
        return Pass.from_mapping(value)
    if kind == "BehavioralFailure":
        return BehavioralFailure.from_mapping(value)
    if kind == "InfrastructureUnknown":
        return InfrastructureUnknown.from_mapping(value)
    raise SchemaError("trial outcome kind must be Pass, BehavioralFailure, or InfrastructureUnknown")


@dataclass(frozen=True)
class TrialObservation:
    case_id: str
    repeat: int
    block_id: str
    side: Side
    model_digest: str
    protocol_digest: str
    evaluator_digest: str
    environment_digest: str
    outcome: TrialOutcome

    def __post_init__(self) -> None:
        _text(self.case_id, "TrialObservation.case_id")
        _positive_int(self.repeat, "TrialObservation.repeat", allow_zero=True)
        _text(self.block_id, "TrialObservation.block_id")
        if not isinstance(self.side, Side):
            raise SchemaError("TrialObservation.side must be a Side")
        for field_name in ("model_digest", "protocol_digest", "evaluator_digest", "environment_digest"):
            _text(getattr(self, field_name), f"TrialObservation.{field_name}")
        if not isinstance(self.outcome, (Pass, BehavioralFailure, InfrastructureUnknown)):
            raise SchemaError("TrialObservation.outcome has an unsupported type")

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> TrialObservation:
        value = _mapping(value, "TrialObservation")
        fields = {
            "case_id",
            "repeat",
            "block_id",
            "side",
            "model_digest",
            "protocol_digest",
            "evaluator_digest",
            "environment_digest",
            "outcome",
        }
        _keys(value, fields, "TrialObservation")
        return cls(
            case_id=_text(value["case_id"], "TrialObservation.case_id"),
            repeat=_positive_int(value["repeat"], "TrialObservation.repeat", allow_zero=True),
            block_id=_text(value["block_id"], "TrialObservation.block_id"),
            side=_enum(Side, value["side"], "TrialObservation.side"),
            model_digest=_text(value["model_digest"], "TrialObservation.model_digest"),
            protocol_digest=_text(value["protocol_digest"], "TrialObservation.protocol_digest"),
            evaluator_digest=_text(value["evaluator_digest"], "TrialObservation.evaluator_digest"),
            environment_digest=_text(value["environment_digest"], "TrialObservation.environment_digest"),
            outcome=parse_outcome(value["outcome"]),
        )

    def key(self) -> tuple[str, int, str]:
        return (self.case_id, self.repeat, self.side.value)

    def to_mapping(self) -> dict[str, Any]:
        return {
            "case_id": self.case_id,
            "repeat": self.repeat,
            "block_id": self.block_id,
            "side": self.side,
            "model_digest": self.model_digest,
            "protocol_digest": self.protocol_digest,
            "evaluator_digest": self.evaluator_digest,
            "environment_digest": self.environment_digest,
            "outcome": self.outcome,
        }


@dataclass(frozen=True)
class GateEvidence:
    name: str
    status: GateStatus
    reason: str

    def __post_init__(self) -> None:
        _text(self.name, "GateEvidence.name")
        if not isinstance(self.status, GateStatus):
            raise SchemaError("GateEvidence.status must be a GateStatus")
        _text(self.reason, "GateEvidence.reason")

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> GateEvidence:
        value = _mapping(value, "GateEvidence")
        _keys(value, {"name", "status", "reason"}, "GateEvidence")
        return cls(
            _text(value["name"], "GateEvidence.name"),
            _enum(GateStatus, value["status"], "GateEvidence.status"),
            _text(value["reason"], "GateEvidence.reason"),
        )

    def to_mapping(self) -> dict[str, Any]:
        return {"name": self.name, "status": self.status, "reason": self.reason}


@dataclass(frozen=True)
class MiningEvaluation:
    cohort_id: str
    mining_partition_commitment: str
    sanitized_failure_facts: tuple[str, ...]
    pass_anchor_ids: tuple[str, ...]

    def __post_init__(self) -> None:
        _text(self.cohort_id, "MiningEvaluation.cohort_id")
        _commitment(
            self.mining_partition_commitment,
            "MiningEvaluation.mining_partition_commitment",
        )
        for fact in self.sanitized_failure_facts:
            _text(fact, "sanitized failure fact")
        for anchor_id in self.pass_anchor_ids:
            _text(anchor_id, "pass anchor id")
        if len(self.sanitized_failure_facts) != len(set(self.sanitized_failure_facts)):
            raise SchemaError("MiningEvaluation contains duplicate sanitized failure facts")
        if len(self.pass_anchor_ids) != len(set(self.pass_anchor_ids)):
            raise SchemaError("MiningEvaluation contains duplicate pass anchor IDs")

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> MiningEvaluation:
        value = _mapping(value, "MiningEvaluation")
        _keys(
            value,
            {"schema", "cohort_id", "mining_partition_commitment", "sanitized_failure_facts", "pass_anchor_ids"},
            "MiningEvaluation",
        )
        if value["schema"] != "MiningEvaluation":
            raise SchemaError("MiningEvaluation.schema is invalid")
        return cls(
            _text(value["cohort_id"], "MiningEvaluation.cohort_id"),
            _commitment(value["mining_partition_commitment"], "MiningEvaluation.mining_partition_commitment"),
            tuple(_text(item, "sanitized failure fact") for item in _sequence(value["sanitized_failure_facts"], "sanitized_failure_facts")),
            tuple(_text(item, "pass anchor id") for item in _sequence(value["pass_anchor_ids"], "pass_anchor_ids")),
        )

    def to_mapping(self) -> dict[str, Any]:
        return {
            "schema": "MiningEvaluation",
            "cohort_id": self.cohort_id,
            "mining_partition_commitment": self.mining_partition_commitment,
            "sanitized_failure_facts": sorted(self.sanitized_failure_facts),
            "pass_anchor_ids": sorted(self.pass_anchor_ids),
        }


def _validate_dataset(observations: tuple[TrialObservation, ...], gates: tuple[GateEvidence, ...]) -> None:
    observation_keys = [observation.key() for observation in observations]
    if len(observation_keys) != len(set(observation_keys)):
        raise SchemaError("dataset contains duplicate case/repeat/side observations")
    gate_names = [gate.name for gate in gates]
    if len(gate_names) != len(set(gate_names)):
        raise SchemaError("dataset contains duplicate hard-gate evidence")


@dataclass(frozen=True)
class AdaptivePromotionDataset:
    cohort_id: str
    adaptive_partition_commitment: str
    candidate_label: str
    observations: tuple[TrialObservation, ...]
    hard_gate_evidence: tuple[GateEvidence, ...]

    def __post_init__(self) -> None:
        _text(self.cohort_id, "AdaptivePromotionDataset.cohort_id")
        _commitment(self.adaptive_partition_commitment, "AdaptivePromotionDataset.adaptive_partition_commitment")
        _text(self.candidate_label, "AdaptivePromotionDataset.candidate_label")
        _validate_dataset(self.observations, self.hard_gate_evidence)

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> AdaptivePromotionDataset:
        value = _mapping(value, "AdaptivePromotionDataset")
        fields = {
            "schema",
            "cohort_id",
            "adaptive_partition_commitment",
            "candidate_label",
            "observations",
            "hard_gate_evidence",
        }
        _keys(value, fields, "AdaptivePromotionDataset")
        if value["schema"] != "AdaptivePromotionDataset":
            raise SchemaError("AdaptivePromotionDataset.schema is invalid")
        return cls(
            _text(value["cohort_id"], "AdaptivePromotionDataset.cohort_id"),
            _commitment(value["adaptive_partition_commitment"], "AdaptivePromotionDataset.adaptive_partition_commitment"),
            _text(value["candidate_label"], "AdaptivePromotionDataset.candidate_label"),
            tuple(TrialObservation.from_mapping(item) for item in _sequence(value["observations"], "observations")),
            tuple(GateEvidence.from_mapping(item) for item in _sequence(value["hard_gate_evidence"], "hard_gate_evidence")),
        )

    def to_mapping(self) -> dict[str, Any]:
        return {
            "schema": "AdaptivePromotionDataset",
            "cohort_id": self.cohort_id,
            "adaptive_partition_commitment": self.adaptive_partition_commitment,
            "candidate_label": self.candidate_label,
            "observations": [item.to_mapping() for item in self.observations],
            "hard_gate_evidence": [item.to_mapping() for item in self.hard_gate_evidence],
        }


@dataclass(frozen=True)
class FinalAuditDataset:
    cohort_id: str
    final_partition_commitment: str
    audit_epoch: str
    access_receipt: str
    candidate_label: str
    observations: tuple[TrialObservation, ...]
    hard_gate_evidence: tuple[GateEvidence, ...]

    def __post_init__(self) -> None:
        _text(self.cohort_id, "FinalAuditDataset.cohort_id")
        _commitment(self.final_partition_commitment, "FinalAuditDataset.final_partition_commitment")
        _text(self.audit_epoch, "FinalAuditDataset.audit_epoch")
        _text(self.access_receipt, "FinalAuditDataset.access_receipt")
        _text(self.candidate_label, "FinalAuditDataset.candidate_label")
        _validate_dataset(self.observations, self.hard_gate_evidence)

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> FinalAuditDataset:
        value = _mapping(value, "FinalAuditDataset")
        fields = {
            "schema",
            "cohort_id",
            "final_partition_commitment",
            "audit_epoch",
            "access_receipt",
            "candidate_label",
            "observations",
            "hard_gate_evidence",
        }
        _keys(value, fields, "FinalAuditDataset")
        if value["schema"] != "FinalAuditDataset":
            raise SchemaError("FinalAuditDataset.schema is invalid")
        return cls(
            _text(value["cohort_id"], "FinalAuditDataset.cohort_id"),
            _commitment(value["final_partition_commitment"], "FinalAuditDataset.final_partition_commitment"),
            _text(value["audit_epoch"], "FinalAuditDataset.audit_epoch"),
            _text(value["access_receipt"], "FinalAuditDataset.access_receipt"),
            _text(value["candidate_label"], "FinalAuditDataset.candidate_label"),
            tuple(TrialObservation.from_mapping(item) for item in _sequence(value["observations"], "observations")),
            tuple(GateEvidence.from_mapping(item) for item in _sequence(value["hard_gate_evidence"], "hard_gate_evidence")),
        )

    def to_mapping(self) -> dict[str, Any]:
        return {
            "schema": "FinalAuditDataset",
            "cohort_id": self.cohort_id,
            "final_partition_commitment": self.final_partition_commitment,
            "audit_epoch": self.audit_epoch,
            "access_receipt": self.access_receipt,
            "candidate_label": self.candidate_label,
            "observations": [item.to_mapping() for item in self.observations],
            "hard_gate_evidence": [item.to_mapping() for item in self.hard_gate_evidence],
        }


@dataclass(frozen=True)
class CaseSpec:
    case_id: str
    block_id: str
    critical: bool

    def __post_init__(self) -> None:
        _text(self.case_id, "CaseSpec.case_id")
        _text(self.block_id, "CaseSpec.block_id")
        if not isinstance(self.critical, bool):
            raise SchemaError("CaseSpec.critical must be a boolean")

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> CaseSpec:
        value = _mapping(value, "CaseSpec")
        _keys(value, {"case_id", "block_id", "critical"}, "CaseSpec")
        if not isinstance(value["critical"], bool):
            raise SchemaError("CaseSpec.critical must be a boolean")
        return cls(_text(value["case_id"], "CaseSpec.case_id"), _text(value["block_id"], "CaseSpec.block_id"), value["critical"])

    def to_mapping(self) -> dict[str, Any]:
        return {"case_id": self.case_id, "block_id": self.block_id, "critical": self.critical}


@dataclass(frozen=True)
class StratumSpec:
    name: str
    case_ids: tuple[str, ...]

    def __post_init__(self) -> None:
        _text(self.name, "StratumSpec.name")
        for case_id in self.case_ids:
            _text(case_id, "StratumSpec case id")
        if not self.case_ids or len(self.case_ids) != len(set(self.case_ids)):
            raise SchemaError("StratumSpec must contain unique case IDs")

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> StratumSpec:
        value = _mapping(value, "StratumSpec")
        _keys(value, {"name", "case_ids"}, "StratumSpec")
        return cls(
            _text(value["name"], "StratumSpec.name"),
            tuple(_text(item, "StratumSpec case id") for item in _sequence(value["case_ids"], "StratumSpec.case_ids")),
        )

    def to_mapping(self) -> dict[str, Any]:
        return {"name": self.name, "case_ids": sorted(self.case_ids)}


@dataclass(frozen=True)
class MetricSpec:
    name: str
    kind: MetricKind
    margin: Decimal

    def __post_init__(self) -> None:
        _text(self.name, "MetricSpec.name")
        if not isinstance(self.kind, MetricKind):
            raise SchemaError("MetricSpec.kind must be a MetricKind")
        if not isinstance(self.margin, Decimal) or not self.margin.is_finite():
            raise SchemaError("MetricSpec.margin must be a finite Decimal")
        if not Decimal(0) <= self.margin <= Decimal(1):
            raise SchemaError("MetricSpec.margin must be in [0, 1]")

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> MetricSpec:
        value = _mapping(value, "MetricSpec")
        _keys(value, {"name", "kind", "margin"}, "MetricSpec")
        return cls(
            _text(value["name"], "MetricSpec.name"),
            _enum(MetricKind, value["kind"], "MetricSpec.kind"),
            _decimal(value["margin"], "MetricSpec.margin"),
        )

    def to_mapping(self) -> dict[str, Any]:
        return {"name": self.name, "kind": self.kind, "margin": self.margin}


@dataclass(frozen=True)
class MultiplicityFamily:
    candidates: int
    rounds: int
    metrics: int
    strata: int
    composites: int
    fallbacks: int
    campaigns: int
    activation_attempts: int

    def __post_init__(self) -> None:
        for field_name in self.__dataclass_fields__:
            _positive_int(getattr(self, field_name), f"MultiplicityFamily.{field_name}")

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> MultiplicityFamily:
        value = _mapping(value, "MultiplicityFamily")
        fields = set(cls.__dataclass_fields__)
        _keys(value, fields, "MultiplicityFamily")
        return cls(**{name: _positive_int(value[name], f"MultiplicityFamily.{name}") for name in fields})

    @property
    def hypotheses_per_decision(self) -> int:
        return self.metrics * self.strata

    @property
    def decision_count(self) -> int:
        return (
            self.candidates
            * self.rounds
            * self.composites
            * self.fallbacks
            * self.campaigns
            * self.activation_attempts
        )

    @property
    def size(self) -> int:
        return self.decision_count * self.hypotheses_per_decision

    def to_mapping(self) -> dict[str, Any]:
        return {name: getattr(self, name) for name in sorted(self.__dataclass_fields__)}


@dataclass(frozen=True)
class DecisionCoordinates:
    candidate: int
    round: int
    composite: int
    fallback: int
    campaign: int
    activation_attempt: int

    def __post_init__(self) -> None:
        for field_name in self.__dataclass_fields__:
            _positive_int(
                getattr(self, field_name),
                f"DecisionCoordinates.{field_name}",
                allow_zero=True,
            )

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> DecisionCoordinates:
        value = _mapping(value, "DecisionCoordinates")
        fields = set(cls.__dataclass_fields__)
        _keys(value, fields, "DecisionCoordinates")
        return cls(**{name: _positive_int(value[name], f"DecisionCoordinates.{name}", allow_zero=True) for name in fields})

    def to_mapping(self) -> dict[str, Any]:
        return {name: getattr(self, name) for name in sorted(self.__dataclass_fields__)}


@dataclass(frozen=True)
class LedgerAllocation:
    role: LedgerRole
    query_limit: int
    error_units_limit: int
    error_scale: int
    error_units_per_hypothesis: int

    def __post_init__(self) -> None:
        if not isinstance(self.role, LedgerRole):
            raise SchemaError("LedgerAllocation.role must be a LedgerRole")
        for field_name in ("query_limit", "error_units_limit", "error_scale", "error_units_per_hypothesis"):
            _positive_int(getattr(self, field_name), f"LedgerAllocation.{field_name}")
        if self.error_units_limit > self.error_scale:
            raise SchemaError("LedgerAllocation.error_units_limit cannot exceed error_scale")

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> LedgerAllocation:
        value = _mapping(value, "LedgerAllocation")
        fields = {"role", "query_limit", "error_units_limit", "error_scale", "error_units_per_hypothesis"}
        _keys(value, fields, "LedgerAllocation")
        return cls(
            _enum(LedgerRole, value["role"], "LedgerAllocation.role"),
            _positive_int(value["query_limit"], "LedgerAllocation.query_limit"),
            _positive_int(value["error_units_limit"], "LedgerAllocation.error_units_limit"),
            _positive_int(value["error_scale"], "LedgerAllocation.error_scale"),
            _positive_int(value["error_units_per_hypothesis"], "LedgerAllocation.error_units_per_hypothesis"),
        )

    def to_mapping(self) -> dict[str, Any]:
        return {
            "role": self.role,
            "query_limit": self.query_limit,
            "error_units_limit": self.error_units_limit,
            "error_scale": self.error_scale,
            "error_units_per_hypothesis": self.error_units_per_hypothesis,
        }


@dataclass(frozen=True)
class LedgerState:
    cohort_id: str
    role: LedgerRole
    queries_used: int
    error_units_used: int
    audit_epoch_burned: bool = False

    def __post_init__(self) -> None:
        _text(self.cohort_id, "LedgerState.cohort_id")
        if not isinstance(self.role, LedgerRole):
            raise SchemaError("LedgerState.role must be a LedgerRole")
        _positive_int(self.queries_used, "LedgerState.queries_used", allow_zero=True)
        _positive_int(self.error_units_used, "LedgerState.error_units_used", allow_zero=True)
        if not isinstance(self.audit_epoch_burned, bool):
            raise SchemaError("LedgerState.audit_epoch_burned must be a boolean")

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> LedgerState:
        value = _mapping(value, "LedgerState")
        fields = {"cohort_id", "role", "queries_used", "error_units_used", "audit_epoch_burned"}
        _keys(value, fields, "LedgerState")
        if not isinstance(value["audit_epoch_burned"], bool):
            raise SchemaError("LedgerState.audit_epoch_burned must be a boolean")
        return cls(
            _text(value["cohort_id"], "LedgerState.cohort_id"),
            _enum(LedgerRole, value["role"], "LedgerState.role"),
            _positive_int(value["queries_used"], "LedgerState.queries_used", allow_zero=True),
            _positive_int(value["error_units_used"], "LedgerState.error_units_used", allow_zero=True),
            value["audit_epoch_burned"],
        )

    def to_mapping(self) -> dict[str, Any]:
        return {
            "cohort_id": self.cohort_id,
            "role": self.role,
            "queries_used": self.queries_used,
            "error_units_used": self.error_units_used,
            "audit_epoch_burned": self.audit_epoch_burned,
        }


@dataclass(frozen=True)
class FrozenEvaluationSpec:
    schema_version: int
    estimator_version: str
    calibration: str
    cohort_id: str
    model_digest: str
    protocol_digest: str
    evaluator_digest: str
    environment_digest: str
    mining_partition_commitment: str
    adaptive_partition_commitment: str
    final_partition_commitment: str
    repeats: int
    cases: tuple[CaseSpec, ...]
    strata: tuple[StratumSpec, ...]
    metrics: tuple[MetricSpec, ...]
    minimum_complete_blocks: int
    maximum_exact_blocks: int
    required_hard_gates: tuple[str, ...]
    multiplicity_family: MultiplicityFamily
    adaptive_ledger: LedgerAllocation
    final_audit_ledger: LedgerAllocation

    def __post_init__(self) -> None:
        if self.schema_version != 1:
            raise SchemaError("FrozenEvaluationSpec.schema_version must be 1")
        if self.estimator_version != ESTIMATOR_VERSION:
            raise SchemaError(f"unsupported estimator version: {self.estimator_version}")
        if self.calibration != SYNTHETIC_CALIBRATION:
            raise SchemaError("Phase 1 accepts synthetic fixture calibration only")
        for field_name in ("cohort_id", "model_digest", "protocol_digest", "evaluator_digest", "environment_digest"):
            _text(getattr(self, field_name), f"FrozenEvaluationSpec.{field_name}")
        for field_name in ("mining_partition_commitment", "adaptive_partition_commitment", "final_partition_commitment"):
            _commitment(getattr(self, field_name), f"FrozenEvaluationSpec.{field_name}")
        if len({self.mining_partition_commitment, self.adaptive_partition_commitment, self.final_partition_commitment}) != 3:
            raise SchemaError("partition commitments must be distinct")
        _positive_int(self.repeats, "FrozenEvaluationSpec.repeats")
        _positive_int(self.minimum_complete_blocks, "FrozenEvaluationSpec.minimum_complete_blocks")
        _positive_int(self.maximum_exact_blocks, "FrozenEvaluationSpec.maximum_exact_blocks")
        case_ids = [case.case_id for case in self.cases]
        if not case_ids or len(case_ids) != len(set(case_ids)):
            raise SchemaError("FrozenEvaluationSpec cases must have unique case IDs")
        stratum_names = [stratum.name for stratum in self.strata]
        if not stratum_names or len(stratum_names) != len(set(stratum_names)):
            raise SchemaError("FrozenEvaluationSpec strata must have unique names")
        known_cases = set(case_ids)
        for stratum in self.strata:
            if not stratum.case_ids or len(stratum.case_ids) != len(set(stratum.case_ids)):
                raise SchemaError(f"stratum {stratum.name} must contain unique case IDs")
            if not set(stratum.case_ids) <= known_cases:
                raise SchemaError(f"stratum {stratum.name} contains an unknown case")
        metric_names = [metric.name for metric in self.metrics]
        if not metric_names or len(metric_names) != len(set(metric_names)):
            raise SchemaError("FrozenEvaluationSpec metrics must have unique names")
        if sum(metric.kind is MetricKind.PRIMARY for metric in self.metrics) != 1:
            raise SchemaError("FrozenEvaluationSpec must have exactly one primary metric")
        for metric in self.metrics:
            if not Decimal(0) <= metric.margin <= Decimal(1):
                raise SchemaError(f"metric margin for {metric.name} must be in [0, 1]")
        gate_names = list(self.required_hard_gates)
        if not gate_names or len(gate_names) != len(set(gate_names)):
            raise SchemaError("required_hard_gates must contain unique names")
        for gate_name in gate_names:
            _text(gate_name, "required hard gate")
        family = self.multiplicity_family
        if family.metrics != len(self.metrics) or family.strata != len(self.strata):
            raise SchemaError("multiplicity metric/stratum counts must match the frozen schemas")
        for role, allocation in (
            (LedgerRole.ADAPTIVE_PROMOTION, self.adaptive_ledger),
            (LedgerRole.FINAL_AUDIT, self.final_audit_ledger),
        ):
            if allocation.role is not role:
                raise SchemaError(f"{role.value} ledger has the wrong role")
            expected_error_units = family.size * allocation.error_units_per_hypothesis
            if allocation.query_limit != family.size or allocation.error_units_limit != expected_error_units:
                raise SchemaError(f"{role.value} ledger must allocate exactly the complete multiplicity family")
        if len({case.block_id for case in self.cases}) > self.maximum_exact_blocks:
            raise SchemaError("block count exceeds maximum_exact_blocks")

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> FrozenEvaluationSpec:
        value = _mapping(value, "FrozenEvaluationSpec")
        fields = set(cls.__dataclass_fields__)
        _keys(value, fields, "FrozenEvaluationSpec")
        return cls(
            schema_version=_positive_int(value["schema_version"], "schema_version"),
            estimator_version=_text(value["estimator_version"], "estimator_version"),
            calibration=_text(value["calibration"], "calibration"),
            cohort_id=_text(value["cohort_id"], "cohort_id"),
            model_digest=_text(value["model_digest"], "model_digest"),
            protocol_digest=_text(value["protocol_digest"], "protocol_digest"),
            evaluator_digest=_text(value["evaluator_digest"], "evaluator_digest"),
            environment_digest=_text(value["environment_digest"], "environment_digest"),
            mining_partition_commitment=_commitment(value["mining_partition_commitment"], "mining_partition_commitment"),
            adaptive_partition_commitment=_commitment(value["adaptive_partition_commitment"], "adaptive_partition_commitment"),
            final_partition_commitment=_commitment(value["final_partition_commitment"], "final_partition_commitment"),
            repeats=_positive_int(value["repeats"], "repeats"),
            cases=tuple(CaseSpec.from_mapping(item) for item in _sequence(value["cases"], "cases")),
            strata=tuple(StratumSpec.from_mapping(item) for item in _sequence(value["strata"], "strata")),
            metrics=tuple(MetricSpec.from_mapping(item) for item in _sequence(value["metrics"], "metrics")),
            minimum_complete_blocks=_positive_int(value["minimum_complete_blocks"], "minimum_complete_blocks"),
            maximum_exact_blocks=_positive_int(value["maximum_exact_blocks"], "maximum_exact_blocks"),
            required_hard_gates=tuple(_text(item, "required hard gate") for item in _sequence(value["required_hard_gates"], "required_hard_gates")),
            multiplicity_family=MultiplicityFamily.from_mapping(value["multiplicity_family"]),
            adaptive_ledger=LedgerAllocation.from_mapping(value["adaptive_ledger"]),
            final_audit_ledger=LedgerAllocation.from_mapping(value["final_audit_ledger"]),
        )

    def allocation(self, role: LedgerRole) -> LedgerAllocation:
        return self.adaptive_ledger if role is LedgerRole.ADAPTIVE_PROMOTION else self.final_audit_ledger

    def policy_mapping(self) -> dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "estimator_version": self.estimator_version,
            "calibration": self.calibration,
            "cohort_id": self.cohort_id,
            "model_digest": self.model_digest,
            "protocol_digest": self.protocol_digest,
            "evaluator_digest": self.evaluator_digest,
            "environment_digest": self.environment_digest,
            "partition_commitments": {
                "mining": self.mining_partition_commitment,
                "adaptive": self.adaptive_partition_commitment,
                "final": self.final_partition_commitment,
            },
            "repeats": self.repeats,
            "cases": [case.to_mapping() for case in sorted(self.cases, key=lambda item: item.case_id)],
            "strata": [stratum.to_mapping() for stratum in sorted(self.strata, key=lambda item: item.name)],
            "metrics": [metric.to_mapping() for metric in sorted(self.metrics, key=lambda item: item.name)],
            "minimum_complete_blocks": self.minimum_complete_blocks,
            "maximum_exact_blocks": self.maximum_exact_blocks,
            "required_hard_gates": sorted(self.required_hard_gates),
            "multiplicity_family": self.multiplicity_family,
            "adaptive_ledger": self.adaptive_ledger,
            "final_audit_ledger": self.final_audit_ledger,
        }

    def to_mapping(self) -> dict[str, Any]:
        return self.policy_mapping()

    @property
    def policy_digest(self) -> str:
        return canonical_digest(self.policy_mapping())


@dataclass(frozen=True)
class GateResult:
    name: str
    status: GateStatus
    reason: str

    def to_mapping(self) -> dict[str, Any]:
        return {"name": self.name, "status": self.status, "reason": self.reason}


@dataclass(frozen=True)
class MetricStatistic:
    metric: str
    stratum: str
    case_effects: tuple[tuple[str, Decimal], ...]
    block_effects: tuple[tuple[str, Decimal], ...]
    observed_effect: Decimal
    adjusted_effect: Decimal
    minimum_block_effect: Decimal
    maximum_block_effect: Decimal
    p_value: str
    alpha: str

    def to_mapping(self) -> dict[str, Any]:
        return {
            "metric": self.metric,
            "stratum": self.stratum,
            "case_effects": {name: value for name, value in self.case_effects},
            "block_effects": {name: value for name, value in self.block_effects},
            "observed_effect": self.observed_effect,
            "adjusted_effect": self.adjusted_effect,
            "minimum_block_effect": self.minimum_block_effect,
            "maximum_block_effect": self.maximum_block_effect,
            "p_value": self.p_value,
            "alpha": self.alpha,
        }


@dataclass(frozen=True)
class LedgerTransition:
    before: LedgerState
    query_debit: int
    error_units_debit: int
    after: LedgerState

    def to_mapping(self) -> dict[str, Any]:
        return {
            "before": self.before,
            "debit": {"queries": self.query_debit, "error_units": self.error_units_debit},
            "after": self.after,
        }


@dataclass(frozen=True)
class ReferenceVerdict:
    verdict: Verdict
    evidence_role: LedgerRole
    estimator_version: str
    policy_digest: str
    coordinates: DecisionCoordinates
    gates: tuple[GateResult, ...]
    statistics: tuple[MetricStatistic, ...]
    ledger: LedgerTransition
    evidence_root: str
    production_activation: Verdict
    production_activation_reason: str

    def to_mapping(self) -> dict[str, Any]:
        return {
            "verdict": self.verdict,
            "evidence_role": self.evidence_role,
            "estimator_version": self.estimator_version,
            "policy_digest": self.policy_digest,
            "coordinates": self.coordinates,
            "gates": [gate.to_mapping() for gate in sorted(self.gates, key=lambda item: item.name)],
            "statistics": [
                statistic.to_mapping()
                for statistic in sorted(self.statistics, key=lambda item: (item.metric, item.stratum))
            ],
            "ledger": self.ledger,
            "evidence_root": self.evidence_root,
            "production_activation": self.production_activation,
            "production_activation_reason": self.production_activation_reason,
        }

    def canonical_json(self) -> str:
        return canonical_json(self.to_mapping())
