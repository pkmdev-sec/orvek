import unittest

from score import summarize


def event(sequence, kind, payload):
    return {"request_id": "fixture-session", "seq": sequence, "type": kind, "payload": payload}


class AccountingTests(unittest.TestCase):
    def test_terminal_usage_includes_warmup_without_counting_operations_twice(self):
        completed = event(3, "run.completed", {
            "usage": {"input_tokens": 100, "cached_input_tokens": 40,
                      "cache_write_input_tokens": 20, "output_tokens": 30,
                      "reasoning_output_tokens": 25, "total_tokens": 130},
            "warmup_usage": {"input_tokens": 10, "total_tokens": 10},
            "duration_ms": 90, "cost_usd": 0.01,
        })
        report = summarize([
            event(1, "model.call.completed", {"usage": {"input_tokens": 100}}),
            event(2, "model.compaction.completed", {"strategy": "snapcompact"}),
            completed, completed,
        ])
        self.assertEqual(report["tokens"]["input_tokens"], 110)
        self.assertEqual(report["tokens"]["output_tokens"], 30)
        self.assertEqual(report["tokens"]["reasoning_output_tokens"], 25)
        self.assertEqual(report["tokens"]["total_tokens"], 140)
        self.assertEqual(report["turns"], 1)
        self.assertTrue(report["accounting_complete"])

    def test_missing_usage_and_uncertain_retries_are_not_reported_as_complete(self):
        report = summarize([
            event(1, "model.call.completed", {"usage": None}),
            event(2, "run.failed", {"usage": {}, "billing_uncertain_response_attempts": 1}),
        ])
        self.assertFalse(report["accounting_complete"])
        self.assertIsNone(report["estimated_cost_usd"])
        self.assertEqual(report["missing_operation_usage"], 1)
        self.assertEqual(report["failed_turns"], 1)
        self.assertFalse(summarize([])["accounting_complete"])

    def test_unidentified_events_cannot_silently_merge(self):
        with self.assertRaises(ValueError):
            summarize([{"type": "run.completed", "payload": {}}])
