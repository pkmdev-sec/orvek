# Local trace bundles

A trace bundle is a local, read-only copy of a pinned host journal prefix and its
bounded artifact closure. Export does not start a host, contact a provider, run a
command, migrate the database, or upload data. It can read a running host's WAL.

## Export and inspect

```sh
orvek trace export --host-root /path/to/host/v1 --output run.trace.json
orvek trace replay run.trace.json
orvek trace review run.trace.json
orvek trace prefixes run.trace.json --output decision-fixtures
```

The first format version exports **all sessions** from cursor zero through a
pinned global cursor. Use `--through N` to select an earlier prefix. Arbitrary
nonzero baselines are not supported. The default bounds are 100,000 events,
10,000 artifact references, 64 MiB of decoded data, and 16 artifact hops. An
oversized journal fails; bounded or missing artifact traversal remains explicit.
Bundle files are created without overwriting existing files, with mode 0600.

**Treat bundles as private.** They can contain user input, source code, tool
outputs, and available model output. `--omit SHA256` removes a selected payload;
it is not automatic secret detection. The resulting bundle is non-exact.
Missing payloads, traversal limits, and unknown digest-shaped references are
also reported. Payload hashes, original aggregate hash chains, global cursors,
aggregate revisions, and reconstructed state/context/outcome identities are
checked on import. Hashes detect corruption; they do not authenticate a
wholesale rewrite by an attacker who can replace the manifest.

## What replay proves

Replay applies recorded events through the existing session and task reducers.
It does not invoke inference, tool dispatch, verification, Docker, or networking.
It validates selected context against its recorded source and compares the
reconstructed task, candidate, evidence, certificate, and outcome identities.
This is a receipt-driven diagnostic stub, **not a fresh verification run**.
`finished_unverified` stays distinct from `complete` and has no certificate.

New parent and child model dispatch receipts contain session, request, task,
call and child IDs, exact request payloads, settings, tools, instructions, and
context/cache identities. Child tool receipts bind those IDs to the admitted
job before execution. Retries retain distinct calls. Old absent links are not
inferred. The review packet includes the recorded intent, contract, candidate,
delivery/patch identities, the patch payload (base64), checks, certificates, costs,
and unresolved data.
`exporter_revision` identifies the exporting binary, not the code that executed
an old task. New dispatches record the executing harness Git revision and dirty
flag (or `unknown` when Git metadata is absent). Session harness bindings and candidate source digests carry the
available execution provenance. An unrecorded original Git revision is unknown.

Costs report the sum of recorded exact USD receipts and a completeness flag.
Unknown receipts are not converted to measured zero. Provider-hidden reasoning
and unrecorded external state remain unavailable.

Prefix fixtures end immediately before each recorded model dispatch. The paired
`decision-*.json` file carries that request's available input, instructions and
tools, but not its response. Future receipt artifacts are removed from the
prefix. Historical calls without a dispatch receipt cannot yield such a fixture.

## Experimental fresh re-execution

```sh
mkdir /tmp/fresh-orvek-workspace
orvek --workspace /tmp/fresh-orvek-workspace trace reexecute run.trace.json \
  --task TASK_UUID --experimental
```

This is a separate online execution path. It requires a new empty workspace and
normal configured host admission. It submits only the original user intent and
allocates fresh session, request, task and call IDs. It does not import old tool
outputs, checks, certificates, or workspace contents as evidence. You must
supply any required starting material through the new task's normal workflow.
Unresolved jobs or effects block this path; reconcile them with the original
host. New commands can have side effects and model calls can incur cost.
