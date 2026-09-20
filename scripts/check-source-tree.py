#!/usr/bin/env python3
"""Reject local state and generated build output in the versioned source tree."""

from pathlib import Path, PurePosixPath
import json
import re
import subprocess
import sys

repository = Path(__file__).resolve().parent.parent
tracked = subprocess.check_output(["git", "ls-files", "-z"], cwd=repository)
private_or_generated = {
    ".agent-map", ".codex", ".claude", ".agents", ".idea", ".vscode", ".tact", ".orvek",
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
    local_marketing = path == PurePosixPath("assets/marketing") or PurePosixPath("assets/marketing") in path.parents
    bundled_output = str(path).startswith(("dist/", "web/review/dist/"))
    if any(part in private_or_generated for part in path.parts) or generated or bundled_output or local_marketing or (environment and not example):
        violations.append(str(path))
if violations:
    print("Remove local/generated files from the Git index:", file=sys.stderr)
    print("\n".join(violations), file=sys.stderr)
    raise SystemExit(1)
print("Tracked source tree contains no local-state or generated-output paths.")

module_declaration = re.compile(
    r'(?m)^[ \t]*(?:#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]\s*)?'
    r'(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;'
)
metadata = json.loads(
    subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--quiet"],
        cwd=repository,
    )
)
roots = []
for package in metadata["packages"]:
    manifest = Path(package["manifest_path"])
    if manifest.is_relative_to(repository):
        roots.extend(Path(target["src_path"]) for target in package["targets"])

reachable = set()
pending = roots
while pending:
    source = pending.pop().resolve()
    if source in reachable or not source.exists():
        continue
    reachable.add(source)
    relative = source.relative_to(repository)
    text = source.read_text(errors="replace")
    for explicit, name in module_declaration.findall(text):
        if explicit:
            candidates = [source.parent / explicit]
        else:
            module_dir = (
                source.parent
                if source.name in {"lib.rs", "main.rs", "mod.rs"}
                else source.parent / source.stem
            )
            candidates = [module_dir / f"{name}.rs", module_dir / name / "mod.rs"]
        child = next((candidate for candidate in candidates if candidate.exists()), None)
        if child is not None:
            pending.append(child)

tracked_rust = {
    (repository / encoded.decode()).resolve()
    for encoded in tracked.split(b"\0")
    if encoded
    and encoded.decode().endswith(".rs")
    and (repository / encoded.decode()).exists()
}
orphaned_rust = sorted(tracked_rust - reachable)
if orphaned_rust:
    print("Remove or wire Rust source files that no Cargo target can reach:", file=sys.stderr)
    print(
        "\n".join(str(path.relative_to(repository)) for path in orphaned_rust),
        file=sys.stderr,
    )
    raise SystemExit(1)
print(f"All {len(tracked_rust)} tracked Rust files are reachable from Cargo targets.")
