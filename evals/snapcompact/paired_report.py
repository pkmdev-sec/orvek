"""Validate paired context-evaluation records and report metrics with 95% intervals."""

import argparse
import json
import math
from collections import defaultdict
from pathlib import Path

SCHEMA_VERSION = 2
CONDITIONS = {"native", "bitmap"}
STATUSES = {"running", "completed", "failed", "interrupted", "invalid_evaluation"}
TOKEN_FIELDS = ("input_tokens", "cached_input_tokens", "output_tokens", "reasoning_tokens")
PAIR_FIELDS = (
    "model", "dataset", "fixture_revision", "task_digest", "settings_digest", "harness_build",
    "tool_access_digest", "environment_digest", "cache_condition", "generation", "branch", "run",
)
METRIC_FIELDS = (
    "provider_receipt_usd", "catalog_estimate_usd", "root_catalog_estimate_usd",
    "retrieval_count", "retrieval_failures",
    "render_ms", "request_bytes", "latency_ms", "peak_memory_bytes", "retries",
)


def _number(value, name):
    if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
        raise ValueError(f"{name} must be a finite non-negative number")
    return value


def validate(record):
    if not isinstance(record, dict) or record.get("schema_version") != SCHEMA_VERSION:
        raise ValueError("requires raw schema_version 2; legacy zero-filled records are untrusted; "
                         "reparse original evidence")
    required = set(PAIR_FIELDS) | set(METRIC_FIELDS) | {
        "condition", "task_passed", "exact_match", "usage", "attempt_id", "attempt_revision",
        "attempt_status", "exit_code", "issues", "child_outcomes",
    }
    missing = sorted(required - record.keys())
    if missing:
        raise ValueError(f"record lacks {', '.join(missing)}")
    for field in PAIR_FIELDS + ("attempt_id",):
        if field in {"generation", "run"}:
            if type(record[field]) is not int or record[field] < 0:
                raise ValueError(f"{field} must be a non-negative integer")
        elif not isinstance(record[field], str) or not record[field]:
            raise ValueError(f"{field} must be a nonempty string")
    if record["condition"] not in CONDITIONS:
        raise ValueError("condition must be native or bitmap")
    if record["cache_condition"] not in {"cold", "warm"}:
        raise ValueError("cache_condition must be cold or warm")
    if record["attempt_status"] not in STATUSES:
        raise ValueError("invalid attempt_status")
    if type(record["attempt_revision"]) is not int or record["attempt_revision"] not in {0, 1}:
        raise ValueError("attempt_revision must be 0 (admitted) or 1 (final)")
    if (record["attempt_status"] == "running") != (record["attempt_revision"] == 0):
        raise ValueError("only admitted attempts may be running")
    if record["exit_code"] is not None and type(record["exit_code"]) is not int:
        raise ValueError("exit_code must be an integer or null")
    if not isinstance(record["issues"], list) or not all(isinstance(issue, str) for issue in record["issues"]):
        raise ValueError("issues must be a list of diagnostic codes")
    if not isinstance(record["child_outcomes"], list):
        raise ValueError("child_outcomes must be a list")
    for child in record["child_outcomes"]:
        if (not isinstance(child, dict) or not isinstance(child.get("agent_id"), str)
                or child.get("status") not in {"running", "completed", "failed", "cancelled", "interrupted"}):
            raise ValueError("invalid child outcome")
    for field in ("task_passed", "exact_match"):
        if record[field] is not None and type(record[field]) is not bool:
            raise ValueError(f"{field} must be a boolean or null")
        if record["attempt_status"] == "completed" and record[field] is None:
            raise ValueError("completed attempts require evaluation results")
        if record["attempt_status"] != "completed" and record[field] is True:
            raise ValueError("unfinished/failed attempts cannot claim success")
    for field in METRIC_FIELDS:
        if record[field] is not None:
            _number(record[field], field)
    if not isinstance(record["usage"], dict) or set(record["usage"]) != {"root", "child"}:
        raise ValueError("usage must contain root and child")
    for owner in ("root", "child"):
        if not isinstance(record["usage"][owner], dict) or set(record["usage"][owner]) != set(TOKEN_FIELDS):
            raise ValueError(f"usage.{owner} has the wrong token fields")
        for field in TOKEN_FIELDS:
            value = record["usage"][owner][field]
            if value is not None and (type(value) is not int or value < 0):
                raise ValueError(f"usage.{owner}.{field} must be a non-negative integer or null")
    return record


def _wilson(successes, total):
    if not total:
        return None
    z = 1.959963984540054
    p = successes / total
    denominator = 1 + z * z / total
    center = (p + z * z / (2 * total)) / denominator
    margin = z * math.sqrt(p * (1 - p) / total + z * z / (4 * total * total)) / denominator
    return [center - margin, center + margin]


def _mean_interval(values):
    if not values:
        return None
    mean = sum(values) / len(values)
    if len(values) == 1:
        return [mean, mean]
    variance = sum((value - mean) ** 2 for value in values) / (len(values) - 1)
    margin = 1.959963984540054 * math.sqrt(variance / len(values))
    return [mean - margin, mean + margin]


def _percentile(values, percentile):
    if not values:
        return None
    ordered = sorted(values)
    rank = (len(ordered) - 1) * percentile
    low = math.floor(rank)
    high = math.ceil(rank)
    if low == high:
        return ordered[low]
    return ordered[low] + (ordered[high] - ordered[low]) * (rank - low)


def _sum_known(records, field):
    values = [record[field] for record in records]
    return sum(values) if values and all(value is not None for value in values) else None


def _max_known(records, field):
    values = [record[field] for record in records]
    return max(values) if values and all(value is not None for value in values) else None


def _condition_summary(records):
    count = len(records)
    passed = sum(record["task_passed"] is True for record in records)
    exact = sum(record["exact_match"] is True for record in records)
    tokens_by_owner = {
        owner: {field: _sum_known([record["usage"][owner] for record in records], field)
                for field in TOKEN_FIELDS}
        for owner in ("root", "child")
    }
    tokens = {field: _sum_known(list(tokens_by_owner.values()), field) for field in TOKEN_FIELDS}
    total_cost = _sum_known(records, "provider_receipt_usd")
    return {
        "runs": count,
        "denominators": {
            "attempts": count,
            **{status: sum(record["attempt_status"] == status for record in records)
               for status in sorted(STATUSES - {"running"})},
            "task_evaluated": sum(record["task_passed"] is not None for record in records),
            "task_passed": passed,
            "task_failed": sum(record["task_passed"] is False for record in records),
            "task_unscored": sum(record["task_passed"] is None for record in records),
            "attempts_with_failed_child": sum(
                "failed_child" in record["issues"] or any(child["status"] == "failed"
                                                         for child in record["child_outcomes"])
                for record in records),
        },
        "issue_counts": {issue: sum(issue in record["issues"] for record in records)
                         for issue in sorted({issue for record in records for issue in record["issues"]})},
        "task_pass_rate": passed / count if count else None,
        "task_pass_rate_95ci": _wilson(passed, count),
        "exact_match_rate": exact / count if count else None,
        "exact_match_rate_95ci": _wilson(exact, count),
        "tokens": tokens,
        "tokens_by_owner": tokens_by_owner,
        "token_measurement_counts": {
            owner: {field: sum(record["usage"][owner][field] is not None for record in records)
                    for field in TOKEN_FIELDS} for owner in ("root", "child")
        },
        "provider_receipt_usd": total_cost,
        "provider_receipt_count": sum(record["provider_receipt_usd"] is not None for record in records),
        "catalog_estimate_usd": _sum_known(records, "catalog_estimate_usd"),
        "root_catalog_estimate_usd": _sum_known(records, "root_catalog_estimate_usd"),
        "cost_per_completed_task_usd": total_cost / passed if total_cost is not None and passed else None,
        "retrieval_count": _sum_known(records, "retrieval_count"),
        "retrieval_failures": _sum_known(records, "retrieval_failures"),
        "render_ms": _sum_known(records, "render_ms"),
        "request_bytes": _sum_known(records, "request_bytes"),
        "latency_p95_ms": _percentile([record["latency_ms"] for record in records], 0.95)
            if all(record["latency_ms"] is not None for record in records) else None,
        "peak_memory_bytes": _max_known(records, "peak_memory_bytes"),
        "retries": _sum_known(records, "retries"),
    }


def _attempts(records):
    snapshots = defaultdict(dict)
    for raw in records:
        record = validate(raw)
        versions = snapshots[record["attempt_id"]]
        revision = record["attempt_revision"]
        if revision in versions and versions[revision] != record:
            raise ValueError(f"conflicting attempt snapshot: {record['attempt_id']}")
        if versions and any(record[field] != next(iter(versions.values()))[field]
                            for field in PAIR_FIELDS + ("condition",)):
            raise ValueError("attempt changed pairing controls")
        versions[revision] = record
    attempts = []
    for versions in snapshots.values():
        record = dict(versions[max(versions)])
        if record["attempt_status"] == "running":
            record["attempt_status"] = "interrupted"
            record["issues"] = record["issues"] + ["unfinished_attempt"]
        attempts.append(record)
    return attempts


def report(records):
    attempts = _attempts(records)
    pairs = defaultdict(lambda: defaultdict(list))
    grouped = defaultdict(list)
    for record in attempts:
        key = tuple(record[field] for field in PAIR_FIELDS)
        pairs[key][record["condition"]].append(record)
        grouped[(record["model"], record["condition"])].append(record)
    deltas = defaultdict(lambda: defaultdict(list))
    paired = 0
    unpaired = []
    for key, candidates in pairs.items():
        if set(candidates) != CONDITIONS or any(len(rows) != 1 for rows in candidates.values()):
            # Do not choose the best retry or silently drop a missing counterpart.
            unpaired.extend(record["attempt_id"] for rows in candidates.values() for record in rows)
            continue
        paired += 1
        native, bitmap = candidates["native"][0], candidates["bitmap"][0]
        for metric, field in (("task_pass_rate", "task_passed"), ("latency_ms", "latency_ms"),
                              ("provider_cost_usd", "provider_receipt_usd")):
            left, right = native[field], bitmap[field]
            delta = float(right) - float(left) if left is not None and right is not None else None
            deltas[key[0]][metric].append(delta)
    models = {}
    for model in sorted({key[0] for key in grouped}):
        models[model] = {condition: _condition_summary(grouped[(model, condition)])
                         for condition in sorted(CONDITIONS)}
        models[model]["paired_deltas_bitmap_minus_native"] = {
            metric: {
                "mean": sum(values) / len(values) if values and all(value is not None for value in values) else None,
                "95ci": _mean_interval(values) if all(value is not None for value in values) else None,
                "pair_count": len(values),
                "measured_pair_count": sum(value is not None for value in values),
            } for metric, values in deltas[model].items()
        }
    return {"version": SCHEMA_VERSION, "attempt_count": len(attempts), "pair_count": paired,
            "unpaired_attempt_count": len(unpaired), "unpaired_attempt_ids": sorted(unpaired), "models": models}


def read_records(path):
    """A torn final append cannot erase the already-flushed admission snapshot."""
    records = []
    input_issues = []
    lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
    for index, line in enumerate(lines):
        if not line.strip():
            continue
        try:
            records.append(json.loads(line))
        except json.JSONDecodeError:
            if index != len(lines) - 1 or line.endswith("\n"):
                raise ValueError(f"invalid raw record at line {index + 1}") from None
            input_issues.append({"code": "truncated_raw_record", "line": index + 1})
    return records, input_issues


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("records", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    records, input_issues = read_records(args.records)
    result = report(records)
    result["input_issues"] = input_issues
    encoded = json.dumps(result, indent=2) + "\n"
    if args.output:
        args.output.write_text(encoded, encoding="utf-8")
    else:
        print(encoded, end="")


if __name__ == "__main__":
    main()
