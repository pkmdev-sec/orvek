# Component design decisions

Review one component at a time. Open its HTML proposal with macOS `open`, then wait for the user's
explicit approval. Feedback revises that component only. An approval locks the named version and
scope; changes to it need another review. Design approval does not authorize native implementation.

The welcome logo and chat bar are approved. The current review is **Conversation and execution,
proposal 1**.

Color constraint: preserve the existing native Orvek theme, including model, effort, and thinking
colors. The HTML uses approximate samples; those values are not a proposed replacement palette.
Implementation must reuse the existing theme roles and user overrides. The separately approved
welcome-logo colors stay fixed.

| Order | Component | State | Artifact |
| --- | --- | --- | --- |
| 1 | Welcome logo | Approved: glyphs and colors only | [Logo](tui-motion-preview.html) |
| 2 | Chat bar | Approved and locked | [Proposal 1](components/composer-v1.html) |
| 3 | Conversation and execution rows | Awaiting approval | [Proposal 1](components/transcript-v1.html) |
| 4 | Actions menu | Not started | Current native popup is the baseline. |
| 5 | Persistent activity indicator | Not started | Shape and placement need approval. |
| 6 | Model and effort selectors | Not started | Review each selector separately. |
| 7 | Session, file, and skill pickers | Not started | Review each picker separately. |
| 8 | Queue and child-agent views | Not started | Preserve existing workflow. |
| 9 | Review prompts and notifications | Not started | Preserve existing actions and event meaning. |
| 10 | Welcome placement and final consistency | Not started | Check approved components together, including light and narrow layouts. |

## Chat bar, proposal 1

The [HTML](components/composer-v1.html) shows one design with interactive input and sample states.
It keeps the rounded border, plain editor surface, top-left context/activity, top-right model/effort,
and bottom-edge hints/workspace. Proposed refinements:

- Two cells of horizontal inset and one blank row around the editor.
- A clearer focus state using existing theme colors.
- The thinking wave stays after context; foreground brightness changes without shifting text.
- Status takes priority over metadata. Overflow uses reserved rows inside the existing frame;
  the editor moves down with its measured region. Hints shorten before the workspace disappears.
- Multiline input grows to six visible rows in the study, then scrolls. The native limit remains
  a review decision; existing keybindings and draft handling are preserved by the implementation.

Use the Activity and Terminal width controls, then type or use the multiline sample. Enter creates
a newline in this browser study. Nothing is sent to an agent.

Approved by the user after confirming the existing color scheme. Approval covers the visual
arrangement and state behavior demonstrated here, using native theme colors. Native glyph-width
handling, editor interactions, performance, and the full layout
matrix still require implementation checks in the [technical plan](tui-motion.md).

Locked artifact: `composer-v1.html` at `04957d4`, SHA-256
`17587b03daa49c939c1dfa757b077fe8e3b885591fed7683eb2dc2588f364377`.
Its embedded pending-review label is historical; this registry records the approval. Preserve the
file bytes so later components cannot silently revise the approved design.

## Conversation and execution, proposal 1

The [HTML](components/transcript-v1.html) keeps the native structure: colored user-message rail,
plain assistant text, compact tool summaries, and indented expanded output. No approval recorded yet.

- Keep existing theme colors and user overrides. HTML colors are approximate samples.
- Keep one blank line between conversation sections and compact adjacent tool summaries.
- Preserve disclosure markers, status, tool name, command, outcome, and duration. At very narrow
  widths, omit duration before hiding outcome; the expanded command remains available.
- When a failed summary cannot fit its error excerpt, show one short error line directly below it.
  This is the principal new presentation behavior for review. Full errors stay in expanded output.
- Clicking a summary expands details without replacing the entry or moving its summary row.
  Keep existing individual expansion state and Ctrl+O behavior.
- Update only the running summary's spinner and duration. Completed entries stay still. Reduced
  motion stops the spinner but still updates elapsed time; idle/hidden views have no decorative timer.

The preview contains fictional commands and results. Its Cancelled state demonstrates how an
actual cancellation should read; it does not introduce a new persisted tool-state variant.
Native source-to-selection mapping, event ordering, output streaming, and performance still require
the existing Rust behavioral tests. Browser layout checks are not native runtime proof.

The approved chat-bar artifact is not embedded or edited during this review. Future components
have no approved design and should not receive speculative preview changes.
