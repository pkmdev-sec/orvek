"""Offline headless fixtures for the measurement and attempt contract."""

import argparse
import io
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import paired_report
import run_paired


def event(kind, data):
    return {"protocol": "orvek.host", "version": 1, "type": kind, "data": data}


def journal(kind, data, sequence):
    return event("event", {"type": "journal", "data": {
        "sequence": sequence, "kind": "session", "aggregate": "session",
        "event": {"type": "command", "data": {"command": {
            "type": kind, "data": {"request": "request", **data},
        }}},
    }})


def transcript(*, receipt=True, cost=True, failed_child=False, cached=None, reasoning=None):
    events = [event("session", {"id": "session"}),
              event("submission_pending", {"session": "session", "request": "request"})]
    events.append(journal("provider_usage", {"call": "call", "usage": {
        "input_tokens": 7, "output_tokens": 3,
        "cached_input_tokens": cached, "reasoning_tokens": reasoning,
    }}, 1))
    if cost:
        events.append(journal("provider_cost", {"call": "call", "cost_usd": "0.1"}, 2))
    if failed_child:
        events.append(journal("response", {"items": [{
            "type": "function_call", "call_id": "wait", "name": "wait_agent",
        }]}, 3))
        events.append(journal("tool_result", {"call_id": "wait", "output": json.dumps({
            "agents": [{"agent_id": "child", "status": "failed", "error": "provider failed"}],
        })}, 4))
    events.append(journal("response", {"items": [{
        "type": "message", "role": "assistant",
        "content": [{"type": "output_text", "text": "expected"}],
    }]}, 5))
    events.append(journal("turn_settled", {"outcome": "finished_unverified", "error": None}, 6))
    if receipt:
        events.append(event("submission_result", {"id": "request", "status": {
            "state": "finished", "outcome": "finished_unverified",
            "error": "Finished on the native host without verification evidence",
        }}))
    return "".join(json.dumps(item) + "\n" for item in events)


class MeasurementContractTest(unittest.TestCase):
    def test_null_usage_is_not_measured_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "log.jsonl"
            path.write_text(transcript())
            observed = run_paired.parse_log(path)
        self.assertEqual(observed["usage"]["input_tokens"], 7)
        self.assertEqual(observed["usage"]["output_tokens"], 3)
        self.assertIsNone(observed["usage"]["cached_input_tokens"])
        self.assertIsNone(observed["usage"]["reasoning_tokens"])
        self.assertIsNone(run_paired.catalog_cost("luna", observed["usage"]))

    def run_fixture(self, text, returncode=0, interrupt=False):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fixture = root / "fixture.txt"
            fixture.write_text("expected\n")
            config = root / "config.toml"
            config.write_text("")
            args = argparse.Namespace(
                binary=Path("/fixture/orvek"), config=config, auth_file=root / "auth.json",
                api_base_url="http://fixture.invalid", thinking="low", generations=1,
                fixture=fixture, logs=root, coding_check=False, dataset_id="fixture",
                tool_access_id="native-read-write", environment_id="fixture-python",
            )
            output = io.StringIO()

            def execute(command, **kwargs):
                kwargs["stdout"].write(text)
                kwargs["stdout"].flush()
                if interrupt:
                    raise KeyboardInterrupt
                return subprocess.CompletedProcess(command, returncode, stderr="fixture failure")

            with patch.object(run_paired.subprocess, "run", side_effect=execute), \
                    patch.object(run_paired, "stop_host"), \
                    patch.object(Path, "read_bytes", return_value=b"fixture build"):
                try:
                    run_paired.run_condition(args, "luna", "native", 0, "fixture", output)
                except (RuntimeError, KeyboardInterrupt):
                    pass
            rows = [json.loads(line) for line in output.getvalue().splitlines()]
            self.assertTrue(rows, "runner lost the attempt before returning failure")
            return rows

    def test_nonzero_exit_writes_typed_attempt(self):
        rows = self.run_fixture(transcript(), returncode=23)
        self.assertEqual(rows[-1]["attempt_status"], "failed")
        self.assertEqual(rows[-1]["exit_code"], 23)
        self.assertIn("nonzero_exit", rows[-1]["issues"])

    def test_missing_submission_receipt_writes_invalid_attempt(self):
        rows = self.run_fixture(transcript(receipt=False))
        self.assertEqual(rows[-1]["attempt_status"], "invalid_evaluation")
        self.assertIn("missing_submission_receipt", rows[-1]["issues"])

    def test_partial_log_and_interrupt_are_preserved(self):
        rows = self.run_fixture(transcript(receipt=False) + '{"type":', interrupt=True)
        self.assertEqual(rows[-1]["attempt_status"], "interrupted")
        self.assertIn("malformed_log", rows[-1]["issues"])
        self.assertIsNone(rows[-1]["provider_receipt_usd"])

    def test_missing_provider_receipt_does_not_become_zero(self):
        rows = self.run_fixture(transcript(cost=False))
        self.assertIsNone(rows[-1]["provider_receipt_usd"])
        self.assertIn("missing_provider_receipt", rows[-1]["issues"])

    def test_failed_child_is_typed_without_fabricated_child_usage(self):
        rows = self.run_fixture(transcript(failed_child=True))
        self.assertEqual(rows[-1]["child_outcomes"], [{"agent_id": "child", "status": "failed"}])
        self.assertIn("failed_child", rows[-1]["issues"])
        self.assertTrue(all(value is None for value in rows[-1]["usage"]["child"].values()))
        summary = paired_report.report(rows)["models"]["luna"]["native"]
        self.assertEqual(summary["denominators"]["attempts_with_failed_child"], 1)
        self.assertEqual(summary["denominators"]["completed"], 1)

    def test_unknown_survives_raw_and_aggregate_with_unmatched_attempt(self):
        rows = self.run_fixture(transcript())
        result = paired_report.report(rows)
        summary = result["models"]["luna"]["native"]
        self.assertIsNone(rows[-1]["usage"]["root"]["cached_input_tokens"])
        self.assertIsNone(summary["tokens"]["cached_input_tokens"])
        self.assertEqual(summary["denominators"]["completed"], 1)
        self.assertEqual(result["unpaired_attempt_count"], 1)

    def test_null_in_one_call_poison_totals_and_measured_zero_survives(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "log.jsonl"
            path.write_text(transcript(cached=0, reasoning=0))
            measured = run_paired.parse_log(path)
            self.assertEqual(measured["usage"]["cached_input_tokens"], 0)
            self.assertEqual(measured["usage"]["reasoning_tokens"], 0)
            extra = journal("provider_usage", {"call": "other", "usage": {
                "input_tokens": 2, "output_tokens": 1, "cached_input_tokens": None,
            }}, 7)
            extra_cost = journal("provider_cost", {"call": "other", "cost_usd": "0"}, 8)
            path.write_text(transcript(cached=0, reasoning=0) + json.dumps(extra) + "\n" + json.dumps(extra_cost) + "\n")
            mixed = run_paired.parse_log(path)
        self.assertEqual(mixed["usage"]["input_tokens"], 9)
        self.assertIsNone(mixed["usage"]["cached_input_tokens"])
        self.assertIsNone(mixed["usage"]["reasoning_tokens"])

    def test_replayed_journal_events_are_not_double_counted(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "log.jsonl"
            text = transcript(cached=0, reasoning=0)
            path.write_text(text + text)
            observed = run_paired.parse_log(path)
        self.assertEqual(observed["usage"]["input_tokens"], 7)
        self.assertEqual(observed["cost"], run_paired.Decimal("0.1"))

    def test_empty_and_malformed_logs_are_unknown_not_zero(self):
        for text in ("", "null\n", '{"type":'):
            with self.subTest(text=text):
                rows = self.run_fixture(text)
                self.assertEqual(rows[-1]["attempt_status"], "invalid_evaluation")
                self.assertTrue(all(value is None for value in rows[-1]["usage"]["root"].values()))
                self.assertIsNone(rows[-1]["retrieval_count"])
                self.assertIsNone(rows[-1]["retries"])

    def test_receipt_identity_mismatch_and_view_gap_invalidate_evaluation(self):
        wrong = transcript().replace('"id": "request", "status"', '"id": "other", "status"')
        gap = transcript() + json.dumps(event("view_gap", {"after": 1, "through": 8})) + "\n"
        for text, issue in ((wrong, "receipt_identity_mismatch"), (gap, "view_gap")):
            with self.subTest(issue=issue):
                rows = self.run_fixture(text)
                self.assertEqual(rows[-1]["attempt_status"], "invalid_evaluation")
                self.assertIn(issue, rows[-1]["issues"])
                self.assertIsNone(rows[-1]["provider_receipt_usd"])

    def test_missing_call_receipt_makes_total_unknown(self):
        extra = journal("provider_usage", {"call": "other", "usage": {
            "input_tokens": 1, "output_tokens": 1,
        }}, 7)
        rows = self.run_fixture(transcript() + json.dumps(extra) + "\n")
        self.assertIsNone(rows[-1]["provider_receipt_usd"])


class WorkspaceFixtureTest(unittest.TestCase):
    def test_generated_emitter_runs_without_evaluator_imports(self):
        with tempfile.TemporaryDirectory() as tmp:
            workspace = Path(tmp)
            fixture = Path(__file__).with_name("fixtures") / "history.txt"
            run_paired.write_workspace(workspace, fixture)
            completed = subprocess.run([os.sys.executable, "emit.py", "1"], cwd=workspace,
                                       capture_output=True, text=True)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertEqual(completed.stdout.splitlines()[0], run_paired.expected_line(fixture, 1))


class RunnerCliTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.binary = self.root / "orvek-fixture"
        self.config = self.root / "config.toml"
        self.config.write_text("")
        self.fixture = self.root / "history.txt"
        self.fixture.write_text("expected\n")
        self.output = self.root / "raw.jsonl"
        self.report_path = self.root / "report.json"
        self.command = [
            os.sys.executable, "-B", str(Path(run_paired.__file__)),
            "--binary", str(self.binary), "--config", str(self.config),
            "--auth-file", str(self.root / "unused-auth"), "--api-base-url", "http://fixture.invalid",
            "--fixture", str(self.fixture), "--output", str(self.output), "--logs", str(self.root / "logs"),
            "--models", "luna", "--generations", "1", "--dataset-id", "fixture",
            "--tool-access-id", "fixture-tools", "--environment-id", "fixture-environment",
        ]

    def binary_program(self, program):
        self.binary.write_text("#!/usr/bin/env python3\nimport os, sys, signal\n" + program)
        self.binary.chmod(0o755)

    def report(self):
        completed = subprocess.run([os.sys.executable, "-B", str(Path(paired_report.__file__)),
                                    str(self.output), "--output", str(self.report_path)], capture_output=True, text=True)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        return json.loads(self.report_path.read_text())

    def test_failure_is_recorded_and_other_condition_still_runs(self):
        self.binary_program(f"sys.stdout.write({transcript()!r})\n"
                            "sys.exit(23 if os.environ['ORVEK_EVAL_CONTEXT_REPRESENTATION'] == 'native' else 0)\n")
        completed = subprocess.run(self.command, capture_output=True, text=True)
        self.assertEqual(completed.returncode, 1, completed.stderr)
        rows = [json.loads(line) for line in self.output.read_text().splitlines()]
        self.assertEqual(len(rows), 4)
        result = self.report()
        self.assertEqual(result["attempt_count"], 2)
        self.assertEqual(result["pair_count"], 1)
        luna = result["models"]["luna"]
        self.assertEqual(luna["native"]["denominators"]["failed"], 1)
        self.assertEqual(luna["bitmap"]["denominators"]["completed"], 1)
        self.assertIsNone(luna["bitmap"]["tokens"]["cached_input_tokens"])
        self.assertIsNone(luna["paired_deltas_bitmap_minus_native"]["task_pass_rate"]["mean"])
        original = self.output.read_bytes()
        subprocess.run(self.command, capture_output=True, text=True)
        self.assertEqual(self.output.read_bytes(), original, "rerun overwrote existing attempts")

    def test_coding_nonzero_exit_is_also_denominator_visible(self):
        self.binary_program(f"sys.stdout.write({transcript()!r})\n"
                            "sys.exit(9 if sys.argv[-1].startswith('Implement candidate.py') else 0)\n")
        completed = subprocess.run(self.command + ["--coding-check"], capture_output=True, text=True)
        self.assertEqual(completed.returncode, 1, completed.stderr)
        rows = [json.loads(line) for line in self.output.read_text().splitlines()]
        coding = [row for row in rows if row["branch"] == "coding" and row["attempt_revision"] == 1]
        self.assertEqual(len(coding), 2)
        self.assertTrue(all(row["attempt_status"] == "failed" for row in coding))
        result = self.report()
        self.assertEqual(result["attempt_count"], 4)
        self.assertEqual(result["pair_count"], 2)

    @unittest.skipUnless(os.name == "posix", "signal loss is a POSIX fixture")
    def test_killed_runner_leaves_an_interrupted_admission(self):
        self.binary_program("os.kill(os.getppid(), signal.SIGKILL)\n")
        completed = subprocess.run(self.command, capture_output=True, text=True)
        self.assertEqual(completed.returncode, -9)
        result = self.report()
        self.assertEqual(result["attempt_count"], 1)
        self.assertEqual(result["unpaired_attempt_count"], 1)
        summary = result["models"]["luna"]["native"]
        self.assertEqual(summary["denominators"]["interrupted"], 1)
        self.assertEqual(summary["issue_counts"]["unfinished_attempt"], 1)
        self.assertIsNone(summary["provider_receipt_usd"])
        with self.output.open("a") as stream:
            stream.write('{"attempt_revision":1')
        recovered = self.report()
        self.assertEqual(recovered["attempt_count"], 1)
        self.assertEqual(recovered["input_issues"][0]["code"], "truncated_raw_record")
        self.assertEqual(recovered["models"]["luna"]["native"]["denominators"]["interrupted"], 1)

    @unittest.skipUnless(os.name == "posix", "signal loss is a POSIX fixture")
    def test_terminated_process_records_partial_log(self):
        self.binary_program(f"sys.stdout.write({transcript(receipt=False)!r}); sys.stdout.flush()\n"
                            "os.kill(os.getpid(), signal.SIGTERM)\n")
        completed = subprocess.run(self.command, capture_output=True, text=True)
        self.assertEqual(completed.returncode, 1)
        result = self.report()
        for condition in ("native", "bitmap"):
            self.assertEqual(result["models"]["luna"][condition]["denominators"]["interrupted"], 1)


if __name__ == "__main__":
    unittest.main()
