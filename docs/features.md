# What works today

The capability inventory of the live editor — what a user can actually do right
now. Read [`roadmap.md`](roadmap.md) for what is coming, and
[`production-plan.md`](production-plan.md) for what is missing before this can
produce a finished video.

> Part of the PBC documentation set — see [`docs/README.md`](README.md) for the map.

- **Composition bar** (top) — editable Size (W×H), FPS, Duration, **BG**. Drives
  canvas fit, playback, frame step, timeline. Comp bounds drawn with a fill +
  border, the fill being `Comp::bg` — a per-comp **setting** saved in the `.pbc`
  (default `#5d677e`), not a theme constant. See *Colours* in [`editor.md`](editor.md).
- **Canvas** — vello rasterizes `evaluate(doc, t)` each frame; click a shape to
  select (front-most, via `NodeId` provenance). Selection gets a yellow outline.
  **Zoomable + pannable**: scroll zooms about the cursor, middle-drag pans, and a
  stacked tool strip at the bottom offers **Fit** plus fixed zoom stops
  (25/50/100/200/400/800%). **Fit** (the default) frames the comp in the canvas
  area with a 20px gap from the surrounding panels, and re-fits as they resize.
  See *The preview camera* in [`canvas-tools.md`](canvas-tools.md).
- **Transform gizmo** — the selected layer gets on-canvas handles: a centre
  square (free move), an X/Y arrow each (move along the *layer's* own axis), a
  box at the end of each axis (scale that axis), a corner box (uniform scale),
  and a ring (rotate). Everything pivots on the **anchor point**. The handles are
  a fixed size in screen points, so they stay grabbable at any zoom. See
  *The transform gizmo* in [`canvas-tools.md`](canvas-tools.md).
- **Transport** — Play/Pause (Space), Restart (R), ←/→ frame step, scrubbable
  playhead (an integer slider, so it can only land on frames). Readout is
  `hh:mm:ss.ff` plus `[frame/last]`. Playback runs off the wall clock but
  *quantizes* to the frame grid, so changing FPS visibly changes the playback
  cadence.
- **Work area** (AE's) — a comp-level **preview range** shown as a translucent
  band on the ruler. `B` sets its start at the playhead, `N` its end; **playback
  loops within it** and Restart returns to its start. It's **view state** — it
  bounds the loop, never evaluation — so it isn't saved with the document and
  resets when a comp opens. Scrubbing and frame-stepping still reach the whole
  comp (only *looping* is confined), so you can inspect a frame outside the band
  while paused. See *The work area* in [`canvas-tools.md`](canvas-tools.md).
- **Layers** (left) — scene tree; select, reorder (▲/▼), add Rect/Ellipse/Group,
  delete (✕), Save…/Load… (`.pbc` JSON via serde — document *and* panel layout).
- **Properties** (right) — resolved values for the selection; drag or click-type
  to edit. A painted **stopwatch** per property (filled = animated, hollow =
  constant) inserts a keyframe at the playhead — first click on a constant
  promotes it to a track (this is how a property *starts* animating).
  Transform (Position/Rotation/Scale/Opacity), **Fill**, **Stroke** (color +
  width, with add/remove — a node without one shows `+ add`), and **parametric
  geometry**: Size for a Rect or Ellipse, Radius for a Rect. Rows appear only
  where the property exists — a group has no fill, an ellipse no radius, an
  imported `Path` no size. **All of them are animatable**, on equal footing:
  every one gets a stopwatch, a dopesheet row, and the full selection /
  retime / copy-paste / easing treatment.
- **Timeline / dopesheet** (bottom) — a **frame ruler** with adaptive ticks
  (1/2/5/10-frame steps plus whole-second multiples, so labels land on round
  timecodes when zoomed out; per-frame minor ticks once frames are ≥6px apart),
  then one row per animated property with keyframes as diamonds and a red
  playhead. Click track to seek, click a diamond to select (Del removes), drag
  to retime (clamped between neighbours). Everything **snaps to frames** at any
  zoom.
  - **Multi-select**: ctrl/shift-click a diamond toggles it in or out of the
    selection; dragging a box on empty track marquee-selects everything inside
    it (replacing the selection, so shrinking the box deselects). Selected keys
    are drawn larger with a red border.
  - **Group retime**: dragging any selected diamond moves the *whole* selection
    as a rigid block, so internal spacing is preserved. The block clamps against
    its own outer neighbours rather than each key's — and across every affected
    property at once, so a multi-property selection translates instead of
    deforming. Dragging an unselected key selects it first.
  - **Copy/paste** (ctrl+C / ctrl+V): copies whole keyframes — values *and*
    easing handles — and pastes them with the block's first key on the playhead,
    spacing intact. Pasting selects what landed, so the next drag moves it.
    Suppressed while a text field has focus.
  - **Scroll** to zoom (the frame under the cursor stays pinned), **shift+scroll**
    to pan.
  - **Edge auto-pan**: while dragging the ruler or a keyframe, hold near either
    end of the track and the view scrolls that way — so a key can be dragged
    past the visible range. Drag-only on purpose; hover-panning would scroll the
    timeline out from under the pointer.
- **Easing editor** — selecting **exactly one** keyframe reveals a CSS-style
  cubic-bezier editor for its outgoing segment: draggable control points +
  Linear/Smooth/Ease In/Ease Out presets. Deliberately hidden for a multi-key
  selection: a segment belongs to one key, so there is no "the" curve for a set.
- **Dockable panels** — every area carries a header: an editor picker to change
  what it shows, plus split (`|`/`-`) and close (`x`). Drag the splitters to
  resize. A **Layout** menu (comp bar) switches between built-in presets
  (`Default`/`Animation`/`Design`) and saves the current arrangement as a
  session preset; the active layout and user presets are written into the
  `.pbc`. The canvas and the comp/transport toolbars are fixed chrome (no
  header), which keeps the single-canvas invariants safe.
- **Graph / expressions** — a summonable **Graph** panel (pick it in any area's
  header) drives the selected node's properties with expressions. `= fx`
  promotes a property (seeded from its current value); `bake` freezes it back to
  a constant. The expression is a **node canvas** — boxes wired parent↔child,
  each a `value` / `ref` (another node's property, at an optional frame offset) /
  `add` / `mul` / `neg` / **`script`** (a Rhai one-liner over `frame`/`time`,
  with its live result or error shown). Drag boxes to arrange them. A cycle or a
  bad script falls back to a neutral value instead of breaking the frame.

- **Export** — two buttons under the composition bar, and they are **different
  verbs** rather than one button with a mode
  ([0018](decisions/0018-two-render-buttons.md)).
  - **Draft** asks nothing: the whole comp, **full resolution**, fast encoder
    settings, written beside the project as `<name>_draft.mp4`. It is cheaper to
    *encode*, never cheaper to render — a preview that silently halved
    resolution would be the quickest way to ship the wrong file. It also refuses
    to write a path a saved preset claims, so a draft can never overwrite a
    deliverable.
  - **Master** renders the project's saved **render preset** — name, path,
    quality, scale and extra ffmpeg flags, stored in the `.pbc` — so two people
    on one project produce identical files. A project with no preset yet gets a
    default one written into it, which makes the *next* press reproducible.
  - The extension picks the container, exactly as on the command line: `.mp4`
    is H.264, a `.mov` master is **ProRes**, and a path with no video extension
    becomes a numbered **PNG sequence**.
  - A progress bar with **Cancel** replaces the buttons while a job runs. The
    editor stays live — the job renders a few frames per redraw rather than
    blocking ([0020](decisions/0020-the-render-job-is-stepped-not-threaded.md))
    — though its frame rate drops, because the export is sharing the GPU.
    Cancelling removes the partial video rather than leaving a complete-looking
    file of the wrong length.
  - The export renders through **the same vello renderer as the preview**, so
    what you see is what is written, minus the editor's own furniture (frame
    border, passepartout, selection, onion skins). One current exception: a
    layer with a **blur** exports without it.
  - The same renders are available headlessly:
    `motion render project.pbc --out film.mp4 --quality master`.
