# Harness replacement integration plan

Orvek's replacement harness is integrated behind a detached host. This document records the
resulting boundary and the checks required to keep the terminal from regaining execution authority.

## Integration rules

- The TUI is a client. It does not own coding-task completion, verification, authoritative queue
  order, durable state, provider dispatch, or child execution.
- The detached host owns task contracts, submissions, journal state, inference attempts, jobs,
  evidence, completion, delivery, and recovery.
- A model turn, tool output, browser review, auxiliary report, or child JSON result is not task
  completion evidence by itself.
- Terminal disconnect, reconnect, or a slow watcher cannot cancel or complete durable work.
- Legacy imports are inert history; historical success text is not native verification evidence.
- Unknown billing and job outcomes remain unknown. Retries use the same durable request ID.
- Secret-owning types stay non-`Clone`, non-`Display`, non-serializing, redacted, and zeroized.

## Integrated shape

1. `app::host` owns the authenticated process boundary, configuration identity, reconnect policy,
   and durable host startup.
2. `core::ConfiguredSession` creates, resumes, and imports sessions through that boundary.
3. `tui::client` translates UI effects into host commands and detaches watches on close; it never
   shuts down accepted host work as a side effect of closing the terminal.
4. `tui::host_projection` derives disposable presentation changes from history, durable journal
   records, and explicitly speculative previews.
5. `app::headless` uses the same session, acknowledgement, watch, and settlement contracts for
   `orvek run`.
6. The legacy Nanocodex worker, transcript database authority, orchestration loop, and direct
   execution-facing subagent graph are absent from the Orvek binary.

The detached-process boundary and its rationale are documented in
[Detached harness host](harness-host.md).

## Frontend contract

Submissions carry UUID request IDs, immutable original intents, effective intents, initial and
edited input digests, explicit queue/steer scheduling, and host-owned ordering. The UI retains a
draft until acknowledgement and retries an uncertain acknowledgement with the exact same request.
Queue editing, movement, promotion, and cancellation use expected input digests.

Journal records are authoritative; provider deltas are disposable previews. A preview gap discards
partial text and waits for settled journal state. Task status renders phase, outcome, scope
revision, unresolved obligations, evidence freshness, unknown jobs, budgets, blockers, and
exceptions without presenting incomplete outcomes as complete.

Review uses frozen host artifacts and records feedback separately from editable composer markdown.
Human shell uses the manual-job path, keeps output as an artifact, and treats command exit and
source adoption as separate facts. Informational requests must not create or mutate coding-task
scope; action requests continue an incomplete task when valid or create a new task.

## Final checks

```sh
cargo check --all-features
just check-fmt
just clippy
just test
python3 scripts/check-docs.py
python3 scripts/check-source-tree.py
```

Additional release gates include fake-host frontend tests, PTY/headless parity, reconnect and slow
watcher coverage, queue/image-edit attacks, review feedback, legacy import, memory and child
adapters, executor native suites, feature/target dependency audits, source-archive packaging, and
benchmarks.
