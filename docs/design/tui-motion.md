# TUI refinement plan

Status: design only, on `design/tui-motion`. Native source baseline: `59bef36`.

**Approved:** the new welcome logo in the [logo preview](tui-motion-preview.html).
**Baseline for everything else:** the existing native Orvek interface.

The chat bar, execution presentation, and Actions menu need focused improvements. Their current
structure stays. The proposed shaded editor, tool cards, extra badges, and replacement palette are
out of scope. Crush and the [interactive TUI references](tui-references.md) supply ideas for specific
interactions; they do not define Orvek's layout.

## Design rules

| Principle | Requirement |
| --- | --- |
| UX | Preserve the familiar layout. Fix unreadable text, inconsistent spacing, and unclear states. |
| DX | Keep rendering ownership explicit; reuse current components, caches, tests, and scheduler. |
| AX | Preserve tool IDs, event order, result visibility, cancellation, queued input, and child-agent state. |
| Performance | Input remains immediate. No new startup dependency or idle decorative work. |

Every proposed change needs a concrete benefit across UX, DX, and AX. Check the actual native
result against these requirements before expanding scope. A new component is not an improvement
by itself. Keep the current theme and density unless a specific visual change is approved.

## Current behavior to preserve

| Area | Source and contract |
| --- | --- |
| Composer | [composer.rs](../../bin/orvek/src/tui/components/composer.rs), `render_focused_with_selection` / `render_chrome`: thin rounded border; context and animated activity at top left; model/effort at top right; hints and workspace on the bottom edge. |
| Text editing | [composer/layout.rs](../../bin/orvek/src/tui/components/composer/layout.rs): cached wrapping and grapheme-aware caret mapping. Keep multiline input, paste, attachments, selection, and current keys. |
| Actions | [actions.rs](../../bin/orvek/src/tui/components/actions.rs), `ActionsMenu`: centered rounded 58 × 19 popup, clamped to the screen; search, compact rows, aliases, selection marker, and keyboard footer. |
| Action behavior | `/` opens from an empty draft. Search is case-insensitive substring matching. Enter/Tab activate; Escape dismisses. Disabled actions remain visible with reasons and cannot execute. |
| Execution | [transcript/tool.rs](../../bin/orvek/src/tui/components/transcript/tool.rs): compact semantic summaries; shell command, status, outcome, and duration on one line when possible. |
| Expanded tools | [tool/shell.rs](../../bin/orvek/src/tui/components/transcript/tool/shell.rs): indented details, full command, process substeps, output, and counts. Keep selectable text and the existing expansion markers. |
| Continuity | [transcript/mod.rs](../../bin/orvek/src/tui/components/transcript/mod.rs): entry-ID expansion state, anchored rows, focused-tool navigation, and Ctrl+O expansion. [transcript/model.rs](../../bin/orvek/src/tui/transcript/model.rs) links ordinary process polling to the original shell entry. |
| Activity | [root.rs](../../bin/orvek/src/tui/components/root.rs), `refresh_activity`: actual transcript, turn, shell, compaction, and child-agent state drive presentation. |

These observations come from source and existing tests. Native visual and performance checks have
not been run in this phase.

## Composer: fix first, then polish

Keep the current border, input surface, and wide-screen information placement. Preserve the animated
thinking text after the context meter. Do not add a separate permanent status panel.

Observed issue: `render_chrome` reserves timer/model/effort width first, then clips status to the
remaining cells. The review wave uses a different prefix offset from the activity wave. This
explains truncation and identifies an overlap case to test. The reported bleed still needs a native
frame-sequence reproduction before its cause is considered confirmed.

Proposed fix:

1. Measure context, status, metadata, and mode badges in terminal cell widths before painting.
2. Allocate disjoint regions. The active status must remain readable; shorten optional metadata
   first. Only when needed, reserve an extra chrome row inside the existing box for overflow.
3. Keep that row outside the editable text region. Root must use composer-owned editor and hit
   rectangles, rather than reconstructing them from border thickness.
4. Clear owned chrome cells and paint each region once. Apply the existing wave within its region.
5. Check long-to-short-to-idle transitions, including `Running in background` and combined review,
   input-mode, timer, model, and child-agent labels. Never truncate a grapheme halfway.

Use the smallest geometry helper justified by these callers. A possible private type is:

```rust
// Proposed shape; not an existing API.
struct ComposerChromeLayout {
    context: Rect,
    status: Vec<Rect>,
    metadata: Vec<Rect>,
    editor: Rect,
}
```

Keep existing draft-layout caching separate. No widget framework, second event model, or rewritten
editor is needed. The fixed `272k` denominator is a separate display issue: use the budget already
observed by [ContextDiagnostics](../../bin/orvek/src/tui/context.rs), and label unknown values honestly.

Polish candidates for later native comparison: consistent edge padding, restrained focus contrast,
and better metadata spacing. These are not approval for a new chat-bar design.

## Actions: improve the current menu

Keep the current popup, search row, ordering, aliases, disabled explanations, and key behavior.
Two source-backed improvements are worth testing before cosmetic work:

- Show a small `No matching actions` message when filtering produces an empty list.
- Match the displayed state-dependent label. `display_label` can show `Disable fast mode`, while
  `Action::matches` currently searches the static `Enable fast mode` label.

After those fixes, compare modest changes to row alignment and selected-item contrast. Do not
replace substring search with fuzzy search or add categories, previews, or new panels without a
separate behavioral decision. External examples are references, not an instruction to copy their keys.

## Execution: preserve its clean structure

Keep the current one-line summaries and indented expanded output. Do not wrap each tool in a card,
add role headings to every row, or duplicate running state across new panels.

A focused candidate: long failed-shell summaries currently preserve outcome/duration but can lose
the first error fragment. Compare a bounded error excerpt within the existing summary, with full
details still available on expansion. Preserve command identity and process continuity.

Keep the timer optimization that replaces summary lines without rebuilding expanded details.
Color and animation must never hide errors, imply successful completion early, or move the viewport.
The small persistent activity mark still needs design review; neither its proposed shape nor its
new placement is approved. Do not add competing spinners around the composer.

## Welcome logo and motion

Preserve the approved glyph shapes and colors. Placement and the brief entrance effect remain
review choices. Input must work immediately; typing can bypass the entrance. Keep the logo as
terminal cells, with an ASCII and reduced-motion fallback.

Reuse [EmptyLogo](../../bin/orvek/src/tui/components/transcript/empty.rs) and the existing
[scheduler](../../bin/orvek/src/tui/scheduler.rs). Render only visible effects. Cache static art and
palettes; stop decorative deadlines when settled, hidden, or disabled. A proposed 20 FPS limit applies
to decorative motion, not to input or semantic updates. No extra process, network request, font
install, asset download, or effects dependency is required.

## Phases and checks

1. **Native baseline and reproduction.** Capture current composer, running shell, expanded output,
   and Actions popup at 40/80/120 columns. Record startup, input-to-paint latency, streaming frame
   time, idle wakeups, memory, and binary size. Keep measurements in local task evidence.
2. **Composer correctness.** Add failing tests for visible status, region isolation, and transition
   clearing; fix allocation and shared editor geometry. Keep the current appearance where it fits.
3. **Approved logo.** Replace only the welcome artwork; verify immediate input and a settled clock.
4. **Small menu/output improvements.** Evaluate the specific candidates above, one native
   before/after comparison at a time. Preserve existing tests for disabled actions and tool continuity.
5. **Review and verify.** Accept visual changes only after checking real terminal frames and
   interactions. Reject performance regressions beyond baseline measurement noise. Remove task-owned
   build output after any later authorized build/installation work is complete.

Test 32/40/50/60/80/100/120-column widths and small heights; long model/status strings; multiline and
wide-character input; selection/copy/paste; attachments; overlays; light/dark/low-color/reduced-motion
modes; cancellation; compaction failure; session switching; resume; forks; and child-agent completion.
Compare reused-buffer transitions with fresh renders, and verify no cells outside the assigned
region change. Animation ticks must not rewrap the draft or reparse the transcript.

Follow AGENTS.md: regression test revision, child fix revision, verification, then squash the fix into
the test revision. New features include their behavioral tests in the same revision.

Later implementation checks: `cargo check --all-features`, `just check-fmt`, `just clippy`, `just test`.
None were run in this design phase. No model evaluations, installs, or native code changes were made.
The HTML now demonstrates the welcome logo only; it does not validate composer layout or performance.
