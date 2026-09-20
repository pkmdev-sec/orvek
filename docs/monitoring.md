# Trace monitoring and native read recovery

The detached host consumes its verified durable journal asynchronously. It records
local counters and can restore one versioned configuration field: the reply byte
envelope for primary native `read_file` calls. Monitoring does not use a paid
provider. A monitor or evaluator failure does not cancel user tasks.

This is a narrow deterministic recovery, **not general autonomous code repair or
proof of model quality**. No prompt, grader, tool schema, repository policy,
contract, binary or task resource limit is changed. Sandbox tools are unchanged.
Model-dependent promotion is unavailable. General code repairs are unavailable;
a future implementation must publish tested patches, not install a running host.

## Operator commands

These commands use the existing same-user Unix operator socket and protocol 6.
They are not model tools, event payload fields, or a public HTTP interface.
Use the same configuration and workspace options as the running host.

- `orvek monitor report`: read status, counters, matched comparisons and episodes.
  Use `--offset` and `--limit` to page; `next` identifies another page.
- `orvek monitor sampling N`: evaluate one of every N eligible underfilled reads.
  N must be 1–10000. Operational counters are not sampled.
- `orvek monitor install-read EXPECTED BYTES NOTE`: publish a version for new
  sessions. `EXPECTED` is the current release digest from the report. `BYTES`
  must be 4096–32768. The default is the existing 32768-byte envelope. `NOTE`
  records operator provenance, not instructions. This can reduce read capacity;
  it is not needed to enable monitoring.
- `orvek monitor rollback EXPECTED`: activate the retained previous version for
  new sessions. Compare-and-swap rejects stale requests.

Every new native session pins the release, reply capacity and executable digest
inside its validated harness revision. Existing sessions, including forks and
handoffs, keep their pinned configuration after activation or rollback. Legacy
sessions keep the compiled 32768-byte default and an unknown build measurement.
When optional monitoring state is unavailable, new sessions use that compiled
default and journal a sanitized session feedback notice.

## Nonfatal diagnostics

Admission fallback, missing monitor origin, failed post-run memory proposals and
completion-hook record failures use the existing session feedback journal. The TUI
shows live notices and restores saved feedback from session history. Headless
clients emit saved feedback as `session_feedback` at startup, then emit matching
session journal records while watching. These notices do not change task outcomes,
completion claims or hook receipts. Post-run
proposals stay asynchronous; a notice can arrive after a headless run exits. Read
the session journal or reconnect to observe that later notice. A proposal diagnostic
can be recorded only while the host remains open; stalled post-run work does not
keep a closed host's store open.

Host-wide problems use typed warning codes in IPC `Info.warnings` and `Warnings`
watch frames. Every watch starts with a current snapshot, even when empty, and
receives changes. These warnings apply to all sessions. The TUI suppresses repeated
snapshots; headless clients emit them as events. Protocol 6 rejects older clients
before streaming these new frames.

Warning state is bounded to one slot per category and lives only in the current
host process. It is not a durable incident log:

- `monitor_unavailable` clears after a successful monitor tick.
- `monitor_status_unavailable` reports that the monitor failure could not be saved.
  It clears when status can be saved again or a monitor tick succeeds.
- `event_intake_stopped` and `completion_hook_recovery_failed` remain until host
  restart. Neither warning authorizes replay of an uncertain effect.
- `session_notice_unavailable` means a session feedback notice could not be saved.
  It remains until restart, because later successful writes cannot recover the
  missing notice.

All these messages are static. Underlying error text, paths and credentials are
not copied into these notices. The monitor still saves a sanitized `last_error`
when its store is available. Durable task, event and hook records remain the
only authority for their outcomes.

## Measurements and uncertainty

Each counter belongs to a cohort with model/settings, observed executable build,
environment, protocol/tool profile, channel and behavior identity. Reports pair
only parent/child releases with matching known build and target identities.
Primary native job admissions record the actual executable digest, including work
on an older session after a binary update. Model-call, sandbox and historical
records currently lack that execution-time build link: their build is unknown,
and reports do not pair them as matched-build evidence. The session-admission
build is metadata, not proof of a later execution build.
Counts from unmatched models, builds or environments are not pooled. Comparisons
are descriptive and always marked uncertain. There is no independence assumption,
Poisson test, confidence interval or automatic model-quality conclusion.

Counters distinguish these observations:

| Signature | Opportunity | Failure or unknown |
| --- | --- | --- |
| `job_outcome` | Admitted jobs | Failed result; `opportunities - measured` is unsettled/unknown |
| `unresolved_job` | Admitted jobs | Unknown settlement; unmeasured jobs remain unresolved observations |
| `unresolved_effect` | Explicit effect identities | Latest intended/unknown effect state |
| `provider_error` | Admitted task model calls | Failed call; unknown receipts stay unmeasured |
| `missing_usage` | Admitted task model calls | Receipt lacks token total; all unmeasured calls remain unknown |
| `cost` | Admitted task calls and other recorded provider-cost identities | Unmeasured cost is unknown; `recorded_usd` is a lower bound, not a total |
| `verification_failure` | Protected check observations | Failed outcome; inconclusive/cancelled are unmeasured |
| `evaluator_failure` | Protected observations and deterministic repair evaluations | Inconclusive check or failed evaluation machinery |
| `user_correction` | Authenticated review decisions | `changes_requested` dispositions; comments and approvals are not corrections |
| `read_underfill` | Successful primary native read receipts | Returned fewer than the requested available bytes, capped at 4096 for this monitor |

Reconciled call/job/effect facts replace their previous contribution. A cursor
retry cannot add a second opportunity. Missing usage and cost are never filled
with zero. A failed provider call is an operational signal, not evidence that
code needs repair. Correlated outages and sparse cohorts remain uncertain.
Internal controller feedback and arbitrary user prose are not relabeled as human
corrections. Review dispositions are signals, not verified task grades.
Auxiliary calls without task reservations contribute recorded costs only; the
monitor does not claim complete cross-provider billing or subjective task grades.

Sampling status reports considered, selected, sampled-out, excluded-origin,
no-hypothesis and capacity skips. Considered/selected counts refer to underfilled
read receipts; origin skips count excluded task journal records. Selection does
not imply a new experiment: several receipts can share one stable episode.
No-hypothesis includes releases already handled by an existing episode. The monitor reads at most 64 journal records
per tick and tests one candidate at a time. These are background-work bounds,
not new limits on user task turns, spend or tool use.

## Automatic recovery scope

An underfilled read alone does not authorize a repair. Before writing a candidate,
the host requires all of the following:

1. The session's pinned release is still active. The tool admission recorded the
   same known executable build as the current evaluator.
2. Its immediate parent used a larger native read reply envelope.
3. The parent's capacity can fit the observed requested page, up to 4096 bytes.
4. This release has no prior episode for this signature.

The host stores the exact configuration diff, triggering receipt and journal
sequence, source-to-symptom hypothesis, minimized regression fixture and held-out
fixture digests before candidate creation. The regression uses synthetic bytes
at the observed boundary; it does not copy the user's private file content.

The candidate is one typed JSON field in a fresh private repair directory. It
cannot name commands, checks, expectations or files. Frozen fixtures stay in the
host artifact store, outside this directory. The compiled grader runs the real
native file tool in fresh fixture directories. It requires the regressed control
to fail, then checks exact bytes, digest, offset, size and truncation metadata for
the candidate. Held-out cases include empty, short, offset, escaped, binary and
UTF-8 reads. Two deterministic passes test repeatability; they are **not repeated
model comparisons**. Candidate or fixture digest changes reject evaluation.

A successful evaluation activates a new config version in one transaction with
its result and previous-version receipt. It does not rewrite an existing session.
A docs-only note with unchanged config cannot match the hypothesis. Provider
outages cannot create candidates. Other failures are recorded but not repaired.
This proof does not generalize to arbitrary tool behavior, code patches, prompts,
model choices, task correctness, or subjective quality.

## Recovery and evidence

Cursor advance, observation updates and episode intent commit together. Release
activation uses compare-and-swap. A crash after an evaluation starts leaves that
episode uncertain; restart never reruns it or retries old task effects. A diagnosed
but unstarted episode may proceed. A changed active release supersedes it.
Repair episodes and evaluation artifacts carry explicit origins. An episode's
stable release/signature identity prevents recursive repair loops.

A corrupt trace stops cursor advance rather than silently dropping evidence.
The worker reports the error and continues independently of user execution.
Reports and artifacts are local and may contain private provenance. No telemetry
export or automatic retention deletion is configured.

Run the controlled evidence with `cargo test -p orvek-harness --test monitor`,
`cargo test -p orvek-harness --lib controller::monitor`, and
`cargo test -p orvek --test monitor`. These use real Host/CLI paths and a local
scripted provider. No live-model comparison has run. The deterministic candidate
executes no arbitrary code and grants no model workspace authority, so it does not
claim Docker-certified repair-agent isolation or a general coding-agent path.
