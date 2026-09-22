# Host-owned context views

Orvek keeps one authoritative conversation history in the durable host. It derives a `ContextView` for each inference request. The TUI displays diagnostics, but it does not render, select, or persist context representations.

## Request structure

Each request has two ordered regions:

1. **Stable prefix.** Instructions, tools, routing controls, and settled history segments. An unchanged prefix has the same bytes and `PromptCacheIdentity` after a live append or resume.
2. **Live suffix.** The newest user input, active tool calls, pending results, and other mutable work.

Settled successful tool output can have two representations:

- `NativeText` sends the original provider protocol item.
- `Bitmap` sends host-rendered PNG pages inside the original `function_call_output`. The original `call_id` stays unchanged.

The append-only session journal remains authoritative. A projection does not delete, summarize, or rewrite history. `ContextProjected` records only a derived view and its deterministic source manifest.

When history exceeds a bounded request, Orvek divides active work into deterministic cache epochs at complete item boundaries. Each epoch reserves an append allowance derived from the configured maximum model output, fills the remaining space with the newest complete history suffix, and then appends current work without rewriting the prompt. A function call and its output stay in the same unit. Crossing the append allowance starts a new epoch: its first request establishes a new prefix, and later requests append to that prefix. Omission notices are fixed for the epoch, and all omitted records remain available through `read_context`. This avoids retaining older settled history by evicting newer context.

## Selection and fallback

The host renders eligible settled output after its first native use. It stores immutable pages and exact source bytes in the artifact store. Later requests can reuse those artifacts after appends, resume, or restart.

Representation selection uses comparable provider receipts for the active model. A comparison requires the same source history, controls, other segment choices, output tokens, and reasoning tokens. Missing or conflicting measurements select native text. A render, artifact, or transport failure also restores native text for only the affected segment.

Orvek reserves up to 32,768 tokens from the configured provider window for output, then bounds projected history conservatively at one serialized byte per remaining input token. Small windows reserve at most half their capacity for output. Context optimization does not add a spend limit, call limit, or execution stop. A provider size rejection can retry an undispatched request from the same authoritative history with native text.

Configure the provider window for new sessions:

```toml
[agent]
context_window_tokens = 1000000
```

The supported range is 16,384 through 1,000,000 tokens. The default is 1,000,000. Legacy `[agent.compaction]` provider settings still map `input_budget_tokens` to this value.

## Exact retrieval

Bitmap pages are an input representation, not a replacement for text. The `read_context` host tool reads exact text from authorized history without rerunning the original tool. Text mode accepts an item index, content index, byte offset, byte limit, and optional literal search. It returns base64 for every byte range and also returns text when the selected bytes are valid UTF-8.

Branch authorization follows the session ancestry cutoff. A fork can read its inherited prefix and its own records. It cannot read later records from its parent or unrelated branches.

## Persistence and recovery

Store schema v9 persists deterministic context manifests and artifact references. Opening a schema-v8 store replays its verified append-only journals and rebuilds derived schema-v9 state. Legacy projection records remain readable, but new writes require complete manifests.

On restart, the host verifies journal hashes, source cursors, history digests, renderer identity, source archives, and page artifact digests. A stale or corrupt derived view is discarded. Authoritative history remains available and the request continues with native text.

Historical SQLite imports preserve unknown tables as opaque data. Keep a user-managed copy when vendor-private legacy archives matter.

See [Context cost boundaries](design/context-cost.md) for receipt accounting and [Detached harness host](harness-host.md) for reconnect behavior.
