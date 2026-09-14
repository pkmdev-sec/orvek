# Phase 12: Compose Winners and Re-test

[Back to overview](overview.md)

## Goal

Compose individually verified type-compatible patches deterministically and require fresh adaptive evidence for the complete composite.

## Hypothesis and exit predicate

**Hypothesis:** typed operations can establish syntactic compatibility, while only a new paired evaluation can establish behavioral compatibility.

Exit only when merge order and digest are deterministic; conflicts and combined ceilings fail closed; composite trials use fresh work keys and ledger allocation; child evidence/certificates cannot be inherited; and failure triggers the frozen fallback without post-result subset search.

## Changes

| File | Change and reason |
| --- | --- |
| `crates/harness/src/evolution/composition.rs` | Define field-aware compatibility, canonical ordering, conflict reports, composite lineage, and fresh evaluation plans. |
| `crates/harness/src/evolution/campaign.rs` | Add legal individual-verdict, composition, composite-trial, and fallback transitions. |
| `crates/harness/src/store/evolution.rs` | Make verified reports, immutable revisions, composition events, and replay dependency checks one typed authority boundary. |
| `crates/harness/tests/evolution_composition.rs` | Cover disjoint/identical/conflicting edits, combined ceilings, and order invariance. |
| `crates/harness/tests/evolution_statistics.rs` | Cover typed authority, fresh evidence, durable fallback, restart, replay tampering, interaction failure, and no subset search. |

Version 1 freezes its fallback before the round: none verified keeps the parent; one verified may advance; several verified attempt the full compatible composite; a failed/inconclusive composite keeps the parent or uses one predeclared single-winner rule already included in the multiplicity family. It never explores new subsets after seeing the composite result.

## Implementation boundary

Phase 12 keeps the existing single-winner path and adds a separate full-set composite path:

```text
CompositionInput(candidate, typed score, bounded proposal)
    -> compose_verified(parent, inputs)
    -> ComposedHarness(CompositePlan, canonical patch, validated revision)

CompositePlan
    = parent digest
    + ordered child(candidate, proposal, score, patch/revision digests)
    + composite candidate and revision digests
    + frozen KeepParent fallback

Campaign round
    Scored children
    -> ComposeComposite verdict
    -> canonical merge
       -> deterministic conflict/combined rejection -> terminal KeepParent
       -> AwaitingTrial composite
    -> Trialed composite
    -> Scored(Verified) -> final audit
    -> Scored(NotVerified/Inconclusive) -> terminal KeepParent fallback
```

`Store::compose_verified_candidates` is the authority boundary. In one transaction it verifies that the request contains every and only individually verified child, constructs the canonical full-set merge, persists the immutable revision, and appends both campaign transitions. Generic campaign events cannot forge this path. The composite receives a derived candidate identity, so child trial receipts cannot satisfy its dataset and its adaptive score consumes a new decision coordinate and ledger allocation.

A deterministic field conflict or combined-manifest rejection is also committed in that transaction as a typed terminal `KeepParent` result. It records the exact verified child set and remains terminal after restart; it is not a retryable API error. Replay reloads every referenced score report and harness revision, validates their campaign and lineage bindings, and fails closed when a dependency is absent or inconsistent.

Top-level manifest fields are replacement operations. Disjoint fields commute; byte-identical replacements are idempotent; different replacements of the same field are a typed conflict. Applying the merged patch to the parent performs the combined compiled-envelope validation. Version 1 exposes only `KeepParent`, so a failed composite has no legal transition that can select a newly discovered subset.

## Static verification

```sh
cargo test -p orvek-harness --test evolution_composition
cargo test -p orvek-harness --test evolution_campaign
cargo test -p orvek-harness --test evolution_statistics
cargo check --all-features
just check-fmt
just clippy
just test
```

Property-test commutativity for disjoint operations and digest stability. Confirm composite scoring accepts only a new `AdaptivePromotionDataset` and a fresh committed ledger debit.

## Runtime verification

Use a fixture where A and B pass individually but A+B regresses. The composite receives fresh trials, fails its protected gate, and remains inactive. Restart during merge and trial scheduling; replay reconstructs the same child set and never schedules a newly chosen subset.

## Revision

`feat(harness): compose and re-test verified candidates`
