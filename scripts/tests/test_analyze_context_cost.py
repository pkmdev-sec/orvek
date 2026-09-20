from __future__ import annotations

import importlib.util
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "analyze_context_cost", ROOT / "scripts/analyze-context-cost.py"
)
assert SPEC is not None and SPEC.loader is not None
ANALYZER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ANALYZER)
FIXTURE = ROOT / "evals/context_cost/recent-usage-fixture.json"


def usage(
    sequence: int,
    call: str,
    *,
    request: str = "request-1",
    input_tokens: int = 10_000,
    cached_input_tokens: int = 8_000,
    output_tokens: int = 100,
    cost_usd: str | None = None,
) -> dict:
    return {
        "sequence": sequence,
        "session": "session-1",
        "type": "provider_usage",
        "data": {
            "request": request,
            "call": call,
            "usage": {
                "input_tokens": input_tokens,
                "cached_input_tokens": cached_input_tokens,
                "output_tokens": output_tokens,
                "reasoning_tokens": 20,
                "total_tokens": input_tokens + output_tokens,
                "cost_usd": cost_usd,
            },
        },
    }


def receipt(sequence: int, call: str, cost_usd: str | None) -> dict:
    return {
        "sequence": sequence,
        "session": "session-1",
        "type": "provider_cost",
        "data": {
            "request": "request-1",
            "call": call,
            "cost_usd": cost_usd,
        },
    }


class ContextCostAnalysisTests(unittest.TestCase):
    def test_sanitized_fixture_reproduces_late_projection_and_reports_costs(self) -> None:
        report = ANALYZER.analyze(ANALYZER.load_document(FIXTURE))

        self.assertEqual(report["sessions"], 1)
        self.assertEqual(report["calls"], 3)
        self.assertEqual(report["requests"], 2)
        self.assertEqual(report["projection_events"], 0)
        self.assertEqual(report["tokens"]["input"], 3_216_154)
        self.assertEqual(report["tokens"]["cached_input"], 2_679_905)
        self.assertEqual(report["cost_usd"]["provider_receipts"], "1.51000000")
        self.assertEqual(report["cost_usd"]["catalog_estimate"], "3.25753800")
        self.assertEqual(report["cost_usd"]["no_cache_catalog"], "12.90519600")
        self.assertEqual(report["cost_usd"]["prompt_cache_savings"], "9.64765800")
        self.assertEqual(report["receipt_calls"], 2)
        self.assertEqual(report["unknown_receipt_calls"], 1)
        self.assertEqual(report["max_input_tokens"], 1_316_154)
        self.assertEqual(report["configured_window_tokens"], 1_000_000)
        self.assertEqual(report["serialized_history_bytes"], 1_817_848)
        self.assertEqual(report["history_bytes_per_max_input_token"], "1.381182")
        self.assertFalse(report["old_projection_threshold_reached"])

    def test_receipts_override_catalog_and_conflicts_remain_unknown(self) -> None:
        document = {
            "sessions": {
                "session-1": {
                    "model": "sol",
                    "context_window_tokens": 1_000_000,
                }
            },
            "events": [
                usage(1, "known", cost_usd="0.05000000"),
                receipt(2, "known", "0.05000000"),
                usage(3, "conflict", cost_usd="0.06000000"),
                receipt(4, "conflict", "0.07000000"),
                usage(5, "missing"),
                receipt(6, "missing", None),
            ],
        }

        report = ANALYZER.analyze(document)

        self.assertEqual(report["cost_usd"]["provider_receipts"], "0.05000000")
        self.assertEqual(report["receipt_calls"], 1)
        self.assertEqual(report["unknown_receipt_calls"], 2)
        self.assertEqual(report["conflicting_receipt_calls"], 1)
        self.assertEqual(
            {request["request"] for request in report["per_request"]},
            {"request-1"},
        )

    def test_invalid_usage_is_rejected(self) -> None:
        document = {
            "sessions": {"session-1": {"model": "sol"}},
            "events": [usage(1, "bad", input_tokens=10, cached_input_tokens=11)],
        }
        with self.assertRaisesRegex(ValueError, "cached input"):
            ANALYZER.analyze(document)

    def test_projection_events_are_counted(self) -> None:
        document = {
            "sessions": {"session-1": {"model": "sol"}},
            "events": [
                usage(1, "call"),
                {
                    "sequence": 2,
                    "session": "session-1",
                    "type": "context_projected",
                    "data": {"request": "request-1"},
                },
            ],
        }
        self.assertEqual(ANALYZER.analyze(document)["projection_events"], 1)

    def test_sqlite_loader_unwraps_durable_commands(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            database = Path(directory) / "v1.sqlite3"
            connection = sqlite3.connect(database)
            connection.executescript(
                """
                CREATE TABLE sessions(
                    id TEXT PRIMARY KEY,
                    revision INTEGER NOT NULL,
                    state BLOB NOT NULL,
                    head TEXT NOT NULL
                );
                CREATE TABLE events(
                    sequence INTEGER PRIMARY KEY,
                    aggregate TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    revision INTEGER NOT NULL,
                    event BLOB NOT NULL,
                    hash TEXT NOT NULL
                );
                """
            )
            state = {
                "config": {
                    "model": {"model": "terra"},
                    "context_window_tokens": 272_000,
                },
                "history": [{"role": "user", "content": "fixture"}],
            }
            event = {
                "type": "command",
                "data": {
                    "operation": "operation-1",
                    "command": {
                        "type": "provider_usage",
                        "data": usage(1, "call-1")["data"],
                    },
                    "at_ms": 1,
                },
            }
            connection.execute(
                "INSERT INTO sessions VALUES (?, ?, ?, ?)",
                ("session-1", 1, json.dumps(state).encode(), "head"),
            )
            connection.execute(
                "INSERT INTO events VALUES (?, ?, ?, ?, ?, ?)",
                (1, "session-1", "session", 1, json.dumps(event).encode(), "hash"),
            )
            connection.commit()
            connection.close()

            document = ANALYZER.load_document(database)
            report = ANALYZER.analyze(document)

        self.assertEqual(document["sessions"]["session-1"]["model"], "terra")
        self.assertEqual(report["calls"], 1)
        self.assertEqual(report["configured_window_tokens"], 272_000)


if __name__ == "__main__":
    unittest.main()
