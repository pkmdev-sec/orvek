#!/usr/bin/env python3
"""Build the deterministic, machine-readable map of the Orvek source tree.

The graph deliberately indexes version-controlled inputs rather than the working
copy. A fresh agent can therefore use it without discovering a developer's
build products, credentials, or unrelated untracked experiments.
"""

from __future__ import annotations

import argparse
import ast
import hashlib
import json
import re
import subprocess
import sys
import tomllib
from collections import Counter
from pathlib import Path, PurePosixPath
from typing import Any, Iterable

ROOT = Path(__file__).resolve().parents[1]
GRAPH_DIRECTORY = Path("docs/codebase-graph")
GRAPH_PATH = GRAPH_DIRECTORY / "graph.json"
DOT_PATH = GRAPH_DIRECTORY / "architecture.dot"
DERIVED_PATHS = {GRAPH_PATH, DOT_PATH}
CAPABILITY_LEDGER_PATH = Path("assets/capabilities.json")
FINGERPRINT_EXCLUDED_PATHS = DERIVED_PATHS | {
    Path("assets/orvex-differentiators.gif"),
    Path("assets/orvex-differentiators.png"),
}
# During local development these authored inputs are untracked until the patch is
# committed. Include them now so the generated graph is identical before and
# after that commit.
AUTHORED_GRAPH_INPUTS = (
    Path("CODEBASE.md"),
    GRAPH_DIRECTORY / "README.md",
    GRAPH_DIRECTORY / "overview.md",
    Path("docs/design/maintainability-baseline.md"),
    Path("scripts/generate-codebase-graph.py"),
    Path("scripts/tests/test_generate_codebase_graph.py"),
    Path("crates/harness/benches/event_replay.rs"),
)

MOD_RE = re.compile(
    r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)
PATH_MOD_RE = re.compile(
    r'(?m)^\s*#\[path\s*=\s*"([^"]+)"\]\s*\n\s*'
    r"(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)
RUST_USE_RE = re.compile(r"(?m)^\s*use\s+([A-Za-z_][A-Za-z0-9_]*)")
TS_IMPORT_RE = re.compile(
    r"(?:\bfrom\s+|\bimport\s*\(\s*|\bimport\s+)"
    r"(?P<quote>['\"])(?P<path>\.{1,2}/[^'\"]+)(?P=quote)"
)
MARKDOWN_LINK_RE = re.compile(r"(?<!!)\[[^\]]*\]\(([^)\s#]+)(?:#[^)\s]+)?\)")
INCLUDE_RE = re.compile(r'(?:include_str|include_bytes|include)!\s*\(\s*"([^"]+)"')
DEPENDENCY_SECTIONS = {"dependencies", "dev-dependencies", "build-dependencies"}

COMPONENTS: tuple[dict[str, Any], ...] = (
    {
        "id": "component:cli",
        "label": "CLI and configuration boundary",
        "summary": "Parses commands, loads configuration and dispatches terminal, headless, host, memory and review flows.",
        "files": ("bin/orvek/src/main.rs", "bin/orvek/src/app/cli.rs", "bin/orvek/src/app/config.rs"),
    },
    {
        "id": "component:session-assembly",
        "label": "Session assembly",
        "summary": "Resolves a workspace, selects skills and memory, and admits or resumes a host session.",
        "files": ("bin/orvek/src/core/mod.rs", "bin/orvek/src/core/extensions/skills.rs"),
    },
    {
        "id": "component:host-client",
        "label": "Detached host client",
        "summary": "Owns compatible-host connection, local IPC requests, watch reconnection and uncertain submission acknowledgement.",
        "files": ("bin/orvek/src/app/host.rs", "bin/orvek/src/app/submission.rs"),
    },
    {
        "id": "component:host-server",
        "label": "Detached host server",
        "summary": "Starts the detached host process, constructs provider and executor dependencies, and serves harness IPC.",
        "files": ("bin/orvek/src/app/host.rs", "bin/orvek/src/app/shutdown.rs"),
    },
    {
        "id": "component:harness",
        "label": "Authoritative harness",
        "summary": "Owns admission, contracts, queueing, durable task state, evidence, completion and recovery.",
        "files": ("crates/harness/src/lib.rs", "crates/harness/src/controller.rs", "crates/harness/src/store.rs"),
    },
    {
        "id": "component:runtime",
        "label": "Inference and execution runtime",
        "summary": "Builds bounded model context, drives provider turns and mediates workspace capabilities and execution.",
        "files": ("crates/harness/src/runtime.rs", "crates/harness/src/inference/protocol.rs", "crates/harness/src/capabilities.rs"),
    },
    {
        "id": "component:executor",
        "label": "Sandbox executor",
        "summary": "Defines the bounded executor transport and packaged Linux supervisor used by the harness runtime.",
        "files": ("crates/executor/src/lib.rs", "crates/executor/src/supervisor.rs"),
    },
    {
        "id": "component:tui",
        "label": "Terminal presentation",
        "summary": "Maintains disposable UI state and projects authoritative host journal records into panes and transcript components.",
        "files": ("bin/orvek/src/tui/client.rs", "bin/orvek/src/tui/host_projection.rs", "bin/orvek/src/tui/components/root.rs"),
    },
    {
        "id": "component:headless",
        "label": "Headless JSONL projection",
        "summary": "Submits a task to the same host and emits the durable session and linked-task projection for automation.",
        "files": ("bin/orvek/src/app/headless.rs",),
    },
    {
        "id": "component:review",
        "label": "Browser review",
        "summary": "Serves review assets and maps browser feedback to authenticated host review commands.",
        "files": ("bin/orvek/src/review/mod.rs", "bin/orvek/src/review/server.rs", "bin/orvek/src/review/diff.rs"),
    },
    {
        "id": "component:memory",
        "label": "Shared memory",
        "summary": "Provides bounded local SQLite and optional remote memory stores, client, server and tool contracts.",
        "files": ("crates/memory/src/lib.rs", "crates/memory/src/store/local.rs", "crates/memory/src/retrieval.rs"),
    },
    {
        "id": "component:subagents",
        "label": "Isolated subagents",
        "summary": "Owns session-scoped child admission, lifecycle, messaging and retained observation state.",
        "files": ("crates/harness/src/controller/subagents.rs",),
    },
    {
        "id": "component:delivery",
        "label": "Verification and delivery",
        "summary": "Records verification evidence and reproduces patch artifacts from controlled snapshots before delivery.",
        "files": ("crates/harness/src/verification.rs", "crates/harness/src/delivery.rs", "crates/harness/src/delivery/git.rs"),
    },
    {
        "id": "component:operations",
        "label": "Build, release and evaluation automation",
        "summary": "Contains workspace commands, CI, Docker packaging, Harbor evaluation adapter and source-tree audits.",
        "files": ("justfile", ".github/workflows/ci.yaml", "evals/README.md", "scripts/audit-component-wiring.py"),
    },
)

COMPONENT_EDGES: tuple[tuple[str, str, str], ...] = (
    ("component:cli", "component:session-assembly", "creates"),
    ("component:cli", "component:tui", "starts"),
    ("component:cli", "component:headless", "starts"),
    ("component:cli", "component:review", "starts"),
    ("component:session-assembly", "component:host-client", "admits_through"),
    ("component:tui", "component:host-client", "queries"),
    ("component:headless", "component:host-client", "queries"),
    ("component:review", "component:host-client", "queries"),
    ("component:host-client", "component:host-server", "connects_to"),
    ("component:host-server", "component:harness", "serves"),
    ("component:harness", "component:runtime", "orchestrates"),
    ("component:harness", "component:subagents", "orchestrates"),
    ("component:harness", "component:delivery", "certifies_with"),
    ("component:runtime", "component:executor", "executes_through"),
    ("component:session-assembly", "component:memory", "validates_configuration_of"),
    ("component:cli", "component:memory", "manages"),
    ("component:tui", "component:memory", "uses"),
    ("component:operations", "component:executor", "packages_and_evaluates"),
)

NODE_KIND_DESCRIPTIONS = {
    "repository": "The source repository root.",
    "directory": "A directory containing indexed source or supporting files.",
    "file": "A version-controlled input file, excluding generated graph outputs.",
    "workspace": "The Cargo workspace declared by the root manifest.",
    "package": "A Cargo package discovered in the source tree; workspace_member states whether it belongs to the workspace.",
    "dependency": "A direct Cargo dependency outside this workspace.",
    "target": "A Rust target group inferred from explicit Cargo declarations and conventional source locations.",
    "module": "A Rust module inferred from a source path.",
    "component": "A curated architectural responsibility spanning one or more files.",
    "capability": "A source-backed product capability with an explicit status, owner, executable path, proof and documentation.",
}

EDGE_KIND_DESCRIPTIONS = {
    "contains": "The source node contains the target node.",
    "declared_in": "The source package or workspace is declared by the target manifest file.",
    "member_of": "The source package belongs to the target workspace.",
    "patches": "The source workspace redirects the named registry package to the target vendored package.",
    "depends_on": "The source Cargo package directly depends on the target package or external dependency.",
    "defines": "The source package defines the target Rust crate target.",
    "implemented_by": "The source module or component is implemented by the target source file.",
    "declares_module": "The source Rust module declares the target Rust module through a file-backed mod declaration.",
    "imports": "The source Rust module imports the target direct Cargo dependency.",
    "imports_local": "The source TypeScript or Python file imports the target local source file.",
    "includes": "The source Rust module includes the target tracked file at compile time.",
    "links_to": "The source Markdown file links to the target local file.",
    "creates": "The source component creates the target component.",
    "starts": "The source component starts the target component.",
    "admits_through": "The source component asks the target component to admit a session.",
    "queries": "The source component acts as a client of the target component.",
    "connects_to": "The source component connects to the target component over the local host boundary.",
    "serves": "The source component hosts the target component's authority.",
    "orchestrates": "The source component coordinates the target component.",
    "certifies_with": "The source component relies on the target component for verification or delivery evidence.",
    "executes_through": "The source component runs commands through the target component.",
    "validates_configuration_of": "The source component validates the target capability configuration without making it host session state.",
    "manages": "The source component manages the target capability through its command surface.",
    "uses": "The source component uses the target capability directly.",
    "packages_and_evaluates": "The source component packages or evaluates the target component.",
    "owned_by": "The source capability belongs to the target architectural component.",
    "entered_through": "The source capability is reached through the target file.",
    "dispatched_by": "The source capability executes through the target file.",
    "persisted_by": "The source capability stores durable state through the target file.",
    "proved_by": "The source capability has focused behavioral evidence in the target file.",
    "documented_by": "The source capability is documented by the target file.",
}

CAPABILITY_REFERENCE_EDGES = {
    "entrypoints": "entered_through",
    "dispatchers": "dispatched_by",
    "persistence": "persisted_by",
    "proof": "proved_by",
    "documentation": "documented_by",
}
CAPABILITY_STATUSES = {"implemented", "config_only", "experimental"}
CAPABILITY_ID_RE = re.compile(r"^[a-z][a-z0-9-]*$")


def relative(path: Path) -> Path:
    return path.relative_to(ROOT)


def path_id(prefix: str, path: Path) -> str:
    return f"{prefix}:{path.as_posix()}"


def text_or_none(path: Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return None


def source_fingerprint(paths: Iterable[Path]) -> str:
    """Identify the exact authored input snapshot without hashing derived outputs."""
    digest = hashlib.sha256()
    for path in sorted(set(paths) - FINGERPRINT_EXCLUDED_PATHS, key=lambda item: item.as_posix()):
        digest.update(path.as_posix().encode("utf-8"))
        digest.update(b"\0")
        digest.update((ROOT / path).read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def validate_capability_reference(
    capability_id: str,
    field: str,
    reference: Any,
    files: set[Path],
    contents: dict[Path, str | None],
) -> Path:
    if not isinstance(reference, dict) or set(reference) != {"path", "anchor"}:
        raise ValueError(
            f"capability {capability_id} {field} reference must contain path and anchor"
        )
    path_value = reference.get("path")
    anchor = reference.get("anchor")
    if not isinstance(path_value, str) or not path_value or not isinstance(anchor, str) or not anchor:
        raise ValueError(f"capability {capability_id} {field} reference is invalid")
    path = Path(path_value)
    if path not in files:
        raise ValueError(f"capability {capability_id} {field} references missing file {path}")
    content = contents.get(path)
    if content is None:
        raise ValueError(f"capability {capability_id} {field} references non-text file {path}")
    if anchor not in content:
        raise ValueError(
            f"capability {capability_id} {field} anchor {anchor!r} is missing from {path}"
        )
    return path


def reference_identity(reference: dict[str, str]) -> tuple[str, str]:
    return reference["path"], reference["anchor"]


def anchored_scope(path: Path, content: str, anchor: str) -> str:
    """Return the formatted function/method body introduced by one exact anchor."""
    if content.count(anchor) != 1:
        raise ValueError(f"execution scope anchor must occur exactly once in {path}: {anchor}")
    position = content.index(anchor)
    line_start = content.rfind("\n", 0, position) + 1
    lines = content[line_start:].splitlines(keepends=True)
    indentation = len(lines[0]) - len(lines[0].lstrip(" \t"))
    if path.suffix == ".py":
        end = len(lines)
        for index, line in enumerate(lines[1:], 1):
            stripped = line.strip()
            if not stripped or stripped.startswith("#"):
                continue
            current = len(line) - len(line.lstrip(" \t"))
            if current <= indentation:
                end = index
                break
    else:
        end = next(
            (
                index + 1
                for index, line in enumerate(lines[1:], 1)
                if line.strip() == "}"
                and len(line) - len(line.lstrip(" \t")) == indentation
            ),
            None,
        )
        if end is None:
            raise ValueError(f"execution scope is not a formatted block in {path}: {anchor}")
    return "".join(lines[:end])


def validate_execution_paths(
    capability_id: str,
    execution_paths: Any,
    references: dict[str, list[Any]],
    files: set[Path],
    contents: dict[Path, str | None],
) -> None:
    if not isinstance(execution_paths, list) or not execution_paths:
        raise ValueError(f"capability {capability_id} has no execution paths")
    entrypoints = {reference_identity(reference) for reference in references["entrypoints"]}
    dispatchers = {reference_identity(reference) for reference in references["dispatchers"]}
    reached_dispatchers: set[tuple[str, str]] = set()
    for path_index, execution_path in enumerate(execution_paths):
        if not isinstance(execution_path, list) or len(execution_path) < 2:
            raise ValueError(
                f"capability {capability_id} execution path {path_index} must have at least two steps"
            )
        steps: list[tuple[str, str]] = []
        for step_index, step in enumerate(execution_path):
            expected_fields = {"path", "anchor"} if step_index == len(execution_path) - 1 else {
                "path",
                "anchor",
                "call",
            }
            if not isinstance(step, dict) or set(step) != expected_fields:
                raise ValueError(
                    f"capability {capability_id} execution path {path_index} step {step_index} is invalid"
                )
            reference = {"path": step.get("path"), "anchor": step.get("anchor")}
            source = validate_capability_reference(
                capability_id, "execution_paths", reference, files, contents
            )
            steps.append(reference_identity(reference))
            if step_index < len(execution_path) - 1:
                call = step.get("call")
                if not isinstance(call, str) or not call:
                    raise ValueError(
                        f"capability {capability_id} execution path {path_index} has no callsite"
                    )
                scope = anchored_scope(source, contents[source] or "", reference["anchor"])
                if call not in scope:
                    raise ValueError(
                        f"capability {capability_id} execution call {call!r} is outside the declared scope in {source}"
                    )
        if steps[0] not in entrypoints:
            raise ValueError(
                f"capability {capability_id} execution path {path_index} does not start at an entrypoint"
            )
        if steps[-1] not in dispatchers:
            raise ValueError(
                f"capability {capability_id} execution path {path_index} does not end at a dispatcher"
            )
        reached_dispatchers.add(steps[-1])
    missing = dispatchers - reached_dispatchers
    if missing:
        raise ValueError(
            f"capability {capability_id} dispatchers lack execution paths: {sorted(missing)}"
        )


def load_capability_ledger(
    files: set[Path], contents: dict[Path, str | None]
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    if CAPABILITY_LEDGER_PATH not in files:
        raise ValueError(f"capability ledger is missing: {CAPABILITY_LEDGER_PATH}")
    ledger = json.loads((ROOT / CAPABILITY_LEDGER_PATH).read_text(encoding="utf-8"))
    if ledger.get("schema_version") != 1:
        raise ValueError("unsupported capability ledger schema")
    overview = ledger.get("overview")
    if not isinstance(overview, dict) or not all(
        isinstance(overview.get(field), str) and overview[field].strip()
        for field in ("title", "subtitle", "footnote")
    ):
        raise ValueError("capability ledger overview is incomplete")
    capabilities = ledger.get("capabilities")
    if not isinstance(capabilities, list) or not capabilities:
        raise ValueError("capability ledger has no capabilities")
    component_ids = {component["id"] for component in COMPONENTS}
    seen: set[str] = set()
    capability_owners: set[str] = set()
    for capability in capabilities:
        if not isinstance(capability, dict):
            raise ValueError("capability ledger entries must be objects")
        capability_id = capability.get("id")
        if (
            not isinstance(capability_id, str)
            or not CAPABILITY_ID_RE.fullmatch(capability_id)
            or capability_id in seen
        ):
            raise ValueError(f"invalid or duplicate capability ID: {capability_id}")
        seen.add(capability_id)
        for field in ("title", "summary"):
            if not isinstance(capability.get(field), str) or not capability[field].strip():
                raise ValueError(f"capability {capability_id} has no {field}")
        status = capability.get("status")
        if status not in CAPABILITY_STATUSES:
            raise ValueError(f"capability {capability_id} has invalid status {status}")
        if capability.get("owner") not in component_ids:
            raise ValueError(f"capability {capability_id} has unknown owner")
        capability_owners.add(capability["owner"])
        modes = capability.get("runtime_modes")
        if (
            not isinstance(modes, list)
            or not modes
            or len(modes) != len(set(modes))
            or not all(isinstance(mode, str) and CAPABILITY_ID_RE.fullmatch(mode) for mode in modes)
        ):
            raise ValueError(f"capability {capability_id} has invalid runtime modes")
        limitations = capability.get("limitations")
        if not isinstance(limitations, list) or not all(
            isinstance(item, str) and item.strip() for item in limitations
        ):
            raise ValueError(f"capability {capability_id} has invalid limitations")
        if status in {"experimental", "config_only"} and not limitations:
            raise ValueError(f"capability {capability_id} must document its limitations")
        diagram = capability.get("diagram")
        if diagram is not None:
            lines = diagram.get("lines") if isinstance(diagram, dict) else None
            if (
                not isinstance(lines, list)
                or not 1 <= len(lines) <= 2
                or not all(isinstance(line, str) and line.strip() for line in lines)
            ):
                raise ValueError(f"capability {capability_id} has invalid diagram lines")
        references: dict[str, list[Any]] = {}
        for field in CAPABILITY_REFERENCE_EDGES:
            value = capability.get(field)
            if not isinstance(value, list):
                raise ValueError(f"capability {capability_id} {field} must be a list")
            references[field] = value
            for reference in value:
                validate_capability_reference(
                    capability_id, field, reference, files, contents
                )
        for required in ("entrypoints", "proof", "documentation"):
            if not references[required]:
                raise ValueError(f"capability {capability_id} has no {required}")
        if status in {"implemented", "experimental"} and not references["dispatchers"]:
            raise ValueError(f"capability {capability_id} has no executable dispatcher")
        if status == "config_only" and references["dispatchers"]:
            raise ValueError(f"config-only capability {capability_id} claims a dispatcher")
        execution_paths = capability.get("execution_paths")
        if status in {"implemented", "experimental"}:
            validate_execution_paths(
                capability_id, execution_paths, references, files, contents
            )
        elif execution_paths not in (None, []):
            raise ValueError(f"config-only capability {capability_id} claims an execution path")
    coverage = ledger.get("capability_coverage")
    if not isinstance(coverage, dict) or set(coverage) != {"mode", "exempt_components"}:
        raise ValueError("capability_coverage needs exactly mode and exempt_components")
    if coverage["mode"] != "complete" or not isinstance(coverage["exempt_components"], list):
        raise ValueError("the product ledger requires complete capability coverage")
    exemptions: set[str] = set()
    for exemption in coverage["exempt_components"]:
        if not isinstance(exemption, dict) or set(exemption) != {"component", "reason"}:
            raise ValueError("capability exemptions need exactly component and reason")
        component, reason = exemption["component"], exemption["reason"]
        if (
            not isinstance(component, str)
            or component not in component_ids
            or component in exemptions
            or not isinstance(reason, str)
            or not reason.strip()
        ):
            raise ValueError("capability exemption has an invalid component or reason")
        exemptions.add(component)
    overlap = capability_owners & exemptions
    if overlap:
        raise ValueError(f"capability owners cannot also be exempt: {sorted(overlap)}")
    uncovered = component_ids - capability_owners - exemptions
    if uncovered:
        raise ValueError(f"complete capability coverage has uncovered components: {sorted(uncovered)}")
    return ledger, capabilities


def tracked_paths() -> list[Path]:
    result = subprocess.run(
        ["git", "ls-files", "-z"], cwd=ROOT, check=True, stdout=subprocess.PIPE
    )
    paths = {Path(item.decode("utf-8")) for item in result.stdout.split(b"\0") if item}
    paths.update(path for path in AUTHORED_GRAPH_INPUTS if (ROOT / path).is_file())
    return sorted(
        (path for path in paths if path not in DERIVED_PATHS and (ROOT / path).is_file()),
        key=lambda path: path.as_posix(),
    )


def file_kind(path: Path, content: str | None) -> str:
    if content is None:
        return "binary-asset"
    if path.name == "Cargo.toml":
        return "cargo-manifest"
    if path.suffix == ".rs":
        return "rust-source"
    if path.suffix in {".ts", ".tsx"}:
        return "typescript-source"
    if path.suffix == ".py":
        return "python-script"
    if path.suffix == ".md":
        return "documentation"
    if path.parts[:2] == (".github", "workflows"):
        return "ci-workflow"
    if path.suffix in {".toml", ".yaml", ".yml", ".json", ".jsonc", ".hcl"}:
        return "configuration"
    if path.suffix in {".html", ".css", ".svg"}:
        return "web-asset"
    if path.suffix in {".sh", ".Dockerfile"} or path.name.endswith("Dockerfile"):
        return "automation"
    return "supporting-file"


def source_directories(paths: Iterable[Path]) -> list[Path]:
    directories = {Path(".")}
    for path in paths:
        parent = path.parent
        while parent != Path("."):
            directories.add(parent)
            parent = parent.parent
    return sorted(directories, key=lambda path: (len(path.parts), path.as_posix()))


def add_edge(edges: set[tuple[str, str, str]], source: str, kind: str, target: str) -> None:
    edges.add((source, kind, target))


def dependency_names(value: dict[str, Any]) -> set[str]:
    names: set[str] = set()
    for key, child in value.items():
        if key in DEPENDENCY_SECTIONS and isinstance(child, dict):
            names.update(child)
        elif isinstance(child, dict):
            names.update(dependency_names(child))
    return names


def cargo_packages(paths: list[Path]) -> tuple[list[dict[str, Any]], dict[str, dict[str, Any]]]:
    packages: list[dict[str, Any]] = []
    for path in paths:
        if path.name != "Cargo.toml":
            continue
        manifest = tomllib.loads((ROOT / path).read_text(encoding="utf-8"))
        package = manifest.get("package")
        if not isinstance(package, dict) or not isinstance(package.get("name"), str):
            continue
        packages.append(
            {
                "name": package["name"],
                "manifest": path,
                "directory": path.parent,
                "manifest_data": manifest,
            }
        )
    packages.sort(key=lambda package: package["name"])
    return packages, {package["name"]: package for package in packages}


def package_for_path(path: Path, packages: list[dict[str, Any]]) -> dict[str, Any] | None:
    candidates = [
        package
        for package in packages
        if package["directory"] != Path(".") and path.is_relative_to(package["directory"])
    ]
    return max(candidates, key=lambda package: len(package["directory"].parts), default=None)


def configured_target_for_source(path: Path, package: dict[str, Any]) -> str | None:
    """Return the Cargo target suffix for an explicit target root or its sibling modules."""
    relative_path = path.relative_to(package["directory"])
    manifest = package["manifest_data"]
    configured: list[tuple[str, str, Path]] = []
    library = manifest.get("lib")
    if isinstance(library, dict):
        configured.append(("lib", library.get("name", "lib"), Path(library.get("path", "src/lib.rs"))))
    for kind in ("bin", "bench", "example", "test"):
        for target in manifest.get(kind, []):
            if not isinstance(target, dict) or not isinstance(target.get("name"), str):
                continue
            default = f"src/bin/{target['name']}.rs" if kind == "bin" else f"{kind}es/{target['name']}.rs"
            configured.append((kind, target["name"], Path(target.get("path", default))))
    for kind, name, root in configured:
        if relative_path == root:
            return "lib" if kind == "lib" else f"{kind}:{name}"
        module_directory = (
            root.parent if root.name in {"lib.rs", "main.rs", "mod.rs"} else root.with_suffix("")
        )
        if relative_path.is_relative_to(module_directory):
            return "lib" if kind == "lib" else f"{kind}:{name}"
    return None


def target_for_source(path: Path, package: dict[str, Any]) -> tuple[str, str]:
    """Return a target grouping from explicit Cargo declarations and Cargo conventions."""
    relative_path = path.relative_to(package["directory"])
    package_name = package["name"]
    suffix = "lib" if relative_path == Path("src/lib.rs") else configured_target_for_source(path, package)
    if suffix is None:
        has_lib = (ROOT / package["directory"] / "src/lib.rs").is_file()
        if relative_path == Path("src/main.rs"):
            suffix = f"bin:{package_name}"
        elif relative_path == Path("build.rs"):
            suffix = "build"
        elif relative_path.parts and relative_path.parts[0] == "tests":
            suffix = f"test:{relative_path.stem}"
        elif relative_path.parts and relative_path.parts[0] == "benches":
            suffix = f"bench:{relative_path.stem}"
        elif relative_path.parts and relative_path.parts[0] == "examples":
            suffix = f"example:{relative_path.stem}"
        elif has_lib:
            suffix = "lib"
        else:
            suffix = f"bin:{package_name}"
    target_id = f"target:{package_name}:{suffix}"
    return target_id, f"{package_name} {suffix}"


def module_name_for_source(path: Path, package: dict[str, Any], target_id: str) -> str:
    relative_path = path.relative_to(package["directory"])
    target_suffix = target_id.split(":", maxsplit=2)[2]
    if relative_path.parts and relative_path.parts[0] == "src":
        parts = list(relative_path.parts[1:])
        if path.name in {"lib.rs", "main.rs"}:
            module_parts: list[str] = []
        elif path.name == "mod.rs":
            module_parts = parts[:-1]
        else:
            module_parts = parts[:-1] + [path.stem]
    else:
        module_parts = list(relative_path.with_suffix("").parts)
    suffix = "::".join(module_parts)
    return f"module:{package['name']}:{target_suffix}" + (f"::{suffix}" if suffix else "")


def module_target(source: Path, name: str, configured_path: str | None = None) -> Path | None:
    if configured_path is not None:
        candidate = (source.parent / configured_path).resolve()
        return candidate if candidate.is_file() else None
    base = source.parent if source.name in {"lib.rs", "main.rs", "mod.rs"} else source.parent / source.stem
    for candidate in (base / f"{name}.rs", base / name / "mod.rs"):
        if candidate.is_file():
            return candidate.resolve()
    return None


def local_link_target(source: Path, destination: str, files: set[Path]) -> Path | None:
    candidate = (source.parent / PurePosixPath(destination)).resolve()
    try:
        path = relative(candidate)
    except ValueError:
        return None
    return path if path in files else None


def typescript_import_target(source: Path, destination: str, files: set[Path]) -> Path | None:
    """Resolve TypeScript's extensionless and emitted-JavaScript local import forms."""
    base = (source.parent / PurePosixPath(destination)).resolve()
    candidates = [base]
    if base.suffix in {".js", ".mjs", ".cjs"}:
        candidates.extend(base.with_suffix(extension) for extension in (".ts", ".tsx"))
    if not base.suffix:
        candidates.extend(base.with_suffix(extension) for extension in (".ts", ".tsx", ".js", ".mjs"))
        candidates.extend(base / f"index{extension}" for extension in (".ts", ".tsx", ".js", ".mjs"))
    for candidate in candidates:
        try:
            path = relative(candidate)
        except ValueError:
            continue
        if path in files:
            return path
    return None


def python_module_index(paths: Iterable[Path]) -> dict[str, set[Path]]:
    """Index importable local Python names, including short script-style imports."""
    index: dict[str, set[Path]] = {}
    for path in paths:
        if path.suffix != ".py":
            continue
        parts = list(path.with_suffix("").parts)
        if path.name == "__init__.py":
            parts.pop()
        if not parts or not all(part.isidentifier() for part in parts):
            continue
        for start in range(len(parts)):
            name = ".".join(parts[start:])
            index.setdefault(name, set()).add(path)
    return index


def local_python_imports(path: Path, content: str, index: dict[str, set[Path]]) -> set[Path]:
    """Return unambiguous local Python imports without mistaking stdlib for source."""
    tree = ast.parse(content, filename=path.as_posix())
    imported: set[Path] = set()
    for statement in ast.walk(tree):
        names: list[str] = []
        if isinstance(statement, ast.Import):
            names = [alias.name for alias in statement.names]
        elif isinstance(statement, ast.ImportFrom) and statement.level == 0 and statement.module:
            names = [statement.module]
        for name in names:
            candidates = index.get(name, set())
            if len(candidates) == 1:
                imported.update(candidates)
    return imported


def validate_component_files(
    files: set[Path], components: Iterable[dict[str, Any]] = COMPONENTS
) -> None:
    for component in components:
        for filename in component["files"]:
            if Path(filename) not in files:
                raise ValueError(
                    f"curated component {component['id']} references missing file {filename}"
                )


def render_graph() -> tuple[str, str]:
    paths = tracked_paths()
    files = set(paths)
    nodes: dict[str, dict[str, Any]] = {}
    edges: set[tuple[str, str, str]] = set()
    reference_coverage = {
        "rust_compile_time_includes": {"detected": 0, "resolved": 0},
        "typescript_relative_imports": {"detected": 0, "resolved": 0},
    }
    unresolved_static_references: list[tuple[Path, str, str]] = []

    nodes["repository:orvek"] = {"id": "repository:orvek", "kind": "repository", "label": "Orvek"}
    for directory in source_directories(paths):
        directory_id = path_id("directory", directory)
        label = "." if directory == Path(".") else directory.name
        nodes[directory_id] = {
            "id": directory_id,
            "kind": "directory",
            "label": label,
            "path": directory.as_posix(),
        }
        if directory == Path("."):
            add_edge(edges, "repository:orvek", "contains", directory_id)
        else:
            add_edge(edges, path_id("directory", directory.parent), "contains", directory_id)

    contents: dict[Path, str | None] = {}
    for path in paths:
        content = text_or_none(ROOT / path)
        contents[path] = content
        node: dict[str, Any] = {
            "id": path_id("file", path),
            "kind": "file",
            "label": path.name,
            "path": path.as_posix(),
            "file_kind": file_kind(path, content),
        }
        if content is not None:
            node["lines"] = len(content.splitlines())
        nodes[node["id"]] = node
        add_edge(edges, path_id("directory", path.parent), "contains", node["id"])

    capability_ledger, capabilities = load_capability_ledger(files, contents)

    workspace_manifest = Path("Cargo.toml")
    root_manifest: dict[str, Any] = {}
    workspace_member_patterns: tuple[str, ...] = ()
    workspace_exclude_patterns: tuple[str, ...] = ()
    if workspace_manifest in files:
        root_manifest = tomllib.loads((ROOT / workspace_manifest).read_text(encoding="utf-8"))
        workspace = root_manifest.get("workspace")
        if isinstance(workspace, dict):
            workspace_member_patterns = tuple(workspace.get("members", ()))
            workspace_exclude_patterns = tuple(workspace.get("exclude", ()))
            nodes["workspace:orvek"] = {
                "id": "workspace:orvek",
                "kind": "workspace",
                "label": "Orvek Cargo workspace",
            }
            add_edge(edges, "workspace:orvek", "declared_in", path_id("file", workspace_manifest))

    packages, package_by_name = cargo_packages(paths)
    workspace_members = {
        package["name"]
        for package in packages
        if any(package["directory"].match(pattern) for pattern in workspace_member_patterns)
        and not any(package["directory"].match(pattern) for pattern in workspace_exclude_patterns)
    }
    for package in packages:
        package_id = f"package:{package['name']}"
        nodes[package_id] = {
            "id": package_id,
            "kind": "package",
            "label": package["name"],
            "path": package["directory"].as_posix(),
            "workspace_member": package["name"] in workspace_members,
        }
        add_edge(edges, package_id, "declared_in", path_id("file", package["manifest"]))
        if package["name"] in workspace_members:
            add_edge(edges, package_id, "member_of", "workspace:orvek")

    patches = root_manifest.get("patch", {})
    crates_io_patches = patches.get("crates-io", {}) if isinstance(patches, dict) else {}
    if isinstance(crates_io_patches, dict):
        for name in crates_io_patches:
            if name in package_by_name:
                add_edge(edges, "workspace:orvek", "patches", f"package:{name}")

    external_dependencies: set[str] = set()
    for package in packages:
        for dependency in dependency_names(package["manifest_data"]):
            target = (
                f"package:{dependency}"
                if dependency in package_by_name
                else f"dependency:{dependency}"
            )
            if dependency not in package_by_name:
                external_dependencies.add(dependency)
            add_edge(edges, f"package:{package['name']}", "depends_on", target)
    for dependency in sorted(external_dependencies):
        nodes[f"dependency:{dependency}"] = {
            "id": f"dependency:{dependency}",
            "kind": "dependency",
            "label": dependency,
        }

    python_imports = python_module_index(paths)
    source_modules: dict[Path, str] = {}
    for path in paths:
        if path.suffix != ".rs":
            continue
        package = package_for_path(path, packages)
        if package is None:
            continue
        target_id, target_label = target_for_source(path, package)
        nodes.setdefault(
            target_id,
            {"id": target_id, "kind": "target", "label": target_label, "package": package["name"]},
        )
        add_edge(edges, f"package:{package['name']}", "defines", target_id)
        module_id = module_name_for_source(path, package, target_id)
        nodes[module_id] = {
            "id": module_id,
            "kind": "module",
            "label": module_id.removeprefix("module:"),
            "path": path.as_posix(),
            "target": target_id,
        }
        add_edge(edges, target_id, "contains", module_id)
        add_edge(edges, module_id, "implemented_by", path_id("file", path))
        absolute_source = (ROOT / path).resolve()
        source_modules[absolute_source] = module_id

    for path, content in contents.items():
        if content is None:
            continue
        source_file_id = path_id("file", path)
        if path.suffix == ".rs":
            source_module = source_modules.get((ROOT / path).resolve())
            if source_module is not None:
                configured_modules = PATH_MOD_RE.findall(content)
                configured_names = {name for _, name in configured_modules}
                for configured_path, name in configured_modules:
                    target = module_target(ROOT / path, name, configured_path)
                    if target is not None and target in source_modules:
                        target_module = source_modules[target]
                        source_target = nodes[source_module]["target"]
                        if nodes[target_module]["target"] != source_target:
                            target_suffix = source_target.removeprefix(
                                f"target:{package_for_path(path, packages)['name']}:"
                            )
                            alias = f"module:{package_for_path(path, packages)['name']}:{target_suffix}::{name}"
                            nodes[alias] = {
                                "id": alias,
                                "kind": "module",
                                "label": alias.removeprefix("module:"),
                                "path": relative(target).as_posix(),
                                "target": source_target,
                            }
                            add_edge(edges, source_target, "contains", alias)
                            add_edge(edges, alias, "implemented_by", path_id("file", relative(target)))
                            target_module = alias
                        add_edge(edges, source_module, "declares_module", target_module)
                for name in MOD_RE.findall(content):
                    if name in configured_names:
                        continue
                    target = module_target(ROOT / path, name)
                    if target is not None and target in source_modules:
                        add_edge(edges, source_module, "declares_module", source_modules[target])
                package = package_for_path(path, packages)
                if package is not None:
                    for imported in RUST_USE_RE.findall(content):
                        if imported in package_by_name and imported != package["name"]:
                            add_edge(edges, source_module, "imports", f"package:{imported}")
                        elif imported.replace("_", "-") in package_by_name:
                            add_edge(
                                edges,
                                source_module,
                                "imports",
                                f"package:{imported.replace('_', '-')}",
                            )
                for destination in INCLUDE_RE.findall(content):
                    reference_coverage["rust_compile_time_includes"]["detected"] += 1
                    target = local_link_target(ROOT / path, destination, files)
                    if target is None:
                        unresolved_static_references.append((path, "include", destination))
                        continue
                    reference_coverage["rust_compile_time_includes"]["resolved"] += 1
                    add_edge(edges, source_module, "includes", path_id("file", target))
        elif path.suffix in {".ts", ".tsx"}:
            for match in TS_IMPORT_RE.finditer(content):
                destination = match.group("path")
                reference_coverage["typescript_relative_imports"]["detected"] += 1
                target = typescript_import_target(ROOT / path, destination, files)
                if target is None:
                    unresolved_static_references.append((path, "typescript import", destination))
                    continue
                reference_coverage["typescript_relative_imports"]["resolved"] += 1
                add_edge(edges, source_file_id, "imports_local", path_id("file", target))
        elif path.suffix == ".py":
            for target in local_python_imports(path, content, python_imports):
                add_edge(edges, source_file_id, "imports_local", path_id("file", target))
        elif path.suffix == ".md":
            for destination in MARKDOWN_LINK_RE.findall(content):
                target = local_link_target(ROOT / path, destination, files)
                if target is not None:
                    add_edge(edges, source_file_id, "links_to", path_id("file", target))

    validate_component_files(files)
    for component in COMPONENTS:
        nodes[component["id"]] = {
            "id": component["id"],
            "kind": "component",
            "label": component["label"],
            "summary": component["summary"],
        }
        for filename in component["files"]:
            path = Path(filename)
            add_edge(edges, component["id"], "implemented_by", path_id("file", path))
    for source, target, kind in COMPONENT_EDGES:
        add_edge(edges, source, kind, target)
    for capability in capabilities:
        capability_id = f"capability:{capability['id']}"
        nodes[capability_id] = {
            "id": capability_id,
            "kind": "capability",
            "label": capability["title"],
            "summary": capability["summary"],
            "status": capability["status"],
            "runtime_modes": capability["runtime_modes"],
            "limitations": capability["limitations"],
            "references": {
                field: capability[field] for field in CAPABILITY_REFERENCE_EDGES
            },
            "execution_paths": capability.get("execution_paths", []),
        }
        add_edge(edges, capability_id, "owned_by", capability["owner"])
        for field, edge_kind in CAPABILITY_REFERENCE_EDGES.items():
            for reference in capability[field]:
                add_edge(
                    edges,
                    capability_id,
                    edge_kind,
                    path_id("file", Path(reference["path"])),
                )

    if unresolved_static_references:
        details = ", ".join(
            f"{path.as_posix()} ({kind}: {destination})"
            for path, kind, destination in unresolved_static_references
        )
        raise ValueError(f"unresolved static source references: {details}")

    ordered_nodes = [nodes[node_id] for node_id in sorted(nodes)]
    ordered_edges = [
        {"source": source, "kind": kind, "target": target}
        for source, kind, target in sorted(edges)
    ]
    graph = {
        "schema_version": 2,
        "description": "Deterministic navigation graph for the Orvek repository.",
        "generated_by": "scripts/generate-codebase-graph.py",
        "scope": {
            "included": "Version-controlled repository inputs plus the authored graph entry documents.",
            "excluded": [
                "Untracked files, including developer-local experiments.",
                "Git internals, build outputs and ignored dependency environments.",
                "Derived graph.json and architecture.dot outputs to avoid self-reference.",
            ],
            "limitations": [
                "Rust module links cover file-backed mod declarations, including conditional #[path] alternatives; inline modules and macro-generated code are not expanded.",
                "Rust import links cover direct first-party package imports; Cargo dependency links are the complete direct package-level view.",
                "Curated component edges describe architecture and do not replace source-level dependency analysis.",
                "Capability edges validate declared source anchors and executable ownership; they do not prove live runtime behavior.",
            ],
        },
        "node_kinds": NODE_KIND_DESCRIPTIONS,
        "relationship_kinds": EDGE_KIND_DESCRIPTIONS,
        "statistics": {
            "nodes": len(ordered_nodes),
            "edges": len(ordered_edges),
            "source_fingerprint": source_fingerprint(paths),
            "nodes_by_kind": dict(sorted(Counter(node["kind"] for node in ordered_nodes).items())),
            "capabilities_by_status": dict(
                sorted(Counter(capability["status"] for capability in capabilities).items())
            ),
            "capability_exempt_components": len(
                capability_ledger["capability_coverage"]["exempt_components"]
            ),
            "files_by_kind": dict(
                sorted(
                    Counter(
                        node["file_kind"]
                        for node in ordered_nodes
                        if node["kind"] == "file"
                    ).items()
                )
            ),
            "static_reference_coverage": reference_coverage,
        },
        "nodes": ordered_nodes,
        "edges": ordered_edges,
    }
    validate_graph(graph)
    json_output = json.dumps(graph, indent=2, sort_keys=True) + "\n"
    dot_output = render_dot()
    return json_output, dot_output


def validate_graph(graph: dict[str, Any]) -> None:
    fingerprint = graph["statistics"].get("source_fingerprint")
    if not isinstance(fingerprint, str) or not re.fullmatch(r"[0-9a-f]{64}", fingerprint):
        raise ValueError("graph source fingerprint is invalid")
    node_ids = [node["id"] for node in graph["nodes"]]
    if len(node_ids) != len(set(node_ids)):
        raise ValueError("graph contains duplicate node IDs")
    node_id_set = set(node_ids)
    for edge in graph["edges"]:
        if edge["source"] not in node_id_set or edge["target"] not in node_id_set:
            raise ValueError(f"graph edge has an unknown endpoint: {edge}")
        if edge["kind"] not in graph["relationship_kinds"]:
            raise ValueError(f"graph edge has an undocumented kind: {edge}")
    edge_keys = {(edge["source"], edge["kind"], edge["target"]) for edge in graph["edges"]}
    if len(edge_keys) != len(graph["edges"]):
        raise ValueError("graph contains duplicate edges")
    if graph["statistics"]["nodes"] != len(node_ids):
        raise ValueError("graph node count does not match its statistics")
    if graph["statistics"]["edges"] != len(edge_keys):
        raise ValueError("graph edge count does not match its statistics")
    for name, coverage in graph["statistics"]["static_reference_coverage"].items():
        if coverage["detected"] != coverage["resolved"]:
            raise ValueError(f"unresolved {name} references are not represented")


def render_dot() -> str:
    lines = [
        "digraph orvek_architecture {",
        '  graph [label="Orvek architectural responsibility map", labelloc="t", rankdir="LR"];',
        '  node [shape=box, style="rounded", fontname="Helvetica"];',
        '  edge [fontname="Helvetica"];',
    ]
    for component in COMPONENTS:
        dot_id = component["id"].replace(":", "_").replace("-", "_")
        label = component["label"].replace('"', r'\"')
        lines.append(f'  {dot_id} [label="{label}"];')
    for source, target, kind in COMPONENT_EDGES:
        source_id = source.replace(":", "_").replace("-", "_")
        target_id = target.replace(":", "_").replace("-", "_")
        lines.append(f'  {source_id} -> {target_id} [label="{kind}"];')
    lines.append("}")
    return "\n".join(lines) + "\n"


def write_or_check(path: Path, expected: str, check: bool) -> bool:
    absolute = ROOT / path
    actual = absolute.read_text(encoding="utf-8") if absolute.is_file() else None
    if actual == expected:
        return True
    if check:
        print(f"{path} is stale; run python3 scripts/generate-codebase-graph.py", file=sys.stderr)
        return False
    absolute.parent.mkdir(parents=True, exist_ok=True)
    absolute.write_text(expected, encoding="utf-8")
    print(f"wrote {path}")
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if generated files are stale")
    args = parser.parse_args()
    graph, dot = render_graph()
    graph_ok = write_or_check(GRAPH_PATH, graph, args.check)
    dot_ok = write_or_check(DOT_PATH, dot, args.check)
    return 0 if graph_ok and dot_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
