# Context projection and legacy compaction data

The durable host owns context projection. It applies provider-compatible projection automatically
when required and records the completed projection in the session journal. The TUI displays the
configured window, the automatic projection threshold, completed projections, and available usage
diagnostics; it does not own or run projection.

Set the model window for new sessions in the agent configuration:

```toml
[agent]
context_window_tokens = 1000000
```

The supported range is 16,384 through 1,000,000 tokens, with a default of 272,000. Automatic
projection begins at 85% of the configured window. The host currently converts that threshold to a
request byte budget using four bytes per token, so it is a conservative estimate rather than the
provider's exact tokenizer count.

Existing provider configurations remain compatible: `[agent.compaction]` with
`strategy = "provider"` maps `input_budget_tokens` to the model window. New effective
configuration output uses `agent.context_window_tokens`. The former terminal-owned SnapCompact
renderer remains unsupported because it would create a second context authority.

## Persistence and recovery

Native host sessions keep authoritative history under `<config-directory>/host/v1`. Closing the
terminal detaches the client and does not stop accepted work. Reconnect or resume reconstructs the
visible transcript from host history and durable journal records.

Historical SQLite sessions can be imported through the host. Unknown legacy tables, including old
compaction archives, are preserved as opaque data during import; Orvek does not claim it can decode
or restore vendor-private archive formats. Keep a user-managed copy of the legacy database when
such archives matter.

The host records completed projection facts, but current provider events do not expose exact
post-projection token counts. The diagnostics panel reports those values as unavailable rather than
estimating them.

For the execution and reconnect boundary, see [Detached harness host](harness-host.md).
