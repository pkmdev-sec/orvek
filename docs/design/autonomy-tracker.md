# Orvek autonomy and harness tracker

Status: T02 and T03 implemented and verified. T01, T04, and the T09 hook phase are in progress. Baseline: `720210c0cbceebe89c123038049e9aebf77ba47d`. Sources inspected on 2026-09-20.

## Decision

Prioritize working host integrations and trustworthy execution records before adding more agent behaviors. Then build five capabilities: evidence-backed memory, executable traces, programmatic tool composition, model-directed context, and durable context-aware delegation.

“Robustness without guardrails” means fewer approval gates and less operator babysitting. It does **not** mean removing isolation, credential boundaries, explicit uncertainty, cancellation, or evidence-based completion. Those mechanisms let an agent act independently. Neither these sources nor the code establish that any harness can eliminate all external operational controls or guarantee safe arbitrary side effects.

This is an implementation tracker, not a feature announcement. Proposed tools, commands, types, and schemas below do not exist unless explicitly marked existing. No LangChain runtime dependency is proposed.

## Scope and evidence

The supplied list contains **15 direct blog links, two X links, and one repository**, rather than 16 direct blog links. Both X pages were readable and have matching LangChain articles. All 18 supplied links are covered below, using L01–L15, X01–X02, and D01. The provisional source selection made before the links arrived is not used as task evidence.

Navigation began with `CODEBASE.md`, `docs/codebase-graph/overview.md`, and `.agent-map/architecture.md`. The agent-map overlay is orientation, not proof of runtime wiring. Findings below follow actual callers and tool definitions. The companion [evidence manifest](autonomy-tracker.sources.json) records source URLs, content hashes, code anchors, dependencies, and statuses. Local source references O01–O29 are indexed at the end.

Deep Agents is pinned to `c3a041e3d8f593e4e4c9bfc273d52264ceff165b`. Its current implementation can differ from an older article. Article performance numbers are not Orvek results.

Verification at the research baseline:

- `cargo test -p orvek-harness --test subagents --test native_host --test session_journal`: 20 passed.
- `cargo test -p orvek-harness --test completion_contract`: 26 passed.
- `python3 -B -m unittest discover -s evals/snapcompact -p 'test_*.py'`: 6 passed.
- A local fixture calling the baseline `run_paired.parse_log` reproduced missing cached/reasoning token counts becoming zero. T03 records the regression target.
- Docker crash tests and live-model comparisons were not run during research. Subsequent implementation verification is recorded below.


### Verified implementation results

- **T02**, `87bc77f`: 11 deterministic child-execution tests pass. Four additional real-Docker tests pass: parent/child command conformance, host crash and exact-container fencing, receipt-commit loss without re-execution, and running-command cancellation. Crash recovery also preserves an unrelated container.
- **T03**, `757b6b4`: all 32 SnapCompact tests pass, including null measurements, failed/interrupted admissions, malformed logs, child failures, torn records, pairing controls, and report denominators. The original six tests were retained or migrated to schema v2; archived zero-filled records are not accepted as measured evidence.
- The existing Docker boundary suite also passes: 14 tests against Docker 29.7.2 with `debian:bookworm-slim` and the static ARM64 executor helper.

The manifest records rerun commands. No live-model or competitor comparison has run. Repository-wide integration checks remain pending while other work lands.

## Preserve these strengths

| Existing capability | Evidence | Keep this boundary |
| --- | --- | --- |
| Detached host, journal, revision checks, restart recovery | O11, O17; `controller.rs::Host::open_backend` | TUI and headless clients never become a second execution authority. |
| Intent-before-execution, unknown effects, reconciliation, content-addressed artifacts | O10, O11, O22 | An acknowledgement failure is not permission to repeat a side effect. |
| Protected checks on frozen candidates and evidence-based completion in sandbox mode | O12, O13 | A model, judge, hook, or memory cannot declare a failed candidate verified. |
| Native execution with the user's environment and explicit unverified completion | O20, O27 | Do not label native results sandbox-certified or silently start Docker. |
| Durable exact history, stable prompt segments, bitmap views, source retrieval, measured representation selection | O06–O08; context tests | Derived context must never replace authoritative history. Unknown measurements stay unknown. |
| Read-only child delegation, schema validation, scoped messaging and stored result blobs | O09, O21 | Extend delegation rather than add a competing agent loop. Lifecycle recovery is incomplete. |
| Local/remote memory, BM25 retrieval, versioned keys, transactional mutations, secret filtering | O04, O05; `crates/memory/src/tests.rs` | Keep the store and concurrency semantics. Fix host integration before redesigning retrieval. |
| Harbor, ATIF evidence, incident replay fixtures, paired context evaluations | O14, O15, O23, O24 | Reuse these evaluators. Do not replace them with a mandatory hosted observability service. |

Two important qualifications: `docs/memory.md` describes agent memory operations, but current host tool lists do not expose them. Also, context history is lossless in storage, but `context::project` can still omit old items from the model view with no summary. These are different claims, not contradictions to hide.

## Source innovation catalog

“Existing” means no new feature ticket. “Improve” means an existing mechanism has a specific integration or quality gap. “New” means a missing capability in the inspected Orvek path, not an industry invention.

| Source | Distilled pattern | Orvek mapping and disposition |
| --- | --- | --- |
| [L01: The art of loop engineering](https://www.langchain.com/blog/the-art-of-loop-engineering) | Separate execution, verification, event-triggered work, and trace-driven improvement loops. | Execution and sandbox verification already exist. Add durable terminal delivery and event intake in T09; measured outer-loop improvement in T10. Do not add a mandatory human gate or another inner loop. |
| [L02: How to build a custom agent harness](https://www.langchain.com/blog/how-to-build-a-custom-agent-harness) | Bundle lifecycle, tools, context, and state at explicit extension points. Fit the harness to the task. | Improve integration in T01 and T09. Start with typed host-owned services, not a general mutable middleware stack around the controller. |
| [L03: Give your agents an interpreter](https://www.langchain.com/blog/give-your-agents-an-interpreter) | Keep live working values outside prompts; compose tools in code; return selected evidence. | New host tool-composition capability, T06. Shell execution already exists and is not the same feature. Preserve tool identities and receipts across the interpreter bridge. |
| [L04: The agent development lifecycle](https://www.langchain.com/blog/the-agent-development-lifecycle) | Version behavior, evaluate changes, deploy, observe quality, and feed results back. | Durable runtime exists. Improve the lifecycle link among evidence, evaluations, and behavior versions in T04/T10. Retain `HarnessBinding` rather than invent a separate policy authority. |
| [L05: Our docs test themselves](https://www.langchain.com/blog/our-docs-test-themselves) | Extract runnable examples with setup/teardown; generate documentation from tested code. | Improve `scripts/check-docs.py`, which checks syntax and links but does not execute examples. T11 starts with current memory/skills wiring claims. |
| [L06: Your harness, your memory](https://www.langchain.com/blog/your-harness-your-memory) | User-owned, inspectable memory and context preserve model/provider choice. | Local custody already exists. T01 makes memory usable by the host; T05 preserves scope and provenance during transfer. Do not infer competitor limitations from this opinion article alone. |
| [L07: How my agents self-heal in production](https://www.langchain.com/blog/how-my-agents-self-heal-in-production) | Compare error signatures before/after releases; establish a causal diff link before asking a coding agent to fix anything. | New analysis-to-repair workflow, T10. Normalize by exposure, separate provider incidents, retain uncertainty. Article opens repair PRs; it does not prove automatic live self-rewriting is reliable. |
| [L08: Traces start the agent improvement loop](https://www.langchain.com/blog/traces-start-agent-improvement-loop) | Turn labeled traces into regression cases; choose code, tool, prompt, or memory fixes from observed failure modes. | Improve existing journal/eval assets, T03/T04/T10. A trace is evidence, not automatically a durable lesson. |
| [L09: Agent evaluation readiness checklist](https://www.langchain.com/blog/agent-evaluation-readiness-checklist) | Grade actual state changes; separate capability/regression and infrastructure/task failures; use isolated repeated trials and N−1 conversation prefixes. | Existing task evidence and Harbor already cover much of this. Fix unknown-value and failed-attempt accounting in T03; add prefix replay and paired qualification in T04/T10. Do not grade a prescribed tool sequence. |
| [L10: Autonomous context compression](https://www.langchain.com/blog/autonomous-context-compression) | Let the model request a context transition at a useful task boundary, rather than only at a size threshold. | New model-facing control over existing host-owned views, T07. Keep original bytes and contracts. Do not copy destructive message replacement or an 85% trigger. |
| [L11: The anatomy of an agent harness](https://www.langchain.com/blog/the-anatomy-of-an-agent-harness) | General code execution, durable files, delegation, retrieval, and self-verification extend model capability. | Most components already exist. T01 closes skill delivery; T02/T08 improve delegation. No ticket to “add bash,” “add checkpointing,” or “add verification.” |
| [L12: How coding agents are reshaping engineering, product and design](https://www.langchain.com/blog/how-coding-agents-are-reshaping-engineering-product-and-design) | Cheaper prototypes shift effort to review; retain versioned intent beside generated artifacts. | Existing contracts and review snapshots are the base. T04 packages intent, patch, checks, and limitations together. No proposed product/design role automation. |
| [L13: Production monitoring](https://www.langchain.com/blog/production-monitoring) | Monitor task quality and correction patterns, not just uptime; use sampled asynchronous evaluation and clustering. | New cross-run quality analysis in T10, over local T04 traces. Evaluator outages must not stop accepted tasks. Export remains optional. |
| [L14: How we built Agent Builder's memory system](https://www.langchain.com/blog/how-we-built-agent-builders-memory-system) | Familiar files over durable storage, schema validation, procedural learning, and consolidation instead of endless additions. | Improve existing skills/memory in T01/T05. Keep SQLite ownership and CAS. Portable file views can be adapters; do not replace the store or import the article's approval-per-memory-write default. |
| [L15: In software, code documents the app; in AI, traces do](https://www.langchain.com/blog/in-software-the-code-documents-the-app-in-ai-the-traces-do) | Trace bundles explain observed behavior and enable decision-point debugging. | Improve O14/O17 into T04. Traces document observable actions and available rationale, not hidden model reasoning or unproven causal explanations. |
| [X01: Organizing context in a multi-agent harness](https://x.com/LangChain_OSS/status/2097372136247902519), [article](https://www.langchain.com/blog/organizing-context-in-a-multi-agent-harness) | Fork inherited context for continuation workers; isolate independent reviewers/researchers. | New explicit context mode in T08. Orvek children currently start without parent history. Preserve branch cutoffs and do not share a mutable conversation. |
| [X02: Building self-correcting memory](https://x.com/colifran_/status/2092280107033616451), [article](https://www.langchain.com/blog/self-correcting-memory-openwiki) | Bind claims to versioned evidence; source changes imply stale, not false; uncertainty persists until rechecked. | New evidence freshness for O04 memory, T05. Read-frequency probation is not factual validation. Do not transfer OpenWiki's reported improvement percentages to Orvek. |
| [D01: Deep Agents source](https://github.com/langchain-ai/deepagents/tree/c3a041e3d8f593e4e4c9bfc273d52264ceff165b) | Implemented composition, context modes, compaction tool, memory injection, interpreter state and snapshot integrity. | Concrete reference for T01/T06/T07/T08/T09. Important implementation caveats follow. |

### What the pinned Deep Agents code actually supports

All paths below are relative to the pinned D01 repository.

- `libs/deepagents/deepagents/graph.py::create_deep_agent` assembles middleware. `DeepAgentState` uses delta channels for messages. Orvek already has journal/checkpoint infrastructure; do not add delta persistence merely because it appears here.
- `middleware/memory.py::MemoryMiddleware.before_agent` loads configured files only when `memory_contents` is absent. `modify_request` injects them. Do not assume same-session refresh follows automatically from editing a file. T01 must record which context version each request actually used.
- `middleware/subagents.py::_fork_messages` uses effective parent history and removes the final tool-calling message. T08 needs equivalent protocol-pair handling, plus Orvek's existing source-cursor authorization.
- `middleware/summarization.py::create_summarization_tool_middleware` implements model-requested compaction. Borrow the control point, not its exact storage design.
- `libs/partners/quickjs/langchain_quickjs/middleware.py::CodeInterpreterMiddleware` distinguishes call, turn, and thread lifetimes. Its documentation explicitly says programmatic tool calls bypass the normal ToolNode path. Orvek must route each bridged call through its existing admission and receipt path instead.
- That interpreter's timeout measures VM time, not time awaiting host tools. Snapshots can fail or be discarded. Optional snapshot HMAC binds data to a slot identity; `_snapshot.py` also implements delta snapshots. T06 must define wall-time cancellation and state-loss behavior, not promise transparent recovery of live resources.

## Ranked implementation queue

T02 and T03 are complete. T01, T04, and the T09 hook phase are in progress in isolated worktrees. “Ready” means the next step is sufficiently specified, not that implementation is verified. Scores are prioritization judgments, not measurements: empowerment and competitive value each range from 1 to 5. Order also respects correctness prerequisites. Effort is relative: S is a narrow fix, M spans a subsystem, L crosses persistence/runtime boundaries.

| Rank | ID | Deliverable | Empowerment / edge | Effort | Status | Dependencies |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | T01 | Host-visible memory and skill context | 5 / 4 | M | In progress | None |
| 2 | T02 | Consistent child execution identity and outcome accounting | 5 / 4 | M | Done | None |
| 3 | T03 | Honest evaluation records, including failed attempts | 4 / 5 | S | Done | None |
| 4 | T04 | Portable causal trace bundles and offline replay | 5 / 5 | L | In progress | T02, T03 |
| 5 | T05 | Scoped, evidence-backed, self-correcting memory | 5 / 5 | L | Planned | T01, T04 |
| 6 | T06 | Persistent interpreter over host-admitted tools | 5 / 5 | L | Planned | T02, T04 |
| 7 | T07 | Model-directed, lossless-source context transitions | 5 / 4 | M | Planned | T03, T04 |
| 8 | T08 | Durable child lifecycle, valid results, explicit context modes | 5 / 4 | L | Planned | T02, T04 |
| 9 | T09 | Working terminal hooks and durable event intake | 4 / 3 | M | Hook fix in progress; intake later | None for hook fix; T02, T04 for intake |
| 10 | T10 | Trace-driven quality monitoring and autonomous repair experiments | 5 / 5 | L | Planned | T03, T04, T09 |
| 11 | T11 | Executable capability documentation | 4 / 3 | M | Ready for inventory | T01, T09 for end-to-end examples |

### T01: Make configured memory and skills reach the model

**Evidence:** O01–O04, O18, O20; L02/L06/L11/L14 and D01. `ConfiguredSession::admit` validates memory configuration and discards the selected store. It reduces the rendered skill catalog to UI metadata. Neither native nor sandbox tool definitions expose memory. `MemoryTool` exists in the memory crate, but has no production instantiation in the inspected first-party code.

**Decision:** Keep `orvek-memory` responsible for storage. Install a host-owned service and protocol adapter, not a TUI callback or a dependency on terminal configuration types. Reuse scan/read/put/delete semantics. Deliver skill metadata through a versioned context manifest; load bodies on demand. Document supported instruction files explicitly rather than claiming automatic `AGENTS.md` loading from the mere presence of that filename in a repository.

- [ ] First reproduce with a fake provider through host IPC: memory enabled and a discoverable skill must appear in the actual request/tools. Cover native tasks, sandbox tasks, and auxiliary conversation separately.
- [ ] Resolve backend identity at the application boundary; pass an owned service to the host. Do not journal credentials or serialize a token into session state.
- [ ] Record catalog/memory version references used by each request. Refresh at a recorded turn boundary; preserve old manifests for replay and invalidate only affected cache identity.
- [ ] Expose discovery failures instead of silently discarding production `SkillDiagnostic` values. A bad optional skill must not erase valid skills.
- [ ] Verify two sessions can retrieve the same intended memory, disabled memory is absent, stale writes conflict, and remote failure never silently switches to a local corpus.

**Done:** a fresh headless process uses an enabled memory and selected skill after host restart, without a TUI or manual prompt paste. Reconcile `docs/memory.md` with the proven behavior. Do not start by replacing BM25 or increasing arbitrary record limits.

### T02: Preserve one execution identity through child tools

**Evidence:** O09/O10/O21/O22; L04/L09. Baseline defect, fixed and tested against real Docker in `87bc77f`. `ChildLoop::run_tool` previously journaled a job and an environment containing only `runner: subagent`. `WorkspaceChildTools::execute` generated different task/job IDs and generation zero. Recovery expects the original Docker environment and recorded job identity. The adapter also lost execution metadata and turned uncertainty into an ordinary error.

**Decision:** Pass the admitted `ToolContext` and backend identity into child execution. Return the same typed recorded result as parent execution. Never invent a second job identity in an adapter.

- [x] Add a deterministic identity regression before the fix. Assert journal job, executor job, receipt job, task, and generation agree.
- [x] Retain execution status, diagnostics, and unknown outcomes. A nonzero shell exit must not become a succeeded job merely because JSON was returned.
- [x] Kill the host after child dispatch; reopen against the original Docker backend; reconcile the exact original container. Test cancellation and receipt-commit failure. Never replay the uncertain command.
- [x] Apply a shared scenario table to parent native, parent sandbox, and child dispatch: intent-before-execution, receipt identity, error/unknown distinction, and no late settlement. Declare intentional backend differences explicitly.

**Done:** the real child command is fenced or remains explicitly unknown after restart. Unrelated containers are untouched. Native unknown jobs retain the existing no-replay behavior in O27. General native-network reconciliation is not solved by this patch.

### T03: Fix the measurement contract before hill climbing

**Evidence:** O23/O24; L08/L09/L13. Baseline defects, fixed in `757b6b4`: `parse_log` converted null token counts to zero; reports required numbers; nonzero process exits raised before writing an attempt; some child/retrieval fields were hardcoded zero.

**Decision:** Use measured value versus unknown throughout parsing, raw records, and aggregation. Preserve every attempt, including infrastructure error and interrupted run. Only derive zero from recorded absence.

- [x] Add a fixture with input/output counts present and cached/reasoning counts null. Assert null remains null in raw and aggregate output.
- [x] Add nonzero-exit, missing receipt, failed-child, and partial-log fixtures. Each produces a typed attempt record before the runner returns failure.
- [x] Version the raw schema and migrate report readers together. Do not quietly reinterpret historical zero-filled data as measured evidence.
- [x] Report completed, failed, interrupted, and invalid-evaluation denominators separately. Pair trials by dataset, task digest, model/settings, harness build, tool access, and environment.

**Done:** deterministic tests prove unknown is not zero and failed attempts cannot disappear from a comparison. Existing six SnapCompact tests continue to pass. This is a prerequisite for any cost, quality, or superiority claim.

### T04: Make traces portable, causal, and executable

**Evidence:** O11/O14/O15/O17; L08/L09/L12/L15. Journal and artifact read APIs, headless JSONL, and Harbor ATIF already exist. The Harbor exporter currently combines a task into a coarse agent step. Child cost records do not provide complete child-call attribution; export alone cannot reconstruct missing causality.

**Decision:** Add a read-only trace bundle over a pinned journal range and its artifact closure. Include session/request/task/model-call/job/child IDs, source revision, model settings, context/tool/behavior digests, candidate identity, verification results, and explicit gaps. Keep native unverified outcomes distinct from certificates.

- [ ] Define a versioned manifest and bounded artifact traversal. Record missing or deliberately omitted payloads. Do not upload traces by default.
- [ ] Record missing child causal links at execution time. Split model/tool/check spans without manufacturing intermediate rationale.
- [ ] Implement offline event/receipt replay with a provider/tool stub. No network calls or command execution in replay mode. Compare state, selected context, and outcome identities.
- [ ] Add a separate experimental re-execution path in a fresh environment. Reusing old tool outputs is diagnostic replay, not proof that changed code works.
- [ ] Build N−1 prefix fixtures and a review packet containing user intent, contract, patch, verification, cost confidence, and unresolved facts.
- [ ] Test child work, retries, rejected completion, successful repair, tampered blobs, missing artifacts, and cursor gaps. Export filtering must mark a bundle non-exact when payloads are removed.

**Done:** another local installation explains and replays the same recorded task offline. A redacted review bundle cannot masquerade as an exact replay bundle. Provider-hidden reasoning and unrecorded external state remain unavailable.

### T05: Give memory scope, evidence, and a refresh path

**Evidence:** O04/O05, T01; L06/L14/X02. Local schema has no repository scope or evidence references. Remote import deliberately loses namespace provenance. Probation ends on read, which measures use, not truth.

**Decision:** Extend the existing versioned store with explicit global/repository scope, origin, and evidence references. Treat preference, procedural instruction, and code-backed claim differently. For a code claim, freshness derives from persisted source identity and current content, not from a self-reported confidence score. Changed evidence means stale, not false.

- [ ] Migrate without relabeling old unscoped records as repository facts. Preserve legacy records as unscoped/unverified and preserve origin on future imports.
- [ ] Store source path/range or artifact digest, repository identity, checked revision/content digest, and producing trace. Use content identity for dirty worktrees; a Git commit alone is insufficient.
- [ ] Surface stale/unavailable evidence on scan/read. Recheck changed sources only; an unrelated edit must not force a full-model review of the corpus.
- [ ] Refresh or correct content and evidence atomically with CAS. An interrupted refresh remains stale. A read does not certify a claim.
- [ ] Add an asynchronous post-run lesson proposal that links to evidence and a behavior test. Merge repeated lessons; do not store whole transcripts or silently rewrite managed instruction files.
- [ ] Export/import a portable manifest plus readable records. Verify namespaces, provenance, versions, and citations survive transfer; keep ordinary file views as adapters.

**Done:** a commit sequence containing a feature change, unrelated edit, deletion, and revert produces correct stale/verified/unknown states. A second model can reuse the same store without relearning preferences. A false tool-output instruction remains data and cannot gain host authority through a memory write. No approval-per-memory-write loop is introduced.

### T06: Compose tools in a persistent interpreter

**Evidence:** O03/O10/O21; L03 and D01 QuickJS code. Native/sandbox shell tools can already execute code. They do not provide persistent in-loop values with typed bridges to host services.

**Decision:** Prototype a small interpreter as a host-owned capability. Keep model reasoning in the model, large working values in the interpreter, and durable artifacts in the artifact store. Do not mandate QuickJS until embedding, cancellation, and persistence tests justify it. This adds a tool; it does not remove shell access.

- [ ] Route every bridged tool call through the same dispatch, task/job identity, cancellation, and receipt code as ordinary tools. An outer `eval` receipt cannot stand in for its inner side effects.
- [ ] Start with read/search/context retrieval and child orchestration. Enable writes only through the already-admitted capability path, not ambient host access.
- [ ] Give each session an isolated runtime. Specify which values persist across calls, turns, and restart. Initially checkpoint explicit serializable values; invalidate live handles rather than serializing processes, sockets, or credentials.
- [ ] On restore, verify version/source/digest and restore recorded values only. Never rebuild state by rerunning effectful cells. Mark state loss explicitly and let the model reconstruct pure computations.
- [ ] Make long tool waits resumable through handles and host events. VM execution limits do not bound host-call wall time. Preserve cancellation and resource controls without adding a blanket task call/spend cap.
- [ ] Compare serial tool use against programmatic composition on the same structured-data and delegation tasks. Measure correctness, model round trips, context bytes, total provider cost, and latency.

**Done:** one program filters a large result, delegates selected slices, and returns cited evidence without inserting every intermediate value into the prompt. Restart never duplicates an external action. L03's reported token savings are motivation, not an acceptance target.

### T07: Let the model choose context transitions without losing source

**Evidence:** O06–O08; L10. `project` currently cuts old complete tool pairs to fit a byte allowance and inserts an omission notice. Exact history remains retrievable. Continuous bitmap conversion and cache-aware accounting already exist.

**Decision:** Add a model-requested transition for a settled history range, with a purpose such as “research complete; implementation begins.” Store a derived task-state summary or evidence index linked to original bytes. Preserve the native live tail, original request, and protected contract. The model proposes a view, not edits to task truth.

- [ ] Test useful phase boundaries and negative cases where a transition would discard active work. Do not force compaction after an arbitrary number of turns.
- [ ] Record summary provenance, covered source ranges, pending obligations, and retrieval links. Validate ranges and protocol pairs, not the truth of arbitrary summary prose by assertion.
- [ ] Integrate with existing stable segments, bitmap selection, and cache identity. Measure cache loss and extra summary/retrieval calls, not only fewer input tokens.
- [ ] Keep prior/native view available on failure. Provider-size recovery still obeys actual provider limits; do not claim fallback always fits.
- [ ] Force frequent transitions in isolated evals, then test goal retention, exact needle retrieval, and continuation after restart/fork. Keep stress settings out of production defaults.

**Done:** the model can request a phase transition and later retrieve omitted source byte-for-byte. It does not declare completion because obligations disappeared. Ship only with paired quality and full-cost evidence; no new runtime spend cap or forced task stop.

### T08: Make delegation durable and choose context deliberately

**Evidence:** O09/O21/O28/O29; X01 and D01. Child registry is in memory and starts empty after host restart. Result blobs exist without a complete durable lifecycle link. Children use the parent's selected model, run read-only in sandbox mode, and receive no parent history. Prose fallback returns `submitted: false` but is still treated as completed. The caller's output schema is compiled into a validator but is not delivered in the child instructions or generic `submit_result` tool definition.

**Decision:** Preserve the existing child manager. Separate `schema-valid result`, `unsubmitted answer`, `interrupted`, and `failed` outcomes. Add `isolated` versus `fork_at_cursor` context choice, independent of tool permissions. Independent reviewers should default to isolated context.

- [ ] Retain and deliver the caller's output schema in the child-facing tool definition or context before the first model response. Capture the provider request and assert a caller-specific required field and enum are present.
- [ ] Fix prose-as-completed behavior and feed schema errors back as actionable observations within existing task resources. Test an invalid submission followed by a valid resubmission. Repeated failures provide diagnostic context, not a new mandatory stop heuristic.
- [ ] Journal spawn, message acceptance/consumption, completion, and result digest. Rebuild list/wait snapshots after restart; mark interrupted work instead of blindly respawning it.
- [ ] Test crashes after spawn, after result storage, and before terminal publication. Deduplicate by durable event identity.
- [ ] Add a pinned parent cursor and effective context manifest for forks. Exclude unfinished tool pairs and later parent records. Test independent reviewer context contains no parent conclusions unless explicitly supplied.
- [ ] Pin workspace evidence for child reads or disclose the observed generation. A shared live workspace is not a frozen review snapshot.
- [ ] Compare duplicated discovery and result quality for isolated versus inherited-context workers. Keep current sandbox-only/read-only behavior in the first release. Writable worktrees and heterogeneous child models require separate evidence and designs.

**Done:** a completed child remains discoverable after host restart. Repeated list/wait calls return the same schema-valid result and digest, with one durable terminal transition per child. Test repeatable reads before and after restart. Fork context never grants extra execution authority. No claim of process-resumable children until that behavior is separately implemented.

### T09: Deliver terminal hooks reliably, then accept events

**Evidence:** O25/O26/O17; L01/L02. `completion_hook` parses and affects configuration identity but has no execution consumer in the inspected host. Generic middleware would not fix that wiring defect by itself.

**Decision:** First implement the configured terminal notification through one host-owned post-settlement path, or explicitly reject unsupported configuration until it exists. Hook failure cannot change task verification. Only after this works, add event-driven submissions through the existing host queue.

- [ ] Add a failing headless/TUI parity test for the configured hook. Define covered terminal outcomes, output handling, timeout, and cancellation.
- [ ] Persist delivery intent and result with a stable delivery ID. Offer idempotency keys to receivers. An arbitrary shell hook cannot promise exactly-once external effects after lost acknowledgement.
- [ ] Treat ambiguous delivery as unknown; retry automatically only when the receiver can deduplicate or the action is demonstrably repeatable.
- [ ] For later schedule/webhook intake, persist trigger identity, deduplication key, payload digest, selected session, and submission receipt. Reuse the admission queue rather than run a second controller.
- [ ] Define catch-up after downtime, trigger coalescing, and cancellation. Local adapters first; no public unauthenticated server is implied.

**Done:** reconnect does not rerun a settled notification. A repeated event submits one logical task. Event automation needs no per-event human approval, but cannot authorize tools beyond the configured host authority.

### T10: Close the improvement loop without live self-corruption

**Evidence:** O14/O15/O19 and T03/T04; L01/L04/L07/L08/L09/L13.

**Decision:** Build an asynchronous consumer of durable traces. Separate operational anomalies, task-quality failures, and evaluator failures. Generate a candidate fix in an isolated workspace; test it against both the triggering case and held-out regressions. Existing tasks stay pinned to their admitted behavior. Never let a repair agent weaken the grader to make its own patch pass.

- [ ] Start with known signatures and denominators: job outcome, provider error, unresolved effect, verification failure, user correction, and missing usage. Keep sampling decisions and skipped evaluations observable.
- [ ] Compare matched model/build/environment cohorts and failures per relevant opportunity, not raw counts from unequal traffic windows. Label sparse data and provider outages as uncertain.
- [ ] Require a source/diff-to-symptom hypothesis before coding. L07's Poisson test assumes independence; use it only when justified and account for multiple signatures and correlated failures.
- [ ] Produce a regression fixture, then a candidate code/tool/prompt/memory change. Run deterministic checks and isolated repeated live comparisons where behavior requires a model. Keep evaluation data separate from training/tuning inputs.
- [ ] Promote versioned behavior/configuration automatically only after its predeclared outcome checks pass. Publish code repairs as tested patch artifacts; installing a new host build is a separate release operation, never an in-place rewrite of a running host.
- [ ] Record the previous version and support rollback for new sessions. Do not rewrite in-flight contracts, tool schemas, or replay evidence.

**Done:** an injected release regression creates a diagnostic trace, regression test, tested candidate, and reversible behavior update without an approval gate. A provider outage and unrelated docs-only change do not produce a speculative code fix. Monitor failure leaves user tasks running. Subjective quality with no reliable evaluator remains unproven, not automatically “passed.”

### T11: Make capability docs prove real host behavior

**Evidence:** O01/O02/O16/O25; L05/L12. Current documentation checks parse examples and validate links. They do not prove the displayed memory operations or configured hooks reach the host.

**Decision:** Extract a small set of executable examples and generate their documentation snippets from those files. Use fake providers for deterministic wiring tests; isolate external-service examples. Do not introduce a large docs framework.

- [ ] Inventory examples as runnable, illustrative, or external-service. Require an explicit reason for non-runnable examples.
- [ ] Start with memory scan/read, skill discovery, native unverified completion, sandbox verified completion, reconnect, and terminal hook delivery.
- [ ] Run examples against public CLI/IPC boundaries with setup and teardown. Assert actual resulting state, not “Done” text.
- [ ] Generate snippets; check generated content in CI alongside existing syntax/link checks. Link each example to a sanitized trace bundle once T04 exists.

**Done:** documentation cannot advertise enabled memory or hook behavior when the real host does not provide it. Examples remain short enough to read and rerun.

## Five differentiators to pursue

These are hypotheses to prove, not claims that Codex, Claude Code, or OpenCode lack similar features. [Codex's repository](https://github.com/openai/codex) confirms a local CLI. [Claude Code hooks](https://code.claude.com/docs/en/hooks) already expose extensive lifecycle customization. [OpenCode plugins](https://opencode.ai/docs/plugins/) already expose event/tool extensions. Deep Agents already combines memory, context, delegation, and interpreters. “Has hooks/memory/subagents” is not a differentiator.

| Priority | User-visible advantage | Work | Comparative proof required |
| --- | --- | --- | --- |
| 1 | The agent remembers a lesson, detects when its evidence changes, and carries it to another model. | T01/T05 | Repository commit/revert sequence plus provider switch. Measure stale-claim use, correct recall, correction burden, and preservation of provenance. |
| 2 | Every result comes with a local, replayable account of what ran, changed, failed, and was verified. | T02/T03/T04/T10 | Same injected failure/restart tasks. Measure duplicate effects, recoverable evidence coverage, diagnosis time, and operator interventions. |
| 3 | The agent programs its own tool workflow without paying a model round trip for every intermediate value. | T06 | Same structured-data and delegated tasks, same tools/model where supported. Compare correct outcomes, latency, context bytes, and total root/child cost. |
| 4 | Long tasks retain intent and exact source while the model controls useful context transitions. | T07 | Long refactor/research tasks with follow-ups and forced context pressure. Measure task quality, exact retrieval, goal drift, cache behavior, and total cost. |
| 5 | Delegation survives client and host loss, with informed workers and independent reviewers. | T02/T08 | Restart and fork/isolated-context matrix. Measure retained results, rediscovery work, false completion, and review error detection. |

Run two comparisons: a same-model harness comparison where products permit it, and a best-supported-product comparison with different models clearly disclosed. Pin product versions, tasks, access, environments, and attempt counts. Use fresh state per independent trial, repeated trials, and confidence intervals. Report unsupported scenarios and unavailable metrics as such. No competitor execution or head-to-head benchmark was performed in this analysis; none of the five advantages is established superiority yet.

## Design decisions and rejected shortcuts

| Decision | Rationale and constraint |
| --- | --- |
| One host writer and authority path | Memory adapters, interpreter bridges, hooks, and triggers must compose with the controller, not bypass it. |
| Learning changes future context, not task truth | Evidence-backed memory cannot override current instructions, accepted contracts, credentials, or verification results. |
| Immutable source; replaceable views | Summaries, bitmap pages, and interpreter values are derived. Keep exact history and source identity. |
| No new approval layer | Use existing authority plus inspectable receipts and reversible changes. The articles' HITL recommendations are not adopted as defaults. |
| No new blanket turn, spend, or time cap | Optimize representation and orchestration. Retain existing resource controls and cancellation; do not disguise a stop heuristic as autonomy. |
| Native and sandbox are different products of the same host | Share receipt invariants, not false security/certification equivalence. Native process cleanup cannot prove a remote side effect did not occur. |
| Retry according to effect identity | Retry pure computation and demonstrably idempotent operations. Unknown arbitrary effects remain unknown until evidence resolves them. No universal exactly-once claim. |
| Typed services before general middleware | Implement memory and terminal delivery first. Extract a shared lifecycle API only after real consumers demonstrate its shape; hook code cannot mutate authoritative history arbitrarily. |
| Local traces first | Optional external exporters consume local records. No mandatory LangSmith, remote telemetry, or copying secrets into traces. A digest checks integrity, not confidentiality. |
| Evaluate outcomes, not obedience to a workflow | A creative correct solution passes. Efficiency is a separate metric. Use deterministic checks when available and disclose judge uncertainty. |
| No model-weight training project | Sources motivate learning, but Orvek lacks evidence here for an RL training pipeline. Context/tool improvements are the immediate work. |
| Do not erase existing strengths for novelty | No replacement journal, generic checkpoint framework, vector store by default, new sandbox stack, or generic Ralph loop. |

## Self-directed execution protocol

1. Select the highest-ranked item whose dependencies are complete. T01, T02, T03, the T09 hook fix, and the T11 inventory can start independently; avoid concurrent edits to shared controller paths.
2. Update status to `in_progress` and record the owner, baseline revision, next command, and evidence location in the source manifest's work item. No human approval is needed to start reversible local work.
3. For defects, create the failing regression revision first, implement in a child revision, verify, and squash as required by `AGENTS.md`. Use Conventional Commit descriptions. For features, keep tests with the meaningful change.
4. Run the narrow acceptance tests, then repository format/lint/test checks appropriate to the touched paths. Docker and provider-dependent acceptance stays explicitly pending until run. Compilation alone cannot close a behavioral item.
5. Record actual results, failed attempts, and unresolved uncertainty. Mark an item `done` only when its work-card acceptance criteria hold. A source inspection or passing old test is not evidence the new behavior exists.
6. Refresh affected docs and navigation graphs after layout or module changes. Recheck code anchors rather than blindly updating their hashes.
7. If evidence falsifies a proposal, mark it `rejected` with the result. If an external dependency blocks it, record the blocker and continue with the next independent item.

T02 and T03 landed first, with failing-before and passing-after tests. T01 remains in progress. Continue by dependency order; do not infer overall autonomy or comparative quality from these foundational fixes.

## Local code evidence index

These anchors identify the latest reviewed source, not permanent line-number APIs. For changed files, the manifest retains the research hash and line alongside the implementation review. The checker detects new file/anchor drift and requires another review.

| ID | Source and anchor |
| --- | --- |
| O01 | [bin/orvek/src/core/mod.rs](../../bin/orvek/src/core/mod.rs) at line 71, `configured_memory_store(config, &workspace)?;` |
| O02 | [bin/orvek/src/core/extensions/skills.rs](../../bin/orvek/src/core/extensions/skills.rs) at line 188, `pub(crate) fn rendered_instructions` |
| O03 | [crates/harness/src/controller.rs](../../crates/harness/src/controller.rs) at line 3421, `fn tool_definitions(discovery: bool)` |
| O04 | [crates/memory/src/model.rs](../../crates/memory/src/model.rs) at line 47, `pub struct MemoryRecord` |
| O05 | [crates/memory/src/store/local.rs](../../crates/memory/src/store/local.rs) at line 278, `pub async fn merge_remote_export(` |
| O06 | [crates/harness/src/context.rs](../../crates/harness/src/context.rs) at line 298, `pub fn project(` |
| O07 | [crates/harness/src/context.rs](../../crates/harness/src/context.rs) at line 199, `pub fn read_text_page(` |
| O08 | [crates/harness/src/context_cost.rs](../../crates/harness/src/context_cost.rs) at line 156, `pub fn recommendation(` |
| O09 | [crates/harness/src/controller/subagents.rs](../../crates/harness/src/controller/subagents.rs) at line 453, `async fn spawn(` |
| O10 | [crates/harness/src/capabilities.rs](../../crates/harness/src/capabilities.rs) at line 90, `pub fn requires_reconciliation(` |
| O11 | [crates/harness/src/store.rs](../../crates/harness/src/store.rs) at line 99, `pub struct Store` |
| O12 | [crates/harness/src/completion.rs](../../crates/harness/src/completion.rs) at line 15, `pub fn evaluate(` |
| O13 | [crates/harness/src/verification.rs](../../crates/harness/src/verification.rs) at line 17, `pub struct CheckProgram` |
| O14 | [evals/harbor_adapter/evidence.py](../../evals/harbor_adapter/evidence.py) at line 62, `def populate_context(` |
| O15 | [evals/incident_replay/generate.py](../../evals/incident_replay/generate.py) at line 381, `def generate_tasks(` |
| O16 | [scripts/check-docs.py](../../scripts/check-docs.py) at line 2, `without running them` |
| O17 | [bin/orvek/src/app/headless.rs](../../bin/orvek/src/app/headless.rs) at line 26, `orvek.host` |
| O18 | [crates/harness/src/controller/auxiliary.rs](../../crates/harness/src/controller/auxiliary.rs) at line 279, `let instructions = format!(` |
| O19 | [crates/harness/src/admission_profile.rs](../../crates/harness/src/admission_profile.rs) at line 87, `pub struct HarnessBinding` |
| O20 | [crates/harness/src/controller.rs](../../crates/harness/src/controller.rs) at line 3370, `fn native_tool_definitions(` |
| O21 | [crates/harness/src/controller/subagents.rs](../../crates/harness/src/controller/subagents.rs) at line 955, `async fn run_tool(` |
| O22 | [crates/harness/src/controller.rs](../../crates/harness/src/controller.rs) at line 3023, `async fn reconcile_unresolved(` |
| O23 | [evals/snapcompact/run_paired.py](../../evals/snapcompact/run_paired.py) at line 36, `def parse_log(` |
| O24 | [evals/snapcompact/paired_report.py](../../evals/snapcompact/paired_report.py) at line 30, `def validate(` |
| O25 | [bin/orvek/src/app/config.rs](../../bin/orvek/src/app/config.rs) at line 2072, `fn completion_hook_can_be_configured(` |
| O26 | [bin/orvek/src/app/host.rs](../../bin/orvek/src/app/host.rs) at line 422, `config.agent().completion_hook()` |
| O27 | [crates/harness/tests/native_host.rs](../../crates/harness/tests/native_host.rs) at line 813, `restart_keeps_unknown_native_jobs_unfenced_and_never_replays_them` |
| O28 | [crates/harness/src/controller/subagents.rs](../../crates/harness/src/controller/subagents.rs) at line 932, `"submitted": false` |
| O29 | [crates/harness/src/controller/subagents.rs](../../crates/harness/src/controller/subagents.rs) at line 1098, `fn child_definitions(` |

## Rerun this research check

```sh
python3 scripts/check-autonomy-tracker.py
python3 scripts/check-docs.py
python3 scripts/generate-codebase-graph.py --check
```

The tracker checker validates source-file hashes and anchors, source coverage, work-item IDs, and dependency ordering. It does not verify article claims, run feature acceptance tests, or establish comparative performance. External page hashes identify the text inspected; online freshness requires rereading those sources.
