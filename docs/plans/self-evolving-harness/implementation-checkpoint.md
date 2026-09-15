# Self-Evolving Harness Implementation Checkpoint

Recorded 2026-09-14T22:49:55Z on branch `optimize/context-window-cost` at
`7c3f860d56a008e9be335abd396ef439b390726f`.

## Objective and constraints

Implement the phased self-evolving harness plan without giving candidates Host, Store, secret,
sealed-artifact, or activation authority. Preserve the existing dirty Host/TUI migration outside
the evolution paths. Treat paid or externally started work with an unrecoverable result as terminal
infrastructure uncertainty; never infer success or retry it locally.

## Progress

- Phase 1 oracle: 20 Python tests pass.
- Phases 2–9 primitives: 60 focused Rust integration tests pass across manifest, store migration,
  binding, campaign, mining, proposal, and artifact targets.
- Phase 10 paired isolated trials: implemented and covered by 10 focused integration tests.
- Phase 11 scoring and gating: implemented and covered by 9 focused integration tests. The scoring,
  campaign, trial, artifact, and historical migration regression set passes all 41 tests.
- Phase 12 deterministic composition and fresh composite re-test: implemented. Its composition,
  campaign, and statistical regression set passes all 22 tests.

Baseline command logs from the resumed implementation are session-local scratch artifacts and are
intentionally excluded from Git. Re-run the commands in the verification section for durable proof.

## Phase 11 design rationale

The Store accepts a typed `AdaptivePromotionDataset` rather than a caller-computed score. One
immediate transaction reloads the frozen cohort policy, validates the campaign and decision
coordinates, evaluates the complete dataset, persists the immutable report, debits the shared
cohort ledger, and journals the verdict.

The selected shape keeps statistical and promotion authority inside the Host:

- Repeats aggregate inside cases and cases inside declared independent blocks before exact
  block-level sign inference.
- Primary useful-effect and protected non-inferiority metrics have explicit directions, margins,
  exact adjusted error allocations, and finite-value validation.
- Missing, drifted, underpowered, incomplete, or exhausted evidence yields `INCONCLUSIVE`; a valid
  attributable behavioral failure remains negative evidence.
- The multiplicity family charges candidates, rounds, metrics, strata, composites, fallbacks,
  campaigns, and activation attempts against the Store-global cohort ledger.
- Content-addressed reports bind the policy, evidence root, coordinates, ledger transition, gates,
  intervals, rationales, and tri-state verdict.
- Composition authority accepts only the stored `VERIFIED` report for the exact campaign, round,
  candidate, and decision coordinates; generic verdict journal APIs cannot bypass that proof.

The transaction records an exhausted-ledger result as an immutable zero-debit `INCONCLUSIVE`
report. Other validation or scoring errors roll back report, ledger, coordinate, and campaign state
together.

## Phase 12 design rationale

Composition accepts typed inputs that bind each candidate to its stored `VERIFIED` score report and
bounded proposal. The Store transaction reconstructs every child from the registered parent,
requires the complete verified child set, and is the only path allowed to journal a single-winner or
full-set composition.

The selected shape makes deterministic incompatibility a durable campaign result:

- Disjoint top-level replacements commute, identical replacements are idempotent, and different
  replacements of one field produce a typed conflict.
- The merged revision is validated against the combined compiled envelope before it is persisted.
- A field conflict or combined-manifest rejection atomically records the exact verified child set
  and terminates with the frozen `KeepParent` fallback. Restart cannot reopen the round or search a
  subset.
- A compatible composite receives content-addressed child lineage, a distinct candidate and
  revision, fresh trial work identities, and a new adaptive score decision coordinate.
- A failed or inconclusive composite re-test terminates with `KeepParent`; only a verified
  composite can advance to final audit.
- Replay reloads referenced score reports and immutable revisions and validates their campaign,
  round, candidate, parent, and composition identities. Missing dependencies fail closed.

Generic campaign append APIs reject compose verdicts and composition records. The typed single and
multi-candidate Store methods persist the revision and journal the linked transitions in one
transaction.

## Phase 11 paths

- `crates/harness/src/evolution/statistics.rs`
- `crates/harness/src/store/evolution.rs`
- `crates/harness/src/evolution/registry.rs`
- `crates/harness/src/evolution/trial.rs`
- `crates/harness/src/evolution/campaign.rs`
- `crates/harness/src/evolution/mod.rs`
- `crates/harness/src/lib.rs`
- `crates/harness/src/store.rs`
- `crates/harness/tests/evolution_statistics.rs`
- `crates/harness/tests/evolution_campaign.rs`
- `crates/harness/tests/evolution_artifacts.rs`
- `crates/harness/tests/evolution_store_migration.rs`
- `crates/harness/tests/fixtures/store_v1.sql`

## Phase 12 paths

- `crates/harness/src/evolution/composition.rs`
- `crates/harness/src/evolution/campaign.rs`
- `crates/harness/src/store/evolution.rs`
- `crates/harness/src/evolution/mod.rs`
- `crates/harness/src/lib.rs`
- `crates/harness/tests/evolution_composition.rs`
- `crates/harness/tests/evolution_campaign.rs`
- `crates/harness/tests/evolution_statistics.rs`

## Verification

- `python -m unittest evals.self_harness.test_oracle`: 20 passed before the Rust implementation.
- `cargo test -p orvek-harness --test evolution_composition --test evolution_campaign --test
  evolution_statistics`: 22 passed.
- `cargo test -p orvek-harness --test evolution_statistics`: 14 passed, including fresh composite
  scoring, both deterministic composition fallback modes, restart stability, and missing-dependency
  replay rejection.
- `cargo test -p orvek-harness --test evolution_artifacts --test evolution_store_migration --test
  evolution_campaign --test evolution_trials --test evolution_statistics`: 41 passed.
- Historical v1, v2, and v3 Store fixtures migrate to schema v6 without changing existing aggregate
  or journal bytes.
- `cargo check --all-features`, `just check-fmt`, and `just clippy`: passed.
- `just test`: 1,061 passed and 36 skipped.

## Remaining proof and next action

The real-container kill-boundary matrix still needs local Docker images and fixtures. Do not claim
runtime crash-recovery proof from compilation alone. Phase 14 must connect these contracts to the
coordinator and reconcile or fence durable runtime attempts. Synthetic scoring fixtures prove the
mechanism but do not enable production rollout without an owned calibrated suite and power policy.
Next, implement Phase 13's typed final-audit, activation, monitoring, and rollback boundary without
enabling registry writes before the open rollout prerequisites are satisfied.


## Status update, 2026-09-15 (d009644)

Landed since the checkpoint above: GLM via the local Responses bridge (HTTP
default transport, per-model `[models.<name>]` routing, ZAI/OPENAI key envs),
Spark (`gpt-5.3-codex-spark`), bridge-shaped terminal decoding, reasoning-live
streaming, the ActivityMark header, managed default workspace, conversation
framing, and reasoning-sized auxiliary limits.

### Verified cutover gap inventory

| Gap | Evidence (current tree) |
| --- | --- |
| Shell commands | `RootEffect::RunShell` falls into the client catch-all: "waiting for its host capability adapter"; `ShellStarted`/`ShellPublished` map to `Vec::new()` in the projection |
| Memory browser | `LoadMemories`/`DeleteMemory` hit the same catch-all |
| MCP runtime | Only config/render references remain; no host-side MCP execution |
| Subagents | Done in `791898c`: the orvek-subagents tool contract runs on the harness provider (GLM/Spark capable; nanocodex cannot host them), with journaled read-only child jobs, schema-validated digest-referenced results, live TUI child-tree updates, lifecycle tools, and config policy |
| Review rendering | Review submit path is wired; `ReviewRecorded` journal events are dropped from the projection |
| TUI parity | Local prompt echo is host-side only; `Compact`/`CancelCompaction`, `ContinueSubagent`, `PersistSteer`, `Interrupted`/`ContextCompacted` entries, and 18 removed benchmarks/~293 tests remain un-restored |
| Evolution loop | Phase 13 done in `6d70b5c` (promotion boundary); phases 14-15 remain: the campaign coordinator, IPC commands, and the `orvek evolve` CLI still have no callers |

### Sequenced completion plan

Each unit ends in a verifiable state and lands independently.

1. `feat(tui): serve shell commands through the host` — wire `RunShell` to a
   `WorkIntent::Shell` submission and render `ShellStarted`/`ShellPublished`
   in the projection; regression test for the rendered output.
2. `feat(tui): restore the memory browser` — host memory query/delete command
   or documented local-store adapter; same pending-effect removal.
3. `feat(tui): render review records` — map `ReviewRecorded`/`Feedback` out of
   the `Vec::new()` arms.
4. Done (`791898c`) — see the gap table note above.
5. Done (`6d70b5c`) — certificates, bounded approval, CAS activation with
   supersession, rollback naming, and the audit-epoch burn; registry writes
   stay gated behind the open-rollout prerequisites.
6. `feat(harness): phase 14 coordinator` — durable campaign loop
   (mine -> propose -> trial -> score -> compose -> activate) with crash
   reconciliation and fencing; invoked by:
7. `feat(host): evolution IPC and `orvek evolve` CLI` — manual trigger first,
   operator policy later (phase 15).
8. `feat(tui): parity tail` — local echo with host dedupe on resume, manual
   compaction controls, `ContinueSubagent`, motion settings, benchmarks/tests.

Do not enable registry writes (unit 5) before the open-rollout prerequisites
in the section above are satisfied; until then the coordinator runs dry-run
campaigns only.
