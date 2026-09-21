# Configuration

Use `orvek config path` to find the selected file. Use `orvek config show` to print effective
settings with secrets redacted. The file is optional.

## Capabilities

### Configuration, authentication, and skills

Orvek reads an optional TOML file and applies CLI overrides before environment and file settings.
Use the [path and precedence reference](#paths-and-precedence) to select a configuration directory.
[Authentication](#authentication) supports shared Codex credentials, environment API keys, and an
executable helper for short-lived keys. The default host runtime runs tools with your permissions;
see [Agent settings](#agent-settings) for the optional Docker runtime and model controls.

[Skills](#skills) add trusted local `SKILL.md` instructions. Skills can direct tool execution, so
review their trust requirements before enabling them.

### Sessions, review, and reflection

The detached host keeps authoritative session history and an append-only journal. Use `orvek resume`
to choose a session or `orvek --resume SESSION_ID` to open one directly. `Ctrl+T` forks stable history
into a second pane with independent later work. Only one fork can be open at a time. Stored sessions
can contain unredacted conversation data.

Enter `/review` while idle to review the branch diff in a browser. Inline and general feedback returns
to the composer for editing before you send it. Source builds need separately installed browser
assets. Closing the browser does not cancel the review or an active answer.

**Reflect on session** reviews the current conversation and relevant historical sessions, then reports
findings and proposed actions. It does not apply memory or configuration changes. See
[Sessions and review](sessions.md) for asset setup, cancellation, and saved-data handling.

### Local and remote memory

Memory stores conclusions across sessions and is disabled by default. Add this to your configuration
and start a new session:

```toml
[memory]
enabled = true
```

Leave `memory.remote` unconfigured for local-only memory. Sessions share
`<config-dir>/memory/v1.sqlite3`; no Cloudflare service is needed. Agents read memory through
explicit tools. Memory is separate from durable session history.

The local store has no repository, workspace, or author namespaces. Put any scope in the record
itself. Memory does not override current instructions or `AGENTS.md`. The database is unencrypted;
do not store credentials, transcripts, or transient plans.

Optional remote memory selects a remote-only backend for configured workspace roots and their linked
Git worktrees. Other workspaces use local memory. Remote failures never fall back to local storage,
and there is no automatic synchronization or combined search. Credentials control namespace access
and reader or writer permissions; children remain read-only. Explicit `orvek memory push` replaces
the writer's remote namespace, while `orvek memory pull` merges into local memory. See
[Memory](memory.md) for remote setup, transfer precautions, record limits, and expiry rules.

### Context projection and legacy compaction data

The host derives a context view for each inference request without deleting, summarizing, or rewriting
the authoritative history. Settled tool output can remain native text or use host-rendered PNG pages.
Comparable provider receipts guide the choice; missing measurements or failures restore native text
for the affected segment. The `read_context` tool retrieves exact authorized history text without
rerunning a tool.

Set `agent.context_window_tokens` for new sessions. It accepts 16,384 through 1,000,000 tokens and
defaults to 1,000,000. Legacy `[agent.compaction]` settings still map `input_budget_tokens` to this value.
Projection does not add a spend limit, call limit, or execution stop. Historical SQLite imports retain
unknown tables as opaque data; keep your own copy if vendor-private archives matter. See
[Host-owned context views](compaction.md) for selection, branch access, and recovery rules.

### Subagents

Subagents perform focused, read-only work in direct child sessions. They require
`agent.execution = "sandbox"`, enabled subagents, a local Docker service, the configured executor
image, and a matching `ORVEK_EXECUTOR_HELPER`. Host mode does not expose subagent tools.

Children use the parent's selected model but do not inherit its conversation. The parent supplies
the task and a JSON result schema. Children cannot edit files or create other children, and they share
a workspace snapshot rather than separate worktrees. Each child is limited to 12 model calls and
8,192 output tokens. `agent.max_subagents` limits active child turns across the host to at most 32.
The live registry starts empty after the host exits. See [Subagents](subagents.md) for configuration,
management tools, result storage, and failure states.

### Sloppiness diagnostics

The read-only `measure_sloppiness` tool reports source lines, verbosity, and complexity-related
erosion from workspace source. It runs in both execution modes without invoking a compiler, shell,
network service, or external analyzer. Sandbox tasks also receive a frozen-baseline comparison;
host tasks receive only the current report.

Source-line and clone metrics cover detected textual languages. Redundant-AST and complexity analysis
currently have a Rust adapter; other languages report that limitation. These signals are not a
quality score and cannot replace behavior checks or complete a task. See
[Sloppiness diagnostics](sloppiness.md) for metrics, exclusions, and analysis bounds.

### Performance notes

The TUI caches wrapping, syntax styles, and layout work. Its scheduler combines streaming updates
while allowing keyboard input to request an immediate frame. Long output and terminal I/O still add
work; frame-rate limits do not guarantee throughput.

Forks retain the root session's provider cache-routing key so an exact shared prefix can be reused.
Diverged content remains separate, and a cache miss processes the complete projected request. See
[Performance notes](performance.md) for local benchmarks and optional CodSpeed setup. Those benchmarks
measure rendering, not model latency or task success.

### Evaluations

The Harbor adapter runs the pinned Terminal-Bench 2.1 dataset in local Docker environments with each
task's verifier. Setup requires `uv`, Docker with Buildx on a local Unix socket, and a valid
Codex-compatible auth file. Apple Silicon also needs Docker Desktop Rosetta support for x86-64
verifiers. Live trials consume model usage; the setup claims no benchmark score. The optional Docker
execution path has not been validated for Orvek 0.1.0.

`just check-harbor` checks the adapter, Rust code, and configuration without model requests. Trial
results distinguish verifier failures from errors that prevented a valid result. Report both completed
and errored trial counts with mean reward; unavailable subscription costs are not zero. See
[Harbor evaluations](../evals/README.md) for setup, credential isolation, trial commands, and comparison
requirements.

### Portable trace bundles and offline replay

Export local journal records and referenced artifacts with `orvek trace`. Offline replay checks
recorded state and reports missing evidence. Hashes detect changes; they do not authenticate the
source or prove a tool ran correctly. See [Trace bundles](trace-bundles.md).

### Optional interpreter for composed tool calls

Use native tools directly for normal work. Optional `interpreter_eval` cells compose admitted host
tools and filter large results with session-local JavaScript state. Direct tools remain available after
a cell fails. Only explicit checkpoints survive state loss;
the host does not rerun interrupted effects. See [Interpreter](interpreter.md).

### Durable event intake and schedules

Registered event sources feed the host's durable submission queue. Source identity, deduplication,
and admission stay host-owned; payload text cannot grant capabilities. See [Event intake](event-intake.md).

### Trace monitoring and native read recovery

The host records monitor status and evaluates narrowly defined native-read configuration changes
against retained evidence. This is not general automatic code repair or proof of model quality.
See [Monitoring](monitoring.md).

### Experimental context phase transitions

Optional `transition_context` proposals replace eligible settled ranges in the derived context view,
not the journal. Summaries remain model claims; exact source stays available through `read_context`.
The feature is experimental and disabled by default. See [Context views](compaction.md).

## Paths and precedence

- CLI flags override environment variables and file settings.
- `--config PATH` or `ORVEK_CONFIG` selects a file.
- Otherwise, Orvek uses `$ORVEK_HOME/config.toml` or `~/.orvek/config.toml`.
- Earlier directory and environment names remain fallbacks when new settings are absent.

Directories are never merged. The config directory also owns sessions and local memory. Relative
paths in settings resolve from that directory. Configuration files containing inline credentials must use mode `0600` on Unix. Orvek rejects
broader permissions.

## Authentication

The default `auto` mode uses `$CODEX_HOME/auth.json` or `~/.codex/auth.json`, then falls back to
`OPENAI_API_KEY` if no auth file exists.

```sh
orvek auth login
orvek auth status
orvek --auth api-key
```

`orvek auth logout` removes the shared credential file and also signs Codex out. Use
`--auth-file PATH` or `ORVEK_AUTH_FILE` for a separate file. Static API keys are read from the
environment, not written to configuration or printed in status output.

To require subscription authentication, set `[auth] mode = "chatgpt"` and leave
`agent.api_base_url` and `agent.websocket_url` unset. Use `orvek auth status` to check the
selected account. Existing sessions keep their saved reasoning settings; start a new session
or select standard reasoning if the subscription endpoint rejects `reasoning.mode = "pro"`.
The ChatGPT Pro subscription and Orvek's Pro reasoning mode are separate settings.

The subscription endpoint does not accept `max_output_tokens` or `truncation`. Orvek omits
these fields only for ChatGPT authentication, including per-model routes and WebSocket
requests. The provider does not enforce Orvek's requested output-token cap on this route;
controller accounting, budgets, and transport deadlines still apply. API-key requests keep
both fields. Intended-request journal artifacts retain the canonical request, before this
transport adaptation.

ChatGPT can omit the event-stream content type and the final response's output array.
Orvek still requires a valid event stream and a completed terminal event before accepting
streamed output items. Explicitly wrong content types, missing terminal events, conflicting
items, and duplicate tool calls remain errors.

For a gateway that issues short-lived API keys, configure an executable credential helper:

```toml
[auth]
mode = "api-key"
command = "/path/to/token-helper"
refresh_interval_ms = 300000
timeout_ms = 310000
```

The command must write only the API key to standard output. Orvek caches the key, runs the command
again after `refresh_interval_ms`, and force-refreshes it after a pre-generation `401` response.
Orvek retries only the rejected request that is safe to replay. It does not log or store the key.
Relative command paths resolve from the configuration directory. `timeout_ms` must be greater than
zero.

## Agent settings

```toml
[agent]
execution = "host"
thinking = "medium"
reasoning_mode = "standard"
fast_mode = false
max_subagents = 32
context_window_tokens = 1000000
```

`execution` accepts `host`, the default, or `sandbox`. Host mode runs local tools with your
permissions and does not expose subagents. Sandbox mode requires Docker and the matching executor
helper; it enables isolated verification and read-only subagents.

`max_subagents` accepts values from `1` through `32`. Invalid configuration and command-line
overrides are rejected instead of clamped.

`thinking` accepts `low`, `medium`, `high`, `xhigh`, or `max`. `reasoning_mode` accepts `standard`
or `pro`. Mode changes that require a new session are reported in the terminal.

`--model` accepts `sol` (the default), `terra`, `luna`, `glm`, `spark`, or `astra`.
Use `--model astra` or `--model gpt-6-astra` for GPT-6 Astra. `ORVEK_MODEL` is the
environment equivalent. Resumed sessions retain their saved model; no sessions are migrated.

Astra also accepts `agent.model = "astra"` or `agent.model = "gpt-6-astra"` in TOML.
Per-model endpoint overrides use `[models.astra]` or `[models.gpt-6-astra]`, with the same
`api_base_url`, `websocket_url`, and `api_key_env` fields as other model routes. The model
selector lists Astra for new sessions.

The installed Prime Agent catalog identifies Astra as `gpt-6-astra` on the OpenAI Responses
API. Its supported efforts are `low`, `medium`, `high`, `xhigh`, and `max`; `off` and `minimal`
are not supported. This catalog does not establish ChatGPT subscription availability, Pro
reasoning, or priority service support. Endpoint access still depends on the provider and account.
Orvek does not change reasoning, fast-mode, or context-window settings when selecting Astra.

When a provider supplies no cost receipt, Astra uses the installed catalog's standard estimate:
$10 per million uncached input tokens, $1 per million cached input tokens, and $50 per million
output tokens. These estimates are not subscription charges or verified priority prices.

Advanced endpoints use `agent.websocket_url` and `agent.api_base_url`, or the matching CLI flags.
Leave them unset for the configured authentication route.

### Completion notifications

Set `agent.completion_hook` to a shell command, for example:

```toml
[agent]
completion_hook = "/path/to/local-notification-handler"
```

The detached host runs this command after a task turn settles, for both native and sandbox
execution. Headless and terminal clients use the same path. All recorded terminal task outcomes
are covered, including blocked, failed, cancelled, and budget-exhausted tasks. Conversation-only
turns, auxiliary requests, local shell submissions, and requests rejected before a task exists do
not send task notifications. A resumed task gets a new notification for its new request.

The hook runs as `/bin/sh -c COMMAND` in the session workspace with the host user's permissions
and environment. This is a host command, even for sandbox tasks. It requires no extra approval.
The command is pinned in the private session journal before task execution. Do not put credentials
in its text; use a local handler and environment-based credentials instead.

The receiver gets one JSON object on stdin with `version` (currently `1`), `delivery_id`, `session`,
`request`, `task`, and `outcome`. These identities are also available as `ORVEK_COMPLETION_ID`,
`ORVEK_SESSION_ID`, `ORVEK_REQUEST_ID`, `ORVEK_TASK_ID`, and `ORVEK_OUTCOME` environment variables.
Use `ORVEK_COMPLETION_ID` as the receiver's idempotency key. It is stable for the session/request.

Stdout and stderr are discarded. Hooks have a ten-second wall-time limit. Task cancellation still
sends the terminal notification; it does not cancel that notification. The host kills ordinary
process-group descendants at exit or timeout, but cannot contain descendants that deliberately
detach. Keep notification handlers short and do not launch background services from them.

Intent, claim, and result are separate `completion_hook` session-journal events. A failed command
cannot change the task outcome or its verification certificate. Reconnect does not run hooks again.
Restart resumes unclaimed intents after settlement, using their pinned command. Recovery runs
in the background so slow notifications do not block IPC startup. Host shutdown cancels recovery;
a claimed notification without a recorded result stays unknown. A claim is written
before the process starts. Timeout, signal termination, or restart without acknowledgement leaves
an **unknown** delivery; Orvek never retries it automatically. Even a failed command may have made
external changes. Arbitrary shell effects cannot provide exactly-once delivery; receiver-side
deduplication is required before any operator-initiated retry outside Orvek.

Schedule and webhook event intake are not implemented by this setting.


## Memory and context projection

See [Local and remote memory](#local-and-remote-memory) for opt-in storage setup and
[Context projection and legacy compaction data](#context-projection-and-legacy-compaction-data)
for automatic request views and model-window settings.

Changes to skills apply at the next provider-turn boundary. Existing session authority and admitted
runtime tools remain unchanged.

## Themes

```toml
[theme]
mode = "auto"
motion = "full" # Use "reduced" for static decorative effects.
glyphs = "unicode" # Use "ascii" for simpler terminal artwork.
```

`mode` accepts `auto`, `light`, or `dark`. Auto follows the system theme. Put color overrides under
`[theme]`, `[theme.light]`, or `[theme.dark]`. Values may be Ratatui color names, indexed colors,
or RGB strings such as `"#AABBCC"`. `config show` lists all available color fields.

## Skills

Skills are local `SKILL.md` instructions. Enable trusted directories:

```toml
[skills]
enabled = true
roots = ["skills", "/path/to/shared-skills"]
```

Orvek also searches `$CODEX_HOME/skills` or `~/.codex/skills`, and `~/.agents/skills`. Type `$` in
the composer to select a skill. New sessions discover current files; restored sessions retain
their original catalog. Skills can direct tool execution, so treat them as executable guidance.

## Package ownership

Packagers can set `ORVEK_PACKAGE_MANAGER` at build time so updates defer to that package manager.
Source archives can supply `ORVEK_GIT_SHA`, `ORVEK_GIT_BRANCH`, `ORVEK_GIT_COMMIT_TIMESTAMP`, and
`ORVEK_GIT_DIRTY` as build metadata. Source installations update from the Orvek Git repository.

## Generated Terraform cache

Workspace snapshots exclude `.terraform` directories, including nested provider caches. Terraform
source and `.terraform.lock.hcl` remain included. The 256 MiB snapshot limit is unchanged. Existing
snapshots retain their recorded policy; new snapshots use the updated default.
