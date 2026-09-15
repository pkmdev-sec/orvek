# Phase 8: Mine Addressable Failures

[Back to overview](overview.md)

## Goal

Turn only mining-role failures into deterministic, sanitized evidence bundles that separate observed facts from mechanism hypotheses and retain passing anchors.

## Hypothesis and exit predicate

**Hypothesis:** stable failure signatures can identify actionable behavior mechanisms without exposing secrets, benchmark answers, adaptive/final evidence, or evaluator authority.

Exit only when cluster membership and rank are invariant to order; representatives reference immutable sanitized mining evidence; hypotheses cannot masquerade as verifier facts; pass anchors are present; and adversarial trace text remains structured untrusted data with no instruction or tool authority.

## Changes

| File | Change and reason |
| --- | --- |
| `crates/harness/src/evolution/evidence.rs` | Define mining-only facts, redaction provenance, pass anchors, public evidence roots, and bounded mechanism hypotheses. Keep adaptive and final evidence types inaccessible. |
| `crates/harness/src/evolution/mining.rs` | Implement versioned signatures `(terminal_cause, causal_status, abstract_mechanism)`, deterministic clustering/ranking, representative selection, and bundle limits. |
| `crates/harness/tests/evolution_mining.rs` | Test determinism, provenance, redaction, fact/hypothesis separation, noninterference, untrusted text, support ranking, and empty outcomes. |

The signature follows the paper, but every field records whether it came from a verifier receipt or a bounded classifier. Mining proposes no edit. Raw adaptive-promotion and final-audit results, reasons, membership, and handles are absent from the process address space used for proposal input.

## Static verification

```sh
cargo test -p orvek-harness --test evolution_mining
cargo test -p orvek-harness --test protected_verification
cargo check --all-features
just check-fmt
just clippy
```

Review every proposer-bound string as quoted, capped, normalized, untrusted data. Do not claim arbitrary model text has been neutralized; prove instead that it has no authority channel and that any tool-like syntax is encoded as data.

## Runtime verification

Mine shuffled fixtures under different worker schedules and compare canonical bundle bytes and roots. Seed credentials, terminal escapes, instruction-like text, fake tool calls, hidden answers, and oversized traces; require deterministic redaction/rejection provenance. Instrument artifact access and prove no adaptive/final handle is requested.

## Revision

`feat(harness): mine addressable failure evidence`
