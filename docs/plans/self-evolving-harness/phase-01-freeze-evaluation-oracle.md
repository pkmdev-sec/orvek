# Phase 1: Freeze the Evaluation Oracle

[Back to overview](overview.md)

## Goal

Create an implementation-independent reference for evidence roles, pairing, block-valid aggregation, three-way outcomes and verdicts, hard gates, Store-global cohort accounting, and certificate inputs before any self-evolution code exists.

## Hypothesis and exit predicate

**Hypothesis:** the intended promotion policy can be expressed deterministically from a frozen cohort and campaign specification without consulting candidate identity, audit data, or post-result choices.

Exit only when golden fixtures cover improvement, regression, equality, attributable crash, protocol timeout, incomplete pairs, environment drift, critical-case failure, exhausted query/error budget, and insufficient power; null simulations cover the complete planned candidate/round/composite/campaign decision process; two runs produce byte-identical normalized output; and changing candidate labels without changing evidence cannot change the verdict.

## Changes

| File | Change and reason |
| --- | --- |
| `evals/self_harness/oracle.py` | Add a dependency-light reference oracle with canonical input/output. Aggregate repeats within cases, then use declared `independence_block_id` values or one frozen block-valid method. |
| `evals/self_harness/test_oracle.py` | Add golden, metamorphic, boundary, malformed-input, null-family, and power simulations. Cover the complete multiplicity family across candidates, rounds, metrics, strata, composites, fallbacks, campaigns, and activation attempts. |
| `evals/README.md` | Specify the three non-interchangeable evidence roles, global cohort/error ledgers, outcome taxonomy, hiding commitments, estimator versioning, and calibration procedure. |

Data structures: `MiningEvaluation`, `AdaptivePromotionDataset`, and `FinalAuditDataset` have distinct schemas with no implicit conversions. `FrozenEvaluationSpec` names a global cohort, independent blocks, outcome classifier, multiplicity family, and ledger allocation. `ReferenceVerdict` contains named gates and exact query/error-ledger debits.

The frozen outcome taxonomy is `Pass | BehavioralFailure | InfrastructureUnknown`. A valid evaluator rejection, attributable candidate crash, budget exhaustion, or protocol-defined timeout is negative behavioral evidence. Transport loss, missing or tampered receipts, evaluator/environment/model drift, or an unreconciled external attempt is unknown and cannot promote.

## Static verification

```sh
python -m unittest evals.self_harness.test_oracle
python -m compileall -q evals/self_harness
just check-fmt
```

Review the fixtures for case-then-block aggregation, not attempt-level pseudoreplication. Confirm no function accepts an expected winner, converts mining evidence into confirmatory evidence, resets a cohort ledger, or adjusts a threshold from observed candidate data. Partition fixtures use hiding-and-binding commitments with sealed nonces or keyed construction so low-entropy membership cannot be enumerated from a public digest.

## Runtime verification

Run the oracle twice over the checked-in fixture corpus and compare canonical outputs byte for byte. Mutate ordering, candidate names, repeat ordering, and within-block case order; verdicts and evidence roots must remain unchanged. Remove one half of a pair and change one environment digest; both cases must become `INCONCLUSIVE`. Run whole-loop null simulations over the maximum declared search family and confirm the observed false-promotion rate stays within the frozen bound.

This phase can verify the mechanism with synthetic fixtures. Production enablement remains blocked until a representative repository-owned suite establishes margins, repeat count, minimum complete-block count, power, and ledger allocation. If that suite is unavailable, record `INCONCLUSIVE`; do not invent thresholds.

## Revision

`test(evals): freeze the evolution promotion oracle`
