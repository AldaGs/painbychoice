# Gotchas

Hard-won traps, mostly egui's. Every entry here cost real debugging time at
least once; several are load-bearing enough that a test pins them. **Read this
before fighting the UI framework** — the odds are good it is already described.

> Part of the PBC documentation set — see [`docs/README.md`](README.md) for the map.

- ~~**egui default font lacks many glyphs**~~ — fixed by bundling an icon font
  (see *Icons* above). It used to be that `◆ ◇ ● ○ ❚ ⟲ ▸` rendered as tofu, only
  `▶`/`•` were safe, and anything else had to be *painted* (see `key_button`) or
  spelled out in words. Icons now come from `icon::*`; the painted indicators
  that remain are kept because they encode state (a filled vs hollow stopwatch),
  not because a glyph was unavailable.
- **The box-select flag round-trips through egui memory.** Only a row's
  `Response` can tell us a drag began on empty track (a diamond grabs the press
  first), but the marquee rect is needed *before* the rows loop — so "a box is
  live" is stashed with `data_mut` and read on the next frame. The one-frame lag
  is invisible (the box has no area worth hit-testing until the pointer moves);
  don't try to "fix" it by hoisting the hit-test out of the loop.
- **egui eats the shift modifier on shift+wheel**, rewriting it into a
  *horizontal* scroll. So the pan signal is a nonzero `smooth_scroll_delta.x`,
  not `modifiers.shift` — checking `shift` silently does nothing.
- **`ui.max_rect()` is the whole window, even for the canvas leaf.** egui shrinks
  a `Ui`'s *available* region for sibling panels shown before it, but not
  `max_rect`. Measuring the canvas leaf with `max_rect` fit the comp to the whole
  window and floated the zoom strip in the window corner; use
  `available_rect_before_wrap()` for the leftover central region.
- **An egui panel's returned rect is content-driven, and it persists.**
  `Panel::show` hands back the *inner response* rect, which grew (or shrank) to
  whatever the content allocated — clamped only at `max_size` — and egui stores
  that same rect as the panel's `PanelState`, so the next frame starts from it.
  A leaf whose content changes height therefore resizes its own panel and shoves
  every other leaf around, canvas included. This is why selecting a layer used to
  resize the whole window: the dopesheet grows a row per animatable property, so
  every select/deselect moved the preview. The invariant that fixes it is
  `Editor::scroll_wrapped` — **every leaf must either fill its area exactly or
  scroll inside it, never allocate past it** — enforced by a test. Note the
  scroll wrapper is `ScrollArea::vertical` with `auto_shrink([false; 2])`:
  horizontal scrolling would desync the dopesheet tracks from the ruler (frames
  map across the panel's *width*), and letting it auto-shrink reintroduces the
  same bug from the other side.
- **Hit-test a drag at `press_origin`, never at the live pointer.** egui reports
  `drag_started()` only once the pointer has moved past its drag threshold, so
  by the time you handle it the pointer has usually left the small target that
  was under the press. Resolving "what did they grab?" against
  `pointer_latest_pos()` therefore finds nothing and the grab silently fails —
  the smaller the target, the more often. This is what made guides need a *held*
  click to drag: holding still kept the pointer inside the 5pt band long enough
  to be found. Use `ui.ctx().input(|i| i.pointer.press_origin())`.
  **`Response::interact_pointer_pos()` is not a substitute** — despite the name
  it tracks the ongoing interaction and moves with the drag. Both `aids.rs` and
  `gizmo.rs` resolve their grabs this way; the gizmo's handles are big enough
  that it rarely misfired, which is exactly why it went unnoticed there.
- **`is_pointer_over_egui` is area-based, not widget-based.** It asks which
  *layer* is under the pointer, and for the background layer whether the point
  falls outside the root `Ui`'s available rect. So it is `false` everywhere in
  the canvas hole **no matter what you draw or `ui.interact` there** — an
  interactive rect inside the hole does not make egui "want" the pointer. Any
  canvas-space widget that must not double as a canvas click therefore needs its
  own flag; the gizmo uses `App::gizmo_hot`. Getting this wrong is silent: the
  widget works, and the click *also* falls through to the picker.
- **Redraw is event-driven** (`ControlFlow::Wait`). Anything that must keep
  animating while the pointer is held still (edge auto-pan) needs an explicit
  `ctx.request_repaint()`, or it stops the moment input stops.
- Panel sizes are in egui *points*; the canvas fit is in *physical pixels* —
  multiply reserved sizes by `window.scale_factor()` (already done in `render`).
- LF/CRLF warnings on commit are harmless (no `.gitattributes` yet).
- **Don't round-trip source files through PowerShell** `Get-Content -Raw` /
  `Set-Content`: PS 5.1 decodes as ANSI and mojibakes every non-ASCII character
  (`—`, `×`, `▲`). Edit files directly.
