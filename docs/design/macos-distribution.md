# macOS distribution boundary

## Decision

Orvek is distributed as a macOS application for Intel and Apple Silicon. Standalone Linux binaries,
Linux installers, Linux release archives, published container images, and WebAssembly/Cloudflare
targets are outside the product boundary.

This does not remove macOS capabilities that use Linux guests internally. Docker Desktop remains the
isolation backend for sandbox execution, read-only subagents, protected verification, certificates,
reproducible delivery, Harbor evaluation, and the sandbox documentation fixture. The static Linux
executor helper is an implementation detail copied into that Docker guest; it is not a supported
standalone Orvek distribution.

## Retained capabilities

- native macOS host execution;
- Docker Desktop sandbox execution and its executor helper;
- subagent lifecycle, messaging, concurrency controls, and TUI;
- verified contracts, protected checks, certificates, and patch delivery;
- local and generic remote memory;
- Harbor, SnapCompact, context-cost, transition, and incident-replay evaluation;
- browser review, traces, monitoring, hooks, event intake, and interpreter support.

## Removed distribution surfaces

- `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` binary archives;
- Linux branches in the public installer and self-updater;
- GHCR/container release publication;
- Linux product build/test matrices, except the Docker integration job that validates the macOS
  application's Linux guest boundary;
- Linux-only CodSpeed publication;
- the Cloudflare Worker/WebAssembly memory example and its CI/build dependencies.

## Release and CI contract

The release matrix contains exactly:

- `aarch64-apple-darwin`;
- `x86_64-apple-darwin`.

General product compilation, tests, lint, docs, source checks, review UI, feature combinations, and
cache warming run on macOS. The Ubuntu Docker-security job remains deliberately: hosted macOS runners
do not provide Docker Desktop, and the job validates the same Linux guest/helper used by the macOS
Docker backend. Its presence is not a Linux product-support claim.

## Rollback

Commit `081c1bb` is the cost-fixed capability-complete baseline. Local branch
`rollback/cost-fixed-081c1bb` points to it. If this distribution cleanup fails qualification, move
the working branch back to that exact branch; no cost/cache or maintainability work needs to be
reconstructed.

## Verification

- installer and updater accept only the two macOS targets;
- release/cache matrices contain only those targets;
- no container publishing or Cloudflare/WASM target remains;
- Docker sandbox, subagent, verification, delivery, Harbor, and five documentation scenarios remain;
- Cargo, graph, capability artwork, docs, release pipeline, and native/Docker tests pass.
