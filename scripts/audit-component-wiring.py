#!/usr/bin/env python3
"""Fail when first-party source modules become orphaned or dormant evolution code returns."""

from __future__ import annotations

import argparse
import ast
import json
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RUST_MEMBERS = ("bin/orvek", "crates/executor", "crates/harness", "crates/memory")
IGNORED_PREFIXES = (Path("evals/incident_replay"),)
BANNED_PATHS = (
    "crates/harness/src/evolution",
    "crates/harness/src/controller/evolution.rs",
    "crates/harness/src/store/evolution.rs",
    "crates/harness/tests/evolution_artifacts.rs",
    "crates/harness/tests/evolution_binding.rs",
    "crates/harness/tests/evolution_campaign.rs",
    "crates/harness/tests/evolution_composition.rs",
    "crates/harness/tests/evolution_manifest.rs",
    "crates/harness/tests/evolution_mining.rs",
    "crates/harness/tests/evolution_promotion.rs",
    "crates/harness/tests/evolution_proposal.rs",
    "crates/harness/tests/evolution_statistics.rs",
    "crates/harness/tests/evolution_store_migration.rs",
    "crates/harness/tests/evolution_trials.rs",
    "docs/plans/self-evolving-harness",
    "evals/self_harness",
)
BANNED_SYMBOLS = (
    "EvolutionCoordinator",
    "StartEvolution",
    "register_harness_revision",
    "register_evaluation_cohort",
    "append_campaign_event",
    "activate_harness_revision",
    "rollback_harness_revision",
)
REQUIRED_SOURCE_EDGES = {
    "bin/orvek/src/main.rs": ("Cli::parse().run()",),
    "bin/orvek/src/core/mod.rs": ("Command::CreateSession", "SessionAdmissionRequest::new"),
    "crates/harness/src/ipc.rs": ("Command::CreateSession", "Command::Submit"),
    "crates/harness/src/controller.rs": ("resolve_admission", "behavior_instructions()"),
    "crates/harness/src/session.rs": ("SessionAdmissionProfile", "ValidatedHarnessRevision::compiled_baseline", "revision_manifest"),
}
REQUIRED_REPORT_HEADINGS = (
    "## Verdict",
    "## Scope and method",
    "## Query execution path",
    "## Admission compatibility path",
    "## Paper-to-code disposition",
    "## Removed disconnected code",
    "## Retained components",
    "## Compatibility residues",
    "## Verification",
    "## Exclusions",
)
PATH_MOD_RE = re.compile(r'(?m)^\s*#\[path\s*=\s*"([^"]+)"\]\s*\n\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;')
MOD_RE = re.compile(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;")
TS_IMPORT_RE = re.compile(r"(?:from\s+|import\s*\()(?P<quote>['\"])(?P<path>\.{1,2}/[^'\"]+)(?P=quote)")


def relative(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def rust_roots(member: Path) -> set[Path]:
    manifest = tomllib.loads((member / "Cargo.toml").read_text())
    roots: set[Path] = set()
    package = manifest.get("package", {})
    library = manifest.get("lib")
    if library is not None:
        roots.add(member / library.get("path", "src/lib.rs"))
    elif (member / "src/lib.rs").is_file():
        roots.add(member / "src/lib.rs")
    bins = manifest.get("bin", [])
    for binary in bins:
        roots.add(member / binary.get("path", f"src/bin/{binary['name']}.rs"))
    for target_kind, default_dir in (("bench", "benches"), ("example", "examples"), ("test", "tests")):
        for target in manifest.get(target_kind, []):
            roots.add(member / target.get("path", f"{default_dir}/{target['name']}.rs"))
    if not bins and (member / "src/main.rs").is_file():
        roots.add(member / "src/main.rs")
    roots.update((member / "src/bin").glob("*.rs") if (member / "src/bin").is_dir() else ())
    roots.update(member.glob("examples/*.rs"))
    build = package.get("build")
    if build is not False and (member / (build or "build.rs")).is_file():
        roots.add(member / (build or "build.rs"))
    return {path.resolve() for path in roots if path.is_file()}


def rust_module_target(source: Path, name: str) -> Path | None:
    if source.name in {"lib.rs", "main.rs", "mod.rs"}:
        base = source.parent
    else:
        base = source.parent / source.stem
    for candidate in (base / f"{name}.rs", base / name / "mod.rs"):
        if candidate.is_file():
            return candidate.resolve()
    return None


def audit_rust(errors: list[str]) -> tuple[int, int, list[str]]:
    all_source: set[Path] = set()
    roots: set[Path] = set()
    for member_name in RUST_MEMBERS:
        member = ROOT / member_name
        all_source.update(path.resolve() for path in (member / "src").rglob("*.rs"))
        roots.update(rust_roots(member))
    all_source.update(roots)
    reachable = set(roots)
    pending = list(roots)
    while pending:
        source = pending.pop()
        text = source.read_text()
        path_modules = {name: path for path, name in PATH_MOD_RE.findall(text)}
        for name in MOD_RE.findall(text):
            target = (source.parent / path_modules[name]).resolve() if name in path_modules else rust_module_target(source, name)
            if target is None:
                errors.append(f"unresolved Rust module `{name}` declared by {relative(source)}")
            elif target not in reachable:
                reachable.add(target)
                pending.append(target)
    orphaned = sorted(all_source - reachable)
    for path in orphaned:
        errors.append(f"orphaned Rust source module: {relative(path)}")
    return len(all_source), len(reachable), sorted(relative(path) for path in reachable)


def python_target(module: str, files: set[Path]) -> Path | None:
    parts = module.split(".") if module else []
    candidates = [ROOT / "evals" / Path(*parts).with_suffix(".py"), ROOT / "evals" / Path(*parts) / "__init__.py"]
    return next((path.resolve() for path in candidates if path.resolve() in files), None)


def audit_python(errors: list[str]) -> tuple[int, int, int, list[str]]:
    files = {
        path.resolve()
        for path in (ROOT / "evals").rglob("*.py")
        if not any(prefix in path.relative_to(ROOT).parents or path.relative_to(ROOT) == prefix for prefix in IGNORED_PREFIXES)
        and ".venv" not in path.parts
        and "__pycache__" not in path.parts
    }
    ignored = sum(1 for path in (ROOT / "evals").rglob("*.py") if (ROOT / IGNORED_PREFIXES[0]) in path.parents)
    roots = {
        path for path in files
        if path.name.startswith("test_") or "/tests/" in relative(path) or "if __name__" in path.read_text()
    }
    reachable = set(roots)
    pending = list(roots)
    while pending:
        source = pending.pop()
        try:
            tree = ast.parse(source.read_text(), filename=relative(source))
        except SyntaxError as error:
            errors.append(f"invalid Python source {relative(source)}: {error}")
            continue
        package_parts = list(source.relative_to(ROOT / "evals").with_suffix("").parts)
        if source.name == "__init__.py":
            package_parts.pop()
        else:
            package_parts.pop()
        modules: set[str] = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                modules.update(alias.name for alias in node.names)
            elif isinstance(node, ast.ImportFrom):
                base = package_parts[: len(package_parts) - max(node.level - 1, 0)] if node.level else []
                module_parts = base + (node.module.split(".") if node.module else [])
                modules.add(".".join(module_parts))
                modules.update(".".join(module_parts + [alias.name]) for alias in node.names)
        for module in modules:
            target = python_target(module, files)
            if target is not None and target not in reachable:
                reachable.add(target)
                pending.append(target)
                parent = target.parent / "__init__.py"
                if parent.resolve() in files and parent.resolve() not in reachable:
                    reachable.add(parent.resolve())
                    pending.append(parent.resolve())
    for path in sorted(files - reachable):
        errors.append(f"orphaned Python source module: {relative(path)}")
    return len(files), len(reachable), ignored, sorted(relative(path) for path in reachable)


def ts_target(source: Path, specifier: str, files: set[Path]) -> Path | None:
    base = (source.parent / specifier).resolve()
    candidates = [base, base.with_suffix(".ts"), base / "index.ts"]
    return next((path for path in candidates if path in files), None)


def audit_typescript(errors: list[str]) -> tuple[int, int, list[str]]:
    root = ROOT / "web/review"
    files = {path.resolve() for path in root.rglob("*.ts") if "node_modules" not in path.parts}
    roots = {path for path in files if path.name in {"app.ts", "build.ts", "dev.ts"} or path.name.endswith(".test.ts")}
    reachable = set(roots)
    pending = list(roots)
    while pending:
        source = pending.pop()
        for match in TS_IMPORT_RE.finditer(source.read_text()):
            target = ts_target(source, match.group("path"), files)
            if target is None:
                errors.append(f"unresolved TypeScript import {match.group('path')} in {relative(source)}")
            elif target not in reachable:
                reachable.add(target)
                pending.append(target)
    for path in sorted(files - reachable):
        errors.append(f"orphaned TypeScript source module: {relative(path)}")
    return len(files), len(reachable), sorted(relative(path) for path in reachable)


def audit_contract(report: Path, errors: list[str]) -> None:
    for item in BANNED_PATHS:
        if (ROOT / item).exists():
            errors.append(f"disconnected prototype path returned: {item}")
    first_party = [ROOT / path for path in RUST_MEMBERS] + [ROOT / "evals", ROOT / "web/review"]
    for symbol in BANNED_SYMBOLS:
        for base in first_party:
            if not base.exists():
                continue
            for path in base.rglob("*"):
                if path.is_file() and path.suffix in {".rs", ".py", ".ts"} and symbol in path.read_text(errors="ignore"):
                    errors.append(f"disconnected prototype symbol `{symbol}` returned in {relative(path)}")
    for name, tokens in REQUIRED_SOURCE_EDGES.items():
        path = ROOT / name
        if not path.is_file():
            errors.append(f"required execution-path file is missing: {name}")
            continue
        text = path.read_text()
        for token in tokens:
            if token not in text:
                errors.append(f"required execution edge `{token}` is missing from {name}")
    if not report.is_file():
        errors.append(f"audit report is missing: {relative(report)}")
        return
    text = report.read_text()
    for heading in REQUIRED_REPORT_HEADINGS:
        if heading not in text:
            errors.append(f"audit report is missing heading: {heading}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", type=Path, default=ROOT / "docs/audits/self-harness-wiring.md")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    report = args.report if args.report.is_absolute() else ROOT / args.report
    errors: list[str] = []
    rust = audit_rust(errors)
    python = audit_python(errors)
    typescript = audit_typescript(errors)
    audit_contract(report, errors)
    result = {
        "rust_modules": {"source": rust[0], "reachable": rust[1], "files": rust[2]},
        "python_modules": {"source": python[0], "reachable": python[1], "excluded_untracked": python[2], "files": python[3]},
        "typescript_modules": {"source": typescript[0], "reachable": typescript[1], "files": typescript[2]},
        "errors": errors,
    }
    if args.json:
        print(json.dumps(result, indent=2))
    elif errors:
        print("component wiring audit failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
    else:
        print(
            f"component wiring audit passed: {rust[1]} Rust, {python[1]} Python, "
            f"and {typescript[1]} TypeScript modules are reachable"
        )
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
