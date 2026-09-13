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
    totals = dict.fromkeys(TOKEN_FIELDS, 0)
    seen = set()
    turns = failures = duration = uncertain = missing = local = remote = 0
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
        duration += payload.get("duration_ms", 0)
        uncertain += payload.get("billing_uncertain_response_attempts", 0)
        for group in ("usage", "warmup_usage"):
            usage = payload.get(group) or {}
            for field in TOKEN_FIELDS:
                count = usage.get(field, 0)
                if type(count) is not int or count < 0:
                    raise ValueError("invalid token count")
                totals[field] += count
        reported_cost = payload.get("cost_usd")
        if reported_cost is None:
            cost_reported = False
        elif type(reported_cost) not in (int, float) or not math.isfinite(reported_cost) or reported_cost < 0:
            raise ValueError("invalid cost estimate")
        else:
            cost += reported_cost
    return {
        "turns": turns,
        "failed_turns": failures,
        "local_compactions": local,
        "provider_compactions": remote,
        "summed_turn_duration_ms": duration,
        "tokens": totals,
        "estimated_cost_usd": cost if turns and cost_reported else None,
        "missing_operation_usage": missing,
        "billing_uncertain_attempts": uncertain,
        "accounting_complete": bool(turns) and cost_reported and not missing and not uncertain,
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
