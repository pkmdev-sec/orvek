#!/usr/bin/env python3
"""Reject local state and generated build output in the versioned source tree."""

from pathlib import Path, PurePosixPath
import subprocess
import sys

repository = Path(__file__).resolve().parent.parent
tracked = subprocess.check_output(["git", "ls-files", "-z"], cwd=repository)
private_or_generated = {
    ".codex", ".claude", ".agents", ".idea", ".vscode", ".tact", ".orvek",
    "target", "node_modules", "__pycache__", ".venv", ".cache", ".dev", ".jj",
}
violations = []
for encoded in tracked.split(b"\0"):
    if not encoded:
        continue
    path = PurePosixPath(encoded.decode())
    environment = path.name == ".env" or path.name.startswith(".env.")
    example = path.name in {".env.example", ".env.sample", ".env.template"}
    generated = path.name == ".DS_Store" or path.suffix == ".pyc"
    bundled_output = str(path).startswith(("dist/", "web/review/dist/"))
    if any(part in private_or_generated for part in path.parts) or generated or bundled_output or (environment and not example):
        violations.append(str(path))
if violations:
    print("Remove local/generated files from the Git index:", file=sys.stderr)
    print("\n".join(violations), file=sys.stderr)
    raise SystemExit(1)
print("Tracked source tree contains no local-state or generated-output paths.")
