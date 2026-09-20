# Harbor evaluations

Run the pinned Terminal-Bench 2.1 dataset through Harbor. Each trial uses a local Docker environment
and the task's verifier. Live runs consume model usage; no benchmark score is claimed by this setup.

## Setup

Requires `uv`, Docker with Buildx on a local Unix socket, and a valid Codex-compatible auth file.
Apple Silicon also needs Docker Desktop Rosetta support for tasks with x86-64 verifiers.

```sh
just harbor-bootstrap
just check-harbor
just build-harbor-agent
```

`check-harbor` checks the adapter, Rust code, and configuration without model requests. The build
writes `.orvek/installed/orvek`; rebuild after source changes. The optional Docker execution path
has not been validated for Orvek 0.1.0.

## Run trials

Start with one task and one attempt:

```sh
just harbor-eval \
  --include-task-name terminal-bench/openssl-selfsigned-cert \
  --n-attempts 1 --n-concurrent 1
```

Omit the task filter to use the full configured dataset. Other forwarded options include
`--n-tasks`, `--exclude-task-name`, `--n-attempts`, and `--n-concurrent`. Each concurrent trial
starts task/proxy containers and makes separate model requests.

The configured model is `openai/gpt-5.6-sol`. Changing Harbor's label alone does not change
Orvek's model and is rejected.

Optional delegation checks:

```sh
just harbor-eval \
  --include-task-name terminal-bench/openssl-selfsigned-cert \
  --agent-kwarg effort=high \
  --agent-kwarg max_subagents=8 \
  --agent-kwarg minimum_subagents=2 \
  --agent-kwarg require_wait=true \
  --agent-kwarg fail_on_subagent_error=true \
  --n-concurrent 1
```

These checks require child starts, a successful wait, and no child errors. Leave them off for
ordinary task-completion measurements.

## Credentials

Auth file lookup order is `ORVEK_CODEX_AUTH_FILE`, `$CODEX_HOME/auth.json`, then `~/.codex/auth.json`.
Run `orvek auth login` if needed. A trial requires at least one hour of token validity.

The task container receives fake credentials. A pinned local `iron-proxy` substitutes the real
access token/account ID only on matching Codex requests. The original auth file, refresh/identity
tokens, and proxy secrets are not mounted into the task container. Remote Docker contexts and
unsafe container settings are rejected before credential reads. Trial images are not published.

## Results

Results live under `.orvek/harbor/jobs/<job-id>/`. Open them with `just harbor-view`.

- Reward `1` means the verifier passed.
- Reward `0` means the verifier ran and its checks failed.
- An error means the trial did not produce a valid result.

Always report completed and errored trial counts with the mean reward. Environment failures are
not ordinary task failures. Preserve failed attempts when comparing versions.

For a comparison, match `task_name`, `task_checksum`, dataset release, model settings, tool access,
timeout, and attempt count. Report pass rate, median/tail agent time, tokens, and cost when available.
Subscription runs may report cost as `null`; do not interpret it as zero.

```sh
jq '{task_name, task_checksum, reward: .verifier_result.rewards.reward,
  agent_execution, cost_usd: .agent_result.cost_usd,
  input_tokens: .agent_result.n_input_tokens,
  cached_input_tokens: .agent_result.n_cache_tokens,
  output_tokens: .agent_result.n_output_tokens, exception_info}' \
  .orvek/harbor/jobs/JOB/TRIAL/result.json
```

Useful files under each trial:

| File | Use |
| --- | --- |
| `result.json` | Outcome, usage, timing, and exceptions |
| `verifier/reward.txt` | Verifier reward |
| `verifier/test-stdout.txt` | Failed checks or environment errors |
| `trial.log` | Trial lifecycle |
| `agent/stderr.log` | Orvek diagnostics |
| `agent/events.jsonl` | Durable host journal and terminal receipt |
| `agent/trajectory.json` | ATIF trajectory and exact available durable usage |

Inspect verifier output before interpreting a surprising reward. Crashes, missing dependencies,
timeouts, and missing verifier output can invalidate the result.
