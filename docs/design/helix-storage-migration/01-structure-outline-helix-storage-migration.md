---
task: replace local SQLite persistence with embedded HelixDB
artifact_type: structure-outline
status: planned-blocked
last_updated: 2026-09-22
source_paths:
  - CODEBASE.md
  - assets/capabilities.json
  - docs/harness-host.md
  - docs/harness-integration.md
  - docs/memory.md
  - docs/trace-bundles.md
  - crates/harness/src/controller.rs
  - crates/harness/src/store.rs
  - crates/harness/src/store/
  - crates/harness/src/import.rs
  - crates/harness/src/trace.rs
  - crates/memory/src/store/
  - bin/orvek/src/app/host.rs
  - bin/orvek/src/app/cli.rs
  - bin/orvek/src/core/context.rs
  - bin/orvek/src/tui/client.rs
---

# Helix storage migration implementation outline

## Authority and change control

This document is the authoritative implementation outline for replacing Orvek's local SQLite
persistence with embedded HelixDB. Read the adjacent
[`02-handoff-helix-storage-migration.md`](02-handoff-helix-storage-migration.md) before starting or
resuming work.

The decisions in [Settled decisions](#settled-decisions) are approved. Do not change them silently.
If source evidence makes a decision unsafe or impossible, stop the affected phase and record:

1. the contradicted decision;
2. exact source or runtime evidence;
3. the smallest viable replacement;
4. affected phases and acceptance checks;
5. the user's explicit resolution.

Update this outline and the handoff before implementing the replacement decision. Do not treat an
agent summary, passing compilation, or a storage-unit test as authority to weaken a product
contract.

## Goal

Replace every local SQLite persistence owner with embedded HelixDB while preserving Orvek's
host-authority, journal, recovery, trace, memory, packaging, and capability contracts. Finish with no
runtime SQLite fallback, no dual writes, and no production `rusqlite` or `libsqlite3-sys` dependency.

Current local data is disposable. The migration does not need to import it. Existing v1 files must
remain untouched so code rollback stays possible.

## Why this migration is difficult

SQLite is an implementation detail in some paths and part of the behavior in others. The host store
uses it to combine these properties:

- one globally increasing reconnect cursor;
- contiguous per-session and per-task revisions;
- event hash chains over exact stored bytes;
- cached projections checked against replayed state;
- checkpoints at revision 1 and each 128th revision;
- transactions that update task and session journals, projections, leases, and queue state together;
- durable operation IDs for lost-acknowledgement recovery;
- crash recovery that does not retry unknown external effects;
- point-in-time trace export from an active host;
- memory compare-and-swap, stable IDs, deterministic ranking, telemetry, pruning, and bounded sync.

Disposable data removes record conversion. It does not remove these contracts.

## Settled decisions

### Ownership

- The detached host remains the only local mutation authority.
- Use two logical embedded Helix databases: one for host state and one for local memory.
- The detached host owns both writer handles. Do not let TUI, CLI, review, or headless clients open a
  writer independently.
- Keep the host and memory databases separate. They have different retention, export, recovery, and
  optional-remote behavior.
- Keep content-addressed artifacts on the filesystem. HelixDB does not replace `ArtifactStore`.
- Keep a filesystem owner lock. Add a config-global lock and hold the legacy v1 lock during the
  compatibility window so a v2 host cannot run beside a v1 host.

### Data model

- Model host records as Helix nodes with top-level indexed identity and ordering fields.
- Store canonical state and event payloads as opaque exact bytes encoded without parse-and-reserialize
  drift. Prove the chosen encoding against maximum payloads before integration.
- Do not add graph edges to the host journal merely because HelixDB supports graphs.
- Use unique composite keys for identities that SQLite currently enforces with primary or unique
  constraints.
- Use explicit range indexes for ordered journal and paging queries.
- Keep a durable application-owned journal counter. Do not substitute Helix node IDs, wall-clock
  timestamps, or storage engine sequence numbers for Orvek's public journal cursor.
- Keep the local memory high-water allocator. Do not substitute Helix node IDs for public memory IDs.

The initial label mapping is:

| Current table | Helix label |
| --- | --- |
| `tasks` | `Task` |
| `sessions` | `Session` |
| `events` | `JournalEvent` |
| `checkpoints` | `Checkpoint` |
| `leases` | `Lease` |
| `event_sources` | `EventSource` |
| `event_intake` | `EventIntake` |
| `monitor_state` | `MonitorState` |
| `monitor_releases` | `MonitorRelease` |
| `monitor_activations` | `MonitorActivation` |
| `monitor_facts` | `MonitorFact` |
| `monitor_measures` | `MonitorMeasure` |
| `monitor_episodes` | `MonitorEpisode` |
| `monitor_origins` | `MonitorOrigin` |
| `memories` | `Memory` |
| `memory_metadata` | `MemoryMeta` |

Add indexed derived fields for current queries. These include recent-input type and workspace,
session and task insertion order, pending event order, settled state, task-to-session links, monitor
episode status, monitor cohort identity, and checkpoint revision. Do not replace indexed queries with
unbounded scans.

### Runtime shape

The target ownership path is:

```text
TUI / CLI / headless / review
              |
      lightweight host IPC
              |
     +--------+---------+
     |                  |
HostStore            MemoryService
Helix host-v2        Helix memory-v2 or remote HTTP
     |
filesystem ArtifactStore

Task provider and Docker runtime initialize only when task execution needs them.
```

- Memory administration must remain usable when Docker is stopped and provider credentials are
  unavailable. Build a storage-only host path or lazy task-runtime initialization before moving local
  memory operations behind IPC.
- Keep remote memory's wire and storage protocol unchanged.
- Route local and remote memory operations through one ownership policy. Do not create different TUI
  or CLI behavior for each backend.
- Keep the current outer Tokio mutex during initial parity. Optimize concurrency only after behavior
  and performance measurements pass.
- Make store operations asynchronous while SQLite remains the reference implementation. Do not mix
  the caller conversion with the Helix behavior change.

### Compatibility and intentional removals

- Use fresh `host/v2` and `memory/v2` state. Never modify or delete v1 state automatically.
- Remove legacy SQLite session import. Remove its IPC commands, TUI discovery, documentation, tests,
  SQLite backup features, and supporting scripts in the final deletion phase.
- Preserve the public trace-bundle format and replay behavior.
- Preserve memory's deterministic BM25 ranking, compare-and-swap behavior, logical limits, scope,
  telemetry, probation, secret filtering, archive format, and remote transfer semantics.
- Retire the 4 MiB SQLite physical-file limit explicitly. Replace it with measured Helix storage and
  cache budgets while keeping the current logical record and content limits.
- Defer vector embeddings, graph-based retrieval, and Helix-native ranking. They require a separate
  product decision and evaluation.

### Migration mechanics

- Use the SQLite implementation only as a test reference during migration.
- Do not ship a runtime database selector.
- Do not dual-write.
- Do not retain a lowest-common-denominator production database trait.
- A test-only adapter may run the same behavioral contract against SQLite and Helix.
- Delete the SQLite reference, feature, and dependency in the final cutover wave.

## Proposed module shape

Keep callers behind the existing `Store` concept while moving storage knowledge into cohesive
modules:

```text
crates/harness/src/store/
├── mod.rs
├── schema.rs
├── journal.rs
├── submissions.rs
├── jobs.rs
├── event_intake.rs
├── monitor.rs
├── trace.rs
└── test_contract.rs       # test-only behavioral adapter

crates/memory/src/store/
├── mod.rs
├── helix.rs
├── remote/
└── test_contract.rs       # test-only behavioral adapter
```

`Store` remains a deep module. Controllers issue domain operations. They do not build Helix queries,
manage indexes, allocate journal sequences, or interpret transaction result counts.

The journal module should accept one typed commit request that contains all required guards and
writes for a state transition. The exact type names remain an implementation detail, but the shape
must express:

```rust,ignore
struct JournalCommit {
    operation: Uuid,
    guards: Vec<AggregateGuard>,
    aggregates: Vec<AggregateWrite>,
    events: Vec<EventAppend>,
    checkpoints: Vec<CheckpointWrite>,
    lease_changes: Vec<LeaseChange>,
}

struct CommitReceipt {
    first_sequence: u64,
    last_sequence: u64,
    aggregate_revisions: Vec<AggregateRevision>,
}
```

One Helix transaction must validate every guard, allocate all required journal sequences, write every
event and projection, update checkpoints, and apply lease changes. A missing guard must produce no
partial mutation. The implementation must verify the returned mutation count before it acknowledges
success.

## Phase overview

- [ ] Phase 0: select and qualify the baseline.
- [ ] Phase 1: prove required Helix behavior outside Orvek.
- [ ] Phase 2: freeze storage-independent behavior.
- [ ] Phase 3: establish cross-version ownership and storage-only IPC.
- [ ] Phase 4: make the store boundary asynchronous while SQLite remains authoritative.
- [ ] Phase 5: implement the Helix candidate by cohesive subsystem.
- [ ] Phase 6: run full candidate parity and failure qualification.
- [ ] Phase 7: cut over to v2 and delete SQLite.
- [ ] Phase 8: optimize from measurements.
- [ ] Phase 9: qualify packaging, installation, and release behavior.

## Phase 0: select and qualify the baseline

### Scope

Choose one clean authoritative revision after the current unrelated work is resolved. Create an
isolated migration worktree. Do not implement in a dirty or stale-graph checkout.

Record the following in the handoff:

- repository path, branch, commit, tree, and worktree path;
- `git status --short --branch`;
- applicable instructions;
- Helix source revision;
- complete baseline check results and log paths;
- baseline cold-start, RSS, journal, resume, trace, and memory measurements.

### Checks

Run each check independently and retain bounded logs:

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

### Success criteria

- The worktree is clean.
- The codebase graph matches the chosen source revision.
- All required checks pass, or the handoff records a pre-existing failure with an explicit decision.
- Performance budgets derive from recorded baseline measurements. Do not invent thresholds before
  measuring the baseline.

## Phase 1: prove required Helix behavior outside Orvek

### Scope

Use a disposable standalone probe pinned to one immutable Helix revision. Do not modify Orvek runtime
code in this phase.

Prove these operations with the public embedded API that Orvek will ship:

1. open, clean close, reopen, and read;
2. acknowledged write followed by immediate process abort and reopen;
3. forced kill at each step of a multi-record transaction;
4. one atomic transaction that allocates multiple journal sequences, checks multiple expected
   revisions, updates task and session projections, writes exact event bytes and checkpoints, and
   deletes a lease;
5. no partial write when any guard fails;
6. immediate read-after-commit visibility;
7. unique composite identity enforcement;
8. ascending and descending ordered paging with stable continuation;
9. asynchronous index creation, activation, restart, and failure handling;
10. exact round-trip of maximum event and cached-projection payloads;
11. a pinned concurrent read snapshot while the writer continues;
12. writer fencing, unknown commit outcome handling, and bounded non-automatic reconciliation;
13. clean shutdown that joins Helix background work before releasing the owner lock;
14. macOS and Linux build viability for supported targets.

### Packaging gate

As of 2026-09-22, `helix-db` commit
`81e1e741e8d3d61e6b09577e1d46137850fc9343` compiles in embedded mode through a Git dependency.
The published `helix-db 3.0.0` crate does not expose the `embedded` feature. `cargo package` therefore
fails when it rewrites the Git dependency to the published crate.

Resolve one of these choices before production cutover:

- preferred: use an official published Helix embedded crate;
- alternative requiring explicit user approval: stop publishing the affected Orvek crates to
  crates.io and distribute from Git and signed binaries.

Do not vendor or fork HelixDB without a new explicit decision. That would add a large maintenance
burden to the repository.

### Success criteria

- Every listed proof is an executable test or script with retained output.
- The public API can express Orvek's complete atomic journal commit.
- Any unsafe or unknowable commit outcome fails closed and does not trigger an automatic retry.
- The packaging decision is resolved.
- A failed gate stops the migration before Orvek integration.

## Phase 2: freeze storage-independent behavior

### Scope

Build a backend-neutral behavioral contract suite while SQLite remains the only production backend.
Test externally meaningful behavior rather than table layouts.

The suite must cover:

- global journal cursor monotonicity and paging;
- per-aggregate revision continuity and hash chains;
- checkpoint placement and tail replay;
- multi-aggregate atomic transitions;
- request idempotency and conflicting reuse;
- queue ordering, edits, movement, cancellation, and recovery;
- leases, jobs, accounting, unknown effects, verification, completion, and delivery;
- event intake deduplication, capacity, schedule cursor, cancellation, and recovery;
- monitor status, pages, release activation, episodes, facts, measures, and origins;
- trace prefix export, exact bytes, cached-state comparison, replay, and read-only behavior;
- memory IDs, versions, CAS, sync, export, import, scope, BM25 ordering, telemetry, probation,
  logical limits, and secret filtering;
- owner-only state directories, symlink-root refusal, Unix socket ownership and mode, unexpected
  socket refusal without deletion, peer-credential checks, and owner-only no-follow diagnostics;
- feature combinations, Cloudflare/WASM exclusion, packaging, and scripts.

### Rules

- Do not delete a direct-SQL test until a behavior-equivalent replacement passes.
- Keep physical corruption tests where Helix exposes an equivalent supported seam. Mark a claim
  `INCOMPLETE` when no supported seam exists. Do not silently weaken it.
- Bind every implemented or experimental capability in `assets/capabilities.json` to its preserved
  entrypoint, dispatcher, persistence owner, proof, and documentation.

### Success criteria

- The contract suite passes against SQLite.
- Every known SQLite-coupled production path and test has a recorded Helix disposition.
- Intentional removals and changed limits are visible in user documentation.

## Phase 3: establish ownership and storage-only IPC

### Scope

Make the process boundary safe before changing databases.

- Add a config-global host lock.
- During the compatibility window, a v2 host must also acquire or check the legacy v1 lock and
  socket. A v1 and v2 host must never serve simultaneously.
- Preserve the local security boundary: state roots remain owner-only and reject symlinks; Unix
  sockets remain owned by the effective user with no group or other permissions; peer credentials
  remain mandatory; unexpected sockets are refused without deletion; diagnostics remain owner-only
  and no-follow.
- Split storage startup from task-runtime startup. Memory administration and trace maintenance must
  not require provider authentication or Docker.
- Route TUI and CLI memory operations through bounded IPC.
- Add stable pagination and staged atomic upload for memory export, import, pull, and sync.
- Use immutable operation IDs and reconciliation for lost replies.
- Keep remote and local memory behavior behind the same client contract.

### Success criteria

- Memory list, delete, export, import, push, and pull work with Docker stopped and no provider
  credentials.
- A second old or new host cannot acquire mutation authority.
- State-root, socket, peer-credential, unexpected-socket, and diagnostic-file security tests pass
  against v2.
- IPC frames remain within the 8 MiB bound at maximum supported memory metadata and record counts.
- Interrupted uploads leave no partial authoritative memory snapshot.
- All behavior still uses SQLite in this phase.

## Phase 4: make the store boundary asynchronous

### Scope

Convert store calls and their callers to async while retaining SQLite behavior. Keep the existing
outer Tokio mutex during this phase.

- Preserve method meaning and error classification.
- Do not call controller code from a store future.
- Do not reacquire the same store lock from inside a store operation.
- Keep host and memory locks separate.
- Add watchdog tests around long and nested paths to detect lock cycles.

### Success criteria

- The complete SQLite contract and integration suites still pass.
- No store future can call back into a path that needs its held mutex.
- The compiler exposes and the phase resolves every caller conversion.
- No Helix runtime behavior is present yet.

## Phase 5: implement the Helix candidate

### Scope

Implement a non-default, test-only Helix candidate. Do not add runtime backend selection or dual
writes. Port cohesive subsystems in this order:

1. open, close, schema version, index lifecycle, and owner lock; preserve and wire the filesystem
   `ArtifactStore` without moving artifact bytes into Helix;
2. journal counter, task/session aggregates, exact events, hash chains, and checkpoints;
3. submissions, leases, jobs, accounting, verification, completion, and delivery;
4. event sources and event intake;
5. monitor records and activation transactions;
6. local memory, high-water IDs, sync, and transfer;
7. trace snapshot export and replay comparison.

Run focused contract cases after each subsystem. Use one Conventional Commit revision for each
independently verified increment. Keep new feature tests in the same revision. For a regression,
follow the repository rule: add the failing test, implement the fix in a child revision, verify, and
squash the fix into the test revision.

### Success criteria

- Each subsystem passes the same behavioral cases as SQLite.
- Startup serves nothing until required indexes are active.
- `ArtifactStore` remains filesystem-backed with its existing digest, budget, and closure checks.
- Exact event bytes survive storage and trace export.
- Indexed queries replace every current bounded SQL lookup. No unbounded scan hides a missing index.
- Store errors preserve actionable distinctions for conflict, missing data, terminal state, capacity,
  fencing, unknown outcome, and storage failure.

## Phase 6: qualify the complete candidate

### Scope

Run the complete suite against the Helix candidate before making it the default.

Required proof includes:

- fresh-process crash cuts around every multi-record transaction;
- cross-version and same-version owner exclusion;
- journal order with no gaps or duplicates;
- immediate watch visibility and bounded pages;
- restart, reconnect, slow watcher, lost acknowledgement, and interrupted work;
- native and sandbox task completion;
- event intake and monitor restart behavior;
- concurrent trace export with an unchanged public bundle format;
- maximum-size memory paging and atomic upload;
- memory maintenance with unavailable provider and Docker;
- all capability-ledger proofs;
- baseline-relative cold-start, latency, RSS, storage, and build measurements.

### Success criteria

- No required behavior is skipped.
- Every unexecuted environmental check is marked `INCOMPLETE`, not passed.
- The handoff contains exact commands, results, bounds, and retained log paths.
- The candidate meets the performance budgets established in Phase 0 or the user explicitly accepts
  a measured tradeoff.

## Phase 7: cut over and delete SQLite

### Scope

Make Helix the only production local database in one controlled wave.

- Switch to fresh `host/v2` and `memory/v2` roots.
- Leave all v1 files byte-for-byte and timestamp unchanged.
- Remove SQLite schema code, migrations, direct queries, legacy import, compatibility tests, and
  SQLite-specific scripts.
- Remove `rusqlite`, `libsqlite3-sys`, backup/limits/hooks features, and obsolete Cargo feature
  wiring.
- Remove the temporary SQLite reference and test selector.
- Update documentation, capability persistence references, the codebase graph, packaging metadata,
  NOTICE, and source archives.
- Do not regenerate capability fingerprints until source edits and behavior checks are final.

### Success criteria

- `rg` finds no unintended production SQLite code or physical database assumptions.
- `cargo tree` and `Cargo.lock` contain no `rusqlite` or `libsqlite3-sys`.
- A v2 startup never creates or writes SQLite.
- A failed Helix startup serves nothing.
- Old v1 state is unchanged.
- There is no runtime fallback or dual-write code.
- All repository checks pass after deletion.

## Phase 8: optimize from measurements

### Scope

Change performance behavior only after correctness parity.

- Tune Helix memory, cache, flush, and compaction settings for a local coding agent.
- Benchmark empty, 10k-event, and 100k-event stores.
- Measure host cold start, append, journal page, session resume, trace export, memory scan, RSS,
  on-disk size, binary size, and build time.
- Remove or narrow the outer mutex only if measurements show it is the bottleneck. Treat that as a
  separate concurrency change with its own failure tests.
- Evaluate Helix text or vector memory retrieval only in a separate product change with matched
  relevance tests and an explicit embedding lifecycle.

### Success criteria

- Each optimization has before-and-after measurements.
- Correctness and crash tests remain green.
- Documentation states measured results and their bounds. It does not claim general speedups from
  configuration alone.

## Phase 9: qualify packaging, installation, and release behavior

### Scope

Verify the artifact users receive, not only the workspace build.

- Run source archive and package-content checks.
- Run `cargo package` or the approved replacement distribution check.
- Build supported macOS and Linux targets.
- Install a fresh binary through the supported installer.
- Exercise interactive, headless, review, memory, trace, native, and sandbox flows with fresh v2
  state.
- Verify clean shutdown and restart from the installed binary.
- Verify downgrade behavior is explicit. An older binary may use untouched v1 state; it must not
  interpret or modify v2 state.

### Success criteria

- The installed artifact contains the required Helix runtime and licenses.
- Fresh installation and upgrade paths pass.
- Release documentation names the intentional data reset and removed SQLite import.
- The capability ledger and codebase graph match the shipped revision.

## Verification matrix

| Claim | Required evidence | Bound |
| --- | --- | --- |
| One mutation authority | Two-process v1/v2 and v2/v2 lock tests, then installed-host reproduction | Supported local platforms |
| Local host isolation | Root mode and symlink tests; socket owner, mode, unexpected-socket and peer-credential tests; no-follow diagnostic tests | Supported Unix platforms |
| Atomic journal commit | Helix primitive test plus store contract with injected cuts | Every multi-record transition shape |
| Stable journal order | Property tests, 10k/100k paging, reconnect reproduction | `i64::MAX` application cursor bound |
| Exact event history | Byte-for-byte round trip, hash replay, unchanged trace export | Maximum accepted event/projection size |
| No blind retry | Fencing and unknown-outcome subprocess tests | Every external-effect boundary |
| Memory compatibility | Shared contract suite and maximum IPC corpus | Current logical limits; physical 4 MiB cap retired |
| Degraded maintenance | Memory and trace commands with provider and Docker unavailable | Local maintenance operations |
| Capability preservation | Capability ledger validation plus each focused proof | Every implemented or experimental entry |
| Release viability | Package, source archive, install, and target builds | Approved distribution channels and targets |
| Performance | Baseline-relative benchmark report | Recorded hardware, data sizes, and build mode |

## Merge rules

- Start no phase until its dependencies and blocking decisions are complete.
- End every phase in a runnable, independently verified state.
- Use Conventional Commits for every revision.
- Preserve unrelated user work and dirty worktrees.
- Use one writer per implementation worktree.
- Do not delete or weaken a test to make Helix appear compatible.
- Do not mark missing runtime, provider, Docker, platform, or packaging proof as passed.
- Review the complete diff and capability graph before each merge.
- Update the handoff at every phase boundary and before any context transition.

## Blocking decisions

The next agent must resolve these before implementation:

1. Which clean Orvek revision is the authoritative migration base?
2. Must Orvek preserve crates.io publication? The recommended answer is yes.
3. If the published Helix crate still lacks embedded support, should work stop at the isolated
   candidate, or has the user explicitly accepted Git and signed-binary-only distribution?

Do not infer answers from this outline.
