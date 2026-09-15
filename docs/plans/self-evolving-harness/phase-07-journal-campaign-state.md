# Phase 7: Journal Campaign State

[Back to overview](overview.md)

## Goal

Add a durable campaign aggregate to the existing Store authority, linked to a Store-global cohort and to effect intent/accounting records.

## Hypothesis and exit predicate

**Hypothesis:** a campaign can survive process death without a second journal, ambiguous transition, resettable cohort history, or duplicate accounting.

Exit only when stored projection equals replay for every event prefix; campaign events preserve the existing kind/aggregate/revision/hash-chain rule; each effect has a deterministic identity and intent-before-effect record; cohort debits are atomic with verdict use; and crash injection yields a reconciled receipt, a fenced `InfrastructureUnknown`, or a negative attributable outcome without double counting.

## Changes

| File | Change and reason |
| --- | --- |
| `crates/harness/src/evolution/campaign.rs` | Define campaign, round, candidate, effect, verdict, approval, activation, monitoring, and terminal states with a total legal transition table. |
| `crates/harness/src/store/evolution.rs` | Append/project campaign events under expected revision; link the campaign to one immutable cohort; transact effects, leases, fences, budgets, and global ledger debits. |
| `crates/harness/tests/evolution_campaign.rs` | Cover transition legality, replay, stale revisions, cross-campaign ledger persistence, effect accounting, corruption, and crash prefixes. |

`CampaignId` is UUID-compatible. `EffectId` binds the exact campaign transition and external work identity. Verification, approval, activation, supersession, monitoring, and rollback are separate states. An effect may have one terminal accounting result without claiming the external system executed or billed exactly once.

## Static verification

```sh
cargo test -p orvek-harness --test evolution_campaign
cargo test -p orvek-harness --test session_journal
cargo check --all-features
just check-fmt
just clippy
```

Review the generated transition table. Every state names legal events, required evidence, expected revision, effect ownership, reconciliation rule, budget debit, and terminal behavior.

## Runtime verification

Replay campaigns with multiple candidates, a shared cohort, leases, unknown effects, and terminal outcomes into a fresh Store; compare normalized projections and each aggregate root. Kill before intent, after intent, after external start, after response, and before/after receipt commit. Restart must resume from durable state, never reset cohort history, and never blindly retry an unknown paid intent.

## Revision

`feat(harness): persist evolution campaign lineage`
