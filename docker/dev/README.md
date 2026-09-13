# Optional development container

This container mounts a local workspace and Orvek state. A separate `iron-proxy` container holds
model credentials and replaces placeholders on matching requests. Use `run.py`; do not pass real
credentials directly to Compose.

No Orvek release image is published yet. The Dockerfile expects `ghcr.io/pkmdev-sec/orvek:latest`;
configure an available binary stage or publish that image before using this build. The native CLI
does not need Docker.

## Requirements and commands

Requires Docker with Buildx/Compose v2, a local Unix-socket context, and Python 3.11 or newer.
`just` provides shortcuts. Remote Docker contexts are rejected before credentials are read.

```sh
just -f docker/dev/justfile build
just -f docker/dev/justfile run
python3 docker/dev/run.py --workspace /path/to/project -- --help
```

The workspace mounts read-write at `/workspace`. The container uses the host UID/GID. The local
image is `orvek-dev:local`; the build does not push it. The `shell` recipe opens Bash through the
same launcher.

## Credentials

`--auth auto` tries a valid ChatGPT credential, then `OPENAI_API_KEY` or `OPENAI_API_KEY_FILE`.
Use `--auth api-key` or `--auth chatgpt` to choose explicitly.

API-key mode replaces only the Authorization header on OpenAI `/v1` requests. ChatGPT mode reads
the Codex auth file and uses only its access token and account ID. Replacement is restricted to
GET/POST requests under `https://chatgpt.com/backend-api/codex`.

Credential files inside the workspace or writable Orvek state are rejected. The development
container receives fake auth; only the proxy receives the real selected credential and CA key.
Proxy credentials cached by `iron-proxy` are outside the launcher's zeroization guarantee.

## Files and permissions

The config directory mounts read-write at `/run/orvek-state`. `ORVEK_CONFIG` or `ORVEK_HOME`
selects it. Earlier configuration paths remain supported. A missing config becomes an empty private file. Config
symlinks are rejected. Keep literal MCP secrets and credentials out of this writable mount.

The launcher copies the selected global `AGENTS.override.md` or `AGENTS.md`, plus default Codex
and agent skill roots, into a read-only directory. Skill symlinks are skipped.

```sh
python3 docker/dev/run.py --no-instructions --no-skills
python3 docker/dev/run.py --skill-root /path/to/skills
```

Tool caches use the `orvek-dev-state` volume. Avoid concurrent mutation of shared state/caches.
The `clean` recipe removes the image and cache volume, not host Orvek state.

## Isolation limits

The services share a network namespace. The CONNECT listener binds to `127.0.0.1:8080`; no host
ports are published. Containers drop capabilities and prohibit privilege gains.

This isolates credentials, not the mounted filesystem or all network traffic. Tools can change
the workspace/state and use direct egress. Proxy-aware clients use the tunnel; localhost traffic
uses `NO_PROXY`. The entrypoint installs trust for the temporary CA before starting Orvek.
Listener readiness does not verify an upstream API.

## Tests and troubleshooting

```sh
just -f docker/dev/justfile test
```

This includes image builds/runs. Unit-only checks are:

```sh
python3 docker/dev/test.py CredentialTests ProxyConfigurationTests LocalAgentFileTests LauncherTests
```

Use a local Docker context. Check host directory permissions for write errors, service logs for
proxy failures, and `/run/orvek-public/ca.crt` for TLS failures. Refresh expired credentials.
Tool pins are in `tools.toml` and `docker/development.dockerfile`; rebuild after changing them.
