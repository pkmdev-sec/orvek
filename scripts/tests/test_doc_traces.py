"""Source-linked fixture traces must remain local, non-exact, and unmodified."""
import base64
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("doc_traces", ROOT / "scripts/doc-traces.py")
traces = importlib.util.module_from_spec(spec)
spec.loader.exec_module(traces)


class TraceFixtureTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        shutil.copytree(ROOT / traces.EXAMPLES, self.root / traces.EXAMPLES)
        script = self.root / "scripts/doc-traces.py"
        script.parent.mkdir()
        shutil.copyfile(ROOT / "scripts/doc-traces.py", script)
        self.runtime = self.root / "crates/example/src/lib.rs"
        self.runtime.parent.mkdir(parents=True)
        self.runtime.write_text("// Test-only runtime hash input.\n")
        self.directory = self.root / traces.TRACES
        self.index_path = self.directory / "index.json"
        self.index = json.loads(self.index_path.read_text())
        self.index["runtime_sha256"] = traces.runtime_hash(self.root)
        self.save_index()

    def save_index(self):
        self.index_path.write_text(json.dumps(self.index))

    def test_all_five_checked_in_traces_pass_offline(self):
        self.assertEqual(traces.check(ROOT), [])
        self.assertEqual(traces.check(self.root), [])
        self.assertEqual(set(self.index["scenarios"]), set(traces.SCENARIOS))

    def test_scenario_fixture_and_generator_changes_require_regeneration(self):
        for name in self.index["sources"]:
            with self.subTest(source=name):
                path = self.root / name
                before = path.read_bytes()
                path.write_bytes(before + b"\n# changed\n")
                self.assertIn("stale trace fixture sources", traces.check(self.root)[0])
                path.write_bytes(before)

    def test_runtime_source_change_requires_regeneration(self):
        self.runtime.write_text("// changed runtime\n")
        self.assertIn("stale trace runtime sources", traces.check(self.root)[0])

    def test_missing_and_unexpected_files_fail(self):
        path = self.directory / "private.trace.json"
        path.write_text("{}")
        self.assertTrue(traces.check(self.root))
        path.unlink()
        (self.directory / "native.trace.json").unlink()
        self.assertTrue(traces.check(self.root))

    def test_missing_execution_provenance_fails(self):
        del self.index["binary_sha256"]
        self.save_index()
        self.assertTrue(traces.check(self.root))

    def test_missing_scenario_fails(self):
        del self.index["scenarios"]["native"]
        self.save_index()
        self.assertIn("missing/unknown trace scenario", traces.check(self.root)[0])

    def mutate_bundle(self, mutate):
        path = self.directory / "native.trace.json"
        envelope = json.loads(path.read_text())
        mutate(envelope["bundle"])
        envelope["digest"] = traces.sha256(traces.encoded(envelope["bundle"]))
        path.write_bytes(traces.encoded(envelope))
        return path

    def test_omission_cannot_claim_exact_even_with_valid_outer_hash(self):
        path = self.mutate_bundle(lambda bundle: bundle.update(exact=True))
        with self.assertRaisesRegex(AssertionError, "cannot claim exact"):
            traces.validate_bundle(path)

    def test_artifact_payload_or_identity_cannot_hide_in_event_only_fixture(self):
        original = (self.directory / "native.trace.json").read_bytes()
        for payload in [{"status": "present", "data": base64.b64encode(b"private").decode()},
                        {"status": "identity"}, {"status": "omitted", "data": "private"}]:
            with self.subTest(payload=payload):
                (self.directory / "native.trace.json").write_bytes(original)
                path = self.mutate_bundle(lambda bundle: bundle["artifacts"].update(
                    {next(iter(bundle["artifacts"])): payload}))
                with self.assertRaisesRegex(AssertionError, "omit every artifact"):
                    traces.validate_bundle(path)

    def test_journal_rewrite_fails_original_hash_chain(self):
        def mutate(bundle):
            bundle["records"][0]["event_base64"] = base64.b64encode(b"{}").decode()
        path = self.mutate_bundle(mutate)
        with self.assertRaisesRegex(AssertionError, "journal hash mismatch"):
            traces.validate_bundle(path)

    def test_changed_bundle_fails_index_hash(self):
        path = self.directory / "native.trace.json"
        path.write_bytes(path.read_bytes() + b"\n")
        self.assertIn("changed trace: native", traces.check(self.root)[0])

    def test_machine_paths_credentials_and_nested_byte_payloads_are_rejected(self):
        private = ["/Users/someone/private", "/home/someone/private", "Bearer canary",
                   "docs-fixture-not-a-secret", "https://private.example", "/tmp/not-a-fixture",
                   "/tmp/ov-doc-fake/workspace/../private", {"api_key": "canary"}]
        for value in private:
            for encoded in [value, json.dumps({"output": value}), list(json.dumps(value).encode())]:
                with self.subTest(value=encoded):
                    with self.assertRaises(AssertionError):
                        traces.vet_event(encoded)
        traces.vet_event({"workspace": "/private/tmp/ov-doc-fixture/workspace"})


if __name__ == "__main__":
    unittest.main()
