# Memory

Memory stores useful facts and decisions across sessions. It is disabled by default. Enable local memory
in `config.toml`:

```toml
[memory]
enabled = true
```

Agents retrieve memory explicitly with `scan` and `read`. Orvek does not insert the whole store into
prompts. Memory content does not override the host's task contract or instructions.
The detached host installs the tools for native tasks, sandbox tasks, and read-only auxiliary
conversation. Neither a TUI nor a pasted prompt is required.

## Local memory

The SQLite store is `<config-dir>/memory/v1.sqlite3`. `<config-dir>` is the parent of the selected
`config.toml`, including one selected through `--config`, `ORVEK_CONFIG`, or `ORVEK_HOME`.

All sessions and agents using that directory share one local store, subject to their tool
permissions. Records have a typed global, repository, or legacy-unscoped scope. Repository identity
is a hash of sorted Git root commits, so clones and linked worktrees can reuse records. This is a
retrieval boundary, not a credential or authorization boundary. Branches sharing those roots share
repository scope. Local memory needs no network service or Cloudflare account.

Schema v3 migrates the existing `v1.sqlite3` file in place. Old records stay legacy-unscoped with
unknown origin and unverified evidence. An older build rejects the new schema rather than dropping
provenance. Back up the database before downgrading.

## Operations and limits

Primary coding tasks can use all five memory-tool operations. Auxiliary conversation and other
read-only assistance can only `scan` and `read`. The host does not currently install memory tools
for delegated children.

| Operation | Behavior |
| --- | --- |
| `scan` | Search with `query` and optional `limit`, from 1 to 5. Return ranked previews. |
| `read` | Fetch complete records using `keys` returned by scan. Omit missing or stale keys. |
| `put` | Store `content` and optional typed `metadata`. Supply `replace` for atomic content-and-evidence CAS refresh. |
| `propose_lesson` | Nominate a short lesson with source evidence and a behavior-test citation. After settlement, the host asynchronously marks it proposed. |
| `delete` | Delete the record identified by `key`. An absent record succeeds; a stale version conflicts. |

Preserve each key's ID, version, and remote namespace:

```json
{"operation":"read","keys":[{"namespace":"alice","id":7,"version":1}]}
```

```json
{"operation":"put","content":"The project uses cargo nextest in CI.","replace":{"namespace":"alice","id":7,"version":1}}
```

```json
{"operation":"delete","key":{"namespace":"alice","id":7,"version":2}}
```

Local keys omit `namespace`. Replacement increments the version. Open `/memory` to inspect records.
The browser uses `list` for up to 512 records without changing scan or read telemetry; `list` is not
an agent-tool operation. Host tool results must fit a 128 KiB encoded JSON bound; if a bulk
`read` exceeds it, request fewer keys. The host returns an error rather than truncating records.

Store one self-contained conclusion per record, such as a durable preference or an expensive
operational finding. Do not store credentials, transcripts, plans, raw output, or transient state.

Search uses lexical BM25 with `k1 = 1.2` and `b = 0.75`. Queries with no searchable terms or no
matching active records return no candidates. Previews contain at most 64 UTF-8 bytes. Scans update
scan telemetry only for returned records; reads update use telemetry. New and replaced records
expire after seven days unless read. Scanning does not end that probation.

| Limit | Value |
| --- | ---: |
| Record content | 1 KiB |
| Scan query | 512 bytes |
| Records per local store or remote namespace | 512 |
| Total content per local store or remote namespace | 256 KiB |
| Local main database file | 4 MiB |
| Scan results | 5 |

Capacity-increasing writes may prune expired unread records. They do not evict active records.
Version checks, writes, telemetry, and capacity checks are transactional at the store's scope.

The local database is unencrypted. Orvek rejects writes flagged by its secret detector and filters
flagged records before use, but memory is not a secret store. Deletion does not erase copies in
SQLite sidecars, backups, transcripts, model-provider records, or other storage.

## Evidence and lesson proposals

Model-facing `metadata` accepts only `scope`, `kind`, and source requests. Scope is `global` or
`repository`; kind is `preference`, `procedure`, or `code_claim`. The host observes the repository
identity, checked revision, complete-file SHA-256 digest, optional line range, and producing
session/request/task trace address. It sets origin to `model`. A model cannot supply a host origin,
source hash, trace identity, or authority flag. Put without metadata remains legacy-unscoped.

```json
{"operation":"put","content":"The feature flag is enabled.","metadata":{"scope":"repository","kind":"code_claim","sources":[{"path":"src/feature.rs","range":{"start":1,"end":8}}]}}
```

Scan before each put or proposal. To correct a claim, send the new content and source requests with
the exact `replace` key. Content, metadata, and version commit together. A conflict or failed
transaction leaves the old evidence unchanged. A dispatched transaction can finish after caller
cancellation; read the current version before deciding whether another write is needed.

Scan/read return `freshness`:

- `current`: every cited file matches its stored content digest. This does **not** mean the claim
  is true or a behavior test passed.
- `stale`: cited bytes changed, including an uncommitted edit at the same Git revision. This does
  **not** mean the claim is false.
- `unavailable`: a cited source is deleted, inaccessible, outside the mounted repository, or is an
  artifact without a mounted resolver.
- `unverified`: no source evidence was recorded. Preferences and old records can remain useful in
  this state. Reads and probation telemetry never certify them.

Only returned records' citations are inspected. Unrelated files and changed Git revisions do not
trigger a corpus-wide review. Reverting a cited file to its recorded bytes restores `current`.
Line ranges identify the citation; the digest covers the complete file. Source reads accept only
regular, non-symlink files up to 8 MiB. Unix descriptor-relative opens start from a pinned root
directory. Git runs from that same directory, so moving or replacing its admission pathname does
not redirect observations into another root. Files and Git state remain live, not an atomic snapshot.
Other platforms report source observation unavailable. The source boundary is the admitted host
repository, not an uncommitted sandbox candidate. The output identifies exactly which bytes were
observed. Artifact citations survive transfer but are unavailable until their source is mounted.

```json
{"operation":"propose_lesson","content":"Test the feature flag before claiming it works.","metadata":{"scope":"repository","kind":"procedure","sources":[{"path":"src/feature.rs"}]},"behavior_test":{"path":"tests/feature.rs"}}
```

A proposal must cite evidence and a behavior-test file. The host stores a pending candidate during
the task and starts consolidation after terminal settlement. Repeated normalized lessons in the
same scope and writer ownership merge producing traces with CAS. New citations become active;
previous citations remain in `historical_evidence` and do not make refreshed evidence stale.
The pending run is separate from trace history. Only its callback can finalize its exact version.
Consolidation queries owned lessons in bounded pages, not the shared 512-record discovery window. No provider call, test execution, or
instruction-file rewrite occurs. A cited test is labelled `cited_not_executed`; it is not a passing
result. The resulting record remains a proposal and reference data, never promoted host policy.
Failure or host exit can leave a pending proposal; the error is logged and task verification is
unchanged. This phase does not promise crash-resumable consolidation or autonomous lesson quality.

Metadata is bounded to 16 KiB, 16 active and 64 historical citations, 32 trace references, and 32
import keys and ownership references. Exceeding a bound rejects the operation without dropping
history. The
same store is independent of model selection. Mechanical reuse is tested with two scripted model
identities. Live-model learning quality and stale-claim avoidance have not been measured.

## Remote memory

Remote memory is optional. Configure a service and the workspaces that use it:

```toml
[memory]
enabled = true

[memory.remote]
endpoint = "https://memory.example.com/"
namespace = "alice"
bearer_token = "replace-with-a-secret-token"
workspace_roots = ["/path/to/team-projects"]
```

The token is stored in `config.toml`. Set the file mode to `0600`. Direct-token configuration
requires Unix permission checks. `orvek config show` and configuration debug output redact it.
Remote deployments must use HTTPS. Plain HTTP is accepted only for loopback addresses.

Each runtime selects one backend. Workspaces inside a configured root, including its linked Git
worktrees, use remote memory only. Other workspaces use local memory. Relative roots resolve from
the configuration directory; scope checks use canonical paths. An in-scope remote failure returns
an error and never switches to local storage. There is no automatic synchronization or combined
search across backends.

The token determines the authenticated namespace and `reader` or `writer` role. The configured
namespace must match. Namespaces contain 1 to 128 ASCII letters, digits, periods, hyphens, or
underscores. Readers can retrieve visible records across namespaces, including their own. Writers
can also mutate their own namespace. Auxiliary requests remain read-only even with writer credentials. Delegated children do not
currently receive this service.

The application selects the backend from the admitted session workspace, not a temporary sandbox
path. The host retains that selection for the run. Reconnect after active tasks finish to replace
an idle host when configuration changes. Existing sessions still obey pinned configuration
compatibility checks. No bearer token is sent over session IPC or copied into a context manifest.

## Context versions and local skills

Before each primary or auxiliary provider turn, the host refreshes the skill catalog and the
memory discovery window. A `ContextPrepared` journal event records the request ID, model-call ID,
and content-addressed manifest. Old manifests remain available as host artifacts. The manifest
contains backend identity and scope-visible keys from the bounded `list` window, not memory bodies or
credentials. A remote window is **not** a revision of the entire shared corpus. Operations read
current backend state, and mutations still require exact CAS keys. Hidden scoped reads do not
increment use counts or clear probation. Scope is retrieval selection, not authorization. Cancellation stops waiting for
context I/O. An already-dispatched atomic mutation may still commit; the host reports that
uncertainty and does not replay it automatically.

Unchanged metadata keeps the same manifest and instruction cache identity. A changed catalog or
key-version window changes the instructions identity without discarding stable history or changing
the routing/tool identity. Scan/read telemetry alone does not invalidate the manifest. Actual
memory results and loaded skill bodies remain in recorded tool outputs; manifests do not replace
that evidence.

Enable local skill discovery separately:

```toml
[skills]
enabled = true
roots = ["/path/to/skills"]
```

The supported instruction-file format is `SKILL.md` with YAML `name` and `description` fields.
Discovery includes configured roots, `$CODEX_HOME/skills` (or `~/.codex/skills`), and `~/.agents/skills`.
A skill's name must match its containing directory. The host sends bounded metadata, not skill
bodies, with each request. `read_skill` loads a catalogued skill by name, including when a sandbox
cannot access its host path. It returns the complete body, canonical path, and content digest;
bodies or encoded tool results over 128 KiB are rejected rather than truncated. Bodies are read at tool-call time, so the
returned digest, rather than the earlier catalog metadata, identifies their exact version. Referenced resource files are not copied
into sandboxes by this tool.

Invalid optional skills produce recorded, model-visible diagnostics without removing valid entries.
File edits refresh at the next provider-turn boundary. `AGENTS.md`, `CLAUDE.md`, and other repository
instruction filenames are not loaded automatically by this service; a model can inspect admitted
workspace files explicitly.

Run the [memory and on-demand skill example](executable-examples.md#local-memory-and-on-demand-skills-across-sessions)
for an isolated, two-session CLI flow. It checks real put/scan/read outputs, the exact skill body
and digest, and `ContextPrepared` manifest artifacts. It uses one host and requires no Docker.

Verify the no-TUI restart path with a local fake provider:

```sh
cargo build -p orvek
python3 scripts/test-host-context.py target/debug/orvek
python3 scripts/test-memory-evidence.py target/debug/orvek
python3 scripts/test-memory-transfer.py target/debug/orvek
```

The test starts two fresh headless clients with a host restart between them. It checks real memory
writes and retrieval, on-demand skill bodies, refreshed metadata, and recorded manifests. It uses
temporary data and loopback HTTP, not a live model account.

## Explicit transfer commands

Transfer commands use the local store from any directory and run whether `memory.enabled` is true or
false. They ignore workspace scope and do not enable memory tools in model sessions. Remote push and
pull require configured remote credentials. File archives do not.

```sh
orvek memory export ./memory-archive
orvek memory import ./memory-archive
orvek memory push --dry-run
orvek memory push
orvek memory pull --all
orvek memory pull --namespace alice --namespace bob
```

`push` requires a writer credential. It replaces that writer's remote namespace with the complete
local snapshot, deleting remote records absent locally. Other namespaces stay unchanged. It keeps
identical records and reconciles concurrent local changes for up to three passes. `--dry-run`
reports the local snapshot without contacting the service.

`pull` accepts either role and requires `--all` or one or more `--namespace` options. It merges
remote records into local memory. Original owning keys (including namespace and version), origin,
scope, evidence, and producing traces survive in import provenance. Equal content from different
namespaces remains distinct. Repeating the same imported snapshot skips identical records.
Validation, pagination, and capacity checks must all succeed before the local transaction commits.
Failure leaves local memory unchanged.

`export` creates a new directory with `manifest.json` and readable, digest-addressed JSON records.
The manifest records the format version and exact source keys. It is written last, so an interrupted
export is not importable. `import` validates all record digests before one local transaction. It
allocates local owning IDs and retains source keys and versions in `metadata.imported_from`.
Persistent random ownership IDs distinguish independently allocated records. `transferred_from`
retains exact owning identities and keys across hops. Identical snapshots merge transfer history;
conflicting payloads stay separate. Self-import and repeated imports do not duplicate content.
Older archives without ownership IDs use their recorded key, payload, and creation time as a fallback identity;
independent legacy stores with byte-identical records cannot be distinguished retroactively.
Archives contain plaintext reference data; they must not be treated as trusted host instructions.

## Remote server integration

The unpublished [`orvek-memory` crate](../crates/memory/README.md) defines `MemoryStore` and the
Axum `server::MemoryServer<S>` wrapper. The wrapper authenticates requests and creates a store
bound to the authenticated namespace. Stores own transactions, persistence, capacity checks,
pagination, and server-assigned timestamps. Implement `scan_scoped`, `read_scoped`, `lesson_page`, and `put_with_metadata` to
support v2 host tools; the default methods reject unsupported metadata operations rather than
silently dropping scope or provenance. Client-side secret filtering is separate from the
server store's storage rules.

Protocol generation `orvek_memory::VERSION` is currently `2`. Routes and session negotiation use
that value. Upgrade remote servers and clients together; v1 is intentionally incompatible.


| Route | Required role | Operation |
| --- | --- | --- |
| `GET /v2/session` | reader | Return version, namespace, and role. |
| `POST /v2/memories/scan` | reader | Search and return at most five candidates. |
| `POST /v2/memories/read` | reader | Read exact visible keys. |
| `POST /v2/memories/list` | reader | Return at most 512 visible records without telemetry changes. |
| `POST /v2/memories/lessons` | writer | Query at most 128 owned lessons after an exclusive ID. |
| `POST /v2/memories/put` | writer | Insert or replace within the writer's namespace. |
| `POST /v2/memories/delete` | writer | Delete within the writer's namespace. |
| `POST /v2/memories/sync` | writer | Atomically replace the writer's namespace from a snapshot. |
| `POST /v2/memories/export` | reader | Export visible records in pages of at most 128. |

Requests carry a bearer token and namespace assertion. The server rejects mismatches. Ordinary
puts cannot supply IDs, versions, timestamps, or telemetry. Export preserves namespaced records in
`namespace, id` order; each page has its own transaction snapshot. Continue with the returned
cursor. A multi-page export is not one database snapshot.

Backends return semantic `MemoryError` variants. Use `MemoryError::backend` or
`MemoryError::unavailable` for backend failures. HTTP errors return stable codes without request
content, credentials, or database diagnostics. The `native-server` feature limits the router to
64 concurrent requests and store operations to 30 seconds. The host owns graceful shutdown and
storage encryption.

Remote deployments provide their own `MemoryStore` implementation and macOS-hosted service
operations. Orvek no longer ships a WebAssembly or Cloudflare deployment target.
