# Orvek codebase map

Start a fresh session with [the codebase graph](docs/codebase-graph/README.md). It provides:

- `docs/codebase-graph/graph.json`: deterministic, machine-readable inventory and edges.
- `docs/codebase-graph/overview.md`: architecture, authority boundaries, runtime flows, and navigation routes.
- `docs/codebase-graph/architecture.dot`: compact component diagram.

Refresh it after moving files, Cargo dependencies, or module declarations:

```sh
python3 scripts/generate-codebase-graph.py
python3 scripts/generate-codebase-graph.py --check
```

The graph intentionally excludes local untracked files and build outputs. It is an orientation and
navigation index; source and behavior checks remain authoritative.

`assets/capabilities.json` is the checked-in product-capability ledger. The graph validates each
implemented capability's owner, executable entry point, dispatcher, proof, and documentation, then
requires a source-bound callsite path from an entrypoint to every dispatcher and projects those
references as `capability:` nodes. Developer-local `.agent-map` overlays are private
derived evidence and are never repository authority.
