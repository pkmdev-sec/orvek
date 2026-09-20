# Orvex comparison sources

Documentation checked **20 September 2026**. This compares documented workflows,
not speed, quality, security, reliability, or feature exclusivity. All three
products support overlapping coding, delegation, memory, and review workflows.
Configuration and installed versions can change what is available.

## Orvex

The marketing name is **Orvex**. These claims use the current source repository:

- [Sessions](https://github.com/pkmdev-sec/orvek/blob/919041755e3b2823f7aeaec5ac7e4497f1213a45/docs/sessions.md): a detached host, authoritative journal,
  resumable sessions, one fork pane, and browser diff feedback.
- [Subagents](https://github.com/pkmdev-sec/orvek/blob/919041755e3b2823f7aeaec5ac7e4497f1213a45/docs/subagents.md): read-only child agents, a parent-provided
  JSON result schema, and sandboxed tools. Docker is required. Children use the
  parent's selected model. They cannot edit files or delegate further.
- [Memory](https://github.com/pkmdev-sec/orvek/blob/919041755e3b2823f7aeaec5ac7e4497f1213a45/docs/memory.md): opt-in, shared local SQLite memory. Agents use
  explicit scan/read tools. Scope is the configuration directory, not automatic
  per-repository isolation. The local database is not encrypted.
- [Code diagnostics](https://github.com/pkmdev-sec/orvek/blob/919041755e3b2823f7aeaec5ac7e4497f1213a45/docs/sloppiness.md): repeatable source metrics, not a
  correctness score. Richer AST complexity analysis currently targets Rust.

The host and machine must remain available for live execution. The artwork does
not promise uninterrupted execution across crashes. Child execution is not
fully durable across host restarts. Browser review requires separately built
web assets. Source distribution is available; no signed binary release is claimed.

## Codex

- [CLI features](https://developers.openai.com/codex/cli/features): saved-chat
  resume, delegation, and built-in code review.
- [CLI reference](https://developers.openai.com/codex/cli/reference): session
  forks, background terminals, `/review`, and `/memories`.
- [Subagents](https://developers.openai.com/codex/multi-agent): configurable
  specialist agents, read-only policies, and model choices.
- [Local memories](https://developers.openai.com/codex/customization/memories):
  generated local memories, with controls for generation and use.
- [App Server](https://developers.openai.com/codex/app-server): integration API
  for thread lifecycle, events, background terminals, and review. An API is not
  the same thing as the default CLI experience.

## Claude Code

- [Agent view](https://code.claude.com/docs/en/agent-view): detached background
  sessions, monitoring, and background forks. These sessions can keep running
  without a terminal attached.
- [Subagents](https://code.claude.com/docs/en/sub-agents): configurable tools,
  read-only agents, background runs, optional worktree isolation, and scoped memory.
- [Memory](https://code.claude.com/docs/en/memory): automatic memory files and
  persistent instructions, plus scoped subagent memory.
- [Interactive mode](https://code.claude.com/docs/en/interactive-mode): `/diff`
  and background tasks.
- [Common workflows](https://code.claude.com/docs/en/common-workflows): review,
  test generation, execution, and repairs.

## Reading the comparison

Orvex's highlighted column shows its design choices. It does not mean that
competitors lack equivalent or extensible capabilities. Structured output,
read-only agents, persistent memory, and background work are not exclusive to
Orvex. Recheck these sources before a later campaign.

Competitor names identify their products. No affiliation or endorsement is implied.
