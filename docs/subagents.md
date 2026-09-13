# Subagents

Subagents delegate work to clean child sessions and return validated JSON results. They share the
root's workspace and tool permissions. They do not inherit its conversation.

## Configuration

These are the defaults:

```toml
[subagents]
enabled = true
allow_luna = true

[agent]
max_subagents = 32
```

Set `enabled = false` to remove subagent tools and delegation instructions from new and restored
sessions. Existing runtimes keep their tools and instructions after configuration reload. Memory,
skills, MCP servers, and ordinary tools remain independent.

`spawn_agent` requires a model choice: `selected` uses the session model; `luna` selects Luna.
Setting `allow_luna = false` removes the explicit `luna` choice. A session already using Luna can
still pass `selected`.

`max_subagents` bounds active child turns across the entire root tree, including grandchildren.
Idle children consume no capacity. The TUI can change the limit during work. Lowering it does not
cancel active turns; it prevents new reservations until capacity is available.

## Tools

Root and child sessions receive the same seven tools. The runtime checks each caller's authority.

| Tool | Contract |
| --- | --- |
| `spawn_agent` | Create a child from `role`, `task`, `model`, and required JSON `output_schema`. |
| `submit_result` | Submit `output` with the active `turn_token`. Only registered children can submit. |
| `send_agent_message` | Send `message` to `agent_id` within the same root tree. |
| `list_agents` | List visible agents, statuses, topology, and caller authority. |
| `wait_agent` | Wait for any of `agent_ids` to become terminal. |
| `interrupt_agent` | Stop the selected agent's active turn and descendants; keep sessions reusable. |
| `close_agent` | Stop and close the selected agent and descendants; prevent reuse. |

Each child starts with its role, task, tree identity, coordination instructions, and output schema.
The runtime validates the schema before creating the child. Completion requires exactly one
accepted `submit_result` for the current turn. Invalid output returns up to four validation errors
for correction. A successful model turn without an accepted result still fails.

Steering rotates the turn token, so a superseded turn cannot submit a result. Roots and
user-created forks cannot submit child results. Memory mutation uses the same registry to enforce
root-only writes, independently of whether subagent tools are enabled.

## Messages and authority

The root can manage every descendant. Children can manage their descendants, but not siblings or
ancestors. Agents can coordinate with other agents in the same root tree. Cross-tree access and
self-messaging are rejected.

`send_agent_message` accepts these routing fields:

| Field | Values and behavior |
| --- | --- |
| `priority` | `deferred`, the default, or `urgent`. Urgent messages steer active turns at a safe boundary. |
| `purpose` | `coordinate`, the default, `delegate`, `finding`, `question`, or `reply`. |
| `in_reply_to` | Required only for `reply`. Use the original message ID and reverse its sender and recipient. |

Messages must be nonempty and at most 2 KiB of UTF-8. `delegate` replaces the task, retains the
output schema, and requires management authority. Other purposes add context without replacing
the task.

Deferred messages start idle recipients or queue behind active turns. If a message is queued, the
sender must finish its turn before delivery can proceed. Do not wait for that delivery inside the
sending turn. Admission and delivery have separate statuses; acceptance does not prove delivery.

The runtime retains at most 256 completed message records per root, plus pending records. Command
channels are bounded. Capacity or channel exhaustion returns an error instead of queuing unlimited
work.

## Lifecycle and failures

`wait_agent` defaults to 30 seconds and accepts at most 300 seconds through `timeout_ms`. A timeout
returns current summaries with `timed_out = true`; it does not cancel work.

Interrupt and close stop descendants before their parent, with a 30-second internal deadline.
Completed, failed, or interrupted children remain inspectable and can receive later work. Closed
children remain inspectable but cannot restart. Root shutdown or replacement closes all children
and awaits runtime tasks.

Invalid schemas or results, stale tokens, duplicate submissions, unauthorized routing, capacity
exhaustion, missing submissions, and model, tool, or shutdown errors produce explicit failures.
Interrupted message delivery also reports failure. None counts as a successful result.

`/subagents` shows the live tree, tasks, statuses, messages, and child transcripts. Registry state is
process-local. Restoring a root session creates an empty tree; old child sessions do not resume.
The TUI rejects updates from replaced runtimes.

## Shared workspace

Children use the configured provider, workspace, base tools, and process authority. They have no
separate filesystem or network sandbox. Concurrent agents see each other's file changes; write
ownership is not enforced by the filesystem. Assign separate work, resolve overlaps, and verify
the combined result.

Message bodies remain agent-authored content. The runtime checks routing, delegation authority,
output validation, and lifecycle permissions. It does not provide remote workers, durable job
queues, independent credentials, or cross-process recovery.

For embedding APIs, see the [`orvek-subagents` crate](../crates/subagents/README.md).
