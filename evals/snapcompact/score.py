"""Summarize existing Orvek JSONL usage without making requests or printing content."""

import argparse
import json
import math
from pathlib import Path


TOKEN_FIELDS = (
    "input_tokens",
    "cached_input_tokens",
    "cache_write_input_tokens",
    "output_tokens",
    "reasoning_output_tokens",
    "total_tokens",
)


def summarize(events):
    samples = []
    durations = []
    uncertainties = []
    seen = set()
    turns = failures = missing = local = remote = 0
    cost = 0.0
    cost_reported = True
    for event in events:
        identity = (event.get("request_id"), event.get("seq"))
        if None in identity:
            raise ValueError("event lacks session/sequence identity")
        if identity in seen:
            continue
        seen.add(identity)
        kind = event.get("type")
        payload = event.get("payload", {})
        if kind == "model.compaction.completed":
            if payload.get("strategy") == "snapcompact":
                local += 1
            else:
                remote += 1
                missing += payload.get("usage") is None
        elif kind == "model.call.completed":
            missing += payload.get("usage") is None
        elif kind == "model.warmup.completed" and payload.get("source") == "response":
            missing += payload.get("usage") is None
        if kind not in ("run.completed", "run.failed"):
            continue
        turns += 1
        failures += kind == "run.failed"
        for field in ("duration_ms", "billing_uncertain_response_attempts"):
            value = payload.get(field)
            if value is not None and (type(value) is not int or value < 0):
                raise ValueError(f"invalid {field}")
        durations.append(payload.get("duration_ms"))
        uncertainties.append(payload.get("billing_uncertain_response_attempts"))
        for group in ("usage", "warmup_usage"):
            usage = payload.get(group) or {}
            sample = {}
            for field in TOKEN_FIELDS:
                count = usage.get(field)
                if count is not None and (type(count) is not int or count < 0):
                    raise ValueError("invalid token count")
                sample[field] = count
            samples.append(sample)
        reported_cost = payload.get("cost_usd")
        if reported_cost is None:
            cost_reported = False
        elif type(reported_cost) not in (int, float) or not math.isfinite(reported_cost) or reported_cost < 0:
            raise ValueError("invalid cost estimate")
        else:
            cost += reported_cost
    totals = {field: sum(sample[field] for sample in samples)
              if samples and all(sample[field] is not None for sample in samples) else None
              for field in TOKEN_FIELDS}
    duration = sum(durations) if durations and all(value is not None for value in durations) else None
    uncertain = sum(uncertainties) if uncertainties and all(value is not None for value in uncertainties) else None
    return {
        "version": 2,
        "turns": turns,
        "failed_turns": failures,
        "local_compactions": local,
        "provider_compactions": remote,
        "summed_turn_duration_ms": duration,
        "tokens": totals,
        "estimated_cost_usd": cost if turns and cost_reported else None,
        "missing_operation_usage": missing,
        "billing_uncertain_attempts": uncertain,
        "accounting_complete": bool(turns) and cost_reported and not missing and uncertain == 0
                               and all(value is not None for value in totals.values()),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("logs", type=Path, nargs="+")
    args = parser.parse_args()
    reports = []
    for path in args.logs:
        with path.open(encoding="utf-8") as stream:
            report = summarize(json.loads(line) for line in stream if line.strip())
        reports.append({"log": str(path), **report})
    print(json.dumps(reports, indent=2))


if __name__ == "__main__":
    main()
