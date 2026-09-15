# Phase 3: Migrate Store and Register Baselines

[Back to overview](overview.md)

## Goal

Create the durable schema foundation for revisions, target-specific active pointers, Store-global evaluation cohorts, and their non-resettable ledgers before any session or campaign depends on them.

## Hypothesis and exit predicate

**Hypothesis:** Store schema version 1 can migrate without losing existing task/session aggregates, artifact identities, revisions, or per-aggregate hash-chain integrity.

Exit only when a real version-1 fixture migrates to version 2; old aggregate replay roots are unchanged; the rebuilt event-kind constraint accepts the new aggregate kinds and still rejects unknown kinds; a compiled baseline revision is registered for each supported target profile; and registry/cohort mutation remains unavailable outside trusted Store transactions.

## Changes

| File | Change and reason |
| --- | --- |
| `crates/harness/src/store.rs` | Add the version-1-to-version-2 migration. Rebuild the `events.kind` constraint transactionally, preserve rows and indexes, and keep hash verification scoped to aggregate kind, aggregate ID, and revision. |
| `crates/harness/src/store/evolution.rs` | Add tables and transactional APIs for immutable revisions, target/channel active pointers, evaluation cohorts, partition commitments, independent blocks, adaptive/final ledgers, and retired audit epochs. |
| `crates/harness/tests/evolution_store_migration.rs` | Exercise migration, downgrade rejection, corruption handling, baseline registration, cohort reuse prevention, and exact replay-root preservation. |

`CampaignId` and any aggregate identifier stored in the existing event table must use its UUID-compatible representation. Registry selection keys bind model, protocol, environment, task-profile, and channel digests. The initial baseline is built from the current Host-owned behavior and stored by digest; it is not supplied by a client.

## Static verification

```sh
cargo test -p orvek-harness --test evolution_store_migration
cargo test -p orvek-harness --test session_journal
cargo check --all-features
just check-fmt
just clippy
```

Review every migration statement for transactional rollback and preservation of unrelated aggregates. Confirm no new public API accepts a raw revision, evaluator output, partition opening, or ledger balance from an IPC caller.

## Runtime verification

Open a checked-in version-1 Store fixture, record every task/session event root and active session projection, migrate, reopen, and compare them exactly. Register a cohort, debit it from two campaigns, and prove the second campaign observes the first debit. Retire an audit epoch, start another campaign, and prove the epoch cannot be reopened or replaced under the same cohort.

## Revision

`feat(store): migrate evolution registry state`
