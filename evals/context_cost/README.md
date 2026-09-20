# Context-cost analysis

`recent-usage-fixture.json` is a sanitized, deterministic version of the expensive context shape found in the durable host database. It contains no prompt, tool-output, workspace, or user content. The fixture keeps only model settings, history size, request identity, usage, receipt, and aggregate tool-output size.

Run the fixture report:

```sh
python3 scripts/analyze-context-cost.py evals/context_cost/recent-usage-fixture.json
```

Run the same report against the local durable host database:

```sh
python3 scripts/analyze-context-cost.py ~/.orvek/host/v1/v1.sqlite3
```

The database command opens SQLite in read-only mode. It prints no session content.

## Formulas

For each call, let `I` be input tokens, `C` be cached input tokens, `O` be output tokens, and let `r_i`, `r_c`, and `r_o` be the catalog rates per token.

```text
catalog estimate = (I - C) * r_i + C * r_c + O * r_o
no-cache catalog = I * r_i + O * r_o
all-cached catalog = I * r_c + O * r_o
prompt-cache savings = no-cache catalog - catalog estimate
```

`provider_receipts` sums only one unambiguous reported cost per call. A provider receipt overrides catalog arithmetic. A null or conflicting receipt remains unknown. The catalog estimate remains visible as an estimate.

The `input_divided_by_3_7_with_unchanged_output` field is a counterfactual based on Stencil's 3.7x carried-context result. It is not a billed value or an Orvek savings claim.

## Fixture behavior

The fixture models an append-only, code-heavy session with repeated tool calls and high cache reuse. Its final history is 1,817,848 serialized bytes. The last call reports 1,316,154 input tokens under a configured 1,000,000-token window. The old 3.4 MB byte trigger does not fire, so the fixture has no `context_projected` event.
