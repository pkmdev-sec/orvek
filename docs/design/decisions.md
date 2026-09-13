# Component design decisions

Review one component at a time. Open its HTML proposal with macOS `open`, then wait for the user's
explicit approval. Feedback revises that component only. An approval locks the named version and
scope; changes to it need another review. Design approval does not authorize native implementation.

The welcome logo, chat bar, execution view, Actions menu, and activity indicator are approved.
The current review is **Model selector, proposal 1**.

Color constraint: preserve the existing native Orvek theme, including model, effort, and thinking
colors. The HTML uses approximate samples; those values are not a proposed replacement palette.
Implementation must reuse the existing theme roles and user overrides. The separately approved
welcome-logo colors stay fixed.

| Order | Component | State | Artifact |
| --- | --- | --- | --- |
| 1 | Welcome logo | Approved: glyphs and colors only | [Logo](tui-motion-preview.html) |
| 2 | Chat bar | Approved and locked | [Proposal 1](components/composer-v1.html) |
| 3 | Conversation and execution rows | Approved and locked | [Proposal 1](components/transcript-v1.html) |
| 4 | Actions menu | Approved and locked | [Proposal 1](components/actions-v1.html) |
| 5 | Persistent activity indicator | Approved and locked | [Proposal 1](components/activity-v1.html) |
| 6 | Model selector | Awaiting approval | [Proposal 1](components/model-v1.html) |
| 7 | Effort selector | Not started | Review separately from model selection. |
| 8 | Session, file, and skill pickers | Not started | Review each picker separately. |
| 9 | Queue and child-agent views | Not started | Preserve existing workflow. |
| 10 | Review prompts and notifications | Not started | Preserve existing actions and event meaning. |
| 11 | Welcome placement and final consistency | Not started | Check approved components together, including light and narrow layouts. |

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
plain assistant text, compact tool summaries, and indented expanded output. Approved by the user.

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

Locked artifact: `transcript-v1.html` at `bcac24a`, SHA-256
`bd87d124c3839e73a86334cac689c12500edafe96bdad2027768afe6a7c1f2d5`.
Its pending-review label is historical; this registry records approval.

Approved preview files remain unchanged. Future components have no approved design and should not
receive speculative preview changes.

## Actions menu, proposal 1

The [HTML](components/actions-v1.html) retains the rounded 58 × 19 popup, title, search row, native
action order, selection marker, and Enter/Tab/Escape behavior. Approved by the user.

- Align existing aliases on the right. At narrow widths, show the selected alias below the list.
- Keep all actions in their current order. Disabled actions remain selectable for explanation,
  but cannot execute. Show the selected action's full reason below the list instead of crowding
  its label. This takes one or two rows from the list; navigation keeps the selected row visible.
- Show `No matching actions` for an empty result.
- Search the visible state-dependent label, the original label, and aliases with the existing
  case-insensitive substring policy. This fixes `Disable fast mode` without losing old matches.
- Keep typing/search selection reset, clamped arrow navigation, grapheme-aware deletion, paste
  sanitization, and Backspace-to-dismiss when the search is empty.
- Click activation and wheel navigation are proposed additions. The native menu currently handles
  keyboard/paste events; mouse handlers must use the same availability checks.
- Use existing theme roles and overrides. No animation, new dependency, category system, or
  background work is needed.

The HTML only reports sample selections. It never executes actions. Sample session presets are
illustrations; native availability remains owned by `RootNode` and `ActionAvailability`, including
independent fork availability. Model selection remains restricted to a new session and compaction
requires conversation content. Preserve existing keys and guards in implementation.

Locked artifact: `actions-v1.html` at `b5079fb`, SHA-256
`56b3d55c9a40c3492dd0e1e0362b11de1b70497f98bccb371548517cc4083bf2`.
Its pending-review label is historical; this registry records approval.

## Persistent activity indicator, proposal 1

The [HTML](components/activity-v1.html) proposes a five-column, two-row mark in the current
top-left activity position, followed by one plain state label. Approved by the user.

- A small O at rest, an orbit for thinking, a moving chevron for work, inward bars for compaction,
  a check for completion, a cross for error, and a dash for cancellation.
- Preserve the existing seven `ActivityState` values and `RootNode::refresh_activity` ownership.
  Tools and child agents map to Working; child completion cannot finish the whole session.
- Preserve current activity color roles: muted for Idle/Cancelled, thinking-medium for Thinking,
  accent for Working/Complete, thinking-high for Compacting, and thinking-xhigh for Error.
- Remove the repeated `ORVEK /` caption. Normal layout uses two terminal rows instead of one;
  no extra blank header row is required. Short terminals keep a one-row static symbol and label.
- The browser draws twenty half-cells. Native rendering can pack them into ten `▀` cells, using
  foreground/background for the upper/lower pixels. This requires no image, font install, or new
  dependency. ASCII fallback reuses the existing native compact symbols.
- Label and semantic color update immediately. A proposed 180 ms shape transition is followed by
  motion only while active. Use at most 12 decorative frames per second, skip missed frames, and
  schedule nothing after idle/final transitions settle. Reduced motion uses distinct static shapes.
- Covered/hidden views stop drawing. The compact fallback stays still. Route deadlines through the
  existing scheduler; do not add an independent native animation loop.

The existing chat-bar thinking wave and execution-row animations remain unchanged. The static
conversation excerpt only illustrates placement. Verify half-block rendering, terminal color
fallbacks, short-height allocation, event precedence, and idle wakeups in native implementation.

Locked artifact: `activity-v1.html`, SHA-256
`ef82946fe8f705227ce94e52fb9084aade7e725fa7c902c138c01224a632c9a7`.
Its pending-review label is historical; this registry records approval.

Native rendering note: the current theme exposes a light/dark scheme, not the terminal's exact
default background RGB. Preserve that background. When it is unknown, use `▀`/`▄`/`█`/space with
reset background for on/off half-pixels; do not add a blocking startup query to reproduce HTML
blending. Native appearance still needs verification.

## Model selector, proposal 1

The [HTML](components/model-v1.html) retains the current three choices and linear interaction.
No approval recorded yet. Source: `components/model_selector.rs`, `RootNode::open_model`, and
`RootNode::update_model`.

- Keep Luna, Terra, Sol in that order, with their existing white, green, and yellow model colors.
  Do not add provider choices, pricing, or unsupported capability/ranking claims.
- Separate `Selected` from `Current`. Arrow movement changes only the pending choice; Enter
  applies, while Escape/Backspace dismisses. Preserve clamped movement with either arrow axis.
- Keep a rounded 52-column popup. Increase height from seven to nine rows to show the current
  value and preserve readable help at narrow widths.
- Use a neutral rail and a single moving selection marker, replacing the filled rail. Each model
  label keeps its own color. A proposed 180 ms movement begins from the current marker position;
  labels and Enter act immediately. Reduced motion snaps without a timer.
- Clicking a model highlights it; Enter still applies. Mouse highlighting is a proposed addition
  to the existing keyboard-only selector. It must not apply on hover or click.
- Preserve the new-session guard. An established conversation cannot open the selector or change
  models through it. Applying the current model remains a no-op; applying another model follows
  the existing `SetModel` flow. Do not change providers, saved sessions, or configuration ownership.

The browser changes local sample values only and reuses its rendered cells during motion. Native
animation deadlines, terminal color handling, small-height layout, and session-transition behavior
remain implementation checks. The effort selector is a separate, future review.
