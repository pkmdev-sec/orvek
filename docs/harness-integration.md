# Harness replacement integration plan

Orvek's replacement harness is being prepared separately. Integration must preserve the current
Orvek terminal experience while moving durable execution authority to a detached host.

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

## Import sequence

1. **Lock the replacement snapshot.** Require a clean replacement commit, protocol and schema
   versions, executor build identity, full test evidence, and an explicit list of unfinished or
   removed behavior. Do not copy a moving working tree.
2. **Import the runtime.** Add the harness and executor crates under Orvek naming, wire workspace
   dependencies and packaging, and verify them independently before touching the frontend.
3. **Add host infrastructure.** Port the detached host process, authenticated IPC, bounded framing,
   configuration identity, startup diagnostics, and reconnect protocol.
4. **Add frontend adapters.** Add host projection, submission, artifact, child, review, auxiliary,
   shell, and legacy-history adapters beside the current components. Do not replace approved TUI
   layouts in this increment.
5. **Bridge effects to commands.** Map current submit, queue, settings, session, fork, resume,
   cancel, reflection, handoff, review, shell, memory, and child effects to host commands while
   preserving current interaction semantics.
6. **Replace state ownership.** Remove UI-owned turn scheduling and persistence only after journal
   rehydration, submission retries, and queue ownership are proven.
7. **Finish product adapters.** Wire memory, MCP, children, skills, configured web/image tools,
   hooks, review assets, shell, recent prompts, legacy sessions, and context projection, or disable
   each unsupported path with a precise diagnostic.
8. **Remove the legacy graph.** After every entry point uses the host, remove Nanocodex packages,
   vendor patches, old worker/storage/journal loops, and execution-facing subagent dependencies.
9. **Verify as one product.** Run repository checks, native host/executor suites, PTY and headless
   parity, adversarial cases, packaging checks, dependency audits, and TUI benchmarks on the final
   integrated candidate.

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
watcher coverage, queue/image-edit attacks, shell recovery, review feedback, legacy import, memory
and child adapters, executor native suites, feature/target dependency audits, source-archive
packaging, benchmarks, and the replacement plan's full adversarial campaign.
