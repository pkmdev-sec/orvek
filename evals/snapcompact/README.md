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
  --tool-access-id native-workspace-profile-v1 \
  --environment-id pinned-runtime-image-or-host-manifest-digest \
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

## Raw records and failure accounting (schema v2)

The runner creates an append-only JSONL file. It refuses to overwrite an existing output or log.
Use new output and log paths for each batch. Keep all batches, including failures.

Each process invocation has an `attempt_id` and two snapshots:

- `attempt_revision: 0`, `attempt_status: "running"`: flushed and synced before launch.
- `attempt_revision: 1`: the final result, written before an execution error returns.

The report resolves snapshots by attempt identity. A missing final snapshot counts as interrupted,
not as a missing trial. An incomplete final JSON line is reported in `input_issues`; the earlier
admission remains in the denominator. Malformed complete lines and conflicting snapshots are errors.
Do not compare reports while a batch is still running.

Final `attempt_status` values are:

| Status | Meaning |
| --- | --- |
| `completed` | Execution settled and the independent evaluation ran. `task_passed` can still be false. |
| `failed` | A process launch, nonzero exit, or terminal submission failed. |
| `interrupted` | Cancellation, signal termination, or an admission with no final snapshot. |
| `invalid_evaluation` | Missing/mismatched terminal receipt, incomplete journal view, malformed log, or evaluator error. |

`exit_code`, `submission_status`, and diagnostic codes in `issues` preserve the distinction.
The runner continues independent conditions after an execution failure, then exits nonzero.
A runner interrupt stops the batch. It does not create records for generations never attempted.
CLI argument and input-identity validation occurs before attempts are admitted.

`task_passed` and `exact_match` are null when evaluation could not run. A headless
`finished_unverified` receipt is a settled native execution, not sandbox certification. The exact
answer or AST check can pass without making that stronger claim.

### Unknown measurements

All numeric measurement fields accept null. A measured zero remains zero. A missing/null token
component in any call makes that component's aggregate unknown. Partial logs cannot supply complete
usage or cost totals. Duplicate journal sequences are counted once; conflicting duplicates invalidate
the view. Receipt totals require a receipt for every recorded usage call, with no unknown receipt.

The raw fields include:

- `usage.root` and `usage.child`: input, cached-input, output, and reasoning tokens;
- `provider_receipt_usd`: recorded root and child provider costs, not a catalog fallback;
- `root_catalog_estimate_usd`: a root-only estimate, null if its pricing inputs are unknown;
- `catalog_estimate_usd`: total root and child estimate, null when child usage is unavailable;
- retrieval count/failures, render time, request bytes, latency, peak memory, and retries.

Current headless telemetry does not provide attributable child usage, reliable retry counts, or
retrieval-failure totals. The runner leaves these null. It does not infer retries by subtracting
usage-event counts from cost-event counts: child calls also emit costs. Unattributed calls make root
usage totals unknown. Render time, request bytes, and peak memory also remain null.

`child_outcomes` preserves observed `spawn_agent`, `wait_agent`, and `list_agents` results. A recorded
child failure adds `failed_child` to `issues` and contributes to the failed-child-attempt denominator.
It does not automatically fail a parent that recovered and passed the independent check. An empty
list does not prove no children ran; the headless stream does not expose all child lifecycle events.

### Pair identity and denominators

Pair identity includes `dataset`, `fixture_revision`, `task_digest`, `model`, `settings_digest`,
`harness_build`, `tool_access_digest`, `environment_digest`, `cache_condition`, `generation`,
`branch`, and `run`. Only `condition` differs (`native` or `bitmap`).

The runner hashes the fixture and task prompt, binary bytes, and model/thinking/configuration/endpoint
inputs. Supply pinned tool-access and runtime identities with `--tool-access-id` and
`--environment-id`. These two identities are caller declarations, not measured host telemetry.
Keep their manifests beside the records. The reader checks equality, not whether the declarations
are true. Pin provider-side model versions and external runtime state separately.

Unmatched records stay in condition summaries. Multiple attempts with the same pair key are
ambiguous; the reader keeps them all but does not select a winning retry. Use distinct `run` values
for planned repetitions. The report lists unmatched attempt IDs and counts.

Each condition reports separate completed, failed, interrupted, and invalid-evaluation counts, plus
attempts with observed failed children and tasks evaluated, passed, failed, or unscored. Task pass rate uses **all
attempts** as its denominator; unknown evaluation is not a success. Metric coverage counts show
which token fields and provider receipts were measured. A total is null if any contributing value
is unknown. Paired deltas also stay null when a matched pair lacks the metric; their measured-pair
count remains visible. Cost per completed task is **all attempt costs divided by successful tasks**,
including failed-attempt costs. It is null if any cost is missing or no task passed.

Generate the report:

```sh
python3 -B evals/snapcompact/paired_report.py raw-records.jsonl --output paired-report.json
```

The report version is 2. It reports task/exact-match Wilson intervals and normal-approximation
paired mean intervals. Repeated resume generations are correlated; these intervals are not proof
of independent-trial significance. Keep the raw records, report, logs, command transcript, build,
fixture, provider profile, and environment/tool manifests together.

### Historical data

Version 1 and unversioned raw records are rejected. They contain unsupported zero-filled values and
may omit failed attempts. Do not migrate them by adding a version number or treating their zeros as
measurements. Reparse original logs and execution receipts where available; otherwise rerun the
trials. Missing controls or attempt evidence cannot be reconstructed from the old aggregate.
The archived `results/2026-09-19/` files remain unchanged historical artifacts, not v2 evidence.

`score.py` reads the separate legacy `run.completed`/`run.failed` event format. Its version-2 output
also keeps missing/null usage, duration, and billing uncertainty unknown. It does not convert these
logs into matched schema-v2 trials. Sparse warmup maps no longer imply zero token components.

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

The 2026-09-19 partial live run is in `results/2026-09-19/`. Its legacy accounting and incomplete attempt coverage prevent release qualification. It also lacks sandbox fork and operational measurements. Do not claim savings from the paper or from offline tests. Reference [SnapCompact](https://stencil.so/blog/snapcompact) and the pinned [research code](https://github.com/can1357/oh-my-pi/tree/e109c5a63fc1ef67e8543094be45494de1252cf4/packages/snapcompact/research) only as external prior work.
