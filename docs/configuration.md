# Configuration

Use `orvek config path` to find the selected file. Use `orvek config show` to print effective
settings with secrets redacted. The file is optional.

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

## Memory and context projection

Both settings have separate purposes:

- [Memory](memory.md) stores conclusions across sessions. It is disabled by default.
- [Context projection](compaction.md) is automatic and host-owned. Configure its model window with
  `agent.context_window_tokens`; legacy provider compaction settings remain accepted.

Changes to agent tools and instructions apply to new or restored sessions. Reloading configuration
does not replace a running agent's tool set.

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

## MCP tools

Add a local stdio server:

```sh
orvek mcp add filesystem -- \
  npx -y @modelcontextprotocol/server-filesystem /path/to/workspace
```

Use `--cwd PATH` for its working directory. `--env NAME` before `--` copies an environment value
into the server configuration. That file then contains the secret, even though `config show`
redacts it.

Remote servers reference environment variables instead of storing their values:

```sh
orvek mcp add docs --url https://example.com/mcp \
  --bearer-token-env-var DOCS_MCP_TOKEN \
  --header-env X-Tenant-ID=DOCS_TENANT_ID
```

Remote URLs must use HTTPS without embedded credentials. Plain HTTP is accepted only for
loopback addresses. A failed server does not prevent
other servers or the session from starting.

## Package ownership

Packagers can set `ORVEK_PACKAGE_MANAGER` at build time so updates defer to that package manager.
Source archives can supply `ORVEK_GIT_SHA`, `ORVEK_GIT_BRANCH`, `ORVEK_GIT_COMMIT_TIMESTAMP`, and
`ORVEK_GIT_DIRTY` as build metadata. Source installations update from the Orvek Git repository.

## Generated Terraform cache

Workspace snapshots exclude `.terraform` directories, including nested provider caches. Terraform
source and `.terraform.lock.hcl` remain included. The 256 MiB snapshot limit is unchanged. Existing
snapshots retain their recorded policy; new snapshots use the updated default.
