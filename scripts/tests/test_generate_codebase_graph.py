from __future__ import annotations

import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "generate_codebase_graph", ROOT / "scripts/generate-codebase-graph.py"
)
assert SPEC is not None and SPEC.loader is not None
GRAPH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GRAPH)


class GraphGeneratorTests(unittest.TestCase):
    def test_source_fingerprint_changes_with_authored_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            original_root = GRAPH.ROOT
            GRAPH.ROOT = Path(directory)
            try:
                path = Path("source.txt")
                (GRAPH.ROOT / path).write_text("first", encoding="utf-8")
                first = GRAPH.source_fingerprint([path])
                (GRAPH.ROOT / path).write_text("second", encoding="utf-8")
                second = GRAPH.source_fingerprint([path])
                self.assertNotEqual(first, second)
                self.assertRegex(first, r"^[0-9a-f]{64}$")
            finally:
                GRAPH.ROOT = original_root

    def test_capability_reference_requires_a_real_source_anchor(self) -> None:
        path = Path("src/feature.rs")
        with self.assertRaisesRegex(ValueError, "missing from src/feature.rs"):
            GRAPH.validate_capability_reference(
                "feature",
                "dispatchers",
                {"path": path.as_posix(), "anchor": "fn missing"},
                {path},
                {path: "fn present() {}"},
            )

    def test_proof_anchor_does_not_replace_ci_check_registration(self) -> None:
        path = Path("src/feature.rs")
        proof = {
            "path": path.as_posix(),
            "anchor": "fn present",
            "check_id": "missing-check",
            "ci_job": "tests",
            "platform": "ubuntu-22.04",
            "features": ["all-features"],
            "mode": "ci",
        }
        with self.assertRaisesRegex(ValueError, "absent from CI-owned commands"):
            GRAPH.validate_capability_proof(
                "feature", proof, {path}, {path: "fn present() {}"}, {"restored-check": "tests"}
            )

    def test_registered_proof_check_restores_validity(self) -> None:
        path = Path("src/feature.rs")
        proof = {
            "path": path.as_posix(),
            "anchor": "fn present",
            "check_id": "restored-check",
            "ci_job": "tests",
            "platform": "ubuntu-22.04",
            "features": ["all-features"],
            "mode": "ci",
        }
        GRAPH.validate_capability_proof(
            "feature", proof, {path}, {path: "fn present() {}"}, {"restored-check": "tests"}
        )
    def test_ci_check_marker_without_a_run_command_in_its_step_is_rejected(self) -> None:
        workflow = Path(".github/workflows/ci.yaml")
        contents = {
            workflow: (
                "jobs:\n  tests:\n    steps:\n"
                "      - name: Missing command\n"
                "        env:\n          ORVEK_CHECK_ID: removed-check\n"
                "      - name: Other command\n        run: cargo test --locked\n"
                "        env:\n          ORVEK_CHECK_ID: retained-check\n"
            )
        }
        with self.assertRaisesRegex(ValueError, "removed-check.*no run command in their step"):
            GRAPH.ci_owned_checks(contents)

    def test_ci_check_markers_with_run_commands_are_registered(self) -> None:
        workflow = Path(".github/workflows/ci.yaml")
        contents = {
            workflow: (
                "jobs:\n  tests:\n    steps:\n"
                "      - name: Primary command\n        run: cargo test --locked\n"
                "        env:\n          ORVEK_CHECK_ID: primary-check\n"
                "          ORVEK_ADDITIONAL_CHECK_IDS: secondary-check,third-check\n"
                "      - name: Independent command\n        run: cargo check --locked\n"
                "        env:\n          ORVEK_CHECK_ID: independent-check\n"
            )
        }
        self.assertEqual(
            GRAPH.ci_owned_checks(contents),
            {
                "primary-check": "tests",
                "secondary-check": "tests",
                "third-check": "tests",
                "independent-check": "tests",
            },
        )

    def test_capability_execution_path_requires_the_inspected_callsite(self) -> None:
        caller = Path("src/caller.rs")
        dispatcher = Path("src/dispatcher.rs")
        references = {
            "entrypoints": [{"path": caller.as_posix(), "anchor": "fn start"}],
            "dispatchers": [{"path": dispatcher.as_posix(), "anchor": "fn execute"}],
        }
        with self.assertRaisesRegex(ValueError, "execution call .* outside the declared scope"):
            GRAPH.validate_execution_paths(
                "feature",
                [
                    [
                        {"path": caller.as_posix(), "anchor": "fn start", "call": "execute()"},
                        {"path": dispatcher.as_posix(), "anchor": "fn execute"},
                    ]
                ],
                references,
                {caller, dispatcher},
                {
                    caller: "fn start() {\n    disconnected();\n}\n\nfn unrelated() {\n    execute();\n}\n",
                    dispatcher: "fn execute() {\n}\n",
                },
            )

    def test_python_multiline_entrypoint_scope_includes_body(self) -> None:
        source = Path("adapter.py")
        content = """class Adapter:
    async def run(
        self,
        value: str,
    ) -> None:
        await self.execute(value)

    async def execute(self, value: str) -> None:
        pass
"""
        references = {
            "entrypoints": [{"path": source.as_posix(), "anchor": "    async def run("}],
            "dispatchers": [
                {"path": source.as_posix(), "anchor": "    async def execute("}
            ],
        }
        GRAPH.validate_execution_paths(
            "feature",
            [
                [
                    {
                        "path": source.as_posix(),
                        "anchor": "    async def run(",
                        "call": "self.execute(value)",
                    },
                    {"path": source.as_posix(), "anchor": "    async def execute("},
                ]
            ],
            references,
            {source},
            {source: content},
        )

    def test_nested_integration_modules_use_the_declared_cargo_target(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            original_root = GRAPH.ROOT
            GRAPH.ROOT = Path(directory)
            try:
                source = Path("package/tests/it/lifecycle/builder.rs")
                absolute_source = GRAPH.ROOT / source
                absolute_source.parent.mkdir(parents=True)
                absolute_source.write_text("", encoding="utf-8")
                package = {
                    "name": "fixture",
                    "directory": Path("package"),
                    "manifest_data": {
                        "test": [
                            {"name": "it", "path": "tests/it/main.rs"},
                        ]
                    },
                }
                self.assertEqual(
                    GRAPH.target_for_source(source, package)[0],
                    "target:fixture:test:it",
                )
            finally:
                GRAPH.ROOT = original_root

    def test_typescript_side_effect_imports_are_detected(self) -> None:
        imports = [
            match.group("path")
            for match in GRAPH.TS_IMPORT_RE.finditer(
                'import "./styles.css";\nimport { render } from "./render.js";'
            )
        ]
        self.assertEqual(imports, ["./styles.css", "./render.js"])

    def test_missing_curated_component_file_is_rejected(self) -> None:
        components = (
            {
                "id": "component:fixture",
                "files": ("src/missing.rs",),
            },
        )
        with self.assertRaisesRegex(ValueError, "src/missing.rs"):
            GRAPH.validate_component_files(set(), components)

    def test_unknown_graph_endpoint_is_rejected(self) -> None:
        graph = {
            "nodes": [{"id": "known"}],
            "edges": [{"source": "known", "kind": "imports", "target": "missing"}],
            "statistics": {
                "nodes": 1,
                "edges": 1,
                "source_fingerprint": "0" * 64,
                "static_reference_coverage": {},
            },
        }
        with self.assertRaisesRegex(ValueError, "unknown endpoint"):
            GRAPH.validate_graph(graph)

    def test_write_or_check_detects_stale_output_without_mutating_it(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            original_root = GRAPH.ROOT
            GRAPH.ROOT = Path(directory)
            try:
                path = Path("graph.json")
                (GRAPH.ROOT / path).write_text("old", encoding="utf-8")
                self.assertFalse(GRAPH.write_or_check(path, "new", check=True))
                self.assertEqual((GRAPH.ROOT / path).read_text(encoding="utf-8"), "old")
                self.assertTrue(GRAPH.write_or_check(path, "new", check=False))
                self.assertTrue(GRAPH.write_or_check(path, "new", check=True))
            finally:
                GRAPH.ROOT = original_root

    def test_rendered_cargo_targets_match_cargo_metadata(self) -> None:
        metadata = json.loads(
            subprocess.run(
                ["cargo", "metadata", "--format-version", "1"],
                cwd=ROOT,
                check=True,
                capture_output=True,
                text=True,
            ).stdout
        )
        expected = set()
        for package in metadata["packages"]:
            manifest = Path(package["manifest_path"])
            if not manifest.is_relative_to(ROOT):
                continue
            for target in package["targets"]:
                kinds = set(target["kind"])
                if kinds & {"lib", "rlib", "cdylib", "proc-macro"}:
                    suffix = "lib"
                elif "custom-build" in kinds:
                    suffix = "build"
                else:
                    kind = next(
                        candidate
                        for candidate in ("bin", "test", "bench", "example")
                        if candidate in kinds
                    )
                    suffix = f"{kind}:{target['name']}"
                expected.add(f"target:{package['name']}:{suffix}")

        rendered, _ = GRAPH.render_graph()
        graph = json.loads(rendered)
        actual = {node["id"] for node in graph["nodes"] if node["kind"] == "target"}
        self.assertEqual(actual, expected)

        reused = [
            node
            for node in graph["nodes"]
            if node.get("path") == "bin/orvek/src/app/mod.rs"
            and node.get("target") == "target:orvek:bench:tui"
        ]
        self.assertEqual(len(reused), 1)

        self.assertIn(
            {
                "source": "file:web/review/app.ts",
                "kind": "imports_local",
                "target": "file:web/review/styles.css",
            },
            graph["edges"],
        )
        capabilities = [node for node in graph["nodes"] if node["kind"] == "capability"]
        subagents = next(
            node for node in capabilities if node["id"] == "capability:subagents"
        )
        self.assertEqual(
            subagents["references"]["dispatchers"][0]["anchor"],
            "pub async fn execute(",
        )
        self.assertEqual(
            subagents["execution_paths"][0][0]["call"],
            ".execute(name, arguments, &run, cancellation)",
        )

        ledger = json.loads((ROOT / "assets/capabilities.json").read_text())
        self.assertEqual(len(capabilities), len(ledger["capabilities"]))
        statuses: dict[str, int] = {}
        for capability in ledger["capabilities"]:
            status = capability["status"]
            statuses[status] = statuses.get(status, 0) + 1
        self.assertEqual(graph["statistics"]["capabilities_by_status"], statuses)
        self.assertEqual(graph["statistics"]["capability_exempt_components"], 6)
        self.assertTrue(
            any(
                edge["source"] == "capability:verification-delivery"
                and edge["kind"] == "owned_by"
                and edge["target"] == "component:delivery"
                for edge in graph["edges"]
            )
        )
        self.assertRegex(graph["statistics"]["source_fingerprint"], r"^[0-9a-f]{64}$")
        self.assertIn(
            {
                "source": "capability:subagents",
                "kind": "dispatched_by",
                "target": "file:crates/harness/src/controller/subagents.rs",
            },
            graph["edges"],
        )


    def test_curated_components_are_connected(self) -> None:
        connected = {
            endpoint
            for source, target, _ in GRAPH.COMPONENT_EDGES
            for endpoint in (source, target)
        }
        isolated = [
            component["id"]
            for component in GRAPH.COMPONENTS
            if component["id"] not in connected
        ]
        self.assertEqual(isolated, [])


if __name__ == "__main__":
    unittest.main()
