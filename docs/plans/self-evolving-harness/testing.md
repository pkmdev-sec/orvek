# Verification Strategy

[Back to overview](overview.md)

The proof obligation is stronger than a score increase. It must establish artifact identity, role-separated evidence, block-valid inference, global adaptive accounting, isolated execution, honest recovery, and an authorized atomic pointer transition.

Results use `VERIFIED`, `NOT_VERIFIED`, or `INCONCLUSIVE`. A valid evaluator rejection, attributable candidate crash, budget exhaustion, or protocol-defined timeout is negative `BehavioralFailure`. Missing/tampered receipts, drift, transport loss, an unreconciled external attempt, exhausted evidence allocation, or insufficient power is `InfrastructureUnknown` and therefore cannot verify.

## Verification layers

| Layer | Primary proof | Failure prevented |
| --- | --- | --- |
| Types and schema | Compile/API checks, canonical fixtures, forbidden-field tests | Candidate changes authority or one evidence role is used as another |
| Store migration and ACL | Version-1 fixture migration, replay roots, capability and quota tests | Existing history loss, sealed-data disclosure, resettable ledgers |
| Pure domain logic | Unit/property/metamorphic tests | Nondeterministic mining, composition, scoring, transitions |
| Isolated execution | Docker/session integration tests and receipts | Unpaired runs, stale workspaces, secret/socket access, outcome laundering |
| Statistical cross-check | Rust/reference vectors and whole-loop simulation | Pseudoreplication, sign errors, optional stopping, multiplicity inflation |
| Recovery | Effect-specific kill matrix and projection replay | Duplicate unknown effects, false completion, corrupt activation |
| Operator journey | Scripted CLI restart/approval/rollback scenario | Client-side policy, misleading status, unrecoverable operation |
| Rollout | Shadow/canary monitoring with explicit gates | Valid offline evidence causing an unsafe stable deployment |

## Required test matrix

### Evaluation contract and evidence roles

- `MiningEvaluation`, `AdaptivePromotionDataset`, and `FinalAuditDataset` are distinct types with no implicit or generic conversion path.
- Mining data cannot satisfy useful-effect gates. Adaptive and final raw results, reasons, memberships, and handles never enter proposal input for the retired cohort.
- The Store-global cohort freezes model, protocol, evaluator, environment, cases, repeats, `independence_block_id`, partitions, policy, budgets, and the complete multiplicity family.
- Query/error allocation covers candidates, rounds, metrics, strata, composites, predeclared fallbacks, campaigns, and activation attempts.
- Starting a campaign cannot reset ledgers. Accessing any final-audit case burns the epoch globally. A failed audit requires prospectively committed non-overlapping confirmatory cases.
- Partition commitments are hiding and binding: sealed nonce/key openings or a keyed construction prevent enumeration of low-entropy membership.
- The outcome taxonomy classifies known candidate failures as negative evidence and reserves unknown for facts the Host cannot establish.

### Manifest and authority boundary

- Canonical serialization and digest are invariant to representational ordering.
- Parent, policy, envelope, and behavior identities are mandatory.
- Unknown fields and source, dependency, evaluator, partition, model, provider, permission, network, Store, IPC, secret, and registry paths fail closed.
- Text-only fields reject path traversal, oversized content, terminal escapes, unsupported encodings, and executable forms.
- Numeric/set budgets cannot exceed compiled Host ceilings.
- Candidate text is untrusted data with no authority channel; tests do not claim arbitrary prompt text is neutralized.
- Secret wrappers remain non-cloneable, non-displayable, non-serializable, redacted in `Debug`, and zeroized on drop.

### Store migration, registry, and artifacts

- A real version-1 Store fixture migrates to version 2 transactionally; old task/session projections and per-aggregate roots remain exact.
- Rebuilt event-kind constraints accept only known kinds. Campaign IDs retain the event table's UUID-compatible representation.
- Registry keys exact model, protocol, environment, task profile, and channel; baseline revisions come from Host-owned behavior.
- Registry miss/fallback is explicit. Activation requires a certificate and expected-active CAS.
- Raw artifact `put/read/path` and `Store::artifacts` are inaccessible outside trusted crate code.
- Public and sealed reads require distinct typed handles and exact purpose. Generic journal/watch/status/export/raw-artifact paths cannot expose sealed bytes or a resolvable handle.
- Artifact reservations enforce a Host-global quota; staged writes survive or cleanly roll back; orphan GC is deterministic and cannot collect referenced content.
- Public pages have explicit byte/item ceilings below the 8 MiB IPC frame.

### Session admission and binding

- `SessionAdmissionProfile` is assembled by Host from immutable authority, request, selected behavior, and protocol identities.
- External IPC/TUI/import callers cannot provide privileged instruction text or an unregistered revision.
- Create, import, resume, compaction, fork, handoff, and retry preserve the pinned digest.
- A session admitted before activation, concurrent CAS, or rollback remains pinned. A later session sees the new pointer.
- Model/protocol/profile settings cannot mutate a bound session; they require a new admission.
- The durable Host owns this flow with Harbor absent.

### Campaign journal and crash recovery

- Stored projection equals replayed projection for every campaign event prefix.
- Event-chain validation follows the existing kind/aggregate/revision chain, not one global event root.
- Invalid transitions, bad revisions, corruption, missing artifacts, bad length/hash, and cohort mismatch fail closed.
- Cohort ledger check, debit, and verdict use commit atomically and remain visible to every campaign.
- Deterministic effect keys survive restart; lease epochs fence stale workers.
- Kill after intent, external start, response, evaluator output, artifact stage, receipt, verdict, audit access, approval, and pointer commit.
- Native Docker uses durable start/inspect/collect/fence where implemented. If crash-after-start cannot be reconciled, it becomes fenced `InfrastructureUnknown` with conservative debit and no retry.
- Provider intents use lookup/idempotency when supported; otherwise unknown result is terminal for that intent. The system does not claim external execution or payment occurred exactly once.
- Store/artifact budget exhaustion yields a durable negative or inconclusive outcome according to the frozen taxonomy without losing lineage.

### Mining and proposal

- Cluster signature/rank are deterministic under input and scheduling permutations.
- Verifier facts and classifier hypotheses are separate; representatives reference immutable sanitized mining evidence.
- Pass anchors protect already-correct behavior.
- Credentials, hidden answers, tool-like syntax, terminal escapes, and oversized traces are redacted/rejected with provenance and remain untrusted data.
- `K` attempts use the frozen target model, protocol, parent, mining root, and Host-registered policy.
- Structured output is the only candidate path. Invalid, no-op, duplicate, stale, over-budget, and out-of-envelope patches fail before trial.
- Candidate identity is deterministic despite completion order. Unknown provider intent is not blindly retried.

### Paired trials and isolation

- Parent/candidate match case, repeat, block, input, model, protocol, evaluator, image, limits, role, and partition epoch.
- Each side has a fresh workspace/container; randomized/interleaved order does not change identity.
- Candidate has no Host socket, Store path, credential, sealed handle, shared writable memory, or undeclared network route.
- Receipts are authenticated or hash-bound; tampering is detected.
- Attributable candidate crash, budget exhaustion, protocol timeout, and evaluator rejection become `BehavioralFailure`.
- Drift, missing/tampered receipt, transport loss, and unreconciled external attempt become `InfrastructureUnknown`.
- Repeats aggregate within case before cases enter a frozen block-valid method.

### Statistics and gates

- Rust matches the independent oracle for canonical output, intervals, bounds, gate names, equality, policy digest, and exact global-ledger debits.
- Input order, candidate label, repeat order, and within-block case order do not change the result.
- Simulations cover null effect over the complete coordinator search family, known positive effect, protected regression, correlation, high variance, sparse strata, equality, minimum blocks, missingness, and ledger exhaustion.
- Empirical false-promotion and power estimates are recorded for the frozen policy.
- Useful effect, protected non-inferiority, correctness, critical cases, policy, provenance, secrecy, cost, latency, and resource ceilings are independent gates.
- Any non-verified hard gate prevents `VERIFIED`; mining or final datasets cannot be passed to adaptive scoring.

### Composition

- Disjoint operations compose canonically; conflicts and combined ceilings fail closed.
- A composite gets fresh paired trials, an adaptive dataset, and ledger allocation. It cannot inherit child evidence.
- Fixture: A passes, B passes, A+B fails. The composite remains inactive.
- No subset search occurs after the result unless the whole subset policy and its multiplicity allocation were frozen before the campaign.

### Final audit, activation, monitoring, and rollback

- Hidden final cases and raw reports cannot be accessed before the audit transition.
- Any evaluator access burns the global audit epoch before bytes are returned, including access followed by crash.
- Final results and failure reasons never feed mining/proposal for that cohort.
- The certificate binds lineage, cohort/partition commitments, block manifest, identities, policy, receipts, exact campaign/cohort aggregate roots, ledger debits, merge, audit, expected base, and rollback target.
- Approval references a verified campaign, certificate, bounded channel, and expected pointer; it cannot nominate a raw revision.
- Two campaigns racing a pointer yield one activation; the loser is `VERIFIED`/`SUPERSEDED`.
- Rollback references a prior activation or last-known-good receipt. It appends a transition and does not mutate history or in-flight bindings.

### Coordinator, Host lifecycle, and IPC

- Every aggregate state has a total next-action or terminal mapping; repeated `advance` at one revision is idempotent.
- Startup performs existing interrupted/submission recovery plus evolution reconciliation before new work.
- Evolution uses the existing Host-wide run semaphore and participates in `shutdown_if_idle`; no second scheduler appears.
- Wall-clock, token, tool, attempt, candidate, round, query/error, disk, and concurrency budgets are monotonic.
- Pause/resume/cancel preserve effect ownership. Status distinguishes pending, running, reconciling, paused, terminal, verified, superseded, active, and rolled back.
- IPC enforces same UID, byte/item/page ceilings, expected revisions, and redaction.
- Canonical coarse methods are start/status/watch/pause/resume/cancel/export/approve-rollout/rollback. No private stage is externally callable.

## Runtime journeys

### Deterministic fake campaign

Use fake model, transport, evaluator, clock, Store, cohort, and cases:

```text
start -> mining -> K proposals -> individual paired trials -> adaptive gates
      -> deterministic composition -> fresh composite trials
      -> burn final audit -> certificate -> operator approval -> pointer CAS
      -> monitor -> rollback
```

Repeat with a kill at every intent/effect/receipt/transaction boundary. Resumed and uninterrupted runs must have equivalent normalized lineage, per-aggregate roots, budgets, verdict, certificate, and pointer receipt. Transport-dependent external receipts may differ only where the declared recovery contract records `InfrastructureUnknown`.

### Native isolated smoke test

Run one repository-owned case through native Host/Docker in shadow mode. Verify fresh workspaces, no Host socket/secret mounts, matching identities, outcome classification, evaluator receipt, cleanup, and no registry mutation. Harbor is optional and must pass the same contract before use.

### Operator journey

Drive `orvek evolve` through start/status/watch/pause/restart/resume/cancel/export, terminal verification, approve-rollout, canary activation, monitoring breach, and rollback. Assert redaction, paging, exit codes, stale handling, certificate/activation receipts, active pointer, and pinned-session proof. The client never reconstructs a score or activates a digest directly.

## Phase gates

| Gate | Required evidence | Registry writes |
| --- | --- | --- |
| Mechanism/replay | Store migration, replay equality, schema, ACL, quota, deterministic fake campaign | Disabled in code |
| Dry-run | Manifest, binding, mining, proposal, and state-machine tests | Disabled in code |
| Shadow | Paired transport, scorer cross-check, global-ledger simulation, kill matrix, certificate | Disabled by policy/capability |
| Canary | Owned calibrated suite, one target profile, fixed session budget, monitoring and rollback drill | Canary only |
| Stable | Representative power study, provider reconciliation, sealed ACL proof, clean canary window, operator approval, disaster recovery | Exact stable target only |

Synthetic fixtures can verify mechanisms; they do not enable production. Any missing owned suite, power calibration, provider recovery proof, ACL proof, or canary rule keeps activation `INCONCLUSIVE` and write-disabled.

## Command gate

Run from the repository root after each relevant revision with bounded captured output:

```sh
cargo check --all-features
just check-fmt
just clippy
just test
```

Integration targets must use exact invocations such as `cargo test -p orvek-harness --test controller_execution`; a name used only as a filter may run zero intended tests. Each implementation checkpoint also runs the named target with `-- --list` and fails if it contains no tests. Also run each phase-specific command. A docs-only planning revision requires `git diff --check`, link/file validation, and structural checks; it does not claim Rust test evidence.

No project-local control skill currently proves CLI behavior. Before the final runtime claim, create one or commit a deterministic user-visible verification script and identify the proof used.
