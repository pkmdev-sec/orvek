# Phase 14: Orchestrate and Recover Campaigns

[Back to overview](overview.md)

## Goal

Connect verified primitives through one private bounded coordinator integrated with the existing Host lifecycle, recovery passes, shutdown policy, and global execution semaphore.

## Hypothesis and exit predicate

**Hypothesis:** one-transition-at-a-time orchestration can progress autonomously within frozen limits without adding a second scheduler or bypassing a gate.

Exit only when every state has a next-action or terminal mapping; repeated `advance` at one revision is idempotent; startup recovery reconciles effects before scheduling; evolution shares the Host-wide concurrency limit; idle shutdown accounts for campaigns; pause/resume/cancel preserve ownership; and status/watch/export are bounded redacted projections.

## Changes

| File | Change and reason |
| --- | --- |
| `crates/harness/src/evolution/coordinator.rs` | Implement private `advance`/`reconcile`, stop checks, lease fencing, and one durable transition or effect intent per cycle. |
| `crates/harness/src/controller/evolution.rs` | Integrate coordinator construction with `open_configured`, interrupted/submission recovery, the global runs semaphore, and `shutdown_if_idle`; do not create an independent scheduler. |
| `crates/harness/src/ipc.rs` | Add coarse start/status/watch/pause/resume/cancel/export/approve-rollout/rollback requests with expected revisions and explicit response/page ceilings below 8 MiB. |

`StartEvolution` accepts `TargetProfile`, Host-registered `PolicyId`, requested budgets bounded by policy, and `expected_active`. Approval names a verified campaign/certificate/channel and expected pointer. Rollback names a prior activation or last-known-good receipt. Private mine/propose/run/score/merge/audit/direct-activate stages are not IPC operations.

## Static verification

```sh
cargo test -p orvek-harness --test controller_execution
cargo test -p orvek-harness --test operator_protocol
cargo test -p orvek-harness --test session_journal
cargo check --all-features
just check-fmt
just clippy
```

Review the transition table and Host lifecycle call graph. Confirm campaign work cannot exceed the existing semaphore, keep the Host falsely idle, bypass startup recovery, or return an unbounded event/evidence collection.

## Runtime verification

Run a fake campaign end to end and kill the Host at each durable/effect boundary. Restart until terminal; compare normalized lineage, aggregate roots, receipts, debits, verdict, and pointer with an uninterrupted run. Saturate ordinary and evolution work together and prove the global limit holds. Send oversized, unauthorized, stale, and private-stage IPC requests and require rejection before state change.

## Revision

`feat(harness): resume durable evolution campaigns`
