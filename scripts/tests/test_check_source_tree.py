"""Exercise the source-hygiene CLI against isolated Git indexes."""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class SourceHygieneTests(unittest.TestCase):
    def check_paths(self, paths):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            shutil.copyfile(
                ROOT / "scripts/check-source-tree.py",
                root / "scripts/check-source-tree.py",
            )
            (root / "src").mkdir()
            (root / "src/lib.rs").write_text("")
            (root / "Cargo.toml").write_text(
                '[package]\nname = "hygiene-fixture"\nversion = "0.1.0"\nedition = "2024"\n'
            )
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            for name in paths:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("fixture")
            subprocess.run(
                ["git", "add", "--", *paths, "src/lib.rs"], cwd=root, check=True
            )
            return subprocess.run(
                [sys.executable, str(root / "scripts/check-source-tree.py")],
                cwd=root,
                capture_output=True,
                text=True,
                check=False,
                env={**os.environ, "CARGO_NET_OFFLINE": "true"},
            )

    def test_private_agent_map_is_rejected_at_every_depth(self):
        for path in [
            ".agent-map",
            ".agent-map/graph.json",
            "nested/.agent-map",
            "nested/.agent-map/graph.json",
        ]:
            with self.subTest(path=path):
                result = self.check_paths([path])
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn(path, result.stderr)

    def test_public_graph_and_marketing_sources_are_allowed(self):
        result = self.check_paths(
            ["docs/codebase-graph/graph.json", "assets/marketing/README.md"]
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
