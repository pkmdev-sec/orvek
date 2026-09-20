from __future__ import annotations

import importlib.util
import os
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("package_release", ROOT / "scripts/package-release.py")
assert SPEC is not None and SPEC.loader is not None
PACKAGE_RELEASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE_RELEASE)


class PackageReleaseTests(unittest.TestCase):
    def test_archive_is_independent_of_source_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "release"
            source.mkdir()
            binary = source / "orvek"
            binary.write_bytes(b"binary")
            binary.chmod(0o755)
            (source / "README.md").write_text("read me", encoding="utf-8")

            first = root / "first.tar.gz"
            second = root / "second.tar.gz"
            PACKAGE_RELEASE.create_archive(source, first, 1_700_000_000)
            os.utime(binary, (1_800_000_000, 1_800_000_000))
            os.utime(source / "README.md", (1_600_000_000, 1_600_000_000))
            PACKAGE_RELEASE.create_archive(source, second, 1_700_000_000)

            self.assertEqual(first.read_bytes(), second.read_bytes())
            with tarfile.open(first, "r:gz") as archive:
                members = archive.getmembers()
            self.assertEqual([member.name for member in members], ["release", "release/README.md", "release/orvek"])
            self.assertTrue(all(member.mtime == 1_700_000_000 for member in members))
            self.assertTrue(all(member.uid == 0 and member.gid == 0 for member in members))
            self.assertEqual(members[-1].mode, 0o755)


if __name__ == "__main__":
    unittest.main()
