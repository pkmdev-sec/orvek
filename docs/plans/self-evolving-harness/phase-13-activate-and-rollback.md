# Phase 13: Audit, Activate, Monitor, and Roll Back

[Back to overview](overview.md)

## Goal

Use a globally one-use final audit to issue a certificate, then keep verification, operator approval, pointer activation, monitoring, supersession, and rollback as separate durable transitions.

## Hypothesis and exit predicate

**Hypothesis:** final confirmation can remain independent of search, and registry mutation can require both a valid certificate and an explicit bounded-channel approval.

Exit only when first audit access burns the epoch globally; a failed/burned audit retires the cohort or requires prospectively committed non-overlapping confirmatory cases; raw results/reasons never return to proposal; certificates bind exact evidence roots and per-aggregate event roots; approval references campaign/certificate/channel rather than a raw digest; CAS losers remain verified/superseded; and rollback names a prior activation receipt or last-known-good receipt.

## Changes

| File | Change and reason |
| --- | --- |
| `crates/harness/src/evolution/promotion.rs` | Define typed `FinalAuditDataset`, audit burn, certificate, approval, activation/supersession/monitoring states, and rollback validation. |
| `crates/harness/src/store/evolution.rs` | Transact audit burn before access, certificate persistence, expected-active pointer changes, approvals, and append-only rollback receipts. |
| `crates/harness/tests/evolution_promotion.rs` | Cover global burn, noninterference, fresh-cohort requirement, certificate roots, CAS race, supersession, channel approval, monitoring, rollback, and pinned sessions. |

The certificate binds lineage, target, cohort and hiding commitments, block manifest, policy/model/protocol/environment/evaluator identities, paired receipts, exact campaign/cohort aggregate roots, gate report, ledger debits, merge receipt, audit consumption, expected base, and rollback target. Public output contains bounded summaries and commitments, not raw final outcomes.

## Static verification

```sh
cargo test -p orvek-harness --test evolution_promotion
cargo test -p orvek-harness --test controller_execution
cargo check --all-features
just check-fmt
just clippy
```

Review transaction isolation, information flow, and status labels. Only committed CAS emits `Active`. Verification alone cannot move a pointer; approval cannot nominate an arbitrary digest; a stale base cannot be rebound or rebased.

## Runtime verification

Race two approved campaigns against one expected pointer; exactly one activation commits and the loser is verified/superseded. Kill before and after audit access, receipt, certificate, approval, and CAS. Access-before-crash burns the audit. Restart after committed CAS returns the durable receipt. Trigger a canary hard-gate breach and roll back future sessions to the named last-known-good receipt while existing sessions stay pinned.

## Revision

`feat(harness): audit and activate harness revisions`
