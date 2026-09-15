# Phase 5: Bind Host Sessions to Registered Harnesses

[Back to overview](overview.md)

## Goal

Resolve a target-specific registered harness in the Host and pin its exact digest and assembled admission profile for the lifetime of each session.

## Hypothesis and exit predicate

**Hypothesis:** session admission can bind registered behavior without changing current isolation or allowing a client to inject privileged instruction text.

Exit only when Host-derived `SessionAdmissionProfile` is the only privileged assembly input; registry misses use an explicit recorded baseline policy; two concurrently admitted sessions remain pinned across pointer transitions; and the session record proves authority, request, behavior, model, protocol, environment, task-profile, and channel identities.

## Changes

| File | Change and reason |
| --- | --- |
| `crates/harness/src/evolution/registry.rs` | Resolve an exact `TargetProfile` and channel to one registered revision and return an immutable `HarnessBinding`; keep mutations behind Store certificates. |
| `crates/harness/src/session.rs` | Replace freely mutable privileged instructions with Host-derived `SessionAdmissionProfile` and persist the binding. Model/profile changes require a new session. |
| `crates/harness/src/controller.rs` | Resolve and assemble authority, request, and behavior at admission, then pass only the immutable profile to execution. |

The Host keeps current request content separate from immutable authority and selected behavior. It resolves once; provider retries, compaction, handoff, and continued turns reuse the pinned binding instead of consulting the current pointer.

## Static verification

```sh
cargo test -p orvek-harness --test controller_execution
cargo test -p orvek-harness --test task_admission
cargo check --all-features
just check-fmt
just clippy
```

Audit for mutable `SessionConfig.instructions`, re-resolution during a turn, and registry access below admission. Keep a temporary legacy constructor only if it is crate-private, explicitly marked for Phase 6 removal, and cannot be reached from IPC.

## Runtime verification

Admit A on R1, activate R2, admit B, roll back to R1, and admit C. A stays on R1, B stays on R2, and C uses R1. Attempt an unregistered digest, arbitrary authority text, and a mid-session model/profile change; all fail before provider execution and leave no partially admitted session.

## Revision

`feat(harness): pin sessions to registered revisions`
