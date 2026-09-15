# Phase 9: Generate Bounded Candidates

[Back to overview](overview.md)

## Goal

Ask the frozen target model for `K` materially distinct minimal manifest patches through one structured-output boundary, then validate and persist only safe candidates.

## Hypothesis and exit predicate

**Hypothesis:** the target model, under immutable proposal authority, can generate candidate diversity from mining evidence without gaining execution, hidden-evidence, scoring, or activation authority.

Exit only when each accepted proposal binds the parent/evidence/policy digests, passes the envelope, is non-no-op and distinct, remains within budgets, and cannot encode source/evaluator/permission changes; provider uncertainty never causes a blind retry.

## Changes

| File | Change and reason |
| --- | --- |
| `crates/harness/src/evolution/proposal.rs` | Define proposal context, one structured patch output, canonical validation/deduplication, diversity dimensions, and lineage. |
| `crates/harness/src/controller/evolution.rs` | Run `K` proposal intents with the frozen model/protocol and Host-registered `PolicyId`; record provider identities and receipts without evaluating candidates. |
| `crates/harness/tests/evolution_proposal.rs` | Cover malformed, no-op, duplicate, stale, forbidden, over-budget, provider-unknown, cross-role evidence, and valid diverse outputs. |

Proposal input contains the editable schema, mining facts/hypotheses, pass anchors, bounded history summaries, and strict output limits. It contains no raw trace handle, adaptive/final result, evaluator operation, active-pointer operation, credential, or unrestricted tool. Information learned from adaptive or final results for a retired cohort cannot be serialized back into a later proposal against that cohort.

## Static verification

```sh
cargo test -p orvek-harness --test evolution_proposal
cargo test -p orvek-harness --test controller_execution
cargo check --all-features
just check-fmt
just clippy
```

Confirm proposer model identity equals the frozen target model in version 1. A different proposer requires a new prospective protocol/cohort and cannot reuse prior certificates.

## Runtime verification

Return valid, duplicate, forbidden, and delayed patches in different completion orders; candidate identities and canonical ordering must match. Disconnect after billable intent. If provider lookup cannot establish the result, record `InfrastructureUnknown`, conservatively debit the configured budget, and do not retry the intent.

## Revision

`feat(harness): generate bounded harness proposals`
