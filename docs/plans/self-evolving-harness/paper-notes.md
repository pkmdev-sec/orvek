# Paper Notes: Self-Harness

[Back to overview](overview.md)

## Source and reading method

- Title: *Self-Harness: Harnesses That Improve Themselves*
- Authors: Hangfan Zhang et al.
- Length: 31 pages
- Source digest: SHA-256 `eb38452b5d357499ff3a2f7aa8bd7b8c1c1409dbafadaff3624071b2f7b1b302`
- Review method: page-by-page text extraction plus visual inspection of figures, tables, algorithms, appendices, and example harness patches.

The digest is the durable source identity. The PDF itself is not copied into the repository.

## The paper's loop

The paper fixes the underlying model and evaluator, then improves the harness around the model:

1. Evaluate the current harness on held-in and held-out tasks.
2. Mine held-in failures with a deterministic signature containing terminal verifier cause, causal status, and an abstract failure mechanism.
3. Generate `K` minimal, evidence-based harness patches in parallel.
4. Evaluate every patch under the same protocol.
5. Accept a patch when its held-in and held-out deltas are non-negative and at least one is positive.
6. Merge compatible accepted patches and continue from the merged harness.
7. Keep the current harness when no patch passes.

The examples show small changes to prompts, tool guidance, verification behavior, recovery instructions, and task decomposition. They support a narrow declarative edit envelope; they do not justify autonomous changes to the evaluator, runtime authority, permissions, or source code.

## Reported results

The paper reports improvement in all nine model/domain combinations:

| Domain | Model | Baseline | Self-Harness | Delta |
| --- | --- | ---: | ---: | ---: |
| Terminal | MiniMax | 42.2 | 53.9 | +11.7 |
| Terminal | Qwen | 18.0 | 36.7 | +18.7 |
| Terminal | GLM | 46.1 | 57.0 | +10.9 |
| SWE | MiniMax | 46.0 | 52.5 | +6.5 |
| SWE | Qwen | 19.5 | 41.5 | +22.0 |
| SWE | GLM | 52.0 | 55.5 | +3.5 |
| AppWorld | MiniMax | 48.6 | 58.9 | +10.3 |
| AppWorld | Qwen | 22.5 | 52.2 | +29.7 |
| AppWorld | GLM | 44.4 | 85.0 | +40.6 |

These results establish that harness search can be useful. They do not establish that every reported gain is statistically stable or safe to deploy.

## Gaps that Orvek must correct

1. The repeatedly consulted held-out set is adaptive validation, not an untouched final test. Orvek needs mining, adaptive-promotion, and one-use final-audit roles with non-interchangeable types.
2. Repeated selection across candidates and rounds creates one multiplicity family. Orvek must account for candidates, rounds, metrics, strata, composites, fallbacks, campaigns, and activation attempts against a Store-global evaluation cohort.
3. Attempt-level samples can be correlated. Orvek must aggregate repeats within a case and use declared independent blocks or a frozen cluster-valid method.
4. A simple non-negative delta is not enough. Orvek needs useful-effect, protected non-inferiority, critical-case, correctness, safety, cost, latency, provenance, and resource gates with explicit uncertainty.
5. A merged patch needs a complete fresh evaluation. A failed merge must not trigger post-result subset search unless that search family was predeclared.
6. The paper does not specify production-grade crash recovery, artifact secrecy, authorization, activation CAS, rollback, or session pinning. Orvek must add those mechanisms.
7. The paper does not report enough ablations, equal-compute baselines, stopping details, or full cost accounting to choose production thresholds. Those values require a repository-owned power/noise study.
8. Repeatedly starting a new campaign must not reset validation history. Cohort ledgers and audit consumption are global, durable, and non-resettable.

## Translation rule

Orvek adopts the paper's hypothesis loop, not its deployment policy. A model may propose a typed manifest patch. Only compiled Host policy may decide what evidence is visible, run trials, score results, consume an audit, issue a certificate, or move an active pointer.
