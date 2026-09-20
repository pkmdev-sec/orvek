# Experimental context transitions

T07 is **default-disabled**. Live-model quality and full-cost acceptance are pending.
Scripted tests prove wiring, byte integrity, recovery and completion boundaries. They do not
prove that a model retains goals better or spends less.

## Operator opt-in

Start the CLI and its host with `ORVEK_EXPERIMENTAL_CONTEXT_TRANSITIONS=1`.
The switch participates in application host configuration identity, so an incompatible
already-running host is not silently reused. Library users opt in with
`Host::with_experimental_context_transitions(true)` before serving requests.

The primary task model then receives `transition_context`. It supplies a half-open history
range, purpose, summary/evidence index, and declared pending obligations. Only history before
`settled_history_items` is eligible. This conservative first version requires the earlier
request to settle; it cannot summarize an active request's completed tools. The prompt states
the eligible range. Use `read_context` to discover source items.

The host derives source cursors and digests. It rejects empty, active, overlapping and
protocol-splitting ranges. It does not certify prose or the completeness of declared
obligations. User messages remain exact. The original request and protected contract remain
in instructions. The live tail remains native. The journal is unchanged.

New transitions are previewed before commit. Rejection keeps prior derived state intact.
Actual provider byte limits still apply. Neither the prior/native view nor a summary is
guaranteed to fit. Accepted transitions survive restart without a summarizer or tool replay.
Forks start from original source with their own view; ancestor retrieval remains cursor-scoped.
The old default renderer identity and empty-default session serialization are unchanged.
An unresolved tool protocol prefix conservatively prevents new range transitions until the
source can be represented without splitting that protocol; exact retrieval remains available.

## Repeatable deterministic checks

Build the CLI with `cargo build -p orvek --bin orvek`.
Run `cargo test -p orvek-harness --test context_transitions --test native_host`.

Run the paired native/transition stress fixture with
`python3 -B evals/context_transitions/run_paired.py --binary target/debug/orvek --output /tmp/t07-paired`.
It creates fresh HOME/workspace/host/provider state for each condition. The local scripted
provider reads historical evidence, requests a phase transition in the treatment, retrieves
exact UTF-8 and split UTF-8 bytes, writes a growing refactor fixture, checks its API and old
values, and restarts after each phase. Frequent transitions exist only in this stress fixture.

Run the real Docker completion test with
`python3 -B evals/context_transitions/verify_sandbox.py target/debug/orvek`.
It needs Docker, `debian:bookworm-slim`, and `ORVEK_EXECUTOR_HELPER`. A misleading summary
claims completion with no pending obligations. Protected verification rejects that claim.
Only a repaired candidate receives a certificate.

## Accounting and limits

`attempts.jsonl` records each admission and final attempt, including failures. The runner
uses T03's `parse_log`. Summary-producing calls, retrieval calls and their provider receipts
are included, not subtracted. Exact request body bytes come from recorded model-call receipts;
the receipt payloads are retained under `receipts/` for inspection. Cache identities are
recorded per call. Unknown cached/reasoning tokens, cache misses and billing stay null.
Latency is real local fixture latency, not live-provider latency. Synthetic usage is labeled.
No child calls are requested. The two conditions differ only in the experimental capability
and its requested transitions; both use the existing native/bitmap selection machinery.

This is a paired **mechanism** harness, not qualification for release. No live provider or paid
calls are authorized or made. Paired actual-model goal retention, correctness, cache behavior,
summary/retrieval overhead, provider billing and repeated-run latency remain acceptance work.
Transitions can increase total bytes and calls, especially when source is small or summaries
are large. No savings claim is made. No approval loop, task spend cap, or turn-count trigger
is added.
