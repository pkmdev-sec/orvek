"""Docs classifications and snippets must not silently drift."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import sys
import tomllib
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("doc_examples", ROOT / "scripts/check-doc-examples.py")
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)
fixture_spec = importlib.util.spec_from_file_location("host_fixture", ROOT / checker.EXAMPLES / "fixture.py")
fixture = importlib.util.module_from_spec(fixture_spec)
fixture_spec.loader.exec_module(fixture)


class ExampleDriftTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for source in [ROOT / "README.md", *sorted((ROOT / "docs").glob("*.md")),
                       *sorted((ROOT / checker.EXAMPLES).glob("*.py"))]:
            destination = self.root / source.relative_to(ROOT)
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text(source.read_text())
        self.rows = json.loads((ROOT / checker.EXAMPLES / "inventory.json").read_text())
        (self.root / checker.DOCUMENT).write_text(checker.render(self.root, self.rows))

    def test_failed_setup_stops_children_and_removes_workspace(self):
        host = fixture.HostFixture(sys.executable, [])
        with self.assertRaises(RuntimeError):
            with host:
                self.fail("failed host must never become ready")
        self.assertIsNotNone(host.host.poll())
        self.assertFalse(host.provider_thread.is_alive())
        self.assertFalse(host.root.exists())
        self.assertNotIn("HTTPS_PROXY", host.env)
        self.assertEqual(host.env["HOME"], str(host.home))

    def test_checked_in_examples_are_current(self):
        self.assertEqual(checker.check(ROOT, self.rows), [])

    def test_missing_classification_fails(self):
        self.assertTrue(any("unclassified" in error
                            for error in checker.check(self.root, self.rows[1:])))

    def test_empty_reason_fails(self):
        rows = copy.deepcopy(self.rows)
        rows[0]["reason"] = " "
        self.assertTrue(any("needs a reason" in error for error in checker.check(self.root, rows)))

    def test_guide_cannot_claim_to_be_runnable(self):
        rows = copy.deepcopy(self.rows)
        rows[0]["kind"] = "runnable"
        self.assertTrue(any("invalid guide classification" in error
                            for error in checker.check(self.root, rows)))

    def test_changed_guide_requires_review(self):
        guide = self.root / "README.md"
        guide.write_text(guide.read_text().replace("git clone ", "git clone --depth=1 "))
        self.assertTrue(any("changed guide fence" in error
                            for error in checker.check(self.root, self.rows)))

    def test_new_guide_requires_classification(self):
        (self.root / "docs/new-guide.md").write_text("```sh\nprintf new\n```\n")
        self.assertTrue(any("unclassified" in error for error in checker.check(self.root, self.rows)))

    def test_snippet_is_generated_from_executable_source(self):
        source = self.root / checker.EXAMPLES / "native.py"
        source.write_text(source.read_text() + "\nassert False, 'changed example'\n")
        self.assertTrue(any("stale generated examples" in error
                            for error in checker.check(self.root, self.rows)))

    def test_deleted_block_and_duplicate_are_rejected(self):
        guide = self.root / "README.md"
        guide.write_text(checker.FENCES.sub("", guide.read_text()))
        self.assertTrue(any("removed or unknown" in error
                            for error in checker.check(self.root, self.rows)))
        self.assertTrue(any("duplicate" in error
                            for error in checker.check(self.root, self.rows + self.rows[:1])))


class HostContextFixtureTests(unittest.TestCase):
    def test_repeated_tool_names_have_distinct_call_ids(self):
        scan, = fixture.tool("memory", {"operation": "scan", "query": "fixture"})
        rescan, = fixture.tool("memory", {"operation": "scan", "query": "fixture"})
        self.assertNotEqual(scan["call_id"], rescan["call_id"])
        self.assertNotEqual(scan["id"], rescan["id"])

    def test_context_configuration_and_skill_exist_before_host_launch(self):
        body = "---\nname: check-note\ndescription: Check notes.\n---\nFixture body.\n"
        host = fixture.HostFixture(sys.executable, [], memory=True, skills={"check-note": body})

        def inspect_setup(*args, **kwargs):
            config = tomllib.loads(host.config.read_text())
            self.assertTrue(config["memory"]["enabled"])
            self.assertTrue(config["skills"]["enabled"])
            root, = config["skills"]["roots"]
            self.assertEqual(Path(root), host.root / "skills")
            self.assertEqual((Path(root) / "check-note/SKILL.md").read_bytes(), body.encode())
            self.assertEqual(host.config.stat().st_mode & 0o777, 0o600)
            self.assertEqual(kwargs["env"]["HOME"], str(host.home))
            self.assertNotIn("CODEX_HOME", kwargs["env"])
            self.assertNotIn("ORVEK_CONFIG", kwargs["env"])
            raise RuntimeError("stop before host launch")

        with patch.object(fixture.subprocess, "Popen", side_effect=inspect_setup):
            with self.assertRaisesRegex(RuntimeError, "stop before host launch"):
                with host:
                    self.fail("host must not start")
        self.assertFalse(host.provider_thread.is_alive())
        self.assertFalse(host.root.exists())


if __name__ == "__main__":
    unittest.main()
