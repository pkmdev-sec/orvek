# Phase 4: Seal and Account for Evolution Evidence

[Back to overview](overview.md)

## Goal

Make artifact visibility, lifecycle, and quota part of the Store boundary before hidden partitions or evaluator reports are written.

## Hypothesis and exit predicate

**Hypothesis:** existing public artifact behavior can remain compatible while all new evolution evidence uses typed, purpose-bound public or sealed handles that generic status, journal, watch, and export paths cannot resolve.

Exit only when raw `put`, `read`, `path`, and `Store::artifacts` access is private or crate-private; public and sealed reads require different typed capabilities; existing artifacts migrate as public; partial writes are staged and recoverable; reservations prevent quota races; orphan collection is deterministic; and every external projection remains redacted and below the IPC frame limit.

## Changes

| File | Change and reason |
| --- | --- |
| `crates/harness/src/artifacts.rs` | Add visibility and purpose metadata, typed `PublicArtifactRef`/`SealedArtifactRef`, staging, reservation, commit, cancellation, and orphan-GC operations. Narrow raw content-addressed APIs to trusted crate code. |
| `crates/harness/src/store/evolution.rs` | Transact artifact reservations and evidence metadata with cohort/campaign state; authorize sealed reads by exact Host-owned purpose and burn audit access before returning audit content. |
| `crates/harness/tests/evolution_artifacts.rs` | Cover compatibility migration, capability separation, wrong-purpose reads, digest guessing, crash points, reservation races, global quota, GC, redaction, and bounded projections. |

`MiningEvidenceRef`, `AdaptivePromotionRef`, and `FinalAuditRef` wrap the appropriate visibility handle but remain distinct types. There is no generic evidence enum accepted by scoring or proposal code and no conversion from final/adaptive evidence to mining input.

## Static verification

```sh
cargo test -p orvek-harness --test evolution_artifacts
cargo test -p orvek-harness --test operator_protocol
cargo check --all-features
just check-fmt
just clippy
```

Search all raw artifact call sites and classify each trusted caller. Generic JSON event payloads may contain only redacted metadata and typed public identities, never a sealed digest that another public method can resolve.

## Runtime verification

Race reservations against the Host-global artifact quota, kill before and after staged write/metadata commit, and reopen the Store. Recovery must commit one valid artifact or collect an unreachable stage without leaking it. Attempt sealed resolution through Host raw-artifact, journal, watch, status, export, and guessed-digest paths; every path must fail without disclosing existence or content. Verify worst-case pages serialize below 8 MiB with a lower explicit response ceiling.

## Revision

`feat(store): seal evolution evidence artifacts`
