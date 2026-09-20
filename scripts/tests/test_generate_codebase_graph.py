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
