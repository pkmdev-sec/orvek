# Context compaction

Provider compaction is the default. SnapCompact is an optional strategy that replaces older tool
text with PNG pages. Original text stays in a local archive for exact retrieval. Model recall and
total cost savings remain unverified.

## Enable bitmap compaction

Use this source checkout and add:

```toml
[agent.compaction]
strategy = "snapcompact"
profile = "openai-8x16-experimental-v1"
fallback = "stop"
input_budget_tokens = 272000
max_generated_pages = 64
max_request_bytes = 33554432
```

The experimental profile covers Sol, Terra, and Luna. Custom endpoints are rejected. No profile
qualifies for `validated-auto` yet. Evaluations never run automatically.

Settings are saved with the session. `--compaction provider` or `--compaction snapcompact` overrides
the saved strategy; `ORVEK_COMPACTION` is the environment equivalent. SnapCompact still needs an
explicit experimental profile.

## Request behavior

Only older successful function/custom-tool text results are candidates. The following stay native:

- Instructions, user/assistant messages, tool calls and arguments.
- Existing media, opaque reasoning, failed results, and pending calls.
- The newest two provider-response groups.

Each result retains its role, call ID, status, order, and media. Page labels identify the source
item, content block, and byte range. Historical tool content remains data, not new instructions.
After replacement, the next request sends the full projected history without provider continuation.

The renderer preserves printable ASCII, spaces, tabs, LF, and CRLF. Unsupported characters keep
the entire result native. It uses the bundled 8x13 font on an 8x16 grid, monochrome PNGs, original
image detail, and a 1,568-pixel maximum edge. It does not clip pages or substitute unknown glyphs.
Repeated compaction reuses unchanged pages and renders new ones from source, never from OCR.

Automatic compaction starts at 90% of the configured input budget. Installation requires at least
10% estimated reduction and a result within 70% of the budget. Estimates include retained text,
images, preprocessing, and overhead. These policy limits are not measured savings. Native-heavy
sessions may not meet them.

## Exact retrieval

The agent's `read_context` tool accepts an item locator, optional content index, byte offset,
literal search, and output limit. It returns `next_offset` when more text remains.

- `source = "model_visible"` returns text previously shown to the model.
- `source = "original"` returns text before Nanocodex's ordinary output truncation.

The archive cannot recover data discarded inside a tool or before a legacy snapshot was saved.
Imported results lack a verified success record and stay native. New successful results can be
compacted. Each agent can read its branch and inherited fork prefix; fresh child agents cannot
read their parent's archive.

## Persistence

Sources, pages, and manifests live in `<config-dir>/sessions/v2.sqlite3`. Schema 3 migrates schema 2
without changing existing records. Sources and pages have SHA-256 checks and size limits.

Pages are durable before replacement. Completed turns publish normal checkpoints. Manual idle
compaction and initial forks save before reporting completion. A crash resumes the last successful
boundary and never re-executes unfinished tools automatically.

Each resume/fork gets a new archive branch with a fixed parent cutoff. Later or unfinished records
outside that cutoff stay excluded. Legacy upgrades validate the old instructions/tool definitions
and retain the previous checkpoint as a backup on first publication.

## Recovery

To restore native text while retaining retrieval, including when PNGs are damaged but source and
manifest integrity remain valid:

```sh
orvek --compaction provider --resume SESSION_ID
```

If source or manifest data is damaged and a pre-SnapCompact backup exists, close the session and run:

```sh
orvek context restore-backup SESSION_ID
orvek --compaction provider --resume SESSION_ID
```

Backup recovery checks the hash, preserves the replaced checkpoint, and is safe to repeat. Sessions
started in SnapCompact have no pre-SnapCompact backup; restore a user-managed database backup if
their source or manifest is damaged.

Rendering errors, cancellation, stale candidates, and insufficient reduction retain the previous
checkpoint. `fallback = "provider"` permits provider compaction from restored native text only if
that request fits the budget. It never drops pages to force a fit.

Archives have no automatic garbage collection. A branch scope permits at most 100,000 source
records and 256 MiB of source representations. Exceeding limits stops writes instead of deleting
history. Back up related forks and ancestor records together.

## Privacy and checks

Database, WAL, snapshot, and backup files contain unredacted data and are not encrypted at rest.
Application-owned archive/render/retrieval buffers zeroize on drop. SDK, parser, codec, and SQLite
copies are outside that guarantee.

The terminal reports page counts and estimated token changes. Choose **Compact context** while
idle; Escape cancels. SnapCompact headless runs keep normalized JSONL events and omit raw transport
payloads that can duplicate images.

```sh
cargo test --bin orvek core::compaction
cargo test --bin orvek sessions::archive
```

These offline tests check behavior, not model reading accuracy. See the
[evaluation protocol](../evals/snapcompact/README.md) for quality and total-cost comparisons.
The two vendored Nanocodex extensions need a compatible release before standalone crate publication.
See [vendor provenance](../vendor/README.md) and the [SnapCompact reference](https://stencil.so/blog/snapcompact).
