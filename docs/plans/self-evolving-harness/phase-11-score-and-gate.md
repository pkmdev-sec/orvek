# Phase 11: Score Evidence and Apply Gates

[Back to overview](overview.md)

## Goal

Implement the frozen scoring policy in Rust and atomically spend the Store-global cohort ledger before any adaptive verdict can be used.

## Hypothesis and exit predicate

**Hypothesis:** a typed `AdaptivePromotionDataset` can produce a deterministic block-valid tri-state verdict with complete multiplicity accounting and hard safety/resource gates.

Exit only when Rust and the Phase 1 oracle agree; repeats aggregate within cases and cases within declared blocks; the full search family is charged; ledger check/debit/verdict persistence is one transaction; no mining/final dataset compiles at this boundary; and partial, drifted, underpowered, or exhausted evidence cannot verify.

## Changes

| File | Change and reason |
| --- | --- |
| `crates/harness/src/evolution/statistics.rs` | Implement the versioned block-valid estimator, useful-effect and protected non-inferiority bounds, hard gates, missingness, and required cohort-ledger debit. |
| `crates/harness/src/store/evolution.rs` | Atomically reserve/debit query and error allocation across campaigns and persist the exact gate report/verdict use. |
| `crates/harness/tests/evolution_statistics.rs` | Cross-check oracle vectors and cover ordering, labels, block structure, duplicate attempts, boundaries, cross-campaign exhaustion, and role misuse. |

The multiplicity family includes `K`, rounds, protected metrics, critical strata, composites, any predeclared fallback, repeated campaigns, and activation attempts against the cohort. A valid evaluator failure is data; infrastructure unknownness follows the frozen missingness rule and can only yield `INCONCLUSIVE` or a predeclared conservative result.

## Static verification

```sh
python -m unittest evals.self_harness.test_oracle
cargo test -p orvek-harness --test evolution_statistics
cargo test -p orvek-harness --test evolution_campaign
cargo check --all-features
just check-fmt
just clippy
```

Review units, inequality direction, finite-number handling, inclusive/exclusive boundaries, block sample counts, and transaction isolation. There is no API that accepts a caller-computed score or ledger delta.

## Runtime verification

Feed canonical oracle vectors to both implementations and compare gates, intervals, debits, and policy digests. Run null, known-effect, regression, high-variance, correlated-block, sparse-stratum, incomplete-pair, and exhausted-ledger simulations. Start two campaigns on one cohort and race the last allocation; at most one verdict use commits.

## Revision

`feat(harness): score paired evolution evidence`
