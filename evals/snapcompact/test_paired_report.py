import importlib.util
import unittest
from uuid import uuid4
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("paired_report", Path(__file__).with_name("paired_report.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def record(condition, *, cost, passed=True, exact=True):
    return {
        "schema_version": 2,
        "attempt_id": str(uuid4()),
        "attempt_revision": 1,
        "attempt_status": "completed",
        "exit_code": 0,
        "issues": [],
        "child_outcomes": [],
        "dataset": "snapcompact-fixture",
        "task_digest": "task",
        "harness_build": "build",
        "tool_access_digest": "tools",
        "environment_digest": "environment",
        "model": "luna",
        "fixture_revision": "fixture",
        "settings_digest": "settings",
        "cache_condition": "cold",
        "generation": 5,
        "branch": "resume",
        "run": 1,
        "condition": condition,
        "task_passed": passed,
        "exact_match": exact,
        "usage": {
            "root": {field: 10 for field in MODULE.TOKEN_FIELDS},
            "child": {field: 2 for field in MODULE.TOKEN_FIELDS},
        },
        "provider_receipt_usd": cost,
        "catalog_estimate_usd": cost,
        "root_catalog_estimate_usd": cost,
        "retrieval_count": 1,
        "retrieval_failures": 0,
        "render_ms": 3,
        "request_bytes": 100,
        "latency_ms": 50,
        "peak_memory_bytes": 1000,
        "retries": 0,
    }


class PairedReportTest(unittest.TestCase):
    def test_reports_root_and_child_usage_and_paired_cost_delta(self):
        result = MODULE.report([record("native", cost=2), record("bitmap", cost=1)])
        luna = result["models"]["luna"]
        self.assertEqual(luna["bitmap"]["tokens"]["input_tokens"], 12)
        self.assertEqual(
            luna["paired_deltas_bitmap_minus_native"]["provider_cost_usd"]["mean"], -1
        )
        self.assertEqual(result["pair_count"], 1)

    def test_retains_unpaired_and_unknown_receipt_stays_unknown(self):
        unpaired = MODULE.report([record("native", cost=None)])
        self.assertEqual(unpaired["unpaired_attempt_count"], 1)
        result = MODULE.report([record("native", cost=None), record("bitmap", cost=None)])
        self.assertIsNone(result["models"]["luna"]["native"]["provider_receipt_usd"])

    def test_rejects_mismatched_pair_controls(self):
        native = record("native", cost=2)
        bitmap = record("bitmap", cost=1)
        bitmap["settings_digest"] = "different"
        result = MODULE.report([native, bitmap])
        self.assertEqual(result["pair_count"], 0)
        self.assertEqual(result["unpaired_attempt_count"], 2)

    def test_rejects_legacy_records_instead_of_trusting_zeros(self):
        legacy = record("native", cost=0)
        del legacy["schema_version"]
        with self.assertRaisesRegex(ValueError, "legacy zero-filled"):
            MODULE.report([legacy])

    def test_all_comparison_controls_must_match(self):
        for field in ("dataset", "task_digest", "model", "settings_digest", "harness_build",
                      "tool_access_digest", "environment_digest"):
            with self.subTest(field=field):
                native, bitmap = record("native", cost=2), record("bitmap", cost=1)
                bitmap[field] = "different"
                result = MODULE.report([native, bitmap])
                self.assertEqual(result["pair_count"], 0)
                self.assertEqual(result["unpaired_attempt_count"], 2)
                self.assertEqual(result["attempt_count"], 2)

    def test_all_outcomes_stay_in_denominator_and_total_completion_cost(self):
        records = []
        for index, status in enumerate(("completed", "failed", "interrupted", "invalid_evaluation")):
            for condition in ("native", "bitmap"):
                row = record(condition, cost=2)
                row.update(run=index, attempt_status=status)
                if status != "completed":
                    row.update(task_passed=None, exact_match=None)
                records.append(row)
        result = MODULE.report(records)
        self.assertEqual(result["pair_count"], 4)
        summary = result["models"]["luna"]["native"]
        for status in ("completed", "failed", "interrupted", "invalid_evaluation"):
            self.assertEqual(summary["denominators"][status], 1)
        self.assertEqual(summary["task_pass_rate"], 0.25)
        self.assertEqual(summary["cost_per_completed_task_usd"], 8)
        records[2]["provider_receipt_usd"] = None
        result = MODULE.report(records)
        self.assertIsNone(result["models"]["luna"]["native"]["cost_per_completed_task_usd"])

    def test_ambiguous_retries_remain_visible_without_selecting_winner(self):
        rows = [record("native", cost=2), record("native", cost=3, passed=False), record("bitmap", cost=1)]
        result = MODULE.report(rows)
        self.assertEqual(result["pair_count"], 0)
        self.assertEqual(result["unpaired_attempt_count"], 3)
        self.assertEqual(result["models"]["luna"]["native"]["runs"], 2)

    def test_unknown_in_one_pair_keeps_aggregate_and_delta_unknown(self):
        rows = []
        for run in (0, 1):
            for condition in ("native", "bitmap"):
                row = record(condition, cost=1)
                row["run"] = run
                rows.append(row)
        rows[-1]["provider_receipt_usd"] = None
        rows[-1]["usage"]["root"]["cached_input_tokens"] = None
        result = MODULE.report(rows)
        bitmap = result["models"]["luna"]["bitmap"]
        self.assertIsNone(bitmap["provider_receipt_usd"])
        self.assertIsNone(bitmap["tokens"]["cached_input_tokens"])
        self.assertEqual(bitmap["provider_receipt_count"], 1)
        delta = result["models"]["luna"]["paired_deltas_bitmap_minus_native"]["provider_cost_usd"]
        self.assertIsNone(delta["mean"])
        self.assertEqual(delta["measured_pair_count"], 1)

    def test_missing_condition_has_no_measured_zero_totals(self):
        result = MODULE.report([record("native", cost=2)])
        missing = result["models"]["luna"]["bitmap"]
        self.assertEqual(missing["runs"], 0)
        self.assertIsNone(missing["provider_receipt_usd"])
        self.assertIsNone(missing["tokens"]["input_tokens"])

    def test_completed_execution_with_wrong_answer_is_an_evaluated_failure(self):
        result = MODULE.report([record("native", cost=1, passed=False, exact=False)])
        summary = result["models"]["luna"]["native"]
        self.assertEqual(summary["denominators"]["completed"], 1)
        self.assertEqual(summary["denominators"]["task_failed"], 1)
        self.assertEqual(summary["task_pass_rate"], 0)
        self.assertIsNone(summary["cost_per_completed_task_usd"])

    def test_rejects_conflicting_snapshots(self):
        native = record("native", cost=1)
        changed = {**native, "provider_receipt_usd": 2}
        with self.assertRaisesRegex(ValueError, "conflicting attempt"):
            MODULE.report([native, changed])


if __name__ == "__main__":
    unittest.main()
