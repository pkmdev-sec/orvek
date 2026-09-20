# Paired context-representation evaluation

Offline tests prove protocol and recovery behavior. They do not prove model readability, quality, or savings. A model profile qualifies for automatic bitmap selection only after comparable provider receipts exist for that model and source segment.

## Test assets

- `fixtures/history.txt` contains six deterministic generations. It covers exact identifiers, hashes, punctuation, tabs, repeated glyphs, page boundaries, and hostile text that claims authority.
- `fixtures/manifest.json` pins the fixture digest and required checks.
- `paired_report.py` validates raw paired records and reports rates, resource totals, and 95% confidence intervals.
- `score.py` summarizes legacy Orvek JSONL logs without making provider requests.

Verify the offline artifacts:

```sh
python3 -B -m unittest discover -s evals/snapcompact -p 'test_*.py'
sha256sum evals/snapcompact/fixtures/history.txt
```

The expected fixture digest is recorded in `fixtures/manifest.json`.

## Live paired procedure

Live runs use provider resources. Run conditions serially so one condition cannot warm the other condition's cache or compete for memory.

For each of Sol, Terra, and Luna:

1. Pin the Orvek revision, fixture revision, model settings, reasoning settings, instructions, tools, and verifier.
2. Create isolated host state for the native and bitmap conditions. Do not reuse a database across conditions.
3. Set `ORVEK_EVAL_CONTEXT_REPRESENTATION=native` for the native host and `ORVEK_EVAL_CONTEXT_REPRESENTATION=bitmap` for the bitmap host. This override exists only to form evaluation pairs. Production selection remains receipt-driven.
4. Submit generation 0. Make the agent read the corresponding fixture block through a tool and preserve the successful call/result pair.
5. Resume the same session for generations 1 through 5. Add one settled tool result per generation. Verify the prior generation before adding the next one.
6. Fork after generation 5. Ask the parent and fork the same exact question. Verify that each branch sees only its authorized history cutoff.
7. Run a cold-cache pair in isolated state. Run a warm-cache pair by repeating an unchanged stable prefix after the provider cache is populated.
8. Repeat complete pairs. Keep every failed, retried, or incomplete run in the raw data.

The serial runner covers the six resume generations, can add an independently verified coding artifact, and preserves every raw Orvek log:

```sh
python3 -B evals/snapcompact/run_paired.py \
  --binary target/release/orvek \
  --config /path/to/config.toml \
  --auth-file /path/to/auth.json \
  --api-base-url https://provider.example/v1 \
  --output raw-records.jsonl \
  --logs raw-logs \
  --coding-check
```

The runner does not implement the fork case. Run it in sandbox mode with the same pair identity and append its records before producing a complete release report. Host-mode tasks are intentionally unverified and cannot supply the settled source checkpoint required by a fork.

Use an independent verifier. It must check:

- exact case, identifiers, hashes, punctuation, whitespace, tabs, and page-edge sentinels;
- the next tool name, arguments, and preserved `call_id` pairing;
- that hostile tool output remains data rather than instructions;
- the final coding artifact and its tests;
- resume and fork authorization.

Do not tune on final evaluation runs.

## Raw record format

Write one JSON object per condition and paired run. A pair must have identical values for `model`, `fixture_revision`, `settings_digest`, `cache_condition`, `generation`, `branch`, and `run`. Use `condition` to distinguish `native` from `bitmap`.

Each record contains:

- `task_passed` and `exact_match`;
- root and child input, cached-input, output, and reasoning tokens;
- provider receipt and catalog estimate as separate fields;
- retrieval count and failures;
- render time, serialized request bytes, latency, peak memory, and retries.

Provider receipts are authoritative. Preserve `null` when the provider does not report a value. Do not replace an unknown receipt with zero or a catalog estimate.

Generate the report:

```sh
python3 -B evals/snapcompact/paired_report.py raw-records.jsonl   --output paired-report.json
```

The report sums root and child work, reports task and exact-match Wilson intervals, and reports paired bitmap-minus-native mean intervals. Keep `raw-records.jsonl`, the report, command transcript, Orvek revision, fixture digest, and provider profile together.

## Interpretation

Report these values per model and condition:

- task pass rate and exact-match rate;
- input, cached-input, output, and reasoning tokens;
- provider receipts and catalog estimates;
- retrieval frequency and failures;
- render time, request bytes, latency, peak memory, and retries;
- total cost per completed task.

Prompt-cache savings and representation savings are different effects. A warm-cache result does not prove that bitmap representation is cheaper. A representation estimate is not a billed total.

Call out these comparison targets, but do not enforce them as runtime stops:

- task pass-rate loss is no more than two percentage points;
- total model cost is at least 15% lower at matched quality;
- p95 task time is no more than 20% higher.

The 2026-09-19 partial live run is in `results/2026-09-19/`. It covers resume and independent coding verification but not sandbox fork or three operational measurements, so it is not a complete release qualification. Do not claim savings from the paper or from offline tests. Reference [SnapCompact](https://stencil.so/blog/snapcompact) and the pinned [research code](https://github.com/can1357/oh-my-pi/tree/e109c5a63fc1ef67e8543094be45494de1252cf4/packages/snapcompact/research) only as external prior work.
