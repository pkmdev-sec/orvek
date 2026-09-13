# Configuration

Use `orvek config path` to find the selected file. Use `orvek config show` to print effective
settings with secrets redacted. The file is optional.

## Paths and precedence

- CLI flags override environment variables and file settings.
- `--config PATH` or `ORVEK_CONFIG` selects a file.
- Otherwise, Orvek uses `$ORVEK_HOME/config.toml` or `~/.orvek/config.toml`.
- Earlier directory and environment names remain fallbacks when new settings are absent.

Directories are never merged. The config directory also owns sessions and local memory. Relative
paths in settings resolve from that directory. Keep files containing credentials private, with
mode `0600` where required.

## Authentication

The default `auto` mode uses `$CODEX_HOME/auth.json` or `~/.codex/auth.json`, then falls back to
`OPENAI_API_KEY` if no auth file exists.

```sh
orvek auth login
orvek auth status
orvek --auth api-key
```

`orvek auth logout` removes the shared credential file and also signs Codex out. Use
`--auth-file PATH` or `ORVEK_AUTH_FILE` for a separate file. API keys are read from the environment,
not written to configuration or printed in status output.

## Agent settings

```toml
[agent]
thinking = "medium"
reasoning_mode = "standard"
fast_mode = false
max_subagents = 32
```

`thinking` accepts `low`, `medium`, `high`, `xhigh`, or `max`. `reasoning_mode` accepts `standard`
or `pro`. Mode changes that require a new session are reported in the terminal.

`--model sol`, `--model terra`, or `--model luna` selects a model for a new agent. `ORVEK_MODEL`
is the environment equivalent. Resumed sessions retain their saved model.

Advanced endpoints use `agent.websocket_url` and `agent.api_base_url`, or the matching CLI flags.
Leave them unset for the configured authentication route. Bitmap compaction currently rejects
custom endpoints.

## Memory and compaction

Both settings have separate purposes:

- [Memory](memory.md) stores conclusions across sessions. It is disabled by default.
- [Compaction](compaction.md) reduces model context. Provider compaction is the default;
  local bitmap compaction requires an explicit experimental profile.

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

Remote URLs must use HTTP or HTTPS without embedded credentials. A failed server does not prevent
other servers or the session from starting.

## Package ownership

Packagers can set `ORVEK_PACKAGE_MANAGER` at build time so updates defer to that package manager.
Source archives can supply `ORVEK_GIT_SHA`, `ORVEK_GIT_BRANCH`, `ORVEK_GIT_COMMIT_TIMESTAMP`, and
`ORVEK_GIT_DIRTY` as build metadata. Source installations update from the Orvek Git repository.
