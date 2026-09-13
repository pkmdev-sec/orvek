# Component design decisions

Review one component at a time. Open its HTML proposal with macOS `open`, then wait for the user's
explicit approval. Feedback revises that component only. An approval locks the named version and
scope; changes to it need another review. Design approval does not authorize native implementation.

Components through the review download dialog are approved. The current review is
**Notifications and review status, proposal 1**.

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
| 10 | Skill picker | Approved and locked | [Proposal 1](components/skills-v1.html) |
| 11 | Message queue | Approved and locked | [Proposal 1](components/queue-v1.html) |
| 12 | Child-agent view | Approved and locked | [Proposal 1](components/agents-v1.html) |
| 13 | Review download dialog | Approved and locked | [Proposal 1](components/review-v1.html) |
| 14 | Notifications and review status | Awaiting approval | [Proposal 1](components/notifications-v1.html) |
| 15 | Recent prompt picker | Not started | Preserve prompt history and draft handling. |
| 16 | Memory browser | Not started | Preserve existing data and actions. |
| 17 | Context diagnostics | Not started | Preserve measured values and recovery details. |
| 18 | Theme selector | Not started | Preserve theme roles and overrides. |
| 19 | Keyboard help | Not started | Show the actual bindings. |
| 20 | Welcome placement and final consistency | Not started | Check approved components together, including light and narrow layouts. |

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

The [HTML](components/skills-v1.html) keeps the 72 × 14 rounded skill picker. Approved by the user.
Source: `components/skill_picker.rs`, `core/extensions/skills.rs`, and Root's skill-mention
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

Locked artifact: `skills-v1.html` at `7f19562`, SHA-256
`a37faa63d4d2253e1345d8aa81fb4d917e723e586bdf614425dc39126887d640`.
Its pending-review label is historical; this registry records approval.

## Message queue, proposal 1

The [HTML](components/queue-v1.html) keeps the bordered stack above the approved composer. Approved
by the user. Source: `components/queue.rs`, Root's queue/edit/steer handlers, and
`Submission` in `tui/prompt.rs`.

- Keep rounded borders and separators between messages. Use the existing accent color and a
  selection marker for focus, instead of an inverted text background. Keep non-queued items muted.
- Show concise row states only when needed: editing, sending, accepted, retained. Ordinary queued
  messages need no repeated state label. Accepted means acknowledged, not applied.
- Keep pending steers visible until the existing applied/promoted handling resolves them. Preserve
  the grayscale steering wave and stop it when no steer is pending or motion is disabled/hidden.
- Render a viewport of complete message rows. Normal review capacity is three messages; Root may
  allocate less space to preserve the composer and transcript. Scroll to the selected item and
  map mouse rows through the visible range. Never draw separators over the footer.
- Keep selection attached to its QueueId when a different item is removed by a background event.
  This is a proposed correctness improvement over index-only repair. Explicit deletion of the
  selected item chooses a nearby remaining item.
- Keep clamped arrows, Shift+arrows reorder, E edit, Enter steer, D/Delete/Backspace removal, and
  Escape return. Only Queued items permit edit/delete/steer; reordering cannot cross non-Queued items.
- Keep submission order within the pending steer portion. Rejected steers return to the waiting
  portion without splitting pending steers. Cancelled steers remain retained for the existing
  late-event/drain logic; they are not silently discarded or marked applied.
- Preserve the acknowledgement-before-application and application-before-acknowledgement paths.
  Preserve ready-prefix draining and `Submission::join`; do not promise one future turn per row.
- Unfocused help offers Tab to manage the queue. Preserve Root's existing empty-draft Enter route;
  do not advertise a different target or change dispatch implicitly.
- Empty queues render no widget. Rendering uses sanitized display text only; steering and normal
  submission keep the original structured payload, text, and ordering.

Editing continues through the approved composer. Keep the original draft/input mode, stable queue
identity, save/cancel behavior, and the existing empty-save removal rule. The HTML's separate text
editor and event controls are test scaffolding, not proposed native widgets.

Observed correctness issue to resolve during implementation: the native edit path currently passes
`Submission::display_text()` into a String editor and reconstructs `text.into()` in `finish_edit`.
That path does not preserve image payloads. Add a focused multimodal edit regression and carry a
structured submission through the composer's attachment-aware editing path; retaining old image
ranges after arbitrary text edits is insufficient. Do not claim image-safe editing from this
text-only preview.

The HTML tests the visible states and a bounded text-only queue simulation. Native checks must
cover callback order, late events after cancellation, promotion, failed steering, edit barriers,
selected-ID stability, bounded rendering, click/viewport mapping, multimodal payloads, draft
restoration, and idle CPU. No native queue or agent implementation changed. Child-agent views are
next after approval.

Locked artifact: `queue-v1.html` at `1f8b578`, SHA-256
`8406b2aba5080217576c019e768ee3448ad22ada4d2980f92bc49b05b1afcee2`.
Its pending-review label is historical; this registry records approval.

## Child-agent view, proposal 1

The [HTML](components/agents-v1.html) keeps the tree and read-only transcript inspector. Approved
by the user. Source: `components/subagents.rs`, `subagent_tree_layout.rs`, Root's subagent effects,
and `crates/subagents/src/model.rs` / `capacity.rs`.

- Keep rounded 24 × 4 nodes, the green focus border, and current status/model colors. Keep agent
  IDs and roles on nodes. Node child counts refer to the filtered tree; the inspector distinguishes
  visible and total children.
- Wrap the selected task into two fixed rows. For a failed agent, use the second row for a bounded
  error excerpt from the existing status value. Keep model and session identity in the inspector.
- Show the descriptor's actual parent. If filtering hides that parent, label it hidden rather than
  implying the child belongs directly to the root. Preserve the native visible-tree promotion rule.
- Keep the active filter's exact meaning: Pending, Running, Closing. All retains completed,
  interrupted, failed, and closed agents for inspection. Opening or changing filters selects the
  oldest matching AgentId, as the current implementation does.
- Keep parent/child and same-level navigation, h/j/k/l aliases, Home, F filter, Enter inspect,
  +/- limit, and Escape. Preserve remembered-child navigation. Mouse focus is a proposed addition;
  clicking a node does not start, stop, or message an agent.
- If the visible tree fits, show the whole tree without moving the camera between its nodes.
  Otherwise keep the focus visible using the current bounded camera approach. Reduced motion
  snaps. Below 80 columns, use a compact indented hierarchy with the same navigation semantics.
- Keep the active count and concurrency limit distinct. Lowering the limit may leave more active
  agents than the new limit; do not terminate or relabel them. Preserve `SetMaxSubagents` and the
  current runtime capacity policy. Preview limit changes affect only sample state.
- Enter opens the selected agent's transcript full-screen, using the approved conversation style.
  Keep scrolling, expansion, selection, links, and Escape's blur-before-back behavior. The browser
  transcript is illustrative and does not replace the native transcript component.

Performance requirement: the current `animation_deadline` / `advance` iterate over every child
transcript. Restrict recurring rendering work to the visible tree/camera or selected inspector.
Continue ingesting and storing all hidden agent events and messages. Do not pause agents to reduce
UI work. Cache layout by hierarchy/filter changes and update metadata without relaying out the tree.

Keep AgentId scoped to the existing root/runtime routing; ignore stale updates through the current
ownership boundary. Retain native cycle/orphan handling, reusable-session semantics, and typed
status updates. Do not infer completion from assistant text or add kill/restart/send controls.

The browser uses a valid fixed hierarchy, not a replacement layout engine. Native checks must cover
filter promotion, real parent identity, focus repair, cousin navigation, interrupted camera motion,
status updates during inspection, links/selection, limit changes below active count, empty views,
small dimensions, and hidden-transcript wakeups. Review dialogs and remaining utility views follow
after approval.

Locked artifact: `agents-v1.html` at `5206d8d`, SHA-256
`79a445f69f2b25a81c56d3dbebc41a820088f65dcb764be2b995676aaf33d8f8`.
Its pending-review label is historical; this registry records approval.

## Review download dialog, proposal 1

The [HTML](components/review-v1.html) keeps the rounded 64 × 9 confirmation. Approved by the
user. Source: `components/review_confirmation.rs`, `components/floating.rs`,
`RootNode::update_review_confirmation`, the `RootEffect::Review` handler and `spawn_review` in
`tui/mod.rs`, `ReviewAssets::availability` / `download` in `review/assets.rs`, and
`tui/review_controller.rs`.

- Keep the title and native theme roles. Use the existing accent for the primary action, regular
  text for keys, and muted text for explanation and Cancel. No new palette or shaded panel.
- Explain that this Orvek version's bundle will be downloaded and verified, then the browser
  review will open. Label the primary action `Download & open`; keep a separate `Cancel` action.
  Successful opening remains conditional on the existing download, preparation, and browser flow.
- Say the interface needs installation. The current `DownloadRequired` result also covers an
  invalid managed bundle, so saying it is always absent would be inaccurate.
- Preserve Enter/Y confirmation and Esc/N cancellation, including uppercase and key repeats.
  Ignore release events. Do not introduce selectable defaults or change Enter to select Cancel.
  Click targets for the two actions are proposed additions; use the same Confirm/Dismiss effects.
- Keep the normal 64 × 9 frame. Compute required height from wrapped text and action rows, bounded
  by the available area. Add a small body inset. On narrow screens, stack the two actions.
- With insufficient height, reserve the actions and a more/back indicator, then scroll only the
  explanation. Arrows, Page Up/Down, Home/End, and wheel scrolling are proposed additions. These
  currently ignored keys must not change focus or reach the underlying composer while open.
- Reflow after resize and clamp the scroll position. Keep all explanatory text reachable and both
  action hit regions inside the frame. The preview covers widths of at least 32 and heights of
  at least 9; smaller native rectangles still need a bounded fallback and resize tests.
- No animation, decorative timer, network lookup, or dependency belongs in this component.
  Measure fixed copy on resize and repaint only on relevant events.

Keep the existing runtime boundary: Ready assets skip this prompt; DownloadRequired prompts before
starting the download; development installs and invalid explicit overrides retain their current
error paths. Confirmation closes the overlay and emits `RootEffect::Review { download_assets: true }`.
Cancellation closes it without starting review work or changing the draft. Preserve the active-review
guard, installation verification and lock, ReviewIdentity generations, and task cancellation.
Do not reopen this prompt automatically on a failure, or interpret dismissal as cancelling a
separate running task. Notifications and ongoing review status are a later component review.

Implementation can retain `Floating` and the existing effect enums. Give `ReviewDownloadConfirmation`
only a body scroll offset and measured render geometry; replace the unit-struct construction with
its default state. Use the same geometry for painting and mouse targets. Keep installation paths,
URLs, and credentials out of this static dialog.

The HTML simulates only choosing an action; it never downloads, installs, calls a model, or starts
a browser review. Native checks must cover key kinds, exactly one effect before overlay closure,
dismissal with an unchanged draft, hit testing, wrapped copy and borders, short-height scrolling,
resize recovery, Ready/DownloadRequired/development/error routing, stale review events, and
interruption. Reuse existing asset/controller tests for their contracts. Browser layout checks do
not prove native terminal rendering, download behavior, or performance.

Locked artifact: `review-v1.html` at `b5f28a0`, SHA-256
`2768e0477430183d1c292bff7fcddb84b2aea23e287e802f86a242c926fbb197`.
Its pending-review label is historical; this registry records approval.

## Notifications and review status, proposal 1

The [HTML](components/notifications-v1.html) retains the small rounded notices and review status in
composer chrome. No approval recorded yet. Sources: `Notification`, `render_notification`,
`update_review_input`, `update_key_confirmation`, and review event handlers in `components/root.rs`;
`AppNode::update` in `components/app.rs`; `ComposerEvent::ReviewWaiting` and `render_chrome` in
`components/composer.rs`; `components/waved_text.rs`; review routing in `tui/mod.rs`; and
`ReviewServer::url` in `review/server.rs`.

### Notices

- Keep one current notice per pane. New notices replace it, as today. No notification feed, durable
  history, sound, or entrance animation. Keep green success/update, yellow cancellation/warning,
  and red failure colors. Preserve update version emphasis and the reset-color update command.
- Keep the popup at the top of the transcript area, below the activity header. It floats over
  history without resizing the transcript or moving the composer. Constrain its rectangle to the
  transcript, excluding queue and input. Never draw it over an open picker or active selection.
- Keep short messages centered. Wrap longer messages with a small left inset. Cap width at 64
  columns and the body at four rows, including an overflow hint when needed. Measure actual wrapped
  lines with the native text-width rules; character-count division is insufficient for word wrap.
- Click a visible notice or press F2 to read its full text. Show `F2 details` when truncated.
  F2 is a proposed binding; no existing F-key binding was found in the current TUI sources.
  Do not claim an arbitrary host terminal will deliver it without configuration.
- The details view reuses a rounded, scrollable, read-only popup. Arrows/Page Up/Down/Home/End scroll,
  C copies the displayed message through the existing copy effect, and Escape returns. It never
  submits text, follows an embedded command, opens a URL, or cancels running review work.
- Capture pointer events inside a notice before underlying transcript selection, tools, or links.
  Clicking elsewhere retains existing input behavior. Opening details clears a pending two-key
  cancellation confirmation so Escape cannot accidentally complete it on return.
- Preserve the ten-second display duration, but count time only while the notice is visible. Pause
  it behind overlays/details, during overlapping selection, or when the transcript cannot fit it.
  A new notice received while covered becomes the pending current notice. Resume on visibility;
  do not start an animation timer to poll for space. Visible unfocused split panes still count.
- Details keep the message opened by the user. A newer notice may replace the pending current
  notice without changing the text being read. Closing details shows the newest notice. Retain
  only these bounded slots; no backlog. Full text is available while the notice is retained.

Observed issues: Root renders notifications after overlays, so a notice can cover a picker.
`render_notification` estimates height with `text_width.div_ceil(body_width)` while Paragraph wraps
at word boundaries. These are source-backed risks; the existing narrow-message test covers one
phrase, not arbitrary wrapping. Add failing native cases before fixing them.

### Review status

- Preserve the approved composer geometry and the wave after context. This preview reuses its
  layout model unchanged, then supplies the review labels and current green review color/border.
  Keep each `O reopen` and `C copy link` hint together when wrapping. Metadata uses the approved
  extra row when needed; neither status nor its wave may paint into the draft.
- `ReviewStarted` still means waiting for review. The event does not distinguish downloading,
  preparation, or browser startup. Do not invent percentages or claim a specific stage.
- `ReviewReady` exposes O reopen and C copy link; it does not unlock the composer or prove that the
  browser opened. Preserve the real URL in the existing narrowly scoped action state. Keep it out
  of routine visible status. Draft text stays visible while typing, submission, and image paste
  remain blocked according to current behavior.
- Preserve Escape twice within two seconds to request review cancellation, including repeat-key
  suppression. Keep Ctrl+C twice to exit and Ctrl+T's existing fork route. A request is not a
  completion event. The sample reports requests separately from the selectable completion events.
- Finished feedback is inserted at the cursor, with the existing separators and remaining draft.
  It is not sent automatically. Add one concise success notice after insertion: `Review feedback
  added to draft.` Do not label the review approved or the coding task complete from that text.
- Cancelled/failed events clear review state and keep the draft, with the existing yellow/red
  notice. Preserve pane generations and ReviewIdentity checks before updates reach the root.
  Browser-open failure leaves review ready so retry and explicit link copy remain available.

Security correction within this flow: `ReviewServer::url` embeds its access token in the path.
The initial browser-open failure currently interpolates that URL into `NotifyError`. Replace that
visible text with O retry / C copy link instructions and a safe OS error cause, without the URL.
Do not keep an unredacted copy in notification details, clipboard-message content, or diagnostics.
The explicit review-link copy action remains available. Reuse terminal-control sanitization for
all displayed messages; it is not a substitute for removing secrets at the producing boundary.
Do not make broader secret-ownership guarantees about the existing review service.

### Implementation boundary and checks

Keep Root as the owner of notifications and review state. A small proposed
`components/notification.rs` can own wrapping, visible-time accounting, and details geometry;
use existing Floating, styles, scheduler, and Copy effects. There is no new backend API or store.
Route F2/click entry and notification details before review's blocking-input handler, while
preserving global exit handling. Other overlays retain their existing routes. Inspection must not compete
with O/C actions unless that details view is open. Future keyboard help must list F2.

Use WavedText's existing 140 ms cadence and eight shades for review status. Repaint only its cells;
stop decorative ticks when covered, hidden, static, or reduced motion is requested. A visible notice
needs one expiry deadline, not a recurring clock. Measure/cache wrapping by message and width, and
reuse transcript/composer caches. The browser has a held timeout option for visual inspection;
that option is test scaffolding, not a proposed application preference.

Native checks must cover exact word wrapping, Unicode/control text, tiny rectangles, notices with
queue/picker/selection, mouse capture, expiry and hidden-time accounting, replacement during details,
message-copy versus review-link-copy routing, two-key cancellation, repeats, resize, and split-pane
ownership. Verify preserved text and image attachments when feedback arrives at different cursor
positions. Test browser launch failure without exposing its URL/token, and stale/cancelled review
callbacks. Keep existing update styling and controller tests. Measure idle/covered wakeups and
long-transcript repaint work before claiming a performance improvement.

The preview uses fictional messages and a text-only draft. It never opens a browser review, copies
to the system clipboard, downloads, calls a model, or submits a prompt. Its pure layout/state checks
do not prove native input handling, rendering, clipboard behavior, or performance. Recent prompt
history is the next component after approval.
