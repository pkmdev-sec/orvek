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
