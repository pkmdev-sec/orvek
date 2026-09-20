# Persistent tool composition

`interpreter_eval` runs an async JavaScript function body inside the host. It is
available to task requests in native and sandbox sessions. It is not a shell,
a second agent loop, or a separate execution authority.

```javascript
globalThis.rows = JSON.parse(
  (await host.call("read_file", {path: "rows.json"})).result.content.data
);
globalThis.selected = rows.filter(row => row.important);
host.checkpoint({selected});
return {count: selected.length, selected: selected.slice(0, 5)};
```

Use the ordinary tool's bounded paging fields for files larger than one result.
`globalThis` values survive successful cells and later tasks in the same session
while the host remains alive. Local variables belong to one async function call.
Each session has its own runtime. Forks do not inherit a live interpreter.

## Authority and evidence

`await host.call(name, arguments)` calls the ordinary host dispatcher. The initial
roster is `read_file`, `search`, `read_context`, `read_review_feedback`,
`task_status`, and the existing child orchestration tools. The tool must also be
admitted in the current request. Native sessions still cannot spawn children.
Memory writes, workspace writes, commands, completion controls, and nested
`interpreter_eval` are not exposed by this bridge. A sandbox child retains its
existing read-only execution policy.

The bridge binds the current request, task, scope revision, workspace, and tool
roster for every cell. A saved reference to `host.call` does not retain an earlier
request's backend or grant. The runtime has no filesystem, network, process,
credential, module-loader, or ambient host API.

Every cell has a durable UUID. Inner calls use `cell UUID/ordinal` IDs. The journal
records cell source, runtime identity, actual execution environment, generation,
arguments, and result artifact links. Workspace calls create the same ordinary
execution jobs and receipts as direct tool use. No synthetic model proposals or
jobs are inserted. A successful cell does not settle an unknown inner effect or
make a child result verified.

Only the selected cell output and citation links enter the parent prompt. Full
inner values remain in the interpreter and content-addressed artifacts. CLI/IPC
journal watches expose inner-call identities and receipts without displaying the
full values. The TUI shows status and receipt references.

## Checkpoints and restart

`host.checkpoint(value)` stages one explicit JSON value. It becomes durable only
when the cell succeeds. Serialization rejects functions, promises, cycles,
undefined properties, non-finite numbers, sparse arrays, symbols, accessors,
hidden properties, and live objects. Native object-class checks also reject live
objects whose JavaScript prototype was changed. Return values and host arguments
use the same lossless conversion; an omitted cell return becomes JSON null.

After restart or an interpreter error, a new runtime restores the latest saved
value as `globalThis.restored`. It checks the checkpoint's version, session,
runtime source identity, creating cell source, artifact digest, and data digest.
It parses data only. It never evaluates saved source or repeats a cell to rebuild
state. Hashes detect corruption; they are not authentication against a writer who
can replace both data and its journal.

Unsaved globals and live handles are lost. The next cell result states this
explicitly. Handles stored as JSON strings remain data, not restored processes or
grants. Interrupted cells keep their durable identities and are marked interrupted
when their request settles or the host recovers it. Their inner receipts remain
available. Recovery does not turn an unknown job into success.

## Waiting, cancellation, and limits

One dedicated OS actor thread owns each active session runtime. At most one cell
runs per session. Host calls use promises and channels. Waiting does not block a
Tokio worker, and host-wait time does not consume the VM execution allowance.
Other sessions and IPC inspection remain available while a cell waits.

A pending cell and each pending inner call have journaled handles. A detached host
continues the same operation when a CLI or watch disconnects. Reconnecting clients
resume journal observation; they do not submit a second evaluation. This is not
serialization of a suspended JavaScript stack. A host crash invalidates that
stack and never resumes it by replaying the cell. The provider turn waits for the
cell to settle; there is no separate model-facing background-eval polling tool.

Request cancellation reaches the VM interrupt handler and the ordinary host tool
cancellation path. Errors or cancellation discard the live runtime. Effects already
recorded by inner calls keep their own statuses.

The initial profile uses QuickJS 0.12.1 with a checked native allocator limit of
32 MiB, a 256 KiB VM stack, one second of active VM execution, and one million
interrupt checks per cell. Host waits are excluded. Code is limited to 64 KiB;
serialized values to 4 MiB; the pending-call window to 32 calls. These are per-cell
resource controls, not task turn, spend, or lifetime call caps. The host still
applies existing tool output bounds and task policy. Typed arrays, Atomics, Proxy,
regular expressions, and native I/O intrinsics are not installed.

## Verification

```sh
cargo test -p orvek-harness --lib interpreter
cargo test -p orvek-harness --test interpreter_host
cargo test -p orvek --test interpreter_cli
ORVEK_EXECUTOR_HELPER=/path/to/linux/helper cargo test -p orvek-harness \
  --test interpreter_host interpreter_docker -- --ignored --nocapture
```

The Docker test needs a local daemon and `debian:bookworm-slim`. It compares the same
structured-data/delegation task in serial and composed modes. It checks selected
IDs, real child execution receipts, pending handles, and provider request bodies.
Context bytes count serialized `input` arrays, not instructions, tool schemas, or
provider tokens. Metrics include fixture request counts, those input bytes, and
single-run latency. Provider cost
stays unknown. Scripted response counts are not live-model token, quality, or cost
savings evidence.

Large returned JSON values and checkpoints remain lossless. Interpreter values use content-addressed
artifacts. When the outer tool result exceeds one journal record, the host stores ordered UTF-8 parts
and a digest-checked end record in one transaction. Only the complete result enters history; reconnect
and offline replay reconstruct the same bytes. Individual journal records keep their existing ceiling.
