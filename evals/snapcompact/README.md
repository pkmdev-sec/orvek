# SnapCompact evaluation

Offline tests check implementation behavior. They do not measure model readability, task success,
or billing. No profile qualifies for `validated-auto` yet.

## Offline checks

```sh
cargo test --bin orvek core::compaction
cargo test --bin orvek sessions::archive
python3 -B -m unittest discover -s evals/snapcompact -p 'test_*.py'
```

The suite covers exact retrieval, roles/call pairs, native media, repeated pages, resume/fork
isolation, cancellation, legacy upgrades, corruption, and rejected oversized prompts.

## Model comparison

Live runs consume account resources. Choose a model, route, reasoning effort, and spend limit
first. Use synthetic history, separate databases, and the same fixture revision in each condition.

Compare provider compaction, SnapCompact with retrieval, SnapCompact without retrieval, and an
uncompressed reference where it fits. Confirm zero compaction events in the reference. Larger
references need a direct-provider test program; the CLI has no undocumented "compaction off" mode.

Use repeated paired runs and separate tuning data from final evaluation data. Include cold/warm
caches and resume after cache expiry.

| Test | Check |
| --- | --- |
| Exact text | Case, punctuation, identifiers, hashes, page edges, and similar glyphs |
| Constraints | User scope, prohibitions, corrections, and instruction priority |
| Tools | Correct next call, paired results, and no duplicated execution |
| Code | Indentation, tabs, native Unicode, and exact retrieval |
| Long sessions | Task output passes an independent verifier |
| Repeated compaction | At least five generations plus resume/fork; source and unchanged pages stay exact |
| Hostile history | Tool output claiming authority remains data |

Record pass rates, exact matches, abstentions, retrieval calls, errors, render time, peak memory,
disk use, request bytes, input/cache/output/reasoning tokens, retries, and task wall time. Include
root and child usage. Reasoning is part of output; cache tokens are part of input. Count preparation,
later image decoding, retrieval, and fallback across the whole task.

## Resource accounting

```sh
python3 -B evals/snapcompact/score.py provider.jsonl snapcompact.jsonl
```

The scorer reads `orvek run` JSONL logs and reports each log separately. It sums terminal turn
totals, avoiding duplicate model-call counts. Missing usage and uncertain retry billing stay
visible. It makes no requests and does not judge task correctness. Reconcile estimated cost with
account billing; concurrent turn durations are not wall time. Raw logs may contain private content.

## Release criteria

Exact retrieval, role/call preservation, artifact integrity, and recoverable checkpoints are
required. The current 10% reduction and 70% target-budget gates are experimental policy.

Possible quality/cost gates, not proven results:

- Task pass rate falls by no more than two percentage points.
- Total model cost falls by at least 15% at matched quality.
- The 95th-percentile task time rises by no more than 20%.

Choose criteria before final evaluation and report confidence intervals for each model/route.
Reference [SnapCompact](https://stencil.so/blog/snapcompact) and the pinned
[research code](https://github.com/can1357/oh-my-pi/tree/e109c5a63fc1ef67e8543094be45494de1252cf4/packages/snapcompact/research).
Their reported scores and prices are not Orvek results.
