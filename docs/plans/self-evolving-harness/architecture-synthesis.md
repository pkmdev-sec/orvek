# Architecture Synthesis

[Back to overview](overview.md)

Three independent designs were produced from the same paper and repository evidence, then blind cross-judged. Their decision-relevant findings are preserved here; ephemeral runner artifacts are not required to interpret or implement this plan.

## Scorecard

| Candidate | Paper + corrections | Host/Store authority | Types/replay/recovery | Least privilege | Promotion/rollback | Repository fit | Total |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 2 | 5 | 5 | 5 | 5 | 5 | 5 | 30 |
| 1 | 5 | 5 | 5 | 5 | 5 | 4 | 29 |
| 3 | 5 | 5 | 5 | 4 | 5 | 3 | 27 |

Candidate 2 is the base. It best balances a deep Host boundary, single Store writer, typed digests, deterministic work keys, effect leases, paired statistics, incremental phases, and repository-level tests.

Candidate 1 contributes:

- One explicit Store event/hash/replay authority rather than a competing evolution journal; each aggregate retains its own kind/revision/hash chain.
- `PublicEvidenceRef` versus `SealedEvidenceRef`, including visibility checks on read-by-digest APIs.
- A complete final-audit certificate and a rule that stale active pointers require a fresh campaign, never patch rebasing.
- A predeclared case-level binary win/loss policy option and distinct adaptive/final error-allocation ledgers.

Candidate 3 contributes:

- `ActivationState` distinct from verification verdict: a verified branch can be superseded without ever being reported active.
- Deterministic trial transport fencing and burn-on-access audit epochs.
- Evidence-root, lineage, and rollback-proof fields in certificates.
- No subset search after a failed merge unless its multiplicity policy was frozen in advance.

## Corrections made during synthesis

- Candidate 3 referred to a likely nonexistent `crates/harness/src/harbor.rs` and carried an ambiguous source-delta field. Neither appears in the selected design. All implementation anchors must be rechecked against the current tree.
- Evolution events extend the Store's existing durable authority and replay model. A dedicated projection/table is acceptable; a second journal or writer is not.
- All hidden partition payloads, raw evaluator reports, and hidden receipts use sealed typed references. A commitment is not a security boundary if arbitrary code can resolve its digest.
- Activation requires a committed pointer CAS. `VERIFIED`, lineage acceptance, activation, monitoring, supersession, and rollback are separate states.
- A final-audit epoch is consumed on any evaluator access or disclosure, including access before a crash. It cannot be retried as if unseen.
- Statistical estimator, equality behavior, repeats, case aggregation, missingness, drift handling, and both error ledgers are frozen before mining.
- Harbor remains an optional `TrialTransport`, never durable authority or independent evidence.

## Caller-first shape

The CLI asks the Host for an outcome, not a stage:

```rust
let campaign = client.start_evolution(StartEvolution {
    target,
    policy: policy_id,
    requested_budget,
    expected_active,
})?;

client.watch_evolution(campaign.id)?;
client.cancel_evolution(campaign.id, expected_campaign_revision)?;
client.approve_rollout(ApproveRollout {
    campaign: campaign.id,
    certificate,
    channel,
    expected_active,
})?;
client.rollback_harness(RollbackHarness {
    target,
    prior_activation,
    expected_active: current_revision,
    reason,
})?;
```

Internally, the Host owns a private coordinator:

```rust
impl EvolutionCoordinator {
    async fn advance(&self, campaign: CampaignId) -> Result<AdvanceOutcome, EvolutionError>;
    async fn reconcile(&self, effect: EffectId) -> Result<EffectReceipt, EvolutionError>;
}
```

`advance` is idempotent at the durable state revision. It selects the next permitted transition, records intent, performs or reconciles the effect, records its receipt, and commits the next state. It does not recursively run an unbounded campaign.

## Interface-depth comparison

| Design | Public concepts | Hidden complexity | Assessment |
| --- | --- | --- | --- |
| Public stage API | mine/propose/run/score/merge/audit/activate | Little; clients coordinate policy | Rejected: shallow interface and easy invariant bypass. |
| External evolution daemon | campaign API plus cross-service state and reconciliation | Split across daemon and Host | Rejected: duplicates lifecycle and durable authority. |
| Host-owned campaign aggregate | start/status/pause/resume/export/rollback | Stages, leases, evidence ACLs, statistics, CAS, replay | Selected: deepest boundary and smallest authority surface. |

## Design red-flag screen

- No boolean pile: campaign, verdict, activation, and effect states are enums with transition-specific data.
- No stringly typed identity: model, protocol, environment, task profile, policy, evidence, revision, and certificate use distinct digest/newtype roles.
- No pass-through service layer: the coordinator owns orchestration; Store owns transactional invariants; trial transport owns effects and reconciliation.
- No hidden control flow from models: candidates return a typed patch only. Models cannot call stage or activation operations.
- No premature distribution: one Host and one Store writer remain authoritative.
- No source-evolution escape hatch: declarative manifests and human-only suggestions are separate types with no conversion.
- No verifier leakage: sealed evidence cannot be resolved through generic artifact APIs.
- No statistical pseudoreplication: attempts aggregate inside case and correlated cases aggregate or adjust inside declared independence blocks before inference.
- No optimistic recovery: unknown effects are reconciled or fenced; they are not silently retried.

## Selected module boundary

Begin privately under `crates/harness/src/evolution/`:

```text
evolution/
  harness.rs       immutable manifests, envelope, canonical digest
  registry.rs      target profile, channel, active pointer, session binding
  campaign.rs      aggregate state and events
  evidence.rs      public/sealed references, facts, hypotheses, receipts
  mining.rs        deterministic signatures and ranked evidence bundles
  proposal.rs      structured patch validation and diversity
  trial.rs         paired work identities, transport, reconciliation
  statistics.rs    versioned block-valid estimator and cohort ledgers
  composition.rs   typed compatibility and fresh composite plan
  promotion.rs     certificate, final audit, activation, monitoring, rollback
  coordinator.rs   private idempotent transition driver
```

Module names are a planning sketch, not permission to create all files at once. Each phase should introduce only the boundary it can verify. If two adjacent modules remain small and always change together after Phase 12, merge them before release.

## Final architectural decision

Orvek self-evolves by selecting among immutable behavior manifests under compiled Host authority. Store aggregates record campaign, cohort, registry, and activation transitions through the existing per-aggregate event-chain authority. Isolated trials produce paired, block-valid evidence. A frozen policy produces a tri-state verdict. A globally one-use final audit produces a certificate. Only the Host can atomically move a target-specific active pointer, and every session pins the selected digest. That is the smallest design that preserves the paper's leverage while making the transition testable, reversible, and safe enough to operate.
