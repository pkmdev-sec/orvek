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
The byte limit separately bounds stored decoded data and the sum of expanded
receipt bytes plus serialized spans. Repeated receipt references consume that
second budget each time. These are byte-accounting bounds, not an RSS limit.
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
The `exact` flag covers recorded state and artifact consistency. It does not prove
complete historical causality or replay the controller's decisions. Historical
missing links are not enumerated per call.
`finished_unverified` stays distinct from `complete` and has no certificate.

New parent and child dispatch receipts contain session, request, task, call and
child IDs, logical HTTP templates, settings, tools, instructions, and context/cache
identities. Captured call outcomes separately retain the exact serialized body,
effective transport, dialect, and route source. A prepared body is not proof of
dispatch or remote delivery. Crashes before outcome persistence leave that body
unavailable. Authentication metadata, resolved endpoints, and network framing
are not recorded. Prompt/source content remains private and is retained verbatim.
Child tool receipts bind those IDs to the admitted job before execution. Host
retries retain distinct calls; transport retries retain their attempt numbers
within a call. Old absent links are not inferred. The review packet includes the recorded intent, contract, candidate,
delivery/patch identities, the patch payload (base64), checks, certificates, costs,
and unresolved data.
`exporter_revision` identifies the exporting binary, not the code that executed
an old task. New dispatches record the executing harness Git revision and dirty
flag (or `unknown` when Git metadata is absent). Session harness bindings and candidate source digests carry the
available execution provenance. An unrecorded original Git revision is unknown.

Costs report the sum of recorded exact USD receipts and a completeness flag.
Legacy or unlinked usage makes token totals unknown and cost totals incomplete.
Unknown receipts are not converted to measured zero. Provider-hidden reasoning
and unrecorded external state remain unavailable.

Prefix fixtures end immediately before each recorded model dispatch. The paired
`decision-*.json` file carries that request's logical input, instructions and
tools, but not its response or a future effective body. Future receipt artifacts are removed from the
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
host. This check sees only the pinned prefix, not later activity in the original
host. New commands can have side effects and model calls can incur cost.

## Documentation fixture bundles

The [executable examples](executable-examples.md) link to five checked-in bundles
under `examples/host-docs/traces/`. These are **sanitized, non-exact review bundles**,
not full replay records. They come only from the isolated scripted-provider
fixtures. No live provider, real credentials, user workspace, or upload is involved.

The generator exports a private trace inside each fixture's temporary directory.
It then uses `--omit` for every artifact reference and pins the same journal cursor.
It checks that original event bytes, aggregate hashes and expected replay identities
are unchanged. It vets the retained events, including embedded JSON and byte arrays.
Every artifact payload is absent, including request bodies, context, machine-specific
execution metadata and any credential-bearing artifact data. Cleanup removes the
private trace even when a scenario or export fails. Ordinary example runs do not export.

Omitting artifacts does **not** sanitize arbitrary traces. Version 1 also stores some
input, tool results and paths directly in journal events. These fixtures retain only
controlled example data, compiled behavior instructions, generated IDs/timings,
non-identifying platform names (such as `macos`/`aarch64`), and
synthetic `/tmp/ov-doc-...` workspace/skill paths (sometimes prefixed with `/private`).
Those are not user directories. If the event vetting fails, review the fixture and
stop publication; do not rewrite its events. The checks are guards for these audited
fixtures, not general secret detection. Do not use this generator on a real host.

An exact record replay validates recorded events, payloads and reconstructed state.
It still does not rerun tools or certify changed code. These sanitized bundles instead
allow **non-exact offline review** of retained events and outcome/certificate identities.
Missing artifacts stay explicit. Request details, model/tool payloads, context and
patch bodies may be unavailable. An omitted report cannot support a complete cost
claim. The behavioral assertions run during generation, not during replay. The native
outcome remains `finished_unverified`; only the sandbox fixture records `complete`
and a certificate. Neither provider-hidden reasoning nor external state is captured.

### Regenerate and check locally

Build the current CLI first, as described in [the examples](executable-examples.md).
Set `ORVEK_EXECUTOR_HELPER` to a matching Linux helper and make
`debian:bookworm-slim` available in local Docker. Generation runs all five scenarios,
including actual sandbox execution. No scenario silently skips.

```sh
python3 -B scripts/doc-traces.py --generate --binary /absolute/path/to/orvek
python3 -B scripts/check-doc-examples.py
python3 -B scripts/check-doc-examples.py --check
python3 -B scripts/doc-traces.py --binary /absolute/path/to/orvek
```

The [index](../examples/host-docs/traces/index.json) records scenario/shared-fixture/
generator hashes, a runtime-input hash, the binary hash, exporter revision, original
export digests, journal hashes and bundle hashes. It also records the local Docker
image ID and executor-helper hash, without recording their host paths. Runtime
inputs include Rust/Cargo sources and the embedded font under `bin`, `crates`,
`vendor` and `.cargo`.
The generator trusts the supplied CLI. Its binary hash identifies what ran; the
runtime-input hash describes the checkout, not proof that the binary was built
from it. Build the current CLI before generation. This is not a signed attestation
or proof of reproducible compilation. Exporter revision identifies the exporting
binary; it must not be presented as an inferred historical execution revision. Source hashes also identify
dirty source content. UUIDs and timings make regeneration intentionally non-byte-identical.

`python3 -B scripts/doc-traces.py` checks files, source drift, envelope/artifact policy,
original event hash chains and event vetting without Docker or a built CLI. Supplying
`--binary` also runs the real offline reducer/reviewer and proves that changing
`exact` to `true`, even with a recomputed envelope hash, is rejected. CI checks these
local artifacts and regenerates all five in a temporary output directory. It does
not fetch or publish trace artifacts. Regenerate after a source-hash check fails;
do not update index hashes alone to advertise an old trace as current.
