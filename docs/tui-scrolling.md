# TUI scrolling

Orvek handles scrolling inside the alternate screen. It does not use the terminal
emulator's normal scrollback buffer. Terminals send discrete wheel events, not
pixel offsets, so trackpad motion is translated into row movement.

## Pointer and keyboard ownership

- The wheel targets the pane under the pointer without changing keyboard focus.
- Click a pane to give it keyboard focus. Page keys then act in that pane.
- An open picker or dialog owns scrolling within its visible area. Wheel events
  do not reach the transcript hidden behind it.
- Moving the pointer or resizing the terminal does not close file or skill
  suggestions.

## Scrollable views

| View | Controls and behavior |
| --- | --- |
| Main transcript | Wheel, PageUp/PageDown, Ctrl+Home/End; click the detached banner to follow new output |
| Composer | Wheel over the draft; typing resumes cursor-follow without changing the stored draft |
| Input queue | Bounded viewport; wheel does not steer, reorder, remove, or edit inputs; pages move keyboard selection |
| File, skill, session, action, theme, model, effort pickers | Wheel inside the list and viewport-sized page movement; wheel movement stops at the ends |
| Memory details, help, context diagnostics | Wrapped content, wheel and page movement, bounded offsets after resize |
| Recent prompt preview | Separate list and preview scrolling; cannot scroll beyond the wrapped preview |
| Review download confirmation | Scroll the full question in short panes; Enter/Y still confirms and Escape/N cancels |
| Child transcript | Wheel inside its body; click its detached banner to follow |
| Child tree | Wheel pans vertically; Shift+wheel or horizontal wheel pans horizontally; keyboard navigation resumes focus-follow |

Text views wrap instead of providing horizontal panning. Terminal-native scrollback,
pixel-smooth motion, and draggable scrollbars are not implemented by these changes.

## Verification

```sh
cargo test -p orvek --bin orvek tui::
just check-fmt
just clippy
just test
```

The tests render panes and overlays, send input events, and inspect visible output.
They cover hover routing, modal ownership, clipped content, overscroll, resizing,
queue selection identity, and child-camera behavior. These tests do not prove how
every terminal emulator converts physical trackpad gestures into mouse events.
