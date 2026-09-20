# Transcript visual direction

Date: 2026-09-19

## Result

The transcript needs a stronger reading structure, not more widgets on every row. The recommended direction is a centered reading column with selective message chrome, a connected turn timeline, compact status rows, and clearer type hierarchy. Keep the composer and transcript-first layout. Do not add a permanent header, sidebar, or dashboard.

This review uses screenshot `/var/folders/mg/wdx8vj9d419dcvk_v8_xs82c0000gn/T/otty-paste/image-1789850043950.png` and the Ratatui 0.30.2 website examples.

## What makes the screenshot look basic

1. **The reading line is too wide.** Reasoning text spans almost the full 2,738-pixel screenshot. Long lines are difficult to scan and make the empty background dominate the composition.
2. **Most system information has one visual level.** The host event, reasoning heading, reasoning body, and turn footer all use similar muted text. Color distinguishes the user and answer, but structure does not distinguish event roles.
3. **Spacing separates records instead of grouping a turn.** Every transcript entry receives a trailing blank row. Reasoning, its tool call, the answer, and the turn footer look like unrelated records.
4. **Infrastructure detail dominates user meaning.** The full task UUID is louder than the status `registered with the host`.
5. **The tool summary reads like raw telemetry.** `Task status 0 arguments` has no aligned state, duration, or result signal.
6. **The answer is displayed twice.** The session journal for `d1a43ef6-2af4-4dca-acdd-6a7e9921b0d1` contains one final assistant message at revision 16, but the screenshot renders two identical answers. This is a presentation defect, not duplicate model output.

## Recommended design

### 1. Constrain prose to a reading column

Use a responsive `Layout` with `Constraint::Max(112)` and `Flex::Center` when the terminal is wide. Keep full width on narrow terminals. Tool output, code blocks, diffs, and images can opt into a wider lane when they need it.

This change gives the largest visual improvement with the least chrome. It follows Ratatui's [dynamic layout](https://ratatui.rs/recipes/layout/dynamic/) and [Flex](https://ratatui.rs/examples/layout/flex/) patterns.

### 2. Group each turn as a connected timeline

Treat the user prompt, reasoning, tools, answer, and completion status as one turn. Use a one-cell semantic rail and compact connectors instead of a blank row after every entry. Keep one full blank row between turns.

Use Ratatui's [collapsed borders](https://ratatui.rs/examples/widgets/collapsed-borders/) pattern only for the connected rail. Do not put a full border around every message.

### 3. Use selective blocks

Use `Block` only where containment adds meaning:

- Render the user prompt as a compact rounded block with horizontal padding.
- Render reasoning as a subtle left-border or rounded block with a small `reasoning` title.
- Keep the final answer open and unboxed so it remains the primary reading surface.
- Keep host events and turn completion as compact timeline rows.

Ratatui's [`Block` example](https://ratatui.rs/examples/widgets/block/) shows border types, titles, layout, and padding. The existing Orvek dialog use of `Block::padding` can be reused.

### 4. Add semantic type hierarchy

Use styled `Line` and `Span` values instead of applying one style to each entire record:

- Accent and bold for titles such as `reasoning` and `task_status`.
- Normal text color for readable content.
- Muted text for IDs, metadata, and elapsed time.
- Success, warning, and error colors only for states.
- Italics only for short reasoning metadata, not the full reasoning body.

This follows Ratatui's [text styling recipe](https://ratatui.rs/recipes/render/style-text/) and [`Paragraph` example](https://ratatui.rs/examples/widgets/paragraph/).

### 5. Replace raw host lines with status rows

Render the screenshot's task event as a compact milestone:

```text
◆ Task e7844cc7  registered                                      host
```

Show the short ID by default. Preserve the full ID in selection text, an expanded detail, or a copy action. Use the event's semantic state rather than parsing a formatted string in the renderer.

### 6. Make tool summaries scan like operational rows

Render a collapsed tool call as an aligned row:

```text
├─ task_status                                      ✓  0.2s
│  no arguments
```

Use a `Table` only for expanded arguments, metadata, and results. Keep the current custom tool component for focus, expansion, and nested children. Ratatui's [`Table` example](https://ratatui.rs/examples/widgets/table/) supports the aligned detail view.

### 7. Fix preview-to-final answer reconciliation

When one incomplete assistant preview exists for a request and the confirmed journal item arrives with a different provider item ID, reuse the preview entry instead of adding another entry. Preserve multiple confirmed assistant items when the provider genuinely returns more than one.

The relevant ownership is in `bin/orvek/src/tui/transcript/model.rs`, where assistant entries are keyed by `(request, item)`.

## Target appearance

```text
                         ╭─ you ─────────────────────╮
                         │ Hello                     │
                         ╰───────────────────────────╯

◆ Task e7844cc7  registered
│
├─ reasoning
│  Considering task response
│  I need to inspect the task status before answering.
│
├─ task_status                                      ✓  0.2s
│  no arguments
│
╰─ Hello! How can I help?

✓ Completed in 7s · 2 calls · 13.4k tokens
```

The exact content remains selectable. Narrow terminals collapse labels and metadata before reducing the content width.

## Do not add

- No permanent header or sidebar.
- No card around every assistant paragraph.
- No `List` replacement for the transcript renderer. It cannot preserve variable-height markdown, images, selection, pinned prompts, expandable tools, or semantic anchors.
- No shadows in the transcript. Keep `Shadow` for overlays.
- No charts, gauges, or tabs in the main conversation.

## Implementation order

1. Fix duplicate assistant reconciliation and add a regression test.
2. Add the responsive reading column with standard, wide, and narrow render tests.
3. Replace per-entry blank rows with turn-aware spacing.
4. Add the user block, reasoning rail, and semantic status rows.
5. Improve collapsed and expanded tool presentation.
6. Verify selection, links, images, pinned prompts, scrolling, and narrow terminals before deployment.
