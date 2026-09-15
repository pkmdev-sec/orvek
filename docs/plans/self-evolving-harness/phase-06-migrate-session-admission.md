# Phase 6: Migrate Admission and Session Lifecycle Callers

[Back to overview](overview.md)

## Goal

Remove client-side privileged instruction assembly from every interactive and imported session path so the durable Host is the sole session authority, independently of Harbor.

## Hypothesis and exit predicate

**Hypothesis:** IPC, TUI/core, import, fork, handoff, settings, and resume flows can express user request and target intent without constructing or mutating Host authority.

Exit only when no external request contains arbitrary privileged instructions; imports preserve a verifiable binding or map explicitly to the compiled baseline; fork/handoff preserve the pinned digest; settings cannot mutate model/profile on a bound session; and all compatibility shims from Phase 5 are removed.

## Changes

| File | Change and reason |
| --- | --- |
| `crates/harness/src/ipc.rs` | Replace raw instruction-bearing session admission with bounded target/request intents and return the resolved binding identity. |
| `bin/orvek/src/core/mod.rs` | Stop assembling Host authority in the interactive client; send user request and target selection only, then render the Host-resolved binding. |
| `crates/harness/src/import.rs` | Validate imported binding/provenance, preserve it for continuation, or explicitly admit a new baseline-bound session when legacy provenance is absent. |
| `crates/harness/tests/evolution_binding.rs` | Cover create, import, resume, fork, handoff, compaction, settings, concurrent pointer change, and forged-instruction rejection. |

If adjacent CLI/config/skill-loading modules must change to complete the vertical migration, include them in this revision and list them in its implementation checkpoint. This is the deliberate phase-size exception: leaving one authority-producing caller behind is worse than a mechanically larger migration.

## Static verification

```sh
cargo test -p orvek-harness --test evolution_binding
cargo test -p orvek-harness --test operator_protocol
cargo test -p orvek-harness --test session_journal
cargo test -p orvek --all-features
cargo check --all-features
just check-fmt
just clippy
```

Use repository-wide search to account for every construction and mutation of session instructions, model, provider, and target profile. Each remaining instruction string must be user/request content, immutable compiled authority, registered behavior, or a documented test fixture.

## Runtime verification

Drive the interactive client through create, turn, compaction, fork, handoff, restart, and import while changing the active pointer concurrently. Every continuation keeps its original digest. A settings change that would alter model/protocol/profile offers or creates a new session instead of mutating the old one. Repeat with Harbor absent to prove it is not a prerequisite.

## Revision

`refactor(orvek): centralize session admission in host`
