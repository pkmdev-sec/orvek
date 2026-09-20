import importlib.util
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("paired_report", Path(__file__).with_name("paired_report.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def record(condition, *, cost, passed=True, exact=True):
    return {
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

    def test_rejects_unpaired_and_unknown_receipt_stays_unknown(self):
        with self.assertRaisesRegex(ValueError, "unpaired"):
            MODULE.report([record("native", cost=None)])
        result = MODULE.report([record("native", cost=None), record("bitmap", cost=None)])
        self.assertIsNone(result["models"]["luna"]["native"]["provider_receipt_usd"])

    def test_rejects_mismatched_pair_controls(self):
        native = record("native", cost=2)
        bitmap = record("bitmap", cost=1)
        bitmap["settings_digest"] = "different"
        with self.assertRaisesRegex(ValueError, "unpaired"):
            MODULE.report([native, bitmap])


if __name__ == "__main__":
    unittest.main()
