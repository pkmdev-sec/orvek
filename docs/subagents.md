# Subagents

Subagents run focused, read-only work in direct child model sessions. They do not inherit the
parent conversation. Each child must submit one JSON result that satisfies the schema supplied by
the parent.

## Runtime requirement

Subagents are available only when the isolated Docker runtime is selected:

```toml
[agent]
execution = "sandbox"
max_subagents = 32

[subagents]
enabled = true
allow_luna = true
```

The default `execution = "host"` mode runs shell commands with the user's authority and does not
expose subagent tools. Sandbox mode requires a local Docker service, the configured executor image,
and a matching `ORVEK_EXECUTOR_HELPER`.

`max_subagents` bounds active child turns across the host. Lowering it does not cancel work already
running. The hard maximum is 32. `allow_luna = false` prevents a Luna session from creating Luna
children. Children always use the parent session's selected model.

## Tools

The parent receives these tools:

| Tool | Contract |
| --- | --- |
| `spawn_agent` | Start a direct child from `role`, `task`, `model = "selected"`, and a required JSON `output_schema`. |
| `send_agent_message` | Queue a bounded message for a running child in the same parent session. |
| `list_agents` | List children owned by the current parent session. |
| `wait_agent` | Wait up to 300 seconds for one to eight children owned by the current parent session. |
| `interrupt_agent` | Cancel a running child owned by the current parent session. |
| `close_agent` | Cancel a running child owned by the current parent session. |

Every management operation is scoped to the caller's session. An ID from another session is
reported as unknown.

A child receives only `read_file`, `search`, `exec_command`, and `submit_result`. Workspace tools
run through the isolated executor with read-only admission. Children cannot edit files, create
other children, or manage siblings. A child may make at most 12 model calls and returns at most
8,192 output tokens.

Messages are nonempty and at most 16 KiB. Accepted priorities are `normal` and `urgent`; accepted
purposes are `instruction`, `answer`, and `context`. Messages are consumed between child turns.

## Lifecycle and durability

Invalid schemas or results, capacity exhaustion, missing submissions, provider failures, tool
failures, and cancellation produce explicit failure states. Completed results are stored in the
host artifact store and referenced by digest in lifecycle events.

The live child registry belongs to the detached host process. Restoring sessions after that host
exits starts an empty registry. The TUI displays lifecycle events from the running host; task and
session journals remain the authority for durable parent work.

## Shared workspace

All children in a host see the same workspace snapshot through read-only isolated tools. They do
not have separate worktrees. Concurrent reads are safe, but parent edits can change what later
child tool calls observe.
