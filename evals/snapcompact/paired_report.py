"""Validate paired context-evaluation records and report metrics with 95% intervals."""

import argparse
import json
import math
from collections import defaultdict
from pathlib import Path

CONDITIONS = {"native", "bitmap"}
TOKEN_FIELDS = ("input_tokens", "cached_input_tokens", "output_tokens", "reasoning_tokens")
PAIR_FIELDS = ("model", "fixture_revision", "settings_digest", "cache_condition", "generation", "branch", "run")


def _number(value, name):
    if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
        raise ValueError(f"{name} must be a finite non-negative number")
    return value


def validate(record):
    required = set(PAIR_FIELDS) | {
        "condition", "task_passed", "exact_match", "usage", "provider_receipt_usd",
        "catalog_estimate_usd", "retrieval_count", "retrieval_failures", "render_ms",
        "request_bytes", "latency_ms", "peak_memory_bytes", "retries",
    }
    missing = sorted(required - record.keys())
    if missing:
        raise ValueError(f"record lacks {', '.join(missing)}")
    if record["condition"] not in CONDITIONS:
        raise ValueError("condition must be native or bitmap")
    if record["cache_condition"] not in {"cold", "warm"}:
        raise ValueError("cache_condition must be cold or warm")
    if type(record["task_passed"]) is not bool or type(record["exact_match"]) is not bool:
        raise ValueError("task_passed and exact_match must be booleans")
    for field in ("retrieval_count", "retrieval_failures", "render_ms", "request_bytes", "latency_ms", "peak_memory_bytes", "retries"):
        if record[field] is not None:
            _number(record[field], field)
    for cost in ("provider_receipt_usd", "catalog_estimate_usd"):
        if record[cost] is not None:
            _number(record[cost], cost)
    if set(record["usage"]) != {"root", "child"}:
        raise ValueError("usage must contain root and child")
    for owner in ("root", "child"):
        if set(record["usage"][owner]) != set(TOKEN_FIELDS):
            raise ValueError(f"usage.{owner} has the wrong token fields")
        for field in TOKEN_FIELDS:
            _number(record["usage"][owner][field], f"usage.{owner}.{field}")
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
    return sum(values) if all(value is not None for value in values) else None


def _max_known(records, field):
    values = [record[field] for record in records]
    return max(values) if values and all(value is not None for value in values) else None


def _condition_summary(records):
    count = len(records)
    passed = sum(record["task_passed"] for record in records)
    exact = sum(record["exact_match"] for record in records)
    tokens = dict.fromkeys(TOKEN_FIELDS, 0)
    for record in records:
        for owner in ("root", "child"):
            for field in TOKEN_FIELDS:
                tokens[field] += record["usage"][owner][field]
    receipts = [record["provider_receipt_usd"] for record in records]
    known_receipts = [value for value in receipts if value is not None]
    catalog = [record["catalog_estimate_usd"] for record in records]
    completed_costs = [record["provider_receipt_usd"] for record in records if record["task_passed"] and record["provider_receipt_usd"] is not None]
    return {
        "runs": count,
        "task_pass_rate": passed / count,
        "task_pass_rate_95ci": _wilson(passed, count),
        "exact_match_rate": exact / count,
        "exact_match_rate_95ci": _wilson(exact, count),
        "tokens": tokens,
        "provider_receipt_usd": sum(known_receipts) if len(known_receipts) == count else None,
        "catalog_estimate_usd": sum(value for value in catalog if value is not None) if all(value is not None for value in catalog) else None,
        "cost_per_completed_task_usd": sum(completed_costs) / len(completed_costs) if completed_costs else None,
        "retrieval_count": _sum_known(records, "retrieval_count"),
        "retrieval_failures": _sum_known(records, "retrieval_failures"),
        "render_ms": _sum_known(records, "render_ms"),
        "request_bytes": _sum_known(records, "request_bytes"),
        "latency_p95_ms": _percentile(
            [record["latency_ms"] for record in records if record["latency_ms"] is not None], 0.95
        ) if all(record["latency_ms"] is not None for record in records) else None,
        "peak_memory_bytes": _max_known(records, "peak_memory_bytes"),
        "retries": _sum_known(records, "retries"),
    }


def report(records):
    records = [validate(record) for record in records]
    pairs = defaultdict(dict)
    for record in records:
        key = tuple(record[field] for field in PAIR_FIELDS)
        condition = record["condition"]
        if condition in pairs[key]:
            raise ValueError(f"duplicate {condition} record for pair {key}")
        pairs[key][condition] = record
    incomplete = [key for key, conditions in pairs.items() if set(conditions) != CONDITIONS]
    if incomplete:
        raise ValueError(f"unpaired records: {incomplete[0]}")
    grouped = defaultdict(list)
    deltas = defaultdict(lambda: defaultdict(list))
    for key, pair in pairs.items():
        model = key[0]
        for condition, record in pair.items():
            grouped[(model, condition)].append(record)
        native = pair["native"]
        bitmap = pair["bitmap"]
        deltas[model]["task_pass_rate"].append(float(bitmap["task_passed"]) - float(native["task_passed"]))
        if native["latency_ms"] is not None and bitmap["latency_ms"] is not None:
            deltas[model]["latency_ms"].append(bitmap["latency_ms"] - native["latency_ms"])
        if native["provider_receipt_usd"] is not None and bitmap["provider_receipt_usd"] is not None:
            deltas[model]["provider_cost_usd"].append(bitmap["provider_receipt_usd"] - native["provider_receipt_usd"])
    models = {}
    for model in sorted({key[0] for key in grouped}):
        models[model] = {
            condition: _condition_summary(grouped[(model, condition)]) for condition in sorted(CONDITIONS)
        }
        models[model]["paired_deltas_bitmap_minus_native"] = {
            metric: {"mean": sum(values) / len(values), "95ci": _mean_interval(values)}
            for metric, values in deltas[model].items()
        }
    return {"version": 1, "pair_count": len(pairs), "models": models}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("records", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with args.records.open(encoding="utf-8") as stream:
        result = report(json.loads(line) for line in stream if line.strip())
    encoded = json.dumps(result, indent=2) + "\n"
    if args.output:
        args.output.write_text(encoded, encoding="utf-8")
    else:
        print(encoded, end="")


if __name__ == "__main__":
    main()
