# Subagents

Subagents run focused, read-only work in direct child model sessions. They start with isolated
context by default. The parent can request a pinned conversation fork without granting more
permissions. Only an explicit, schema-valid `submit_result` counts as a completed child.

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
| `spawn_agent` | Start a direct child from `role`, `task`, `model = "selected"`, and a required JSON `output_schema`. Optional `context_mode` is `isolated` (default) or `fork_at_cursor`. |
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

## Context selection

`isolated` sends only the explicit child task and caller schema. Use it for independent reviewers
that should not see parent conclusions. `fork_at_cursor` pins the parent's cursor and source-history
digest, removes unfinished tool pairs, then derives a bounded native view. Later parent records
cannot enter that view. Its stored manifest identifies the source, excluded calls, effective input,
and observed workspace generation. It is not a promise to copy the parent's last provider body or
bitmap representation byte-for-byte.

The caller schema appears inside the child's `submit_result` tool definition before its first
response. Invalid submissions return an actionable validation error so the child can repair and
resubmit within its existing resources. A prose answer without a valid submission stays unsubmitted.

## Lifecycle and durability

The host journals child admission, accepted and consumed messages, and one terminal outcome.
`list_agents` and `wait_agent` expose these distinct states:

| Status | Meaning |
| --- | --- |
| `running` | The admitted child has no durable terminal result yet. |
| `completed` | A schema-valid submitted result and its artifact digest are durable. |
| `unsubmitted` | The child ended with prose, not a schema-valid result. |
| `interrupted` | Cancellation or host loss interrupted the child. |
| `failed` | The child could not finish, for example after a provider or resource failure. |

Results are linked in the journal before they are published to callers. Repeated list/wait calls
retain the same result and digest, including after host restart. A stored but unlinked artifact is
not a completed child. Startup rebuilds the registry and marks unfinished children interrupted;
it does not respawn them or resume a model process. Old unlinked blobs cannot establish ownership.
Unknown job effects remain in the task ledger and are never automatically replayed.

The TUI shows an unsubmitted outcome as a failure with an `unsubmitted` diagnostic. Operator IPC
version 5 carries the new child outcome; incompatible clients must reconnect through a compatible
binary rather than consume unknown event variants.

## Shared workspace

Children use read-only isolated tools, but their workspace is **live, not frozen**. They do not
have separate writable worktrees. The context manifest discloses the admission generation, and
individual tool receipts report their admitted generation. Parent or other actor writes can change
what later reads observe. A generation label is not a frozen-review guarantee or a completion
certificate.
