# Component design decisions

Review one component at a time. Open its HTML proposal with macOS `open`, then wait for the user's
explicit approval. Feedback revises that component only. An approval locks the named version and
scope; changes to it need another review. Design approval does not authorize native implementation.

Components through the file picker are approved. The current review is **Skill picker,
proposal 1**.

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
| 6 | Model selector | Approved and locked | [Proposal 1](components/model-v1.html) |
| 7 | Effort selector | Approved and locked | [Proposal 1](components/effort-v1.html) |
| 8 | Session picker | Approved and locked | [Proposal 1](components/sessions-v1.html) |
| 9 | File picker | Approved and locked | [Proposal 1](components/files-v1.html) |
| 10 | Skill picker | Awaiting approval | [Proposal 1](components/skills-v1.html) |
| 11 | Queue and child-agent views | Not started | Preserve existing workflow. |
| 12 | Review prompts and notifications | Not started | Preserve existing actions and event meaning. |
| 13 | Welcome placement and final consistency | Not started | Check approved components together, including light and narrow layouts. |

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
Approved by the user. Source: `components/model_selector.rs`, `RootNode::open_model`, and
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

Locked artifact: `model-v1.html` at `c639ca2`, SHA-256
`cbfcda7cbfd0a3d4762d34104c4ae59fcb49bf36a57c1a6282214e73da710f94`.
Its pending-review label is historical; this registry records approval.

## Effort selector, proposal 1

The [HTML](components/effort-v1.html) retains the circular effort control and its five native levels:
low, medium, high, xhigh, max. Approved by the user.

- Keep the 48 × 17 normal popup and native circular ordering. Add labels beside all five stops.
  Keep unselected labels muted and use the existing effort color for the selected level and arc.
- Separate selected effort from current effort. Arrow navigation still wraps in both directions;
  Enter applies and Escape/Backspace cancels. Selection text updates immediately during animation.
- Keep Pro separate from effort and preserve the `p` toggle. Label it `Pro for new sessions` so
  changing a saved preference is not mistaken for changing the active session's reasoning mode.
- Retain `EffortEffect::Apply(effort, pro)` and the existing root/config/worker update path. The
  current selector exposes all five levels; do not invent model-specific UI gates or guarantees of
  provider support. Preserve backend validation and error handling.
- Use a labelled list below 44 columns. The compact popup is 13 rows high, keeps the same order and
  keys, and shows selection immediately. Normal dial movement is proposed at 220 ms, with the
  current cubic easing and wrapping arc behavior. Reduced motion skips animation.
- Clicking a label highlights its level; clicking the Pro line toggles its pending value. These
  mouse interactions are proposed additions. Neither gesture applies until Enter.
- Reuse rendered cells and the existing scheduler. No idle animation, new framework, provider
  lookup, model evaluation, or configuration write occurs in the preview.

Source: `components/effort.rs`, `ReasoningEffort::ALL`, `RootNode::open_effort/update_effort`,
`RootEffect::SetEffort` handling in `tui/mod.rs`, and `WorkerCommand::SetThinking` in `worker.rs`.
Native tests still need to cover current/fork propagation, failure behavior, theme colors, reduced
motion, very small heights, and source-aligned apply/cancel semantics. The browser only changes
sample values. Session selection is the next review after approval.

Locked artifact: `effort-v1.html` at `459a028`, SHA-256
`7772262a30efaaeeaf2556e75b98475587291ac28b276a2a8bb6cc671c09ff3a`.
Its pending-review label is historical; this registry records approval.

## Session picker, proposal 1

The [HTML](components/sessions-v1.html) keeps the 76 × 18 rounded picker and its Resume/Mention
modes. Approved by the user. Source: `components/session_picker.rs`, `SessionSummary` in
`sessions/checkpoint.rs`, session discovery in `sessions/storage.rs`, and root picker effects.

- Put the existing saved preview first, with model/effort and saved Pro mode beneath. Use a plain
  `No preview available` fallback; do not generate titles or fetch conversation content on selection.
- Label the age column `Started`, matching `started_at_unix_ms`. Preserve discovery order, which
  the storage query sorts by update time. Do not relabel start age as last activity.
- Show the full selected session ID and workspace below the list. Wrap ID text when necessary;
  selection always returns the stored full ID, never a truncated display value.
- Keep current workspace scoping and active-session exclusion. Resume requires a checkpoint;
  Mention can include stored sessions without one. Do not add an all-workspaces switch.
- Preserve case-insensitive substring search over ID, preview, model, and workspace. Typing resets
  selection; arrows clamp; Enter/Tab confirms; Escape or Backspace on an empty search dismisses.
- Use mode-specific titles and hints. Mention inserts `@@<session_id> `; it never resumes a session.
- Distinguish an empty discovery result from a non-empty result filtered down to zero matches.
- Click selects for inspection; double-click confirms. Wheel navigation is proposed. These mouse
  handlers are additions to the native keyboard/paste picker and must use the same exact-ID path.
- Details use one or more existing popup rows, reducing visible entries from the current layout.
  The selected row stays visible. Reuse rendered cells and hit targets; do not add an idle timer,
  new persistence fields, provider calls, or repeated storage reads during navigation.

Browser sessions and ages are fictional fixtures. Loading and load-error behavior remain in Root;
they are not redesigned inside the picker. Verify native sanitization, glyph widths, stable
selection, duplicate previews, same-workspace filtering, missing checkpoints, and distinct
Resume/Mention effects during implementation. File selection is the next component after approval.

Locked artifact: `sessions-v1.html` at `8adb2c5`, SHA-256
`fcaaa54586b5dfb18f1920539a740f29385b699919da1e6b1389832348ae3b1a`.
Its pending-review label is historical; this registry records approval.

## File picker, proposal 1

The [HTML](components/files-v1.html) keeps the 72 × 14 rounded file/directory picker. Approved by
the user. Source: `components/file_finder.rs` and `RootNode::update_file_finder`.

- Emphasize the basename and mute directory prefixes. Keep directory trailing slashes. Shorten
  long display paths in the middle so the filename remains recognizable.
- Show the selected `@path` below the list, with two reserved rows. Extremely long display
  paths use an ellipsis; `FileFinderEffect::Insert` always carries the original full path. Reserving
  the rows keeps the list stable when selecting paths of different lengths.
- Preserve fuzzy scoring and its score-descending/path-ascending tie order. Keep the existing
  directory exclusions: `.git`, `.jj`, `node_modules`, and `target`. This is not gitignore support.
  Preserve skipped symlinks and control-character path rejection; add no silent discovery-policy change.
- The search row mirrors the query after `@`; the native composer still owns the draft and cursor.
  Preserve token-boundary opening, `@@` session-mention handoff, invalid-character dismissal, and
  replacement of only the active mention range with `@<path> `.
- Keep clamped Up/Down navigation, Enter/Tab insertion, and Escape dismissal. Selecting a directory
  inserts its trailing-slash reference; it does not navigate into that directory.
- Click selects; double-click inserts; wheel moves selection. These are proposed additions.
  Root currently dismisses on mouse movement through its generic non-navigation branch. Route
  picker mouse events before that branch so normal hover does not close the popup.
- Distinguish loading, empty discovery, and no matching paths. Loading must not permit stale
  insertion, and query edits must survive discovery completion.

Performance requirement: `FileFinder::new` currently walks the workspace synchronously. Separate
path discovery from rendering using the existing background-task pattern, then filter a snapshot
in memory. Publish results only to the still-active request/pane; discard late results after
cancellation or a changed workspace. Preserve the current scan policy. No indexing daemon, preview
file reads, provider calls, or per-keystroke filesystem scan is proposed.

The HTML uses fixed path fixtures and a manually controlled loading state. It does not test native
async discovery. Later checks must cover ranking parity, query/cursor forwarding, `@@` handoff,
exact insertion including trailing slashes, ignored/stale discovery results, root mouse routing,
Unicode paths, and responsiveness with large workspaces. The skill picker is next after approval.

Locked artifact: `files-v1.html` at `8be4288`, SHA-256
`50a5dbb2727e0c13bc6ca145e06099e3ad02999c390154f6ae6199411b091d5d`.
Its pending-review label is historical; this registry records approval.

## Skill picker, proposal 1

The [HTML](components/skills-v1.html) keeps the 72 × 14 rounded skill picker. No approval recorded
here yet. Source: `components/skill_picker.rs`, `core/extensions/skills.rs`, and Root's skill-mention
trigger and `update_skill_picker` handler.

- Keep `$name` as the primary label, with aligned short descriptions on wide screens. Narrow
  layouts show names in the list and the selected description below it.
- Reserve two rows for the selected description and one for `Insert: $name`. Long text uses an
  ellipsis. Fixed geometry prevents list movement when descriptions differ in length.
- Use only the current `Skill` metadata: name and description. Do not invent origin paths,
  provider badges, installation controls, or body previews absent from this UI contract.
- Preserve initial catalog order. After a query event, use the existing shared fuzzy scorer over
  names, with descending score/name tie order. Do not silently extend search to descriptions.
- Preserve the composer-owned query after `$`, token-boundary opening, invalid-character and
  marker-deletion dismissal, and replacement of only the active mention span with `$<name> `.
- Keep the native guard: no skill picker for shell input or an empty active-session catalog.
  Inserting a reference does not execute a skill or load its body.
- Keep clamped arrows and Enter/Tab insertion. Click selects, double-click inserts, and wheel
  navigation are proposed additions. Reuse the file-picker mouse-routing policy so mouse movement
  does not hit Root's generic dismissal branch.
- Reuse the active catalog snapshot and rendered cells. Show a clear no-match state. No idle
  animation, catalog rescan, provider call, or framework dependency is introduced.

The browser uses fictional skill metadata and only reports an insertion string. Later native checks
must cover scorer parity, exact names, query/cursor ownership, shell `$` variables, empty catalogs,
mouse routing, description truncation, terminal controls, and cancellation without draft loss.
The queue is the next review after approval.
