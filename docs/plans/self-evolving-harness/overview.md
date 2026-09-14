# Self-Evolving Harness Plan

This plan adapts the loop in *Self-Harness: Harnesses That Improve Themselves* to Orvek without giving a model authority over Orvek's binary, evaluators, permissions, secrets, or activation rules. The selected design is a bounded, Host-owned campaign that evolves immutable, declarative behavior bundles and promotes them only through replayable evidence and an atomic registry update.

The source PDF was read page by page (31 pages; SHA-256 `eb38452b5d357499ff3a2f7aa8bd7b8c1c1409dbafadaff3624071b2f7b1b302`). Durable findings are recorded in [paper-notes.md](paper-notes.md). The paper's useful core is preserved: evaluate, mine failure mechanisms, propose `K` distinct patches, test each patch, compose compatible winners, and repeat. The plan deliberately strengthens the paper where its experimental method is not sufficient for a production harness.

## Definition of done

The work is complete only when all of these predicates are true:

1. A Store-global evaluation cohort freezes the model identity, protocol, environment image, independent block identities, task identities, repeats, evaluator version, statistical policy, budgets, editable envelope, active-base digest, and three disjoint hiding-and-binding partition commitments before mining begins.
2. Mining sees only sanitized failures from the mining role. The proposer sees those failure facts, bounded hypotheses, pass anchors, and the editable schema; it cannot resolve sealed artifacts or invoke evaluator, registry, Store, credential, source-write, or unrestricted tool APIs. Mining, adaptive-promotion, and final-audit evidence have non-interchangeable types.
3. Every candidate is an immutable, canonical, content-addressed declarative patch against one exact parent revision. Invalid, no-op, duplicate, out-of-envelope, or stale-base candidates are rejected before execution.
4. Parent and candidate run as paired trials with the same case, repeat, independent block, model, protocol, evaluator, and fresh environment. A frozen taxonomy distinguishes `Pass`, attributable `BehavioralFailure`, and `InfrastructureUnknown`; only the last category can make evidence inconclusive.
5. Promotion uses a frozen, versioned block-valid estimator. Repeats are aggregated within cases and correlated cases within declared independent blocks; all candidates, rounds, metrics, strata, composites, fallbacks, campaigns, and activation attempts against one cohort debit its Store-global query/error ledger. Useful-effect, non-inferiority, critical-case, cost, latency, and safety gates all pass.
6. Compatible accepted patches are composed deterministically and the full composite is evaluated again. A failed composite is not subset-searched unless a new multiplicity-controlled design was frozen before looking at its result.
7. A sealed final-audit cohort is consumed at most once globally. Any evaluator access or partial disclosure burns and retires its audit epoch. A new campaign cannot reset its ledger or reuse the cohort; it must commit fresh non-overlapping confirmatory cases. Final-audit results are not returned to mining or proposal.
8. A final certificate binds the lineage, partition commitments, model/evaluator/environment identities, gate policy, paired receipts, evidence root, merge receipt, active-base expectation, and rollback proof.
9. Activation is a Store transaction that compare-and-swaps the active registry pointer. A candidate that verifies but loses the CAS remains `VERIFIED` and `SUPERSEDED`; it is not reported as active.
10. Each session pins one exact harness digest at admission. Promotion changes only later sessions. Rollback is an append-only pointer transition and never mutates a revision or silently changes an in-flight session.
11. Crash/restart tests at every effect boundary show one durable authority, deterministic work identities, lease fencing, no double counting, no retry of an unknown paid intent, conservative budget debit, and projection equality after replay.
12. The TUI and CLI expose coarse start/status/watch/pause/resume/cancel/export/approve-rollout/rollback intents. They cannot execute individual stages, select evidence, supply thresholds, reveal sealed data, or promote a raw revision directly.
13. Repository-owned regression suites, adversarial boundary tests, and a deterministic fake campaign pass before shadow mode; shadow, canary, and stable promotion each have explicit stop and rollback conditions.

No score improvement alone satisfies this definition. A revision with incomplete provenance, a broken hard gate, or an uncertain outcome is not promotable.

## Scope and effort

- Rigor: high. This feature changes the behavior selected for future autonomous sessions.
- Delivery shape: 15 reviewable Conventional Commit revisions, each with its tests in the same revision.
- Expected code surface: roughly 35–45 new or changed Rust/evaluation/documentation files, kept to two or three files per revision where possible; the vertical admission migration is the documented exception.
- Durable owner: the existing `crates/harness` Host and Store. There is no second writer and no independent daemon.
- Initial operator: CLI over the existing typed IPC boundary. The TUI remains a projection and can consume the same status stream later.
- Execution transport: existing Host/Docker primitives. Harbor can be added as an optional transport after it proves the same receipts and isolation contract; it is not a prerequisite.

The main prerequisite is a green, stable Host migration baseline. The current working tree contains substantial unrelated migration work and is not evidence that the baseline compiles. Phase 1 must run on a clean implementation branch or worktree descended from the intended Host lineage, and promotion work must not absorb or overwrite the user's current changes.

## In scope

- Immutable harness manifests and typed editable envelopes.
- A model/protocol/environment/task-profile/channel registry with session pinning.
- Durable Store-global evaluation cohorts and campaign, round, candidate, trial, certificate, activation, monitoring, and rollback state.
- Sanitized failure mining with explicit fact-versus-hypothesis types.
- Fixed-model, `K`-way structured proposal with validation and diversity checks.
- Paired isolated trials, reconciliation, fencing, and evidence receipts.
- Versioned statistics, hard gates, deterministic merge, fresh composite evaluation, and sealed final audit.
- Atomic activation, supersession, monitoring, rollback, replay, and operator controls.

## Explicitly out of scope for version 1

- Candidate-authored edits to Rust, binaries, Cargo dependencies, evaluators, scoring rules, task partitions, permissions, network policy, resource ceilings, provider credentials, system authority, or Store/IPC schemas.
- Model-weight training or fine-tuning.
- An always-on autonomous evolution daemon.
- Promotion from raw external benchmark output without Host-owned receipts.
- A global one-size-fits-all harness. Revisions are selected by exact target profile.
- Candidate access to raw traces, secrets, sealed partition contents, or hidden evaluator diagnostics.
- Post-result threshold changes, optional stopping, stale-patch rebasing, or exploratory subset search presented as confirmation.

Source-change ideas may be exported as human-review artifacts. They have no certificate type and no path to the active registry.

## Paper-to-Orvek mapping

| Paper mechanism | Orvek implementation | Required correction |
| --- | --- | --- |
| Held-in evaluation | Mining partition | Failures only; sanitize before clustering. |
| Exact failure signature `(cause, causal status, mechanism)` | Versioned deterministic cluster signature | Store observations as facts and mechanism labels as hypotheses. |
| `K` parallel proposals | Fixed proposer model with one structured patch tool | Enforce schema, allowlist, budgets, diversity, and no hidden-data access. |
| Held-in plus held-out acceptance | Mining plus adaptive-promotion roles | The paper repeatedly consults “held-out” data, so it is adaptive validation, not final evidence. Debit a Store-global cohort ledger across campaigns. |
| Accept non-negative deltas and one positive delta | Versioned paired gate | Add uncertainty, useful effect, non-inferiority, hard failures, critical strata, cost, latency, drift, and missingness rules. |
| Merge compatible accepted edits | Typed deterministic composition | Re-run the complete merged candidate. Never infer compositional safety from individual results. |
| Iterative state transition | Durable campaign aggregate | Journal every intent, effect receipt, verdict, and pointer transition; make replay and rollback first-class. |
| Final reported benchmark | One-use sealed final audit | Do not feed final outcomes back into adaptation; burn on access and require a fresh non-overlapping cohort after failure. |

The paper demonstrates gains across nine model/domain combinations, but it does not report ablations, equal-compute baselines, complete stopping/cost details, or a production security model. The implementation must therefore treat its algorithm as a useful search pattern, not as sufficient evidence for safe self-modification.

## Selected architecture

```text
CLI / future TUI projection
          |
       typed IPC                 candidates never cross this boundary
          v
+--------------------------- Host ----------------------------+
| EvolutionCoordinator -> TrialTransport -> isolated runtime  |
|          |                    |                |             |
|          v                    v                v             |
|   policy + scorer       effect receipts   verifier output    |
|          |                    |                |             |
|          +--------------------+----------------+             |
|                               v                              |
|          Store: per-aggregate chains + projections          |
|       revisions / campaigns / sealed evidence / registry    |
+-------------------------------|------------------------------+
                                v
                   atomic active-pointer CAS
```

The public Host interface stays deep and coarse:

```rust
host.start_evolution(StartEvolution { target, policy: PolicyId, requested_budget, expected_active }) -> CampaignId
host.evolution_status(CampaignId) -> EvolutionStatus
host.pause_evolution(CampaignId, ExpectedRevision) -> PauseReceipt
host.resume_evolution(CampaignId, ExpectedRevision) -> ResumeReceipt
host.cancel_evolution(CampaignId, ExpectedRevision) -> CancelReceipt
host.approve_rollout(ApproveRollout { campaign, certificate, channel, expected_active })
    -> ActivationReceipt
host.rollback_harness(RollbackHarness { target, prior_activation, reason, expected_active })
    -> ActivationReceipt
```

Stage transitions remain private to `EvolutionCoordinator`. Neither IPC clients nor the proposer can request “run final audit”, “mark verified”, or “activate candidate”.

The central types should make illegal states difficult to express:

```rust
struct HarnessRevision { digest: HarnessDigest, parent: Option<HarnessDigest>, behavior: BehaviorLayer }
struct TargetProfile { model: ModelIdentity, protocol: ProtocolDigest, environment: EnvironmentDigest, tasks: TaskProfileDigest, channel: Channel }
struct EvaluationCohort { id: CohortId, partitions: PartitionCommitments, blocks: BlockManifest, ledger: ErrorLedger }
struct CampaignSpec { cohort: CohortId, policy: PolicyId, envelope: EditableEnvelope, budget: CampaignBudget }
enum EvidenceRef { Public(PublicEvidenceRef), Sealed(SealedEvidenceRef) }
enum Observation { Fact(FailureFact), Hypothesis(MechanismHypothesis) }
enum TrialOutcome { Pass(PassReceipt), BehavioralFailure(FailureReceipt), InfrastructureUnknown(UnknownReceipt) }
enum TrialVerdict { Verified(VerificationCertificate), NotVerified(RejectionReason), Inconclusive(InconclusiveReason) }
enum ActivationState { NotRequested, Active(ActivationReceipt), Superseded { current: HarnessDigest }, RolledBack(ActivationReceipt) }
```

`HarnessRevision` contains only declarative behavior: behavior prompts, text-only skill content, bounded subagent roles/tool subsets, closed-enum recovery reminders, verifier scheduling, and budgets within a compiled hard ceiling. The immutable Host authority layer is assembled separately and cannot be represented in a patch.

See [architecture-synthesis.md](architecture-synthesis.md) for the arena comparison, selected base, and required grafts.

## Alternatives considered

### External Python or Harbor-owned evolution driver

Rejected as the authority. It would duplicate campaign state, weaken same-UID IPC and Store fencing, and make crash recovery span two writers. External evaluators can remain test transports behind Host-owned receipts.

### A new Rust crate or daemon

Deferred. It creates an API and persistence boundary before the domain stabilizes. Begin with a private `crates/harness/src/evolution/` module; extract only when it has a genuinely independent policy and lifecycle.

### Automatic source-code self-modification

Rejected for version 1. Source patches can change the guard that judges the patch, require a compiler/toolchain supply chain, and greatly expand rollback and provenance. Declarative manifests provide useful leverage while keeping authority compiled and reviewable.

### One mining set plus one reusable held-out set

Rejected. Repeated selection against the same held-out set leaks information and overstates confirmation. Use mining, adaptively debited promotion, and a sealed one-use final audit.

## Safety and statistical policy

- Freeze one Host-registered versioned estimator before mining. The first implementation should support bounded paired differences over declared independent blocks; a predeclared binary win/loss and loss-rate gate may be added as a policy variant. Do not silently swap estimators.
- Aggregate repeats within case, then account for correlated cases with a frozen `independence_block_id` or a predeclared cluster-valid method. Neither attempts nor unproven-correlated cases are independent samples.
- Define the multiplicity family across candidates, rounds, protected metrics, critical strata, composite/fallback tests, campaigns, and activation attempts. Use separate Store-global cohort ledgers for adaptive promotion and final audit. Do not spend final-audit outcomes on search or reset either ledger by starting another campaign.
- Require a minimum useful effect on the primary metric and non-inferiority margins on all protected metrics.
- Treat correctness, secrecy, policy, provenance, evaluator integrity, resource ceilings, and critical cases as hard gates.
- Classify a valid evaluator rejection, attributable candidate crash, budget exhaustion, or protocol-defined timeout as `BehavioralFailure`. Treat environment/model/evaluator drift, missing or tampered receipts, transport loss, unknown provider billing, insufficient power, or exhausted error/budget ledger as `InfrastructureUnknown` and therefore `INCONCLUSIVE`.
- Use hiding-and-binding commitments: a sealed random nonce or keyed construction prevents dictionary enumeration of low-entropy task identities. Public status exposes only the commitment, never its opening.
- Persist the policy digest and its human-readable parameters in every verdict and certificate.

Exact margins, error allocation, power, repeat count, and minimum complete-block count must come from a Phase 1 power/noise study over repository-owned fixtures. They must not be guessed in production code or selected after observing candidate outcomes.

## Rollout and stop conditions

1. **Replay-only:** import fixture events, rebuild projections, and compare byte-for-byte state.
2. **Dry-run:** construct and validate candidates but do not execute or activate.
3. **Shadow:** run paired trials and certificates while the active pointer is write-disabled.
4. **Canary:** activate only the `canary` channel for an explicit target profile and session budget.
5. **Stable:** require canary monitoring gates and `approve-rollout` referencing a verified campaign/certificate, bounded channel, and expected active pointer.

Stop a campaign when its wall-clock/token/tool/attempt/query/error-allocation budget is exhausted; when there are no addressable clusters; when an effect cannot be reconciled; when drift invalidates pairing; when all candidates are rejected; when the composite fails; or when the audit is burned without a valid certificate. “No update” is a normal successful campaign outcome.

Monitoring compares the activated revision with its pinned baseline on correctness, policy events, critical failures, resource use, latency, and operator rollback signals. A severe hard-gate breach rolls the target pointer back for future sessions and records the reason. Existing sessions remain pinned unless a separate Host safety mechanism terminates them.

## Revision sequence

1. [Freeze the evaluation contract](phase-01-freeze-evaluation-oracle.md)
2. [Model immutable harness revisions](phase-02-model-harness-revisions.md)
3. [Migrate Store and register baselines](phase-03-migrate-store-and-registry.md)
4. [Seal and account for evolution evidence](phase-04-seal-evolution-evidence.md)
5. [Bind Host sessions to registered revisions](phase-05-bind-sessions-to-harnesses.md)
6. [Migrate admission and session lifecycle callers](phase-06-migrate-session-admission.md)
7. [Journal campaign state](phase-07-journal-campaign-state.md)
8. [Mine addressable failures](phase-08-mine-addressable-failures.md)
9. [Generate bounded proposals](phase-09-propose-bounded-candidates.md)
10. [Run paired isolated trials](phase-10-run-paired-isolated-trials.md)
11. [Score evidence and apply gates](phase-11-score-and-gate.md)
12. [Compose winners and re-test](phase-12-compose-and-retest.md)
13. [Audit, activate, monitor, and roll back](phase-13-activate-and-rollback.md)
14. [Orchestrate and recover campaigns](phase-14-orchestrate-and-recover.md)
15. [Expose bounded operator controls and roll out](phase-15-operate-and-roll-out.md)

The full verification matrix is in [testing.md](testing.md). Decisions and evidence status are in [decision-log.tsv](decision-log.tsv).

## Implementation guidance

- Use `long-horizon` for each revision: retrieve only the touched seam, checkpoint decisions, bound command output, and verify before expanding scope.
- Use `architect` when a phase changes a public signature or state machine. Begin with the caller expression and keep orchestration behind the Host interface.
- Use `how` before introducing a new dependency, service, or abstraction; use `interrogate` before accepting an unexplained assumption about Store, Docker, IPC, Harbor, or provider semantics.
- Run `/deslop` and `unslop` on every revision before review. Remove pass-through helpers, ceremonial types, speculative options, and comments that narrate obvious control flow.
- Maintain one canonical `show-me-your-work` trail for each implementation campaign. Record commands, artifact digests, verdicts, blockers, and why any evidence is inconclusive.
- Use the project verification/control skill when one exists. No project-local `control-cli` skill is currently available, so Phase 1 should create a deterministic scripted operator fixture or explicitly schedule `create-verification-skill` before claiming runtime CLI proof.
- Use `babysit` for long Docker/evaluation runs. A timeout, silence, or transport disconnect is not evidence of completion.
- Apply the repository's regression rule: add a failing regression test first, implement the fix in a child revision, verify it, and squash the fix into the test revision. New features keep tests in their feature revision.
- Every revision description must be a Conventional Commit. Do not mix formatting or unrelated current-worktree changes into these revisions.

## Open decisions that block implementation, not planning

1. Which repository-owned task suite supplies enough independent blocks and critical strata for mining, adaptive promotion, and non-overlapping one-use final cohorts?
2. What power/noise study fixes the initial repeats, useful effect, non-inferiority margins, full-family error allocation, block-valid estimator, and minimum complete-block count?
3. Which exact provider idempotency/lookup fields constitute a billable-intent receipt and an unambiguous `InfrastructureUnknown` for each supported provider?
4. What monitoring volume and elapsed time are sufficient to move a target profile from canary to stable?

The artifact ACL and Store migration are no longer open design questions; Phases 3–4 are prerequisites. Until the remaining questions are answered with fixtures and measurements, the system may verify replay, dry-run, and shadow mechanisms but must keep rollout approval and registry writes disabled.
