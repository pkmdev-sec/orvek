# Paired live evaluation — 2026-09-19

This run compared forced native-text and bitmap context for Sol, Terra, and Luna. Each condition used an isolated host, cache, and workspace. The fixture revision is `f05dbeedccb77e4eccba7418a63edbf9b426fbb56f0570622d327521f939f59b`.

Each condition contains six exact reconstruction turns and one coding turn. Before the coding turn, the runner deletes `history.txt` and `emit.py`. The model must reconstruct exact IDs, hashes, punctuation, and decoded indentation in `candidate.py`. An independent AST verifier checks literal data without executing model-written code.

## Results

| Model | Native cost | Bitmap cost | Cost change | Exact matches | Bitmap p95 latency change |
|---|---:|---:|---:|---:|---:|
| Luna | $0.07011552 | $0.06349966 | -9.4% | 7/7 | +4.4% |
| Terra | $0.81741680 | $0.51449520 | -37.1% | 7/7 | -32.9% |
| Sol | $1.31406800 | $0.98333920 | -25.2% | 7/7 | +19.1% |

All 42 evaluated turns passed. Provider receipts were available for every turn. There were no retrievals, retrieval failures, or retries. `records.jsonl` preserves per-run measurements. `report.json` contains aggregate values and 95% intervals.

Terra and Sol passed the 15% cost target at matched observed quality. Luna did not: its six exact turns were 19.4% cheaper with bitmap context, but its independently verified coding turn cost slightly more under bitmap, reducing the combined saving to 9.4%. All profiles stayed within the two-point quality target and the 20% p95 latency target in this seven-pair run. Seven related generations are not enough for a strong statistical claim; confidence intervals remain authoritative.

## Unmeasured and blocked cases

Render time, serialized request bytes, and peak memory were not instrumented and remain `null`; they were not replaced with zero. Root usage is measured. Child usage is zero because these tasks created no subagents. Rendering work is represented by selected representation and page counts.

Host-mode tasks end `finished_unverified`. Their workspaces intentionally retain a pending task instead of asserting a verification certificate. Native forks therefore reject the parent with `parent workspace has no settled source checkpoint`. Sandbox-mode fork evaluation requires Docker, which was unavailable on this machine. Work item 9 remains incomplete until fork and missing operational measurements are covered.
