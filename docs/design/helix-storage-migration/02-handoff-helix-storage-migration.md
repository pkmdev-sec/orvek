---
task: replace local SQLite persistence with embedded HelixDB
artifact_type: handoff
status: planned-blocked
last_updated: 2026-09-22
authoritative_outline: 01-structure-outline-helix-storage-migration.md
repository: /Users/pumaurya/orvek
observed_branch: feat/autonomy-tracker
observed_head: 9ae083aef776d13de54cb4f1092ce3a53a03c83c
---

# Helix storage migration handoff

## Start here

Read these files before doing anything:

1. repository `AGENTS.md` instructions supplied for `/Users/pumaurya/orvek`;
2. [`CODEBASE.md`](../../../CODEBASE.md);
3. [`01-structure-outline-helix-storage-migration.md`](01-structure-outline-helix-storage-migration.md);
4. [`assets/capabilities.json`](../../../assets/capabilities.json);
5. [`docs/harness-host.md`](../../harness-host.md) and
   [`docs/harness-integration.md`](../../harness-integration.md).

The outline is normative. This file is the live checkpoint. Update it at each phase boundary and
before a context transition.

## Objective

Replace local SQLite persistence with embedded HelixDB without silently removing host, journal,
recovery, trace, memory, packaging, or product-capability behavior.

The user accepts losing current local data. Use fresh v2 state. Do not use that permission to weaken
behavioral contracts.

## Current position

- Planning and source investigation are complete.
- Implementation has not started.
- No Orvek source file was changed during the investigation.
- This planning turn added only the two files in `docs/design/helix-storage-migration/`. They are
  currently untracked and will not appear automatically in a worktree created from another commit.
- The repository was inspected at branch `feat/autonomy-tracker`, commit
  `9ae083aef776d13de54cb4f1092ce3a53a03c83c`, with substantial pre-existing user changes.
- The observed worktree had 46 modified files, 164 deleted files, and two untracked files.
- `python3 scripts/generate-codebase-graph.py --check` failed because
  `docs/codebase-graph/graph.json` was stale.
- These counts and states are time-specific. Refresh them before relying on them.

Do not implement in the observed dirty worktree. Resolve the current work, select one authoritative
revision, and create an isolated migration worktree first.

## Settled direction

- Use HelixDB for local host state and local memory.
- Use two logical Helix databases owned by one detached host process.
- Keep filesystem artifacts.
- Keep a config-global owner lock and exclude active legacy v1 hosts.
- Use fresh `host/v2` and `memory/v2` state. Leave v1 untouched.
- Remove legacy SQLite import.
- Preserve trace-bundle format and replay behavior.
- Preserve memory CAS, logical limits, deterministic BM25, scope, telemetry, probation, secret
  filtering, archive, and remote transfer behavior.
- Retire the 4 MiB SQLite physical-file cap explicitly.
- Defer vector and graph retrieval.
- Use SQLite only as a test reference during the migration.
- Do not ship runtime selection, dual writes, fallback, or a permanent generic database trait.
- Delete all production SQLite code and dependencies at cutover.

Any deviation needs evidence and explicit user approval. Record it in both artifacts before changing
code.

## Source findings that constrain the design

### Host state

`crates/harness/src/store.rs` and its submodules own:

- single-writer lock and database startup;
- task and session aggregates;
- global event sequence and aggregate revisions;
- exact event bytes and hash chains;
- cached state and checkpoints;
- leases, jobs, provider accounting, verification, completion, and delivery;
- submissions and queue state;
- event intake and monitor records.

Several operations update task events, session events, projections, checkpoints, leases, and queue
state in one transaction. Helix must preserve the complete transaction, not each write independently.

### Recovery

`crates/harness/src/controller.rs::Host::open_backend` recovers interrupted tasks, submissions, and
child work before serving. Unknown external effects remain unknown. Never retry them automatically.

Operation UUIDs protect lost acknowledgements. Reusing an operation ID with different input must
remain an error.

### Trace bundles

`crates/harness/src/trace.rs` currently opens a live SQLite WAL, pins a prefix, reads exact event
bytes, and compares replayed state with cached projections. Preserving the JSON envelope alone is not
enough. Helix needs an equivalent concurrent snapshot or a host-mediated export with the same
behavior.

### Memory

`crates/memory/src/store/local.rs` owns stable positive IDs, a non-reusing high-water allocator,
versioned CAS, sync, deterministic ordering, BM25 ranking, telemetry, probation pruning, scope,
secret filtering, and logical limits.

The TUI and CLI currently access local memory outside the detached host. Helix writer fencing makes
that unsafe. Move memory ownership behind a storage-only host path while SQLite is still the
reference.

Memory export and import need pagination and staged atomic upload. A maximum corpus can exceed one
8 MiB IPC frame because metadata is larger than the content-only limit.

### Process ownership

The current owner lock lives inside the versioned state root. A new `host/v2` lock alone would not
exclude an active `host/v1`. The migration must add a stable config-global lock and also exclude old
hosts during the compatibility window.

### Packaging

The repository intends to publish to crates.io. The embedded Helix source currently conflicts with
that contract.

## External Helix evidence

Evidence was collected against HelixDB commit
`81e1e741e8d3d61e6b09577e1d46137850fc9343` from 2026-09-21.

The following isolated proofs passed:

- a Git-pinned embedded dependency resolved and compiled;
- an embedded disk write survived clean close and reopen;
- an acknowledged write survived immediate process abort and reopened in a new process.

These proofs are narrow. They do not establish Orvek journal compatibility, multi-aggregate
atomicity, ordered cursor semantics, concurrent snapshots, supported-platform packaging, or full
crash safety.

The following packaging probe failed:

```text
cargo package
  failed to select a version for `helix-db`
  package depends on `helix-db` with feature `embedded`
  but published `helix-db 3.0.0` does not have that feature
```

The Git source exposes `embedded`; the published crate does not. Cargo rewrites the dependency for
packaging and fails. Do not call crates.io release viability complete until this changes.

Do not vendor HelixDB without explicit approval. Its size and maintenance cost conflict with the
user's goal of making Orvek easier to manage.

## Highest-risk silent regressions

Check these first during every relevant phase:

1. global journal cursor gaps, duplicates, or reordering;
2. non-contiguous per-aggregate revisions;
3. partial task/session/checkpoint/lease transactions;
4. changed event bytes that invalidate hash chains or trace replay;
5. lost-ack replay creating duplicate work;
6. restart retrying an unknown provider, shell, execution, or delivery effect;
7. v1 and v2 hosts serving simultaneously;
8. weakened state-root permissions, symlink checks, Unix socket ownership or mode, peer credentials,
   unexpected-socket handling, or no-follow diagnostics;
9. memory writer fencing caused by direct TUI or CLI access;
10. memory operations becoming dependent on provider authentication or Docker;
11. oversized memory IPC frames or partially applied imports;
12. trace export losing its exact concurrent prefix;
13. a deleted physical-SQL test with no behavior-equivalent replacement;
14. a missing index hidden by a full scan;
15. Cloudflare/WASM builds acquiring the embedded local database dependency;
16. packaging or source archives omitting Helix runtime or licenses.

## Required first action

Do not start with Cargo edits or Helix query code.

1. Refresh repository state and applicable instructions.
2. Choose how to preserve these two planning artifacts. Prefer a focused plan-only commit after
   authorization. If a commit is not authorized, keep the source copies and plan a checksum-verified
   copy after the new worktree exists.
3. Ask the user which clean revision is authoritative if it is not already explicit.
4. Resolve whether crates.io publication remains required. Recommend preserving it.
5. Create a clean isolated migration worktree.
6. If the planning artifacts were not committed into the selected base, copy both files into the new
   worktree. Compare source and destination with `shasum -a 256`, confirm the destination files with
   `git status --short`, and do not delete the source copies.
7. Execute Phase 0 from the outline and record results here.
8. Execute the standalone Helix proof phase. Stop if the public API cannot implement one complete
   atomic journal commit.

## Phase checkpoint

| Phase | State | Evidence | Next action |
| --- | --- | --- | --- |
| 0. Baseline | Blocked | Current tree is dirty, graph is stale, and planning artifacts are untracked | Make plan durable, then select clean revision and isolated worktree |
| 1. Helix proof | Partial | Compile, reopen, and one abort recovery passed | Prove complete journal transaction, snapshots, limits, targets, packaging |
| 2. Behavior freeze | Not started | Source and test inventory only | Build shared contract against SQLite |
| 3. Ownership and IPC | Not started | Design and hazards identified | Implement only after Phase 2 passes |
| 4. Async store | Not started | About 158 controller lock sites were observed | Convert while SQLite remains authoritative |
| 5. Helix candidate | Not started | Label and ownership design agreed | Port by subsystem after primitive proof |
| 6. Candidate qualification | Not started | Acceptance matrix defined | Run only after complete candidate exists |
| 7. Cutover and deletion | Not started | Fresh v2 and no-fallback decision agreed | Run only after Phase 6 passes |
| 8. Optimization | Not started | No Orvek Helix benchmarks exist | Measure after correctness parity |
| 9. Release | Blocked | `cargo package` probe failed | Resolve embedded crate/distribution decision |

## Checks to retain

Run the repository checks independently. Keep full logs outside tracked source and record their paths
and bounded results here.

```sh
cargo check --all-features
just check-fmt
just clippy
just test
just check-features
python3 scripts/generate-codebase-graph.py --check
python3 scripts/check-docs.py
python3 scripts/check-source-tree.py
```

Also retain focused subprocess tests for owner exclusion, crash cuts, restart, lost acknowledgement,
trace export, memory paging, degraded storage-only startup, state-root permissions, symlink refusal,
socket ownership and mode, unexpected-socket preservation, peer credentials, and no-follow
diagnostics.

## Proof limits at handoff

- No Orvek implementation exists.
- No current full-suite baseline was run on the dirty observed checkout.
- The current codebase graph was stale.
- No Helix multi-aggregate transaction equivalent was proven.
- No concurrent Helix trace snapshot was proven.
- No maximum event, projection, or memory payload was qualified.
- No macOS/Linux release matrix was run.
- No Orvek cold-start, RSS, latency, binary-size, or build-time comparison exists.
- Crates.io packaging is currently blocked.

Do not upgrade any of these statements without fresh evidence.

## Handoff update template

Replace this section at every phase boundary:

```text
Repository:
Worktree:
Branch:
Commit/tree:
Phase and status:
Objective for this phase:
Settled decisions reused:
Changed paths:
Checks run and exact results:
Retained log paths:
Unresolved proof:
User decisions needed:
Next safe action:
```
