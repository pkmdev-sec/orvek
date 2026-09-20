"""Exercise the source-asset fence used by build-harbor-agent without building Docker."""
import re
import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def rejected_assets(tree):
    recipe = (ROOT / "justfile").read_text().split("build-harbor-agent platform='':", 1)[1]
    command = re.search(r'\$\((find "\$source_tree" -type f.*?)\)', recipe, re.S).group(1)
    arguments = shlex.split(command.replace("\\\n", " "))
    arguments[1] = str(tree)
    return subprocess.check_output(arguments, cwd=ROOT, text=True).strip()


class HarborAssetsTests(unittest.TestCase):
    def test_runtime_assets_are_accepted(self):
        for tree in ("bin/orvek/src", "crates/executor/src", "crates/harness/src", "crates/memory/src", "examples/orvek-memory-cloudflare/src"):
            with self.subTest(tree=tree):
                self.assertEqual(rejected_assets(tree), "", "Harbor build rejects a required runtime asset")

    def test_unknown_assets_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            tree = Path(directory)
            (tree / "lib.rs").write_text("pub fn example() {}")
            self.assertEqual(rejected_assets(tree), "")
            unexpected = tree / "local-state.json"
            unexpected.write_text("{}")
            self.assertEqual(rejected_assets(tree), str(unexpected))


if __name__ == "__main__":
    unittest.main()
