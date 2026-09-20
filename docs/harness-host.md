# Detached harness host

## Problem

Orvek's TUI and headless command are clients of a detached local host. Closing a terminal detaches
the presentation while accepted work and authoritative state remain owned by the host.

## Usage

Normal clients connect through one configuration-aware boundary:

```rust,ignore
let host = HostClient::connect(&config).await?;
let info = host.info().await?;

let request = Request::new(Command::Submit {
    session,
    content,
    intent,
});
let receipt = submission::acknowledge(&host, &request).await?;
let mut watch = host.subscribe(session.journal_sequence).await?;
```

`connect` reuses a compatible host, restarts an incompatible idle host, and refuses to replace an
incompatible busy host. The internal `orvek host` command runs the detached process. Interactive
startup creates or resumes a host session; `orvek run` submits one task and emits versioned
`orvek.host` JSON Lines from the same host.

## Shape

- `app::host::HostClient` owns socket validation, bounded request deadlines, configuration identity,
  detached startup, and reconnect decisions.
- `app::submission::acknowledge` owns uncertain acknowledgement recovery. It retries the immutable
  request ID and queries the durable receipt before a caller can create another submission.
- `app::host::HostWatch` owns partial frame progress and advances its reconnect cursor only for
  authoritative journal records. Disposable previews and preview gaps cannot move that cursor.
- `app::headless` filters host records to the submitted session and linked tasks, reconnects from
  the last authoritative cursor, and emits a stable versioned JSON Lines envelope.
- `app::host::serve` constructs the harness provider and Docker executor, opens durable host state,
  and serves the harness IPC protocol until shutdown.
- The harness `Host` remains the sole owner of task state, queue order, recovery, and execution.
- CLI, TUI, and future automation adapters remain clients. TUI transcript, pane, draft, and preview
  state are disposable projections and do not inherit host authority.
- `tui::client` maps submit, queue, cancel, fork, configure, auxiliary, review, and handoff effects
  to authenticated host commands. Its generation fence prevents late frames from an old pane from
  mutating a replacement session.
- `tui::host_projection` advances only through durable journal records. Preview gaps discard
  speculative output and reconcile from host history.
- `tui::session` discovers native sessions and imports historical SQLite sessions through the host;
  legacy transcripts never become a second execution authority.

The state root is `<config-directory>/host/v1`. Its directory is owner-only, the Unix socket must be
owned by the effective user with no group or other permissions, and peer credentials are checked
after connection. Startup diagnostics are capped and written to an owner-only, no-follow log file.

## History paging

Protocol 6 `History` pages contain at most 64 entries and 768 KiB of encoded entries,
plus a small cursor envelope. Each entry is either `inline` history or a `tool_output`
reference with the original call ID, byte length, and output digest. Both advance the
item cursor. References do not replace or shorten stored results.

To read a reference, send `HistoryText` with the same session cursor, its item index,
`content_index: 0`, a byte offset, and a limit of 1..24576 bytes. This uses the same
exact text reader as `read_context`; `bytes_base64` remains lossless across split UTF-8.
Verify the extent and digest before displaying an assembled output. No artifact copy
or interpreter is required. The TUI retrieves the full text. The optional headless
feedback scan skips tool-output references and keeps scanning inline notices.

A single oversized page is not safe: a valid 4 MiB serialized interpreter value can
exceed the 8 MiB IPC frame after result and history JSON escaping. Bounded text pages
remain below 181 KiB even with worst-case text escaping and base64 together.

## Tradeoffs

The detached host requires a local Unix socket and Docker executor. Orvek therefore reports an
explicit platform or runtime error instead of silently falling back to terminal-owned execution.
Configuration identity includes runtime-affecting settings and non-secret file revision metadata;
secret bytes are never hashed or logged. Client build timestamps are excluded so rebuilding a
protocol-compatible client does not masquerade as a configuration change.

The host-backed TUI keeps presentation state local for responsiveness. A crash can lose an
unacknowledged draft or speculative preview, but reconnect reconstructs accepted work from the
session journal and history. Harbor consumes the headless projection independently; it is not a
prerequisite for interactive durability.

The Docker executor requires the packaged Linux helper and a local Docker service. Host startup
reports a precise bounded diagnostic when either prerequisite is unavailable.

## Alternatives

Running the harness inside each terminal was rejected because disconnect would still own task
lifetime and multiple clients could diverge. Letting the TUI own persistence was rejected because it
would preserve the legacy authority split. A network daemon was rejected because Orvek only needs a
same-user local boundary and Unix peer credentials provide a smaller security surface.

## Remaining release gate

Packaging verification must prove that every release carries the matching executor helper.
