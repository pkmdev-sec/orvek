# Source hygiene and transcript checks

Scope: local source-tree hygiene, transcript rendering, and provider HTTP deadlines.
This is not a security certification or a completed release rehearsal.

## Private graphs

No `.agent-map` files were present in the Git index or `HEAD` at inspection time.
The public navigation graph lives under `docs/codebase-graph/` and remains versioned.

- Git ignores `.agent-map` as a file or directory at any depth.
- Docker contexts exclude root and nested `.agent-map` paths.
- The existing CI source-hygiene check rejects even force-added private map paths.
- Regression tests exercise real temporary Git indexes. Public graph and marketing
  files remain allowed.

## Temporary screenshot path and timeout

The supplied file is a PNG in macOS's `otty-paste` temporary directory. The terminal
identifies itself as Otty. The installed terminal includes a pasted-image attachment
marker. The exact supplied path is absent from the source and application journal.
Orvek's native image clipboard handler encodes PNG data in memory, not that directory.
There is no demonstrated application log leak to suppress or cached image to delete.
Exact local evidence is retained only under the ignored `.agent-map` directory.

The screenshot instead shows a failed provider request. Its durable receipt records
one dispatched attempt lasting 20,001 ms, no HTTP status, no response, and no partial
items. Billing remains uncertain, so the request is not silently replayed.

The HTTP adapter incorrectly wrapped the entire POST/header wait in the connection
timeout. The socket connection retains its 20-second deadline. Waiting for headers
uses the existing 90-second idle deadline, within the unchanged 300-second total
budget. Delayed-header tests cover success, idle expiry, and the total deadline.
This corrects deadline ownership; it does not establish why the remote endpoint
stalled or guarantee that a future network request will succeed.

## Transcript presentation

Reasoning uses a slim left rail. The next assistant heading has an arrow connector.
The rail continues through the existing spacer row. Confirmed final answers keep
their existing rounded boxes. Narrow terminals retain the plain-heading fallback.

Connector columns are excluded from copied source text. Link hit regions and image
positions shift with the gutter. Layout caching still uses entry revision and width;
no extra animation timer, journal mutation, or inference call was added.

Delayed preview chunks cannot append to or recreate already-confirmed provider
items. Auxiliary publication closes its preview stream. New items and requests
remain eligible for previews. A real projection-to-render regression covers the
previous suffix-only assistant row and full commentary before tool output.

## Release safeguards and remaining prerequisites

A tag must match the crate version, belong to main, and have a successful main-push
CI run for its exact commit before release builds start. A missing, pending, or failed
CI result blocks the release. Existing package verification remains required.

Public release readiness still requires registry ownership, signing credentials,
protected branch/tag settings, and the prerequisite dependencies listed in
[RELEASES.md](../../RELEASES.md). In particular, `orvek-harness` depends on the
non-publishable, path-only `orvek-executor` crate. Resolve that distribution decision
and publishable dependency order before enabling crates.io publication. Local tests
cannot verify registry permissions or replace a multi-platform release rehearsal.

## Verification

Run from the repository root:

```sh
python3 -B -m unittest discover -s scripts/tests -p 'test_*.py'
python3 scripts/check-source-tree.py
python3 scripts/check-docs.py
python3 scripts/generate-codebase-graph.py --check
cargo test --locked -p orvek-harness --test provider_protocol
cargo test --locked -p orvek tui::components::transcript::tests
cargo test --locked -p orvek tui::host_projection::tests
cargo test --locked -p orvek --test release_pipeline
cargo check --locked --all-features
just check-fmt
just clippy --locked
just test --locked
```

No build installation, service restart, release publication, or credential change is
part of this change.

### Recorded result

Full workspace verification passed: cargo check --locked --all-features, strict all-target Clippy, 1,187 nextest tests passed with 40 skipped, and nightly formatting. The skips include environment-dependent/ignored tests; no full release rehearsal or Docker-service startup was performed.

An additional cached-layout regression passed after the full run. It checks 32 repeated
frames for layout reuse and confirms that connectors add no animation deadline. Final
all-target application Clippy also passed. All new targeted regressions passed.
