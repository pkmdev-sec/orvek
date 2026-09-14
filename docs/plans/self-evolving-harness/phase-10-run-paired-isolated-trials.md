# Phase 10: Run Paired Isolated Trials

[Back to overview](overview.md)

## Goal

Execute parent and candidate under identical case/repeat/block conditions in fresh isolated environments and produce provenance-complete paired receipts with honest recovery guarantees.

## Hypothesis and exit predicate

**Hypothesis:** the native Host transport can either durably start/inspect/collect/fence a trial or terminate it as unknown without duplicating an unreconciled external effect.

Exit only when usable pairs match every frozen identity; candidates have no Host/Store/secret/sealed access; valid evaluator failures, attributable crashes, budget exhaustion, and protocol timeouts become `BehavioralFailure`; infrastructure uncertainty becomes `InfrastructureUnknown`; and crash tests never turn an unknown paid attempt into an automatic retry.

## Changes

| File | Change and reason |
| --- | --- |
| `crates/harness/src/evolution/trial.rs` | Define paired work keys, independent blocks, randomized order, outcome classification, authenticated/hash-bound receipts, transport capabilities, reconciliation, and fencing. |
| `crates/harness/src/controller/evolution.rs` | Implement native execution with fresh workspaces and hard ceilings. Add durable start/inspect/collect/fence where the runtime supports it; otherwise fence crash-after-start as terminal unknown. |
| `crates/harness/tests/evolution_trials.rs` | Test identity, isolation, classification, scheduling, transport capability levels, crash recovery, drift, quota, and tampering. |

`TrialKey` binds campaign, candidate, role, partition epoch, block, case, repeat, side, model, protocol, evaluator, environment, limits, and input commitment. Two compatible terminal receipts form paired case evidence. Harbor may later implement this transport contract but never owns campaign state or evidence truth.

## Static verification

```sh
cargo test -p orvek-harness --test evolution_trials
cargo test -p orvek-harness --test docker_execution
cargo test -p orvek-harness --test workspace_tools
cargo check --all-features
just check-fmt
just clippy
```

Audit mounts, networks, environment, IPC sockets, credentials, artifact handles, cleanup, and logs. Document each transport's reconciliation strength; do not claim recoverable completion or single billing when its API cannot prove it.

## Runtime verification

Run a paired fixture with randomized side order and stable keys. Kill before launch, after launch, after model completion, after evaluator output, and around receipt commit. Reconcile by durable runtime identity when possible; otherwise fence and debit conservatively. Mutate model/image/protocol/evaluator on one side and require `INCONCLUSIVE`. Cause a verified candidate error, budget exhaustion, and protocol timeout and require negative behavioral evidence.

## Revision

`feat(harness): run paired evolution trials`
