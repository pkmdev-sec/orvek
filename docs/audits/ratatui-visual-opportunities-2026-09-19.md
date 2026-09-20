# Ratatui visual opportunities

Date: 2026-09-19

## Result

Orvek already uses Ratatui 0.30.2. It does not need a framework migration or version upgrade for this work. The current TUI also uses several appropriate Ratatui features:

- `Table` and `Scrollbar` for the message queue.
- `Gauge`, `Table`, and `Sparkline` for context diagnostics.
- Rounded `Block` widgets, `Clear`, and `Shadow` for dialogs.
- Constraint-based `Layout` in the context dashboard.
- `ratatui-image` for transcript images.

The next improvements should extend these patterns to older components. They should not replace the unboxed transcript or the custom composer.

## Recommended replacements

| Priority | Current implementation | Ratatui replacement | Target | Result |
| --- | --- | --- | --- | --- |
| 1 | Fixed RGB and ANSI status colors | Semantic `Style` values from `Theme` | `bin/orvek/src/tui/components/composer.rs:52-54`, `1530-1590`; `bin/orvek/src/tui/theme.rs:252-259` | Light and dark themes keep consistent contrast. Model, timer, path, reference, warning, success, and error colors become configurable. |
| 2 | Hand-drawn `ScrollIndicator` | `Scrollbar` and `ScrollbarState` | `bin/orvek/src/tui/components/choice.rs:192-216`, `221-273`; `dialog.rs:268-277`; `recent_prompt_picker.rs:294` | Lists and dialogs use one thumb-size and track-position algorithm. Styling comes from `theme.scroll_thumb()` and `theme.scroll_track()`. |
| 3 | Dialog content touches the border and reserves a body row for hints | `Block::padding` and right-aligned `Block::title_bottom` | `bin/orvek/src/tui/components/dialog.rs:104-137` | Every overlay gets the same one-cell horizontal inset. Key hints move into the border when width permits, which returns one row to content. Keep the current footer on narrow terminals. |
| 4 | Two-line session `ListItem` values concatenate metadata | Stateful `Table` with responsive columns and a two-line narrow fallback | `bin/orvek/src/tui/components/session_picker.rs:158-203` | Wide dialogs align age, session ID, model, effort, and workspace. Selection and scanning become clearer without changing search or navigation. |
| 5 | Memory records use an unaligned `List`; remote scope is text in the filter line | Stateful `Table`, `Tabs`, and `Scrollbar` | `bin/orvek/src/tui/components/memory.rs:653-718`, `721-767` | Columns align key, preview, namespace, and age. `Tabs` makes `All` and the authenticated namespace visible. The detail view gets a real position indicator. |
| 6 | Fork panes always use a manual 50/50 horizontal split | Breakpoint-based `Layout` with `Constraint`, `Flex`, and `Spacing` | `bin/orvek/src/tui/components/app.rs:490-533` | Keep side-by-side panes on wide terminals. Stack panes vertically on narrow terminals so each transcript and composer retains useful width. |
| 7 | Keyboard shortcuts are space-padded `Paragraph` lines | `Table` plus `Scrollbar` | `bin/orvek/src/tui/components/keybindings.rs:111-144` | Keys and descriptions remain aligned at every width. Scrolling becomes visible. The code no longer computes padding with repeated spaces. |
| 8 | The subagent `Active`/`All` filter is visible only in help text and the inspector | `Tabs` above the tree canvas | `bin/orvek/src/tui/components/subagents.rs:453-520`, `806-808` | The current filter is always visible and mouse-selectable. The tree, camera, inspector, and keyboard controls stay unchanged. |
| 9 | A pinned prompt shows only top and bottom ellipsis markers; the main transcript has no position indicator | Overlay `Scrollbar` widgets that appear only while detached from the tail | `bin/orvek/src/tui/components/transcript/mod.rs:929-972` and transcript scroll rendering | Users can see their location in long prompts and transcripts. Hide the main scrollbar while tail-following to preserve the clean transcript baseline. |
| 10 | Token flow is only a table, and history is only a sparkline | Horizontal `BarChart` for token composition and `Chart` for long history | `bin/orvek/src/tui/components/context_diagnostics.rs:347-432` | Wide, tall terminals can compare cached input, uncached input, output, and reasoning tokens by size and inspect values over calls. Keep the current table and sparkline below the size breakpoint. |

## Suggested order

1. Replace hard-coded colors and the shared scrollbar. These changes affect many screens but do not change interaction behavior.
2. Add dialog padding and bottom-border hints. Update standard and narrow render tests in the same change.
3. Convert the session picker, memory browser, and keyboard help to tables.
4. Add visible tabs for memory scope and subagent filters.
5. Make fork layout responsive.
6. Add transcript position feedback and richer diagnostic charts only after the simpler replacements settle.

## Components to keep custom

Do not replace these components with generic widgets:

- Keep the transcript renderer. A Ratatui `List` does not cover Orvek's variable-height markdown, images, text selection, expandable tool output, pinned prompts, or semantic anchors.
- Keep the composer editor and animated border. `Paragraph` and third-party text areas would lose mentions, queue integration, selection behavior, activity animation, and current cursor rules.
- Keep the model slider and effort dial. They already provide clearer model and effort selection than `Tabs` or `Gauge`.
- Keep the subagent tree renderer. `Canvas` can draw edges, but it does not replace the current label layout, camera, focus navigation, hit testing, and inspector.
- Keep the compact composer context meter. `LineGauge` needs a full row and would make the command deck heavier.

## Ratatui references

- [Built-in widgets in Ratatui 0.30.2](https://docs.rs/ratatui/0.30.2/ratatui/widgets/index.html)
- [`Scrollbar`](https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.Scrollbar.html)
- [`Table`](https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.Table.html)
- [`Tabs`](https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.Tabs.html)
- [`Block`](https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.Block.html)
- [`BarChart`](https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.BarChart.html)
- [`Chart`](https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.Chart.html)
- [Layout module](https://docs.rs/ratatui/0.30.2/ratatui/layout/index.html)
- [Style module](https://docs.rs/ratatui/0.30.2/ratatui/style/index.html)
- [Official Ratatui examples](https://ratatui.rs/examples/)
