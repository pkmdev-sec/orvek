# Phase 15: Expose Bounded Operations and Roll Out

[Back to overview](overview.md)

## Goal

Give operators a clear CLI workflow and advance through replay-only, dry-run, shadow, canary, and stable modes without moving authority or policy into the client.

## Hypothesis and exit predicate

**Hypothesis:** operators can control campaigns and rollout from redacted durable status while the Host remains the only scorer and pointer writer.

Exit only when the CLI implements the canonical coarse API; stale requests fail safely; the scripted user journey proves restart/approval/activation/monitoring/rollback; and production registry writes remain disabled until repository-owned suite, power, provider-recovery, ACL, and canary gates are committed.

## Changes

| File | Change and reason |
| --- | --- |
| `bin/orvek/src/app/cli/evolve.rs` | Add start/status/watch/pause/resume/cancel/export/approve-rollout/rollback handling and human/JSON renderers over typed IPC. |
| `bin/orvek/src/app/cli.rs` | Register bounded arguments. Start selects target, registered policy, requested budgets, and expected active; approval and rollback use proof receipts, not raw revision activation. |
| `docs/harness-host.md` | Document authority and information-flow boundaries, modes, prerequisites, evidence interpretation, stop conditions, monitoring, rollback, disaster recovery, and Harbor as optional transport. |

The TUI remains a projection and may consume the same Host status stream. It does not wait for Harbor and never reconstructs policy or privileged instructions client-side.

## Static verification

```sh
cargo test -p orvek --all-features
cargo test -p orvek-harness --test operator_protocol
cargo check --all-features
just check-fmt
just clippy
just test
```

Check help, JSON schema, paging, exit codes, redaction snapshots, and stale revisions. No flag accepts an evaluator result, threshold override, hidden partition, caller verdict, direct active digest, or private-stage command.

## Runtime verification

Drive start, status, watch, pause, restart, resume, cancel, export, terminal verification, approve-rollout, canary activation, monitoring breach, and rollback. Capture command, exit status, cohort/campaign roots, certificate, activation receipt, active pointer, and pinned-session proof.

No project-local control skill currently proves this journey. Before claiming runtime completion, create one with `create-verification-skill` or commit an equivalent deterministic operator script. Use `babysit` for long trials; unchanged status is normal and a timeout is not completion.

Mechanism completion and production enablement are separate gates. Stable remains disabled until the owned evaluation suite and partition construction, calibrated power/error policy, provider/transport reconciliation, sealed-artifact ACL tests, clean canary window, rollback drill, and operator approval are all `VERIFIED`.

## Revision

`feat(cli): operate bounded harness evolution`
