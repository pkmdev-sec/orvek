# Memory

Memory stores useful facts and decisions across sessions. It is disabled by default. Enable local memory
in `config.toml`:

```toml
[memory]
enabled = true
```

Agents retrieve memory explicitly with `scan` and `read`. Orvek does not insert the whole store into
prompts. Memory content does not override current instructions or `AGENTS.md`.

## Local memory

The SQLite store is `<config-dir>/memory/v1.sqlite3`. `<config-dir>` is the parent of the selected
`config.toml`, including one selected through `--config`, `ORVEK_CONFIG`, or `ORVEK_HOME`.

All sessions and agents using that directory share one local store, subject to their tool
permissions. There are no workspace, repository, branch, session, or author namespaces. State any
workspace scope in the record itself. Local memory needs no network service or Cloudflare account.

## Operations and limits

Root agents can use all four memory-tool operations. Children can only `scan` and `read`.

| Operation | Behavior |
| --- | --- |
| `scan` | Search with `query` and optional `limit`, from 1 to 5. Return ranked previews. |
| `read` | Fetch complete records using `keys` returned by scan. Omit missing or stale keys. |
| `put` | Store `content`. Supply `replace` with the current key to replace a record. |
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
an agent-tool operation.

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
Use HTTPS for remote deployments.

Each runtime selects one backend. Workspaces inside a configured root, including its linked Git
worktrees, use remote memory only. Other workspaces use local memory. Relative roots resolve from
the configuration directory; scope checks use canonical paths. An in-scope remote failure returns
an error and never switches to local storage. There is no automatic synchronization or combined
search across backends.

The token determines the authenticated namespace and `reader` or `writer` role. The configured
namespace must match. Namespaces contain 1 to 128 ASCII letters, digits, periods, hyphens, or
underscores. Readers can retrieve visible records across namespaces, including their own. Writers
can also mutate their own namespace. Child agents remain read-only even with writer credentials.

Configuration reload updates the memory browser. Existing agent runtimes retain their installed
tools and instructions; create or restore a session to apply those changes to the agent.

## Explicit transfer commands

Transfers use the global local store from any directory. They ignore `memory.enabled` and workspace
scope, but still require configured remote credentials.

```sh
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
remote records into local memory, preserves existing content, and skips normalized duplicates.
Imported records lose namespace provenance because local schema v1 has no author field. Validation,
pagination, and capacity checks must all succeed before the local transaction commits. Failure
leaves local memory unchanged.

## Remote server integration

The unpublished [`orvek-memory` crate](../crates/memory/README.md) defines `MemoryStore` and the
Axum `server::MemoryServer<S>` wrapper. The wrapper authenticates requests and creates a store
bound to the authenticated namespace. Stores own transactions, persistence, capacity checks,
pagination, and server-assigned timestamps. Client-side secret filtering is separate from the
server store's storage rules.

Protocol generation `orvek_memory::VERSION` is currently `1`. Routes and session negotiation use
that value:

| Route | Required role | Operation |
| --- | --- | --- |
| `GET /v1/session` | reader | Return version, namespace, and role. |
| `POST /v1/memories/scan` | reader | Search and return at most five candidates. |
| `POST /v1/memories/read` | reader | Read exact visible keys. |
| `POST /v1/memories/list` | reader | Return at most 512 visible records without telemetry changes. |
| `POST /v1/memories/put` | writer | Insert or replace within the writer's namespace. |
| `POST /v1/memories/delete` | writer | Delete within the writer's namespace. |
| `POST /v1/memories/sync` | writer | Atomically replace the writer's namespace from a snapshot. |
| `POST /v1/memories/export` | reader | Export visible records in pages of at most 128. |

Requests carry a bearer token and namespace assertion. The server rejects mismatches. Ordinary
puts cannot supply IDs, versions, timestamps, or telemetry. Export preserves namespaced records in
`namespace, id` order; each page has its own transaction snapshot. Continue with the returned
cursor. A multi-page export is not one database snapshot.

Backends return semantic `MemoryError` variants. Use `MemoryError::backend` or
`MemoryError::unavailable` for backend failures. HTTP errors return stable codes without request
content, credentials, or database diagnostics. The `native-server` feature limits the router to
64 concurrent requests and store operations to 30 seconds. The host owns graceful shutdown and
storage encryption.

The [Cloudflare example](../examples/orvek-memory-cloudflare/README.md) provides a separate Worker
and D1 adapter with local development and deployment instructions. It is not part of the installed
CLI.
