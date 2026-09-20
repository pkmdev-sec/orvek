#!/usr/bin/env python3
"""Report durable Orvek context usage and cost without printing session content."""

from __future__ import annotations

import argparse
import json
import sqlite3
from collections import defaultdict
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any

RATES = {
    "sol": (Decimal("0.000004"), Decimal("0.0000004"), Decimal("0.000020")),
    "terra": (Decimal("0.000002"), Decimal("0.0000002"), Decimal("0.000012")),
    "luna": (Decimal("0.0000002"), Decimal("0.00000002"), Decimal("0.0000012")),
}
COMPRESSION_COUNTERFACTUAL = Decimal("3.7")


def _json(value: bytes | str) -> Any:
    if isinstance(value, bytes):
        value = value.decode("utf-8")
    return json.loads(value)


def _session_metadata(state: dict[str, Any]) -> dict[str, Any]:
    config = state.get("config", {})
    model = config.get("model", {})
    history = state.get("history", [])
    return {
        "model": model.get("model"),
        "context_window_tokens": config.get("context_window_tokens"),
        "serialized_history_bytes": len(
            json.dumps(history, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        ),
    }


def _load_sqlite(path: Path) -> dict[str, Any]:
    connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    connection.row_factory = sqlite3.Row
    try:
        sessions = {
            row["id"]: _session_metadata(_json(row["state"]))
            for row in connection.execute("SELECT id, state FROM sessions")
        }
        events = []
        for row in connection.execute(
            "SELECT sequence, aggregate, event FROM events ORDER BY sequence"
        ):
            event = _json(row["event"])
            if event.get("type") != "command":
                continue
            command = event.get("data", {}).get("command", {})
            kind = command.get("type")
            data = command.get("data")
            if not isinstance(kind, str) or not isinstance(data, dict):
                continue
            events.append(
                {
                    "sequence": row["sequence"],
                    "session": row["aggregate"],
                    "type": kind,
                    "data": data,
                }
            )
        return {"sessions": sessions, "events": events}
    finally:
        connection.close()


def load_document(path: Path) -> dict[str, Any]:
    if path.suffix in {".sqlite", ".sqlite3", ".db"}:
        return _load_sqlite(path)
    document = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(document, dict):
        raise ValueError("analysis input must be a JSON object")
    return document


def _decimal(value: Any, field: str) -> Decimal | None:
    if value is None:
        return None
    if isinstance(value, bool):
        raise ValueError(f"invalid {field}")
    try:
        result = Decimal(str(value))
    except (InvalidOperation, ValueError) as error:
        raise ValueError(f"invalid {field}") from error
    if not result.is_finite() or result < 0:
        raise ValueError(f"invalid {field}")
    return result


def _tokens(usage: dict[str, Any], field: str) -> int:
    value = usage.get(field, 0)
    if type(value) is not int or value < 0:
        raise ValueError(f"invalid {field}")
    return value


def _money(value: Decimal) -> str:
    return format(value, ".8f")


def _event_key(event: dict[str, Any], data: dict[str, Any]) -> tuple[str, str]:
    session = event.get("session")
    if not isinstance(session, str) or not session:
        raise ValueError("event lacks session identity")
    call = data.get("call")
    if not isinstance(call, str) or not call:
        call = f"legacy-event-{event.get('sequence')}"
    return session, call


def analyze(document: dict[str, Any]) -> dict[str, Any]:
    sessions = document.get("sessions")
    events = document.get("events")
    if not isinstance(sessions, dict) or not isinstance(events, list):
        raise ValueError("analysis input requires sessions and events")

    calls: dict[tuple[str, str], dict[str, Any]] = {}
    receipts: dict[tuple[str, str], set[Decimal]] = defaultdict(set)
    receipt_requests: dict[tuple[str, str], str] = {}
    projection_events = 0

    for event in sorted(events, key=lambda item: item.get("sequence", 0)):
        if not isinstance(event, dict) or not isinstance(event.get("data"), dict):
            raise ValueError("invalid event")
        kind = event.get("type")
        data = event["data"]
        if kind == "context_projected":
            projection_events += 1
            continue
        if kind not in {"provider_usage", "provider_cost"}:
            continue
        key = _event_key(event, data)
        request = data.get("request")
        if not isinstance(request, str) or not request:
            request = f"legacy-request-{event.get('sequence')}"
        receipt_requests[key] = request
        if kind == "provider_cost":
            cost = _decimal(data.get("cost_usd"), "provider cost")
            if cost is not None:
                receipts[key].add(cost)
            continue

        if key in calls:
            raise ValueError(f"duplicate provider usage for {key[1]}")
        usage = data.get("usage")
        if not isinstance(usage, dict):
            raise ValueError("provider usage is missing")
        input_tokens = _tokens(usage, "input_tokens")
        cached_tokens = _tokens(usage, "cached_input_tokens")
        output_tokens = _tokens(usage, "output_tokens")
        reasoning_tokens = _tokens(usage, "reasoning_tokens")
        if cached_tokens > input_tokens:
            raise ValueError("cached input exceeds total input")
        model = sessions.get(key[0], {}).get("model")
        if model not in RATES:
            raise ValueError(f"no catalog rate for model {model!r}")
        usage_cost = _decimal(usage.get("cost_usd"), "usage cost")
        if usage_cost is not None:
            receipts[key].add(usage_cost)
        calls[key] = {
            "session": key[0],
            "request": request,
            "model": model,
            "input": input_tokens,
            "cached": cached_tokens,
            "output": output_tokens,
            "reasoning": reasoning_tokens,
        }

    per_request: dict[tuple[str, str], dict[str, Any]] = {}
    catalog = no_cache = all_cached = Decimal(0)
    catalog_input = catalog_output = Decimal(0)
    total_input = total_cached = total_output = total_reasoning = 0
    max_input = 0
    for key, call in calls.items():
        input_rate, cached_rate, output_rate = RATES[call["model"]]
        uncached = call["input"] - call["cached"]
        call_catalog = (
            Decimal(uncached) * input_rate
            + Decimal(call["cached"]) * cached_rate
            + Decimal(call["output"]) * output_rate
        )
        call_no_cache = (
            Decimal(call["input"]) * input_rate
            + Decimal(call["output"]) * output_rate
        )
        call_all_cached = (
            Decimal(call["input"]) * cached_rate
            + Decimal(call["output"]) * output_rate
        )
        catalog += call_catalog
        catalog_input += Decimal(uncached) * input_rate + Decimal(call["cached"]) * cached_rate
        catalog_output += Decimal(call["output"]) * output_rate
        no_cache += call_no_cache
        all_cached += call_all_cached
        total_input += call["input"]
        total_cached += call["cached"]
        total_output += call["output"]
        total_reasoning += call["reasoning"]
        max_input = max(max_input, call["input"])
        group_key = (call["session"], call["request"])
        group = per_request.setdefault(
            group_key,
            {
                "session": call["session"],
                "request": call["request"],
                "calls": 0,
                "input_tokens": 0,
                "cached_input_tokens": 0,
                "output_tokens": 0,
                "catalog": Decimal(0),
                "no_cache": Decimal(0),
                "receipt_keys": set(),
            },
        )
        group["calls"] += 1
        group["input_tokens"] += call["input"]
        group["cached_input_tokens"] += call["cached"]
        group["output_tokens"] += call["output"]
        group["catalog"] += call_catalog
        group["no_cache"] += call_no_cache
        group["receipt_keys"].add(key)

    for key, request in receipt_requests.items():
        group_key = (key[0], request)
        group = per_request.setdefault(
            group_key,
            {
                "session": key[0],
                "request": request,
                "calls": 0,
                "input_tokens": 0,
                "cached_input_tokens": 0,
                "output_tokens": 0,
                "catalog": Decimal(0),
                "no_cache": Decimal(0),
                "receipt_keys": set(),
            },
        )
        group["receipt_keys"].add(key)

    known_receipts = Decimal(0)
    receipt_calls = unknown_receipts = conflicting_receipts = 0
    for key in set(calls) | set(receipt_requests):
        values = receipts.get(key, set())
        if len(values) == 1:
            known_receipts += next(iter(values))
            receipt_calls += 1
        else:
            unknown_receipts += 1
            conflicting_receipts += len(values) > 1

    request_reports = []
    for group in sorted(per_request.values(), key=lambda item: (item["session"], item["request"])):
        group_receipts = Decimal(0)
        group_known = group_unknown = group_conflicts = 0
        for key in group.pop("receipt_keys"):
            values = receipts.get(key, set())
            if len(values) == 1:
                group_receipts += next(iter(values))
                group_known += 1
            else:
                group_unknown += 1
                group_conflicts += len(values) > 1
        group["cache_percent"] = (
            format(Decimal(group["cached_input_tokens"]) * 100 / Decimal(group["input_tokens"]), ".2f")
            if group["input_tokens"]
            else "0.00"
        )
        group["provider_receipts_usd"] = _money(group_receipts)
        group["receipt_calls"] = group_known
        group["unknown_receipt_calls"] = group_unknown
        group["conflicting_receipt_calls"] = group_conflicts
        group["catalog_estimate_usd"] = _money(group.pop("catalog"))
        group["no_cache_catalog_usd"] = _money(group.pop("no_cache"))
        request_reports.append(group)

    context = []
    for session, metadata in sorted(sessions.items()):
        session_calls = [call for call in calls.values() if call["session"] == session]
        session_max = max((call["input"] for call in session_calls), default=0)
        history_bytes = metadata.get("serialized_history_bytes")
        window = metadata.get("context_window_tokens")
        density = None
        if type(history_bytes) is int and session_max:
            density = format(Decimal(history_bytes) / Decimal(session_max), ".6f")
        threshold_reached = None
        if type(history_bytes) is int and type(window) is int:
            threshold_reached = history_bytes >= window * 85 // 100 * 4
        context.append(
            {
                "session": session,
                "model": metadata.get("model"),
                "configured_window_tokens": window,
                "serialized_history_bytes": history_bytes,
                "max_input_tokens": session_max,
                "history_bytes_per_max_input_token": density,
                "old_projection_threshold_reached": threshold_reached,
            }
        )

    single_context = context[0] if len(context) == 1 else {}
    compressed_value = (
        _money(catalog_input / COMPRESSION_COUNTERFACTUAL + catalog_output)
        if calls
        else None
    )

    return {
        "sessions": len(sessions),
        "calls": len(calls),
        "requests": len(per_request),
        "projection_events": projection_events,
        "tokens": {
            "input": total_input,
            "cached_input": total_cached,
            "output": total_output,
            "reasoning": total_reasoning,
        },
        "cost_usd": {
            "provider_receipts": _money(known_receipts),
            "catalog_estimate": _money(catalog),
            "no_cache_catalog": _money(no_cache),
            "all_cached_catalog": _money(all_cached),
            "prompt_cache_savings": _money(no_cache - catalog),
            "input_divided_by_3_7_with_unchanged_output": compressed_value,
        },
        "receipt_calls": receipt_calls,
        "unknown_receipt_calls": unknown_receipts,
        "conflicting_receipt_calls": conflicting_receipts,
        "max_input_tokens": max_input,
        "configured_window_tokens": single_context.get("configured_window_tokens"),
        "serialized_history_bytes": single_context.get("serialized_history_bytes"),
        "history_bytes_per_max_input_token": single_context.get("history_bytes_per_max_input_token"),
        "old_projection_threshold_reached": single_context.get("old_projection_threshold_reached"),
        "session_context": context,
        "per_request": request_reports,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="durable v1 SQLite database or sanitized JSON fixture")
    args = parser.parse_args()
    print(json.dumps(analyze(load_document(args.source)), indent=2))


if __name__ == "__main__":
    main()
