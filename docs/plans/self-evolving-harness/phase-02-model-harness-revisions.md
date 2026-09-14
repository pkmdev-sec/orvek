# Phase 2: Model Immutable Harness Revisions

[Back to overview](overview.md)

## Goal

Define the only artifact an autonomous campaign may change: a canonical, content-addressed behavior manifest bounded by a compiled editable envelope.

## Hypothesis and exit predicate

**Hypothesis:** useful harness adaptations can be represented without source edits or changes to authority, evaluators, secrets, tools, permissions, or hard resource ceilings.

Exit only when canonical round trips are stable, semantically identical manifests have the same digest, every forbidden field/path fails closed, secret-bearing traits are absent, and no type conversion exists from a source suggestion to a promotable revision.

## Changes

| File | Change and reason |
| --- | --- |
| `crates/harness/src/evolution/mod.rs` | Introduce a private evolution namespace. Expose one narrow crate-private facade to Host/session admission; keep stage and storage internals private. |
| `crates/harness/src/evolution/harness.rs` | Define `HarnessRevision`, `BehaviorLayer`, `EditableEnvelope`, `ManifestPatch`, canonical encoding, digesting, parent binding, validation, and compiled ceilings. |
| `crates/harness/src/lib.rs` | Wire the module with the narrowest visibility required by existing crate callers. |
| `crates/harness/tests/evolution_manifest.rs` | Exercise the public facade and fail-closed schema without depending on private implementation details. |

Data structures: immutable authority plus immutable request plus one `BehaviorLayer`; a patch can only produce `ValidatedHarnessRevision` through envelope validation. Host-owned `PolicyId` selects a registered policy; callers and candidates cannot construct policy values or ceilings ad hoc.

Initial editable fields are behavior instructions, text-only skill bodies, closed-enum recovery reminders, bounded subagent roles/tool subsets, verifier scheduling, and resource budgets no greater than Host ceilings. Model/provider identity, credentials, tool implementation, permissions, network, evaluator, registry, Store, IPC, and source are not representable.

## Static verification

```sh
cargo test -p orvek-harness --test evolution_manifest
cargo check --all-features
just check-fmt
just clippy
```

Add compile-fail or API-level tests where the type system should prevent authority fields. Use redacted `Debug`; do not add `Clone`, `Display`, or serialization to any secret wrapper.

## Runtime verification

Load equivalent manifests with reordered map-like inputs and prove identical canonical bytes and digest. Fuzz or table-test every JSON/patch path just outside its numeric/string/set bound. Attempt path traversal, scripts in text-only fields, an unknown tool, a larger budget, a model change, and an evaluator field; all must be rejected before persistence. Confirm the facade exposes validation and identity operations but no direct Store, evaluator, provider, registry-mutation, or activation capability.

## Revision

`feat(harness): model content-addressed harness revisions`
