# Component design decisions

Review one component at a time. Open its HTML proposal with macOS `open`, then wait for the user's
explicit approval. Feedback revises that component only. An approval locks the named version and
scope; changes to it need another review. Design approval does not authorize native implementation.

The welcome logo is already approved. The current review is **Chat bar, proposal 1**.

| Order | Component | State | Artifact |
| --- | --- | --- | --- |
| 1 | Welcome logo | Approved: glyphs and colors only | [Logo](tui-motion-preview.html) |
| 2 | Chat bar | Awaiting approval | [Proposal 1](components/composer-v1.html) |
| 3 | Conversation and execution rows | Not started | Current native layout is the baseline. |
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
- A modest border contrast change on focus.
- The thinking wave stays after context; foreground brightness changes without shifting text.
- Status takes priority over metadata. Overflow uses reserved rows inside the existing frame;
  the editor moves down with its measured region. Hints shorten before the workspace disappears.
- Multiline input grows to six visible rows in the study, then scrolls. The native limit remains
  a review decision; existing keybindings and draft handling are preserved by the implementation.

Use the Activity and Terminal width controls, then type or use the multiline sample. Enter creates
a newline in this browser study. Nothing is sent to an agent.

Approval covers the visual arrangement and state behavior demonstrated here. The document records
no approval yet. Native glyph-width handling, editor interactions, performance, and the full layout
matrix still require implementation checks in the [technical plan](tui-motion.md).

Future components have no approved design and should not receive speculative preview changes.
