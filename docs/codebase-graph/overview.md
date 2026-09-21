# Orvek architecture and navigation guide

## System model

Orvek is a Rust terminal coding agent whose terminal and automation clients deliberately do **not**
own task authority. `bin/orvek/src/main.rs` parses the CLI and dispatches into application code. A
configuration-aware `HostClient` connects to (or starts) a same-user detached host. The host owns
the `orvek-harness` controller, durable state, model calls, execution, evidence, and task outcome.
The TUI, browser review flow, and `orvek run` JSONL flow are projections and clients of that host.

This is the primary mental model to preserve when making a change: presentation may be disposable;
host records, contracts, checks, and completion decisions are authoritative. The boundary is
specified most directly in [`../harness-host.md`](../harness-host.md) and enforced by the source
entry points below.

## Package topology

```text
orvek (terminal binary)
├── orvek-harness (authoritative task runtime)
│   └── orvek-executor (sandbox transport / Linux helper protocol)
└── orvek-memory (local and optional remote memory)

orvek-memory-cloudflare (example Worker) ──> orvek-memory
```

The arrows above are direct first-party Cargo dependency edges. Child-agent behavior has one authoritative implementation in
`crates/harness/src/controller/subagents.rs`. It owns sandboxed admission, session-scoped authority,
messaging, lifecycle retention, and recovery.

## Responsibility map

| Concern | Owner and useful starting files | Key boundary |
| --- | --- | --- |
| CLI, config, authentication, updates | `bin/orvek/src/app/cli.rs`, `config.rs`, `auth.rs`, `update.rs` | Converts operator input and configuration into application actions. |
| Session assembly and extensions | `bin/orvek/src/core/mod.rs`, `core/extensions/skills.rs` | Resolves workspace and skills, and fail-fast validates configured memory before host admission. |
| Detached host client | `bin/orvek/src/app/host.rs`, `submission.rs` | Connects to the compatible local host, sends IPC requests/watches, and protects uncertain submission acknowledgement. |
| Detached host server | `bin/orvek/src/app/host.rs`, `shutdown.rs` | Starts the detached process, constructs provider/executor dependencies, and serves the harness IPC protocol. |
| Interactive terminal | `bin/orvek/src/tui/client.rs`, `host_projection.rs`, `components/root.rs` | Maintains local UI state; does not become an execution authority. |
| Headless automation | `bin/orvek/src/app/headless.rs` | Emits versioned JSONL from the durable host projection. |
| Browser review | `bin/orvek/src/review/mod.rs`, `server.rs`, `diff.rs` | Reads review snapshots and submits feedback through host commands. |
| Host protocol and durable model | `crates/harness/src/ipc.rs`, `session.rs`, `state.rs`, `store.rs` | Defines commands, journals, session/task types, and SQLite/artifact persistence. |
| Admission and task contract | `crates/harness/src/admission.rs`, `contract.rs`, `admission_profile.rs` | Compiles protected profiles and user limits into a contract the model cannot rewrite. |
| Orchestration | `crates/harness/src/controller.rs`, `controller/*.rs`, `submission.rs` | Owns queue order, cancellation, task lifecycle, recovery and side-effect coordination. |
| Model/runtime/tool boundary | `crates/harness/src/runtime.rs`, `inference/`, `context.rs`, `capabilities/` | Builds bounded context, drives provider turns and admits explicit workspace tools. |
| Verification and delivery | `crates/harness/src/verification.rs`, `completion.rs`, `delivery.rs` | Evaluates evidence and reproduces patch artifacts from controlled snapshots. |
| Execution helper | `crates/executor/src/lib.rs`, `supervisor.rs` | Defines bounded execution transport and the Linux supervision stub. |
| Shared memory | `crates/memory/src/lib.rs`, `store/`, `retrieval.rs`, `tool.rs`, `server/` | Provides local SQLite, optional remote client/server, retrieval and tool shapes. |
| Isolated child agents | `crates/harness/src/controller/subagents.rs` | Owns session-scoped child admission, messaging, lifecycle retention and recovery. |
| Packaging, evaluation and release | `justfile`, `.github/workflows/`, `docker/`, `evals/`, `scripts/` | Builds, checks, packages and evaluates the host plus executor helper. |

## Runtime paths

### Interactive task

```text
terminal input
  -> app::Cli / tui::client
  -> core::ConfiguredSession + app::HostClient
  -> HostClient local Unix IPC to detached `orvek host` server
  -> harness IPC command
  -> Controller + Store (authoritative task/journal state)
  -> Runtime (provider turn and admitted tools)
  -> DockerExecutor / executor helper when sandbox execution is selected
  -> durable journal records
  -> HostWatch -> tui::host_projection -> transcript and panes
```

The host-server portion of `app::host::serve` builds the provider and executor, opens host state, and serves harness IPC.
`crates/harness/src/store.rs` is the single host-owned SQLite writer and owns an exclusive lock;
`crates/harness/src/controller.rs` is the coordination point. Do not add an alternative persistence
or task-completion owner in the TUI, review server, or headless path.

### Headless task

```text
orvek run <prompt>
  -> app::headless submits through HostClient
  -> same host submission and task lifecycle
  -> reconnectable journal projection
  -> versioned `orvek.host` JSON Lines
```

The headless path is not a second execution loop. Its role is an automation adapter around the
same host protocol.

### Review and feedback

```text
review browser <-> review::server / review::diff
  -> HostClient command
  -> harness review records and session/task state
```

Review rendering is local presentation. Feedback is meaningful only after it reaches the host's
recorded review flow.

### Memory selection

`core::configured_memory_store` constructs a local store at the configured memory path, and a
remote client only when remote configuration matches the resolved workspace. Session admission calls
it only to fail fast on bad memory configuration; the resulting store is not attached to host session
state. The memory subsystem is deliberately distinct from host session history. `app::cli` manages
push/pull operations and `tui::client` reads and edits memory directly; neither operation is a
harness IPC capability. The local/remote data model and memory tool live in `crates/memory`, while
CLI configuration and credential handling live in `app::config` and `app::secret`.

## Authority and persistence boundaries

- **Host authority:** `orvek-harness` owns contracts, submissions, task state, queue order,
  verification evidence, completion and recovery. `docs/harness-integration.md` describes the
  non-negotiable client/host split.
- **Presentation:** TUI transcript/panes/drafts and headless output are projections. A client may
  reconnect from durable journal history; it may not infer completion from a transient preview.
- **Workspace execution:** Workspace commands are explicit capabilities under the runtime and
  executor policy. The host's SQLite/artifact store is not exposed to the executor.
- **Durable host data:** the host state root is `<config-directory>/host/v1`; `Store::open` uses
  `v1.sqlite3`, WAL/full synchronous SQLite settings, an owner lock, and an artifact store.
- **Secrets:** configuration and memory token types have dedicated ownership/redaction paths.
  Follow the repository's `AGENTS.md` zeroization rules when touching them.

## Change-impact routes

Use these routes before broad searching. They pair a behavioral question with the smallest useful
set of nodes to traverse in `graph.json`.

| If changing… | Start here | Then inspect |
| --- | --- | --- |
| A CLI command or config field | `app/cli.rs`, `app/config.rs` | command dispatch, `core/mod.rs`, and the receiving host IPC command. |
| Session creation, resume, or handoff | `core/mod.rs`, `tui/session.rs` | `harness/ipc.rs`, `session.rs`, `store.rs`, and import code. |
| Submission order, cancel/retry, or recovery | `app/submission.rs` | `harness/submission.rs`, `controller/submissions.rs`, `store/submissions.rs`, and IPC tests. |
| Provider request shape or tool calls | `harness/runtime.rs`, `harness/inference/` | `context.rs`, `capabilities.rs`, `workspace.rs`, and provider protocol tests. |
| Completion, verification, or delivery | `verification.rs`, `completion.rs`, `delivery.rs` | `state.rs`, `store.rs`, contract/admission types, and completion/delivery tests. |
| TUI interaction or rendering | `tui/client.rs`, `tui/components/root.rs` | `host_projection.rs`, the matching component module, and `tui/transcript/`. |
| Review behavior | `review/mod.rs`, `review/server.rs` | `review/diff.rs`, `harness/review.rs`, `harness/feedback.rs`, and IPC commands. |
| Local/remote memory | `memory/src/lib.rs`, `memory/store/` | `memory/tool.rs`, `memory/server/`, `core/mod.rs`, config, and `docs/memory.md`. |
| Sandbox execution or packaging | `harness/runtime.rs`, `executor/` | `app/host.rs`, Docker files, `justfile`, and Harbor adapter sources. |
| Child-agent behavior | `harness/controller/subagents.rs` and `subagents/` | Both models/protocols; verify whether the required consumer is the harness implementation or standalone crate. |

## Evaluation notes

1. **The authority split is intentional and well documented.** The direct source dependencies and
   the host documentation consistently place state transitions and completion in the harness. A
   feature that makes UI state authoritative would violate the central design, even if it appears
   simpler locally.
2. **The highest-density change areas are orchestration/persistence and presentation.**
   `controller.rs`, `store.rs`, `tui/components/root.rs`, and transcript/component modules are
   large integration surfaces. Prefer a narrow behavior change with an end-to-end test over a
   cross-cutting refactor in those areas.
3. **IPC is the critical compatibility seam.** `harness/ipc.rs` is shared by all client forms. A
   command addition normally requires host dispatch, client acknowledgement/reconnect behavior,
   durable record handling, and projection updates—not just a new enum variant.
4. **Memory and subagents have different integration boundaries.** Memory is a direct binary
   dependency, but session assembly only validates its configuration rather than attaching it to
   host state; the CLI and TUI use it directly. Session-scoped child-agent admission, lifecycle,
   messaging, and recovery are authoritative in the harness controller and require sandbox mode.
5. **The graph was challenged against the source rather than trusted as generated.** The audit
   found and corrected fabricated nested Cargo targets, missing TypeScript side-effect imports,
   missing Rust compile-time-include edges, missing local Python-import edges, a misleading
   combined host-client/server component, and an incorrect implication that session assembly
   attaches memory to host state. Package edges and target roots were compared with Cargo-resolved
   metadata. The generator now fails if a recognized Rust compile-time include or static relative
   TypeScript import cannot be resolved, and records the recognized and resolved counts in
   `graph.json`.
6. **The graph is intentionally reproducible rather than exhaustive semantic analysis.** It gives
   every tracked input a crawlable node and records file-backed modules, Cargo packages, local
   TypeScript/Python imports, Rust compile-time includes, local-document links, and curated
   responsibility edges. It does not claim to understand macro expansion, dynamic dispatch, model
   behavior, or runtime configuration; inspect the source and run relevant checks for those
   questions.

## Operational checks

Repository conventions in `AGENTS.md` define the normal Rust checks: `cargo check --all-features`,
`just check-fmt`, `just clippy`, and `just test`. The graph-specific invariant is:

```sh
python3 scripts/generate-codebase-graph.py --check
```

Run it alongside—not instead of—the relevant behavioral checks. The graph is a map of the tracked
system, not evidence that a changed system behaves correctly.
