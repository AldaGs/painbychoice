# Roadmap

The agreed build order and what each completed item actually delivered. For the
production-readiness view — what is missing before a finished video can leave
the app — see [`production-plan.md`](production-plan.md), which supersedes this
page wherever the two disagree about priority.

> Part of the PBC documentation set — see [`docs/README.md`](README.md) for the map.

Decided sequence: **composition settings ✅ → frame-based timeline ✅ → keyframe
UX ✅ → shape/stroke params ✅ → dockable panels ✅ → node graph + expression
IR ✅ → …**. Items 1–5 are complete; the build order now continues under *Agreed
order past #5* on this page (pre-comps first). The stages of each item:

1. ~~**Frame-based timeline.**~~ ✅ Done. Frames are `core`'s native time domain,
   with a ruler, timecode readout, snapping at any zoom, zoom/pan, and edge
   auto-pan. Deliberately *not* borrowed from AE: a separate graph editor and
   nested comps — the inline bezier editor already covers easing.
   - Left open: `duration` is still stored in **seconds** with
     `duration_frames()` derived. Storing frames outright is arguably more
     correct (the comp end would always land on a frame boundary) but it's a
     `.pbc` format change, so it wants to ride along with the next migration.
2. ~~**Keyframe UX polish.**~~ ✅ Done. `selected_key` became a `KeySelection`
   set; ctrl/shift-click toggles, dragging a box on empty track marquee-selects,
   a drag moves the whole selection as a rigid block, and ctrl+C/V copies keys
   (values *and* easing handles) to land on the playhead with their spacing
   intact. The group move clamps against the *block's* outer neighbours
   (`Track::move_keys_limits`), intersected across every affected track so a
   multi-property selection translates instead of deforming.
   - The marquee's "a box is live" flag round-trips through egui memory, so it
     lags the press by one frame — invisible, since the box has no area worth
     hit-testing until the pointer moves. Only a row response can tell us the
     drag began on empty track rather than on a diamond.
   - Paste replaces any key already sitting on a landing frame (the one-key-per-
     frame invariant `sample` needs) and drops keys that would land before frame
     0 rather than piling them up there.
3. ~~**More shape params + stroke editing.**~~ ✅ Done. Rect/Ellipse Size, Rect
   Radius, and Stroke colour + width are editable and fully animatable, plus
   add/remove stroke.
   - This pass also closed a gap: **fill was editable and had a stopwatch but no
     `PropKind` variant**, so fill keyframes existed with no dopesheet row —
     uncreatable-to-manage, invisible to select/retime/delete. Rather than add a
     fifth special case, `PropKind` became the single enumeration of animatable
     properties behind `prop_of`/`prop_of_mut`.
4. ~~**Blender-style splittable/dockable panels**~~ — ✅ Done. The layout
   tree + draggable splitters are done (see *The panel layout tree* above), and
   the canvas fit now derives from the tree instead of hardcoded panel sizes.
   Delivered in order:
   - ~~**Split / join areas** and a per-area dropdown to change which `Editor`
     an area shows.~~ ✅ Done. Each content area carries a header with an editor
     picker and split (`|` left/right, `-` top/bottom) + close (`x`) buttons;
     the ops are pure `Dock` tree rewrites applied after the UI pass (see *Split
     / join / retype* above). Deliberately **not** the literal drag-a-corner
     gesture: layered on egui's own panel splitters that's far more fragile than
     header controls, for the same capability. The canvas and the two toolbars
     stay header-less on purpose, which is what protects the single-canvas and
     innermost-canvas invariants.
   - ~~**Layout presets**: several named defaults plus user-made ones.~~ ✅ Done.
     A **Layout** menu in the comp bar switches between built-ins (`Default`,
     `Animation` — a tall dopesheet; `Design` — no dopesheet, wide canvas) and
     lets you name + save the current arrangement as a preset. Each built-in is
     just a `Dock` constructor listed in `builtin_presets()`; a user preset is a
     cloned tree (`Dock` is now `Clone`). A test pins that *every* preset keeps
     the structural guarantees (one innermost canvas; comp + transport present,
     since those headerless toolbars can't be re-added if a preset drops them).
   - ~~**Save the layout into the project** so a `.pbc` reopens the way it was
     left.~~ ✅ Done. The `.pbc` is now a `Project { document, layout }` wrapper
     (see *Saving the layout* in [`development.md`](development.md)); the active dock and user presets ride
     alongside the document. Built-ins stay code, not data. A loaded layout is
     validated (`Dock::is_valid`) and discarded for the default if it's broken,
     so a hand-edited file can't wedge the editor. Old bare-`Document` `.pbc`
     files still open — the loader falls back to a plain document parse.

   With that, **item #4 is complete.** Next is the node/expression IR (#5).
5. ~~**Node graph + expression IR**~~ (`Value::Expr` / `Value::Parametric`) — the
   big differentiator; the IR/printer discipline borrowed from the EBN project.
   ✅ Done — built in stages:
   - ~~**The `EvalCtx` seam.**~~ ✅ Done. `resolve` takes an `EvalCtx` instead of
     a bare frame (see *The core idea* above).
   - ~~**`Value::Expr` + the IR.**~~ ✅ Done — the headless engine now evaluates
     expressions. See *Expressions* in [`architecture.md`](architecture.md). In short: a `crate::expr` module with
     the dynamic `ExprValue { Num, Vec2, Color, Str }` and its `From`/`ToExpr` edge, a
     tiny IR (`Lit`, `Ref { node, prop, time_offset }`, `Add`/`Mul`/`Neg`), and a
     `ResolveCache` on `EvalCtx` doing per-frame memoization + cycle detection (a
     cycle → a `scene.warnings` entry + a neutral fallback, never a hang).
   - ~~**Node-graph panel.**~~ ✅ Done. A new `Editor::Graph` (summonable into any
     content area via the split/join picker — no default-layout change) lets you
     drive the selected node's properties with expressions: **`= fx`** promotes a
     property (seeded from its current value), **bake** freezes it back to a
     constant, and the expression is edited on a **node canvas** — boxes wired
     parent↔child, each with a kind picker (`value`/`ref`/`add`/`mul`/`neg`/
     `script`) and a compact editor; changing one node's kind grows the tree
     (operators seed neutral inputs). Layout is a tidy-tree auto-placement
     (`layout_expr`, a tested pure function) where each box's **height varies by
     kind** (`box_height`) so a `ref`'s three pickers or a `script`'s field +
     result line get the room they need and the stack stays clear; edits are
     deferred `GraphOp`s addressed by `(property, tree-path)` and applied after
     the UI pass by `apply_graph_op` (a free function, so the whole flow is
     unit-tested) — the same discipline as the dock. Boxes start on the tidy-tree
     layout and can be **dragged** to rearrange; positions are remembered per
     (node, property) in egui memory (ephemeral view state, not saved with the
     document).
   - ~~**Rhai scripting** (first cut).~~ ✅ Done. A `script` node kind holds Rhai
     source (`Expr::Script`), evaluated each frame with `frame`/`time` in scope;
     the result is a number (→ `Num`) or a 2/3/4-element array (→ `Vec2`/`Color`).
     `eval_script` runs on a thread-local engine; a bad script resolves to a
     neutral fallback (never breaks the frame) and the editor shows the error
     live.
   - ~~**The scripting bridge** — `value()` / `wiggle()`.~~ ✅ Done. Rhai's
     registered functions must be `'static`, so a script had no way to reach the
     `&mut EvalCtx` of the evaluation that called it. `mod bridge` in
     `core/src/expr.rs` parks that borrow in a **thread-local raw pointer** for
     exactly the span of one `eval_with_scope` — **the crate's only `unsafe`**,
     kept tiny so its three soundness rules can be checked by reading it: the
     guard clears the pointer on drop (lifetime), `with_ctx` *takes* it out for
     the callback so a second `&mut` can't coexist (aliasing), and it's
     thread-local so it can't escape (threads). A nested script re-parks through
     `enter` from the inner borrow, which is the correct nesting order.
     On top of it: **`value("A", "opacity")`** and **`value_at("A", "opacity",
     frame - 10)`** — by node *name*, first match in tree order — routed through
     the same memoized, cycle-guarded `resolve_prop` as `Expr::Ref`, so a
     self-reference warns and falls back instead of recursing until the stack
     goes; and **`wiggle(freq, amp[, seed])`**, smoothstep value noise that is
     deterministic per frame (scrubbing is stable, a render matches the preview)
     with a seed so x and y are independent streams. Confirmed working in a live
     session.

   - ~~**Exposed parameters** (the first half of `Value::Parametric`).~~ ✅ Done.
     A node carries named, animatable knobs (`Param` / `ParamValue`) that
     expressions read via an `Expr::Param` node and scripts via `param("x")` /
     `param_of("node", "x")` — one control driving many properties, and (once
     comps can nest) what a pre-comp will expose to its parent. Resolved
     through the same memoized, cycle-guarded path as a property reference, so
     a self-driving parameter warns instead of hanging. **A node-relative
     `param()` needs to know whose property is resolving**: `EvalCtx::in_node`
     marks that, and anything resolving a node's properties outside `evaluate`
     (the properties readout, the script preview) must go through it or it will
     show a fallback where the canvas shows the real value.
   - ~~**Procedural generators** (the other half).~~ ✅ Done. Typed-knob motion
     primitives instead of free-text Rhai for the common cases: **`osc`**
     (`offset + amp·wave(freq·frame + phase)`, with a sine/triangle/square/saw
     waveform), **`noise`** (the same value noise behind `wiggle()`, as a knobbed
     node), **`ramp`** (a linear `from→to` across a frame window, clamped flat
     outside), and **`bounce`** (`amp·e^(−decay·frame)·cos(2π·freq·frame)`, the
     classic overshoot-and-settle). Each is a new `Expr::Gen(Generator)` arm
     resolving to a `Num` (feed it through `mul` to broadcast onto a vec/colour),
     and — the reason this waited for parameters — **every knob is itself an
     `Expr`**: it defaults to a literal you drag in the canvas but can be rewired
     to a `param`/`ref`/expression like any other node. Frame-native and
     deterministic (the same contract as `wiggle`: scrubbing is stable, a render
     matches the preview). In the graph canvas a generator's knobs are wired-in
     child boxes labelled by name (`freq`/`amp`/…); picking a generator from any
     box's kind menu seeds it, and edits route through the same `GraphOp` /
     `apply_graph_op` path as every other node, so the whole flow is unit-tested.

## Agreed order past #5

**Agreed order past #5** (decided 2026-07-19): multi-composition / pre-comps ✅
→ document-wide property graph ✅ → Blender-standard graph UI (in progress) →
the Nuke-style *image* graph. Pre-comps come before the big graph because a comp
*is* a graph node, so building the graph first means rebuilding it; the image
graph is last because it needs the raster compositor stage ([0005](decisions/0005-compositor-stage.md)), which isn't
built. Note the distinction: today's graph is a **property** graph (values into
properties); Nuke's is an **image** graph (operations on pixels). They're
different machines. The plan for that scene/composition graph — how it lowers to
the existing IR, and the node-descriptor + socket registry that lets a new
object/effect/plugin auto-integrate as a node — is spec'd in *The composition node
graph — one node system, three scopes*
([0013](decisions/0013-composition-node-graph.md)).

**Canvas gizmos + grids (added 2026-07-20).** Not part of the agreed sequence
above — it came in sideways as a preview-panel need. The **transform gizmo is
done** (see *The transform gizmo*). Still open, in this order:

1. ~~**Grid + rulers + guides**~~ ✅ Done — see *Grid, rulers and guides*.
   Per-comp state saved in the `.pbc`, alongside `Comp::bg`.
2. ~~**Snapping**~~ ✅ Done, pivot *and* bounding-box edges/centres, including
   against other layers — see *Snapping*.
3. ~~**Anchor-point handle + selection bbox**~~ ✅ Done — see *The anchor handle
   and the selection box*.
4. ~~**Onion skinning**~~ ✅ Done — see *Onion skins*.

That completes the canvas-gizmos track as agreed — grid, rulers, guides,
snapping (pivot and bounding box), the anchor handle, selection boxes, the
motion path and onion skins are all in.

> **Graph-UI progress (2026-07-20):** module bodies now have a real editing
> surface — you open a module from the graph panel and edit its body + knobs on
> the same node canvas a property uses (see *Editing a module body* above).
> **Seeding a fresh `Use` link from the kind picker is now done:** since a bare
> `ExprKind` can't name a module, the box's kind combo lists the project's
> modules below the primitives (`ExprKind::ALL`), and choosing one emits a
> `SetModule` op — which repoints an existing link *or* replaces any other kind
> with a fresh `use <module>` (no overrides). No core change: `Use` stays out of
> `ExprKind::ALL`, and `Expr::seed(Use)`'s placeholder module is never reached by
> the picker.
>
> **Override sub-expressions are now editable on the canvas** (the last open
> graph-UI item). A link's overrides became **first-class children** of the
> `Expr::Use` node — `arity`/`child`/`at`/`at_mut` treat `overrides[i]` as slot
> `i` — so the existing canvas lays each override out as a wired box with its own
> kind picker, and edits route through the ordinary `GraphOp`s (path = the link's
> path + the child slot). An override can therefore be a literal, a `ref`, a
> `param`, a script — anything, not just a literal. The `use_editor` row shrank to
> the two-state toggle: **override** seeds a literal `0` child to build from,
> **inherit** (the `x`) drops it so the knob follows the module again; the value
> itself is edited in the child box, labelled with the knob name (derived in the
> canvas, since override names are dynamic and core's `slot_label` is `&'static`).
> `eval_use` is unchanged — it still reads overrides by name — so this is a walk
> change, not a semantics change. With that, the Blender-standard graph UI step is
> complete.

> **Timeline UX + fps retiming (2026-07-20, user-verified):** changing a comp's
> fps now **re-grids** the animation instead of leaving keys on stale frame
> numbers — a key at frame 120 @ 60fps (two seconds in) lands on frame 48 @ 24fps.
> `Comp::set_fps` is the only supported way to write the rate on a comp with
> content (a plain `fps =` shifts every key in seconds); it walks the tree
> rescaling every frame position by `new_fps/old_fps`, `LayerTiming` included.
> The conversion rounds to whole frames, so it's lossy — keys under a frame apart
> merge, first one wins. The fps **drag** applies as a *single* retime off a
> pre-drag snapshot (`App::fps_drag`), not one rounding per delta, or a slow drag
> would shred dense keys passing through every intermediate rate. The keyframe
> **selection rides along**: a `KeyRef` is a track index, and a merge shifts
> indices, so `remap_selection` re-resolves the selection through frames across
> the retime (snapshotted in the drag too, so a long drag can't walk it off its
> keys). Per-delta cost is a tree clone + retime — measured ~1.3 ms at 100k keys
> in release, linear, only a concern near 500k.
>
> Dopesheet + transport got a Blender-style pass: the label/track split is now
> **two resizable columns** sharing one full-height splitter, with every row's
> label cell allocated at the identical width via `allocate_exact_size` (the old
> `allocate_ui_with_layout` grew a row whose button/name didn't fit, desyncing
> its track from the ruler axis). The timeline header gained **zoom in / out /
> fit** buttons (anchored at the playhead; the wheel and the buttons share
> `zoomed()`). The transport is `|◀ ◀◀ ▶ ▶▶ ▶|` — jump to range start, prev/next
> keyframe (disabled when there's none), play/pause, jump to range end — plus
> numeric **Start/End** fields over the existing work area. The old blue playhead
> slider is **gone**: it mapped `0..=last_frame` while the ruler maps the visible
> window, so once zoomed it landed the playhead somewhere other than where you
> dropped it.
>
> Icons are a real font now (`live/src/icon.rs`, Tabler subsetted to the named
> codepoints — regenerate the subset when adding a glyph, see `assets/NOTICE.md`),
> so the old "egui renders ◆◇ as tofu, paint your own" gotcha is retired for
> anything drawn through `icon::`.

> Two riders on this order, both feeding the *reusable animation modules* feature
> spec'd in [`decisions/`](decisions/): the **pre-comps** step also introduces the
> **per-layer in/out time model** (a pre-comp is a layer with a time range) and
> the **layer-local time sources** expressions need to retime one animation to
> each clip; the **document-wide property graph** step is where a shared,
> overridable animation *module* becomes first-class. **Text layers** are a
> separate near-term primitive, independent of the graph work.

> The bigger, further-out features (renderer/compositor model, 2.5D, footage
> import, export, plugins, expressions) have their architecture decided in the
> [`decisions/`](decisions/) — read the relevant ADR before starting any of them.
