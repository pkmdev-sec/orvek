# Maintainability baseline

## Problem

Orvek has a deterministic source graph and a separate capability diagram, but neither proves that
a documented capability reaches an executable dispatcher. Local `.agent-map` overlays contain richer
claims, yet they are ignored developer state and cannot be the repository authority. The product also
retains configuration-only MCP, web-search, image-generation, and instruction surfaces, plus a
Nanocodex compatibility adapter whose vendor tree is not used by Orvek's host runtime.

This change establishes one checked-in capability contract, removes false product surfaces and unused
compatibility code, and projects the contract into the existing graph. It preserves the current
authority boundaries: clients assemble and present sessions; `orvek-harness` owns task truth;
`orvek-executor` owns isolation; `orvek-memory` owns memory storage and protocol behavior; evaluations
measure behavior without becoming runtime policy.

## Usage (caller's view)

Product authors update one entry in `assets/capabilities.json` when a capability changes. An
implemented or experimental entry names its owner, runtime modes, executable entry points,
dispatchers, persistence boundary, proof, and documentation using source-relative paths plus exact
anchors. Configuration-only entries are allowed only when they are explicitly labelled and do not
claim a dispatcher.

```json
{
  "id": "subagents",
  "status": "implemented",
  "owner": "component:subagents",
  "runtime_modes": ["sandbox"],
  "entrypoints": [{"path": "crates/harness/src/controller.rs", "anchor": "async fn dispatch("}],
  "dispatchers": [{"path": "crates/harness/src/controller/subagents.rs", "anchor": "pub async fn execute("}],
  "persistence": [{"path": "crates/harness/src/controller/subagents/lifecycle.rs", "anchor": "pub enum Event"}],
  "proof": [{"path": "crates/harness/tests/subagents.rs", "anchor": "child_reads_the_workspace_records_a_job_and_submits_a_valid_result"}],
  "documentation": [{"path": "docs/subagents.md", "anchor": "# Subagents"}],
  "limitations": ["Children are sandbox-only and read-only."],
  "execution_paths": [[
    {"path": "crates/harness/src/controller.rs", "anchor": "async fn dispatch(", "call": ".execute(name, arguments, &run, cancellation)"},
    {"path": "crates/harness/src/controller/subagents.rs", "anchor": "pub async fn execute("}
  ]]
}
```

The existing command remains the complete repository check:

```sh
python3 scripts/generate-codebase-graph.py --check
```

It rejects invalid ledger states, missing files or anchors, stale graph output, and a stale source
fingerprint. CI already runs this command. The README capability artwork continues to read the same
JSON, deriving its cards from the capability entries instead of maintaining a second list.

## Shape

`assets/capabilities.json` has schema version 1 and two top-level sections:

- `overview` contains presentation-only title, subtitle, and footnote fields.
- `capability_coverage` requires every architectural component to own a capability or carry an
  explicit non-capability exemption.
- `capabilities` contains the complete product contract. Thirteen operator-facing entries also carry
  diagram labels; load-bearing task, workspace, and delivery capabilities remain graph-only.

The graph generator owns these operations behind its existing command surface:

```python
def load_capability_ledger(
    files: set[Path], contents: dict[Path, str | None]
) -> tuple[dict[str, Any], list[dict[str, Any]]]: ...
def validate_capability_reference(
    capability_id: str, field: str, reference: Any,
    files: set[Path], contents: dict[Path, str | None]
) -> Path: ...
def validate_execution_paths(
    capability_id: str, execution_paths: Any,
    references: dict[str, list[Any]], files: set[Path],
    contents: dict[Path, str | None]
) -> None: ...
def source_fingerprint(paths: Iterable[Path]) -> str: ...
```

Validation parses the external JSON once. Implemented and experimental capabilities require at least
one entry point, dispatcher, proof, and documentation reference. Experimental entries also require a
non-empty limitations list. Configuration-only entries must have no dispatchers and must explain the
missing execution path in their limitations. Every referenced path must be a tracked graph input, and
every anchor must occur in that file.
Every claimed dispatcher must terminate an ordered execution path. Nonterminal steps retain the
inspected callsite inside their uniquely anchored function or method body, so leaving an unrelated
dead function—or moving the call into a sibling scope—cannot keep the claim green.

The generated graph adds first-class `capability:` nodes and directional `owned_by`,
`entered_through`, `dispatched_by`, `persisted_by`, `proved_by`, and `documented_by` edges. Its source
fingerprint hashes sorted repository-relative paths and bytes while excluding the graph outputs and
generated capability artwork. The fingerprint identifies the exact authored input snapshot; it does
not prove runtime behavior.

This is a deep interface: capability authors edit one bounded record, while the generator owns schema
rules, source validation, graph projection, statistics, and drift detection. Runtime code does not
depend on the ledger.

## Tradeoffs accepted

- We accept exact textual anchors instead of language-aware symbol resolution in exchange for a
  dependency-free, deterministic check that covers Rust, Python, TypeScript, Markdown, and manifests.
- We accept removing inert user-facing flags now in exchange for making future integrations return as
  complete vertical slices rather than compatibility promises.
- We accept deleting the unpublished Nanocodex compatibility API in exchange for removing the only
  first-party dependency on two large vendored packages.
- We accept leaving large integration modules intact when this change does not alter their authority
  boundary; future behavior changes split them only when a cohesive owner emerges.

## Alternatives considered

- A new `capabilities.toml` plus a separate checker lost because the capability artwork would still
  need translation or duplicate labels. It exposed synchronization to callers instead of hiding it.
- Tracking `.agent-map` lost because it mixes private task evidence with generated architecture and
  depends on an external local skill. A clean checkout and CI could not reproduce it from repository
  inputs alone.
- Importing MiniMax's runtime or MCP stack wholesale lost because it would add a second lifecycle,
  permission, persistence, and vendor model. A future MCP integration may reuse bounded protocol or
  registry ideas while remaining a host-owned Orvek vertical slice.

## Open questions and risks

- Do any external users depend on the unpublished `MemoryTool` Nanocodex adapter? Repository and Cargo
  callers are checked here; external use cannot be proven from this source tree.
- Which complete MCP contract should replace the removed configuration in a later change: a harness
  service interface implemented by the application, or a harness-owned protocol client?
- Does a future source release need a compatibility error for removed configuration keys, or is the
  existing unknown-field diagnostic sufficient while the product remains unpublished?

## Implementation phases

- [x] Ground the current graph, capability claims, false surfaces, and dependency paths.
- [x] Sketch the capability contract and graph projection.
- [x] Implement ledger validation, semantic nodes, and source fingerprinting.
- [x] Remove configuration-only product surfaces and update documentation.
- [x] Remove the Nanocodex compatibility dependency and vendored packages.
- [x] Regenerate derived artifacts and run focused checks.
- [x] Run repository integration checks and review the final diff.

## Verification

- `just test --locked`: 1,371 executed tests passed; 53 repository-defined tests skipped.
- `just clippy --locked`: all targets passed with warnings denied.
- `cargo check --locked --package orvek --all-features`: passed.
- `just check-features`: all 16 `orvek-memory` feature combinations passed.
- Harbor adapter: 28 contract tests passed.
- Executable documentation: all five live scenarios passed, including sandbox delivery, and their
  sanitized trace fixtures were regenerated and replayed.
- Capability graph: repository and global-skill regression suites passed; both reject a missing
  entrypoint callsite even when the dispatcher function remains present.
- Source hygiene, documentation, format, generated artwork, graph freshness, and diff checks passed.

## Next maintenance step

Use the capability ledger as the first change-impact route for the next runtime feature; update its
entrypoint, callsite, dispatcher, proof, and documentation in the same revision as the behavior.
