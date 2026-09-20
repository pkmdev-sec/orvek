# Paired live evaluation — 2026-09-19 (legacy, unqualified)

These unversioned records and the version-1 report predate honest failure/unknown accounting.
They may omit failed attempts and contain unsupported zeroes. The current reader rejects them.
The files remain unchanged for provenance; the values below describe the old report, not
verified schema-v2 measurements or evidence of superiority. Requalification requires original
execution evidence and matched controls, or new trials.

This run compared forced native-text and bitmap context for Sol, Terra, and Luna. Each condition used an isolated host, cache, and workspace. The fixture revision is `f05dbeedccb77e4eccba7418a63edbf9b426fbb56f0570622d327521f939f59b`.

Each condition contains six exact reconstruction turns and one coding turn. Before the coding turn, the runner deletes `history.txt` and `emit.py`. The model must reconstruct exact IDs, hashes, punctuation, and decoded indentation in `candidate.py`. An independent AST verifier checks literal data without executing model-written code.

## Historical report output

| Model | Native cost | Bitmap cost | Cost change | Exact matches | Bitmap p95 latency change |
|---|---:|---:|---:|---:|---:|
| Luna | $0.07011552 | $0.06349966 | -9.4% | 7/7 | +4.4% |
| Terra | $0.81741680 | $0.51449520 | -37.1% | 7/7 | -32.9% |
| Sol | $1.31406800 | $0.98333920 | -25.2% | 7/7 | +19.1% |

The old report includes 42 passing turns and a provider cost for each included turn. It does not establish the full attempt denominator. Its zero retrieval-failure, retry, and child-usage fields are not trustworthy measurements. `records.jsonl` and `report.json` preserve that historical output.

The old report placed Terra and Sol above the 15% cost target at its reported quality. Luna did not: its six exact turns were 19.4% cheaper with bitmap context, but its independently verified coding turn cost slightly more under bitmap, reducing the combined saving to 9.4%. The old report placed all profiles within its quality and latency targets. Missing attempts can change those conclusions. Seven related generations are also not independent trials; the saved intervals do not resolve that limitation.

## Unmeasured and blocked cases

Render time, serialized request bytes, and peak memory were not instrumented and remain `null`; they were not replaced with zero. Root usage and the claim that no children ran require rechecking against original logs. The runner hardcoded child usage to zero; those values cannot prove recorded absence. Rendering work is represented by selected representation and page counts.

Host-mode tasks end `finished_unverified`. Their workspaces intentionally retain a pending task instead of asserting a verification certificate. Native forks therefore reject the parent with `parent workspace has no settled source checkpoint`. Sandbox-mode fork evaluation requires Docker, which was unavailable on this machine. Work item 9 remains incomplete until fork and missing operational measurements are covered.
