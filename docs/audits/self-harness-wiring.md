# Self-Harness Wiring Audit

## Verdict

Orvek does not ship a self-improving harness. Before this audit, it contained a large, internally tested prototype of the *Self-Harness: Harnesses That Improve Themselves* loop, but no coordinator, IPC commands, CLI, or normal-query caller could run that loop. Keeping it made tests look like product wiring and created an unsafe temptation to expose uncalibrated activation code.

The disconnected prototype is removed. Normal queries still use a validated, immutable admission profile. Historical session manifests remain readable. Current sessions always use the source-defined baseline instructions. No user query can propose, score, activate, or roll back harness changes.

## Scope and method

The audit covered all first-party production modules in the Rust workspace, `web/review`, and the tracked Python evaluation packages. It combined:

1. A source inventory of every Rust module declaration and every Python/TypeScript import edge.
2. Caller searches from CLI, IPC, host, controller, store, and session entry points.
3. A paper-concept inventory covering mining, proposal, paired trials, statistical gates, composition, final audit, activation, and rollback.
4. Runtime inspection of the live host database before cleanup.
5. Compiler, lint, test, web, Python, migration, and source-tree checks.

The rerunnable check is `python3 scripts/audit-component-wiring.py`. It fails on orphaned source modules, missing query/admission edges, or reintroduced prototype paths and symbols.

Baseline evidence is retained outside the repository in `/tmp/orvek-component-audit/baseline-findings.md`. At audit start:

- The evolution implementation occupied 11,386 production lines across 13 files.
- Its 39 store methods had only three production-facing fallback-admission callers. All mutation, cohort, campaign, evidence, scoring, activation, and rollback methods were test-only.
- Dedicated evolution tests occupied 6,123 lines across 11 integration-test files.
- `controller/evolution.rs` exported proposal/trial helpers but no production code called them.
- The live database contained zero registered harness revisions, targets, cohorts, campaigns, score reports, activation certificates, activation receipts, or rollback receipts.
- The product README advertised no self-evolution behavior or command.
- The former implementation checkpoint itself recorded that the coordinator, IPC commands, and `orvek evolve` CLI were absent.

## Query execution path

The active user-query path is unchanged and traceable:

1. `bin/orvek/src/main.rs` parses `Cli` and runs the selected command.
2. `bin/orvek/src/core/mod.rs::ConfiguredSession::admit` sends `ipc::Command::CreateSession` with a `SessionAdmissionRequest`.
3. `crates/harness/src/ipc.rs::dispatch` routes `Command::CreateSession` to `Host::create_session_with_id` and `Command::Submit` to the host submission path.
4. `crates/harness/src/controller.rs::resolve_admission` derives the model, protocol, environment, task-profile, configuration, and instruction authority identities.
5. `crates/harness/src/session.rs::SessionAdmissionProfile::compiled` binds the request to the validated source-defined manifest.
6. `Store::create_bound_session` journals the profile. Resume, fork, handoff, import, and request execution validate that profile before use.
7. `controller.rs` reads `SessionAdmissionProfile::behavior_instructions()` when constructing primary and auxiliary inference context.
8. Tool execution, provider accounting, verification, submission, and durable settlement continue through the existing host and store paths.

There is no conditional branch from this path into mining, candidate generation, trials, campaign state, or activation.

## Admission compatibility path

`crates/harness/src/admission_profile.rs` is the retained boundary. It has two jobs:

- Define the typed identities serialized in durable session admission records.
- Parse bounded historical manifests and expose only their validated behavior instructions and identity digests.

New profiles contain only the baseline instruction layer and identity metadata needed by session integrity checks. Old manifests can contain fields written by the removed prototype. Those fields are parsed as compatibility data and do not grant capabilities or trigger behavior.

The SQLite schema remains at version 7 so existing hosts open without downgrade or destructive migration. Fresh stores create only task, session, event, and lease tables. Stores from versions 2 through 6 keep any prototype tables untouched and advance the schema marker; current code neither reads nor writes those tables. Version-1 stores still migrate their event layout and preserve task/session data.

## Paper-to-code disposition

| Paper mechanism | Pre-audit implementation | Product edge | Disposition |
| --- | --- | --- | --- |
| Held-in failure mining and deterministic signatures | `evidence.rs`, `mining.rs` | Tests only | Removed |
| Parallel bounded candidate proposals | `proposal.rs`, controller dispatch helpers | Tests only; no coordinator | Removed |
| Protocol-equivalent paired trials and receipts | `trial.rs`, controller dispatch helpers | Tests only; no scheduler/recovery owner | Removed |
| Block-valid statistics and non-regression gates | `statistics.rs`, Python oracle | Synthetic cross-checks only | Removed |
| Compatible patch composition and retest | `composition.rs` | Tests only | Removed |
| Campaign journal and effect state machine | `campaign.rs`, store APIs | No start/advance/reconcile caller | Removed |
| Hidden final audit and certificates | `promotion.rs`, sealed evidence store | Tests only; known state-binding gaps | Removed |
| Activation, monitoring, and rollback | store pointer and receipt APIs | No IPC, CLI, or operator path | Removed |
| Retain current harness when no candidate passes | Represented in test state machines | No executable campaign | Removed with the dormant loop |
| Immutable harness used by a query | Session admission profile | Every create/import/resume/fork path | Retained and simplified |

This is deliberate subtraction, not a claim that the paper loop is complete. Reintroducing self-improvement would require a new product decision, calibrated production evidence, an owned coordinator, bounded operator commands, recovery semantics, and an end-to-end rollout test. The removed test-only prototype is not treated as a shortcut to that work.

## Removed disconnected code

The cleanup removes:

- The evolution campaign, composition, evidence, mining, promotion, proposal, statistics, and trial modules.
- The dynamic harness registry and all evolution mutation APIs in `Store`.
- Proposal/trial controller dispatch helpers with no coordinator caller.
- Sealed-evidence staging, reservation, quota, and recovery code used only by evolution tests.
- Fresh-database creation of evolution tables. Existing tables are preserved non-destructively.
- Eleven dedicated Rust integration-test targets that tested only inaccessible prototype APIs.
- The standalone Python self-harness oracle and its synthetic fixtures.
- The phased self-evolution plans and checkpoint, which otherwise described deleted code as staged product work.
- The false model instruction that claimed Orvek's harness “evolves separately.”
- The self-harness contract section in the Harbor evaluation README.

The public artifact store remains active and is simpler: one immutable content-addressed directory, one quota, verified reads, atomic writes, and the download cache used by host artifact streaming.

## Retained components

This section records the audited revision. The later macOS distribution change removed the
standalone Cloudflare/WASM example while retaining generic remote memory support.

Every retained production subsystem has an active entry path:

- `bin/orvek`: CLI, configuration, authentication, host bootstrap, headless mode, review service, TUI, and update flow.
- `crates/harness`: IPC, durable sessions/tasks, inference, tools, workspaces, review, verification, submissions, delivery, imports, subagent orchestration, artifacts, and recovery.
- `crates/executor`: sandbox process supervision and its binary entry point.
- `crates/memory`: local and remote memory implementations used by CLI/TUI configuration.
- `web/review`: browser review application, build/dev entry points, and test modules.
- `evals/harbor_adapter` and `evals/snapcompact`: tracked benchmark adapters and analysis entry points.

The module audit follows language-native declarations/imports rather than relying on filename counts alone. Conditional and test modules count as connected when their parent target declares them. Cargo bench/build targets and Python/TypeScript executable/test roots are included.

## Compatibility residues

A small set of names remains because it is part of durable JSON:

- `HarnessProvenance::Registered`
- `Channel::Canary`
- `BaselineReason::UnregisteredTarget`
- Behavior and envelope digests in `HarnessBinding`
- The schema version 7 marker on existing databases

Current production code does not create a registered revision or select a canary. Removing these enum variants or serialized fields would make historical session records fail to decode or change their journal projections. Their purpose is compatibility, not latent self-evolution.

The code does not delete old evolution tables, sealed artifacts, or staging files from a user's existing host directory. Destructive cleanup is outside this audit. Fresh stores no longer create them.

## Verification

The acceptance gates are:

- `python3 scripts/audit-component-wiring.py`
- `just check-fmt`
- `just clippy`
- `cargo check --all-features`
- `PATH=/tmp/orvek-component-audit/tools/bin:$PATH just check-features` (all 16 `orvek-memory` feature combinations)
- `just test --no-fail-fast`
- `git diff --check`
- `cd web/review && bun test && bun run typecheck`
- `cd evals && uv run python -m unittest discover`
- Version-1 store migration and a read-only SQLite backup replay of all 79 durable live sessions

The final command results are recorded in `/tmp/orvek-component-audit/GATES.md`. The wiring script reports exact reachable-module counts and fails if its expected source/report contract drifts.

## Exclusions

`evals/incident_replay/` contains pre-existing untracked work. This audit does not edit or claim it. The wiring script reports its Python file count as excluded so the boundary is visible rather than silently ignored.

At the time of this audit, vendored `nanocodex` crates were third-party snapshots excluded by the
workspace and were not first-party component ownership. They were removed in the later
maintainability-baseline change. Generated build output, `.venv`, `node_modules`, and `target` are
also outside the source-module inventory.
