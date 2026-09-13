# Interactive TUI references

Research date: 2026-09-13. Discovery source: [Awesome TUI](https://awesometui.com/), its
[AI catalog](https://awesometui.com/ai), [coding-agent catalog](https://awesometui.com/ai-coding-agents),
and [linked repository list](https://github.com/alvinunreal/awesometui).

[View the reference gallery](tui-references.html). These are existing applications, not Orvek mockups.
The selections below prioritize a real conversation view and editable input. Visual judgments are
recommendations based on official screenshots and documentation, not measured quality rankings.

## Shortlist

| Reference | What to study | Possible use in Orvek | Boundary |
| --- | --- | --- | --- |
| [Elia](https://github.com/darrenburns/elia) | Bordered message input, clear focus, options dialogs, and spacing. | Refine the current composer border and padding; keep the existing Actions popup. | Its multiple framed messages are not a proposed execution layout. |
| [OpenCode](https://opencode.ai/docs/tui/) | Conversation interleaved with brief tool summaries; optional tool details; slash commands and file references. | Preserve Orvek's concise execution flow and review spacing between summaries and replies. | Do not import its input surface, navigation, or keybindings wholesale. |
| [oterm](https://github.com/ggozad/oterm) | Growing prompt, collapsible thinking, and compact usage information during a chat. | Check how secondary information stays out of the input and how long responses remain readable. | Keep Orvek's thinking wave. Its borderless styling is not the target. |
| [Gurk](https://github.com/boxdot/gurk-rs) | A stable input window below a dense, readable conversation; separate message and input focus. | Check multiline input bounds, cursor visibility, and space use. | No channel sidebar or messaging features are proposed. |
| [Oatmeal](https://github.com/dustinblackman/oatmeal) | Rust chat UI with multiline editing, slash commands, and editor integration. | Compare chat-input proportions and keyboard behavior in an actual terminal conversation. | Its chat bubbles are a lower-priority visual reference given the desired compact output. |

Start visual comparison with **Elia's input** and **OpenCode's execution flow**, using native Orvek as
the baseline. Use Gurk for layout discipline and oterm for long-chat behavior. These references do
not approve any replacement component.

## Evidence and limits

- Elia's official [screenshot collage](https://github.com/darrenburns/elia/assets/5740731/75f8563f-ce1a-4c9c-98c0-1bd1f7010814)
  was inspected. Its [README](https://github.com/darrenburns/elia#readme) documents inline/full-screen
  chat, options, and theme roles. The collage illustrates a published version, not a fresh local run.
- OpenCode's official [conversation screenshot](https://raw.githubusercontent.com/anomalyco/opencode/dev/packages/web/src/assets/lander/screenshot.png)
  was inspected. Its [TUI guide](https://opencode.ai/docs/tui/) documents `/details`, `/` commands,
  and `@` references. The screenshot and current documentation can represent different releases.
- Gurk's official [chat screenshot](https://raw.githubusercontent.com/boxdot/gurk-rs/main/screenshot.png)
  was inspected. Its [README](https://github.com/boxdot/gurk-rs#readme) describes multiline input and
  message navigation. Treat the screenshot as a layout example, not proof of current performance.
- oterm's [README and demo](https://github.com/ggozad/oterm#readme) describe its prompt, thinking
  section, and usage footer. Its streaming-speed statement is an author claim, not an Orvek result.
- Oatmeal's [README and demo](https://github.com/dustinblackman/oatmeal#overview) document chat and
  input actions. Neither its live interaction nor its performance was tested here.

Crush remains a limited reference for event colors and bounded motion. Its inspected source uses
[FSL-1.1-MIT](https://github.com/charmbracelet/crush/blob/d333e04385f9e1d1523cea7b417cb5e8798a713a/LICENSE.md).
This research imports no third-party code or artwork into the native application.

## Framework decision

| Framework | Verified connection | Recommendation |
| --- | --- | --- |
| [Ratatui](https://ratatui.rs/concepts/rendering/) | Orvek already uses 0.30.2. [Gurk](https://github.com/boxdot/gurk-rs/blob/main/Cargo.toml) uses Ratatui; [Oatmeal](https://github.com/dustinblackman/oatmeal/blob/main/Cargo.toml) pins an older Ratatui version. | Keep it. Cleaner layout does not require changing the rendering framework. Existing widget APIs still need compatibility checks before any reuse. |
| [Textual](https://textual.textualize.io/widget_gallery/) | [Elia](https://github.com/darrenburns/elia/blob/main/pyproject.toml) and [oterm](https://github.com/ggozad/oterm/blob/main/pyproject.toml) use it. | Study focus, spacing, and input behavior. Do not introduce a Python UI process. |
| [Bubble Tea / Bubbles](https://github.com/charmbracelet/bubbles) | Go components include text areas, lists, and spinners. | A component reference only; retain Orvek's Rust input and event ownership. |
| [TachyonFX](https://github.com/ratatui/tachyonfx) | Ratatui effects library. | Defer. The approved logo and existing text wave do not justify another dependency. |

Ratatui computes terminal updates from frame buffers. That limits terminal writes, but does not make
expensive layout or parsing free. Preserve Orvek's caches and measure native input/streaming costs.

No referenced application was installed or benchmarked. Official demo links are provided for visual
review. The [refinement plan](tui-motion.md) defines the source-backed changes and later native checks.
