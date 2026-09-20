# Optional Cloudflare memory backend

This standalone example runs an authenticated memory service on Cloudflare Workers and D1. It is
not part of the installed `orvek` CLI. For memory on one computer, enable
[local SQLite memory](../../docs/memory.md#local-memory); no Cloudflare account is needed.

## Local development

Install Rust, Node.js/npm, and Bun. From the repository root:

```sh
cargo install worker-build --version 0.8.5 --locked
cd examples/orvek-memory-cloudflare
npm ci
cp credentials.example.toml credentials.toml
chmod 600 credentials.toml
bun run migrate:local
```

Edit the ignored `credentials.toml`. Use a unique token for each credential:

```toml
[[credentials]]
namespace = "alice"
role = "writer"
token = "replace-with-a-high-entropy-token"

[[credentials]]
namespace = "auditor"
role = "reader"
token = "replace-with-an-independent-token"
```

Readers can scan, read, list, and export visible records across namespaces. Writers can also mutate
their own namespace. Start the Worker:

```sh
bun run dev
```

This validates credentials and writes the ignored `.dev.vars` file with mode `0600` before starting
Wrangler. Configure Orvek with the printed address and a matching credential:

```toml
[memory]
enabled = true

[memory.remote]
endpoint = "http://127.0.0.1:8787"
namespace = "alice"
bearer_token = "replace-with-a-high-entropy-token"
workspace_roots = ["/absolute/path/to/team/workspace"]
```

Set the Orvek config file mode to `0600`. See the [memory guide](../../docs/memory.md) for workspace
selection, child permissions, and explicit push/pull commands.

## Deployment

From this example directory, create the database:

```sh
bun x wrangler d1 create orvek-memory
```

Replace `REPLACE_WITH_D1_DATABASE_ID` in `wrangler.jsonc` with its ID, then run:

```sh
bun run migrate:remote
bun run deploy:check
bun run deploy
bun run credentials:push
```

The credential uploader validates the TOML and passes the Worker secret to Wrangler through stdin.
It creates no intermediate plaintext deployment file. Configure clients with the deployed HTTPS
endpoint.

## Protocol upgrade

This build uses remote protocol v2. Upgrade clients with the Worker and apply all migrations,
including `0002_evidence.sql` and `0003_ownership.sql`. Existing records remain legacy-unscoped and unverified. Metadata
and citations persist through put, sync, and export; scoped scans filter before ranking and scoped
reads filter before use telemetry. Owned lesson pages use the authenticated namespace and do not
share the interactive list window. Ownership IDs preserve independent transfer provenance.

## Limits and operations

Scans rank a bounded shared corpus with BM25 inside the Worker. The configured defaults are
10,240 records and 5 MiB of content. The scan-budget variables in
`wrangler.jsonc` control these limits. The old binding names
remain for compatibility with existing deployments.

If the corpus exceeds either limit, scans return a capacity error. Export and mutation remain
available to reduce or migrate the corpus. Check current [Workers limits](https://developers.cloudflare.com/workers/platform/limits/)
and [D1 limits](https://developers.cloudflare.com/d1/platform/limits/) when sizing a deployment.

With D1 read replication enabled, requests without a bookmark may read stale scan, list, or export
data. Full record reads update telemetry, and mutations use the primary. The remote client and its
clones carry returned bookmarks into later requests to preserve session ordering and observe their
successful writes.

Worker logs contain operation metadata, not memory content, queries, credentials, or token hashes.
Export backups outside D1 when its retention does not meet your recovery needs.

To rotate a token, add another credential with the same namespace and role, upload it, update
clients, then remove the old credential and upload again. Both tokens work during the transition;
duplicate tokens are rejected.
