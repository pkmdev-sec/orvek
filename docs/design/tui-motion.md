# TUI components and motion

Status: proposed. Branch: `design/tui-motion`, based on `59bef36`.

[Open the interactive study](tui-motion-preview.html). It contains sample content and independent
rendering code. Application code, the installed binary, and `main` are unchanged.

## Direction

Use Crush as the component and interaction reference. Build a cleaner Orvek interface around
readable content, a distinct input surface, compact activity, and consistent event colors.
The design covers the welcome screen, composer, tool output, queue, agents, and existing pickers.
It does not change model execution, permissions, memory, or context compaction behavior.

- Use a compact Orvek wordmark with a brief opening light sweep. Input works immediately.
- Place a small persistent activity strip beside the composer status. It remains visible when idle.
- Keep animated **Thinking** text immediately after the context meter when it fits. Wrap longer
  phrases within the status area. Model metadata cannot displace the active status.
- Give the editor its own shaded surface and a clear focus edge. Put model and effort below it.
- Use expandable tool surfaces, compact queue/agent badges, and searchable pickers. Show these
  components when the corresponding state exists.
- Use quiet neutral surfaces, violet for focus, cyan for work, amber for required input, green for
  completion, and red for failure. Labels and symbols carry the same meaning without color.

The preview is a direction for review, not a final logo or a claim of improved native performance.
Its sample command palette and tool output do not execute actions.

## Decision rule: UX, DX, and AX

Every change must improve the user experience (UX), developer experience (DX), and agent experience
(AX). Record a concrete benefit in each area before implementation. Simplify or omit changes that
only add decoration, another abstraction, or another source of state.

| Area | Required benefit | Evidence |
| --- | --- | --- |
| UX | Readable content, immediate input, useful motion, and fast startup. | Native narrow-screen, typing, streaming, and startup checks. |
| DX | Clear ownership, fewer competing render paths, and predictable maintenance. | One composer geometry owner, existing scheduler, deterministic fixtures, bounded caches. |
| AX | Accurate task state and reliable tool interaction without extra agent work. | Event replay preserves tool IDs, ordering, results, cancellation, queues, and child-agent state. |

For example, fixing composer geometry makes status readable, removes duplicated layout rules, and
keeps agent work distinguishable from required user input. A shared activity clock makes motion
smooth, keeps scheduling in one place, and prevents a completed task from appearing active.

Use Crush's component patterns selectively. Keep the existing Rust runtime, Ratatui renderer,
input pipeline, and event contracts. The initial slice needs no new runtime dependency, sidecar,
asset download, or background service. Restyle existing controls before introducing new controls.
Visual effects never enter model context, create tool calls, or delay agent event handling.

## Performance contract

- Add no blocking startup work. The first editable frame must not wait for the welcome effect,
  filesystem scans, network requests, or generated assets introduced by this design.
- Stop decorative timers when idle, hidden, or in motion-off mode. Keep the idle presence mark
  static. Advance visible active effects at a proposed 20 FPS; input and outcomes remain immediate.
- Reuse static layouts and rendered content. A color tick must not rewrap the draft, reparse the
  transcript, reload configuration, or invalidate all message caches.
- Keep effects within small, fixed regions. Bound caches by visible components and theme variants;
  do not retain an animation history for each tool call.
- Under rendering pressure, skip decorative frames. Preserve input, text updates, and task state.
  Never replay missed animation frames in a burst.

Before editing native code, record launch-to-first-editable-frame time, input-to-paint latency,
streaming frame time, idle wakeups, memory use, and binary size on the same machine and fixtures.
Compare cold and warm startup separately. Measure input latency during long-session streaming and
multiple tool updates, not only on the empty screen. Keep these measurements in local task evidence.

Set tolerances from repeated baseline runs before judging changes. Reject measurable regressions
outside that tolerance. No fixed latency target or speed improvement is established by this HTML
study. Removing an effect is preferable to slowing typing or agent output.

## Grounding

Current source inspected at `59bef36`:

| Owner | Observed behavior | Planned change |
| --- | --- | --- |
| [empty.rs](../../bin/orvek/src/tui/components/transcript/empty.rs), `EmptyLogo` | Outline emblem and separate wordmark; animation changes the emphasized row. | Compact wordmark, short entrance, useful workspace information. |
| [activity_mark.rs](../../bin/orvek/src/tui/components/activity_mark.rs), `ActivityMark` | Nine-column mark; four frames for active states. | Small gradient strip with distinct state behavior. |
| [root.rs](../../bin/orvek/src/tui/components/root.rs), `render_root` | Reserves a top row for the mark and `ORVEK / <state>`; independently calculates the editor rectangle. | Put activity beside input; use composer-owned geometry for rendering and hit testing. |
| [composer.rs](../../bin/orvek/src/tui/components/composer.rs), `render_chrome` | Reserves timer/model/effort width before status; clips waves to the remainder. Context denominator is fixed at `272k`. | Separate status, editor, and metadata. Wire the available context budget into display. |
| [waved_text.rs](../../bin/orvek/src/tui/components/waved_text.rs), `WavedText` | Produces animated color spans per character. | Preserve the wave; measure graphemes and paint only its allocated cells. |
| [root.rs](../../bin/orvek/src/tui/components/root.rs), `refresh_activity`, `activity_outcome` | Combines transcript, turn, shell, compaction, and child-agent state. | Keep this event ownership; derive presentation once. |
| [scheduler.rs](../../bin/orvek/src/tui/scheduler.rs) | Coalesces streaming redraws; input can request an immediate frame. | Keep input priority and add no independent animation loop. |
| [context.rs](../../bin/orvek/src/tui/context.rs), `ContextDiagnostics::observe` | Reads an optional input budget from `run.started`. | Reuse observed budget; distinguish unknown budget from a measured value. |

The width allocation explains status truncation. It does **not** prove the reported text bleed is
caused by stale cells. Reproduce consecutive frames before deciding whether clearing, overlapping
spans, cursor geometry, or several causes need fixes. The review wave also uses a different prefix
offset from the activity wave; test combined input-mode/review/status states.

Preserve the cached draft wrapping and grapheme-aware caret mapping in
[composer/layout.rs](../../bin/orvek/src/tui/components/composer/layout.rs), transcript selection,
streaming Markdown caches, and current keyboard actions.

## What to adapt from Crush

Reference snapshot: [`d333e04385f9e1d1523cea7b417cb5e8798a713a`][crush], inspected 2026-09-13.
These are source observations; Crush was not installed or run during this investigation.

| Source pattern | Evidence | Orvek application |
| --- | --- | --- |
| Compact gradient animation with labels and cached frames | [`anim.Settings`, `Anim.Advance`, `Anim.Render`][anim] | Use a bounded activity strip and cached palettes. Keep Orvek's readable thinking wave. |
| One clock for visible active items | [`Chat.EnsureAnimating`, `Chat.Tick`][clock] | Extend the existing scheduler; skip hidden items and stop when nothing visible is moving. |
| Tool-specific output and explicit lifecycle states | [`baseToolMessageItem`, `toolEarlyStateContent`][tools] | Restyle existing expandable tool rows; distinguish pending, running, failed, and cancelled. |
| Different labels for model work | [`renderSpinning`, `isSpinning`][assistant] | Show actual thinking/compaction state. Do not leave a spinner after a finished or restored turn. |
| Queue and task badges with expansion | [`queuePill`, `todoPill`, `renderPills`][pills] | Reuse Orvek's queue and agent counts. Add a task-progress badge only if a real task-state source exists. |
| Separate attachments and editor | [`renderEditorView`][ui] | Keep attachment rows outside draft text and status; preserve image input and selection. |
| Consistent semantic colors | [`CharmtonePantera`][theme] | Add Orvek theme roles for focus, busy, waiting, success, error, and surfaces. |
| Searchable commands and bounded review panels | [`Commands`][commands], [`Permissions`][permissions] | Restyle existing pickers and review UI; preserve their actual actions and authorization semantics. |

Crush uses Go, Bubble Tea, and Lip Gloss. These components are not Rust widgets. Recommend fresh
Ratatui implementations of the selected interaction patterns, with independent artwork and
animation logic. A Go sidecar or full TUI rewrite adds runtime and input ownership problems without
solving the composer bug.

The inspected application source uses [FSL-1.1-MIT][license], including restrictions on competing
commercial use and redistribution terms. Direct code or artwork reuse is a separate licensing
decision. No Crush source or assets are included in this design's public artifacts.

[TachyonFX](https://github.com/ratatui/tachyonfx) remains an optional Rust effects candidate if later
component transitions justify it. Do not add it for a six-cell indicator. The
[TerminalTextEffects showroom](https://chrisbuilds.github.io/terminaltexteffects/showroom/)
provides motion references, not a Python runtime dependency.

## Layout contract

The composer owns measurement, painting rectangles, cursor coordinates, and hit areas. Root places
the remaining transcript and queue around its returned bounds. No caller reconstructs editor
geometry from border thickness.

Priority, from highest to lowest:

1. Editable text, caret, selection, and required user action.
2. Full active status and context information.
3. Mode/effort badges, model name, and elapsed time.
4. Optional counts, hints, and decorative space.

At 80 columns, context and status normally share one row. At 32–50 columns, status can occupy the
following one or two rows. Metadata stays below the editor and can use a short model label whose
picker exposes the full identifier. Required mode badges remain visible. Long secondary labels
wrap or use explicit ellipses; they never overwrite another component.

Below 32 columns or with very little height, use a compact status label such as `Background` and
one editor row. Hide the welcome artwork and optional hints first. Below a usable input size, show
a concise resize message. Avoid width arithmetic underflow and off-screen cursor placement.

Clear each owned surface before painting. Allocate the context, review, activity, counts, and
metadata before producing color spans. Animation only changes foreground styles or cells inside
its assigned region. It cannot modify draft text, move the caret, or paint over a popup.

## Proposed interfaces

The caller shape comes first. These are design sketches, not existing APIs:

```rust
let presentation = self.activity_presentation();
let layout = self.composer.component_mut().layout(available, &presentation);
self.composer_content_area = layout.editor;
// Root allocates transcript and queue above layout.bounds.
self.composer.component_mut().render_with_layout(
    frame, &layout, &presentation, theme, focused, selection,
);
```

Keep existing `ActivityState` as the semantic state owner. Extend it only for distinctions that
real events support. Place composer geometry in proposed `components/composer/chrome.rs`, leaving
`composer/layout.rs` responsible for draft wrapping.

```rust
struct ComposerLayout {
    bounds: Rect,
    presence: Rect,
    context: Rect,
    status_lines: Vec<Rect>,
    attachments: Option<Rect>,
    editor: Rect,
    metadata_lines: Vec<Rect>,
    // Named hit areas for visible model, effort, queue, and agent controls.
}

struct ActivityPresentation {
    state: ActivityState,
    label: String,
    active_agents: usize,
    queued_prompts: usize,
}

enum MotionMode { Full, Reduced, Off }

// Signatures added to the existing ActivityMark, not a second event owner.
fn set_state(&mut self, state: ActivityState, now: Instant) -> bool;
fn deadline(&self) -> Option<Instant>;
fn advance(&mut self, now: Instant) -> bool;
fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme);
```

Reuse `RootNode::refresh_activity` and transcript projection as inputs. Recognize reading/editing
only from known tool metadata. Unknown tools use `Working`. Never infer a successful edit, task
percentage, or completion from elapsed time or animation position.

```mermaid
sequenceDiagram
    participant Events as Existing event handlers
    participant Root as Root activity state
    participant Layout as Composer layout
    participant Clock as Existing scheduler
    participant Paint as Visible components
    Events->>Root: turn / tool / queue / child / compaction update
    Root->>Layout: current status and available area
    Layout-->>Root: bounds, text regions, editor and hit areas
    Root->>Clock: immediate semantic change; optional motion deadline
    Clock->>Paint: frame at monotonic time
    Paint->>Paint: clear owned regions; render content and local effects
    Events->>Root: completion / failure / cancellation
    Root->>Clock: show outcome immediately; retire active motion
```

## Motion and event rules

| State | Visual behavior | End condition |
| --- | --- | --- |
| Opening | Brief light sweep across a compact wordmark. | Settle within about 800 ms; typing bypasses it. |
| Ready | Small static presence mark. | A real task starts. |
| Thinking | Gentle violet strip plus the preserved text wave. | Model phase changes or turn ends. |
| Tool work | Cyan movement in the active tool header; edit accent when known. | Tool result arrives. |
| Background work | Slower movement and an active-count badge. | Last relevant worker exits. |
| Compaction | Inward movement with the full compaction label. | Completed, failed, or cancelled event. |
| Required input | Amber mark and a clear actionable panel. | User resolves or dismisses the actual request. |
| Complete | Brief green settle, then still. | New task starts. |
| Failed / cancelled | Immediate red / muted symbol and text; stop busy motion. | New task starts. |

When states coexist, required input leads, then blocking compaction, then foreground work.
Background activity remains visible as a separate count. Errors stay on the affected tool row even
if another tool is still running. A child result cannot mark the whole session complete.

Update text immediately. Interpolate visual changes over a proposed 180–240 ms; interrupted
transitions start from their current visual state. Use monotonic time and deterministic frames.
Target 20 frames per second for active decorative motion. Input redraws remain independent.
This is a proposed budget, not a measured result.

Use the existing scheduler as the only clock. Cache static wordmarks, gradients, and completed tool
content. Avoid reparsing Markdown or measuring unchanged draft text on animation ticks. Preserve
viewport position when the user has scrolled away. Suppress hidden/covered animations and retire
old session deadlines when switching or forking. Resume historical terminal states without a live
spinner unless current runtime state confirms work is active.

Add proposed `ui.motion = "full" | "reduced" | "off"` and
`ui.characters = "auto" | "unicode" | "ascii"` through
[app/config.rs](../../bin/orvek/src/app/config.rs). These keys are not implemented. Reduced motion
uses static symbols and immediate state changes. Keep light/dark/custom themes and add terminal
color fallbacks through [theme.rs](../../bin/orvek/src/tui/theme.rs). Do not infer motion preference
from `NO_COLOR`; honor color and motion settings separately.

No icon font, image protocol, browser, Go process, or network access is required by the native UI.
Use known-width Unicode cells with an ASCII fallback. Preserve sanitization of terminal controls.
Diagnostics contain state, region sizes, and timing only, not credentials, prompts, or tool output.

## Implementation order

| Phase | Affected modules | Acceptance behavior |
| --- | --- | --- |
| 1. Reproduce and fix geometry | `composer.rs`, proposed `composer/chrome.rs`, `root.rs`, `waved_text.rs` | Full statuses remain visible; no old text survives long-to-short transitions; cursor and selection match the editor. |
| 2. Theme and input surface | `theme.rs`, `app/config.rs`, composer chrome | New focus/surface roles, separated metadata, required mode badges, light/low-color/reduced-motion support. |
| 3. Welcome and activity | `transcript/empty.rs`, `activity_mark.rs`, `root.rs`, `scheduler.rs` | Short entrance, persistent presence, correct event colors, no idle clock or input delay. Remove superseded emblem and frame tables. |
| 4. Tool, queue, and agent components | `components/transcript/mod.rs`, `queue.rs`, `subagents.rs` | Compact expandable output and counts backed by existing state; no duplicate completion, lost tool details, or scroll jumps. |
| 5. Pickers and review surfaces | `session_picker.rs`, `model_selector.rs`, existing completion/action/review components | Shared spacing/focus/selection styles; existing navigation and action semantics remain intact. |
| 6. Native review and cleanup | Existing TUI benches, fixtures, user documentation | Review real terminal recordings, pass required checks, then remove task-owned build output after any authorized installation. |

Keep each phase small. First complete composer correctness, shared styling, and bounded activity.
Proceed to tool surfaces and pickers only after that native slice meets the UX/DX/AX and performance
contracts. Those phases adapt existing interactions; they do not authorize a new widget framework
or unrelated feature work. Each revision must preserve usable input and the existing agent flow.

For regressions, follow AGENTS.md: failing regression test revision, child fix revision, verification,
then squash the fix into the test revision. New components and their behavioral tests belong together.
Do not keep both old and new chrome paths after callers migrate. Saved session payloads need no
migration: visual state is derived from existing records and current runtime state.

## Verification and remaining decisions

Later native checks must cover:

- Widths 32, 40, 50, 60, 80, 100, and 120; heights 8, 12, 24, and 40; tiny dimensions never panic.
- Thinking, Running in background, compaction, review, long model names, pro/fast badges, timers,
  multiple agents, queued input, and attachment rows in combination.
- Long-to-short-to-idle frames on a reused buffer versus a fresh render; no text outside its region.
- Unicode graphemes, wide characters, multiline drafts, selection, copy, paste, resize, and mouse hits.
- Visible/hidden tool animations, live tool completion, cancellation, failed compaction, session
  switching, resume, and forks. Input and semantic outcomes cannot wait for an animation.
- Dark/light/custom/16-color/monochrome/ASCII/reduced-motion views. Meaning remains readable without color.
- Streaming under long transcripts, input latency during animation, frame time, allocations, and
  idle CPU. Set numerical acceptance thresholds from the existing bench and a real terminal baseline.

Required implementation checks: `cargo check --all-features`, `just check-fmt`, `just clippy`,
`just test`. These were **not run in this planning phase**. No model evaluations were run.

Preview-only verification: JavaScript syntax and 2,464 layout combinations checked; full status
text and non-overlapping status/editor regions verified in that model. Native rendering and browser
interaction automation remain unverified. The preview was opened with macOS `open` for visual review.

Before implementation, settle the visual direction with the preview and then review an equivalent
native terminal fixture. The wordmark, palette, indicator geometry, and transition timing remain
review choices. Actual text-bleed reproduction and the available granularity of tool phase events
remain implementation investigations. Direct source reuse from Crush is not part of this plan.

[crush]: https://github.com/charmbracelet/crush/tree/d333e04385f9e1d1523cea7b417cb5e8798a713a
[anim]: https://github.com/charmbracelet/crush/blob/d333e04385f9e1d1523cea7b417cb5e8798a713a/internal/ui/anim/anim.go
[clock]: https://github.com/charmbracelet/crush/blob/d333e04385f9e1d1523cea7b417cb5e8798a713a/internal/ui/model/chat.go
[tools]: https://github.com/charmbracelet/crush/blob/d333e04385f9e1d1523cea7b417cb5e8798a713a/internal/ui/chat/tools.go
[assistant]: https://github.com/charmbracelet/crush/blob/d333e04385f9e1d1523cea7b417cb5e8798a713a/internal/ui/chat/assistant.go
[pills]: https://github.com/charmbracelet/crush/blob/d333e04385f9e1d1523cea7b417cb5e8798a713a/internal/ui/model/pills.go
[ui]: https://github.com/charmbracelet/crush/blob/d333e04385f9e1d1523cea7b417cb5e8798a713a/internal/ui/model/ui.go
[theme]: https://github.com/charmbracelet/crush/blob/d333e04385f9e1d1523cea7b417cb5e8798a713a/internal/ui/styles/themes.go
[commands]: https://github.com/charmbracelet/crush/blob/d333e04385f9e1d1523cea7b417cb5e8798a713a/internal/ui/dialog/commands.go
[permissions]: https://github.com/charmbracelet/crush/blob/d333e04385f9e1d1523cea7b417cb5e8798a713a/internal/ui/dialog/permissions.go
[license]: https://github.com/charmbracelet/crush/blob/d333e04385f9e1d1523cea7b417cb5e8798a713a/LICENSE.md
