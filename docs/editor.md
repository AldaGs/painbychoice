# The editor (`live/`)

The shell around the engine: how the window is organised, how a panel is
written, and the systems every panel shares. Canvas-space tools have their own
page — see [`canvas-tools.md`](canvas-tools.md).

> Part of the PBC documentation set — see [`docs/README.md`](README.md) for the map.

## `live/` module layout

`main.rs` grew past 5,000 lines and was split by concern (2026-07-19) — a pure
move, widening visibility to `pub(crate)` and nothing else, with an identical
test count either side.

```
live/src/
  main.rs       the `use` block, module decls, and `fn main`
  app.rs        App: window/GPU state, winit handler, the per-frame update
  dock.rs       panel layout tree, its editors, the composition bar
  layers.rs     scene-tree panel
  props.rs      properties panel, easing, and the PropKind enumeration
  timeline.rs   transport bar, ruler, dopesheet
  strips.rs     layer bars — the clip view of the timeline
  curves.rs     the value-curve view
  nodegraph.rs  expression / node-graph panel and its GraphOps
  scene.rs      evaluated Scene -> vello, canvas fit/zoom/pan + pick, zoom toolbar
  offscreen.rs  the export render target: vello -> texture -> RGBA8 readback
  renderqueue.rs the two render buttons and the stepped export job
  gizmo.rs      the transform gizmo
  aids.rs       grid, rulers, guides, snapping
  pen.rs        the Bezier pen tool and point editing
  motionpath.rs the motion path overlay
  onion.rs      onion skins
  footage.rs    the decoded-frame cache (the one place pixels are decoded)
  history.rs    undo/redo snapshots
  icon.rs       the bundled icon font
  theme.rs      colours and styling
  tests.rs      the unit tests
```

> The split above was a **pure move**; the list is kept current as modules are
> added. `main.rs` is now a few dozen lines — `app.rs` is the per-frame heart.

Every module opens with `use crate::*;`, and `main.rs` re-exports each module
with `use <module>::*;`. So the crate root is one shared namespace and no module
keeps its own import bookkeeping — moving an item between modules needs no
import edits at either end. (Modules are children of the crate root, so this
glob reaches root's private items too; that's why it works.)

What the split deliberately did **not** do: `App::update` is still ~850 lines.
Moving it made it findable, not simpler. Decomposing it is a separate job — the
UI pass has real ordering constraints (measure the canvas → run egui → apply
edits after) that a naive split would break.

**UI discipline in `live/`:** the egui closure never borrows `App`. Each panel
reads a plain snapshot gathered before the closure and reports intent into a
small `*Edits` struct; `App` applies those after the closure. This keeps the
render-path field borrows (state/context/renderers/doc) from colliding. Follow
this pattern for any new panel.

## The panel layout tree

`Dock` is a binary tree — `Split { side, size, resizable, first, second }` with
`Editor` leaves — borrowed from EBN's `layoutTree`. A split pins `first` to one
edge at `size` points and gives `second` the remainder, so the tree's nesting
*is* egui's outermost-to-innermost panel order, and `show_dock` renders the whole
thing by recursing into a plain `Ui`. `Dock::default_layout()` builds the stock
arrangement; adding named presets means writing more constructors.

Two things are load-bearing:

- **The canvas is a leaf.** vello paints it, not egui, but it must occupy a leaf
  so the tree knows where the leftover hole is. It has to be the *innermost*
  one (there's a test): every other panel claims an edge, the canvas is what
  remains.
- **The fit takes that leaf's measured rect**, not the window minus hardcoded
  panel sizes. The constants version could not survive a draggable splitter:
  `pick` inverts this transform, so stale geometry doesn't just misdraw the
  canvas, it sends every click to the wrong shape. The leaf measures itself with
  **`available_rect_before_wrap()`, not `max_rect()`** — egui shrinks a `Ui`'s
  *available* region for the sibling panels shown before the canvas leaf but
  leaves `max_rect` at the full window, so `max_rect` would fit the comp to the
  whole window and float the tool strip in the window corner (see *Known issues*).

`size` is stored in the tree (and written back from the real panel rect each
frame) rather than living only in egui's panel memory — that's what keeps the
tree the source of truth, so saving layouts is a `serde` derive rather than a
scrape of egui internals.

**Split / join / retype.** Every *content* area wears a thin header
(`area_header`): an editor picker plus `|`/`-`/`x` buttons. Splitting a leaf
rewrites it to a `Split` of two clones; closing an area collapses its parent
`Split` to the surviving sibling; the picker swaps a leaf's `Editor`. These are
pure tree edits (`Dock::apply`), and — like every other panel — they don't
mutate mid-render: an area header only *records* a `DockCmd` against the leaf's
`path` (a `Vec<Branch>` naming it from the root), which `render` applies once
the egui pass is done. Restructuring the tree while its panels are still laid
out would desync egui's per-panel ids.

- **Only the three content editors are `SWAPPABLE`** (Layers, Properties,
  Dopesheet). The canvas and the comp/transport toolbars are *structural* leaves
  with **no header** — so a user can't duplicate, retype, or close them. That's
  not a UI nicety, it's what keeps the two canvas invariants safe: there is
  always exactly one canvas leaf to measure (`canvas_rect`), and it stays the
  tree's innermost leaf. A closed area is always content, so the canvas — its
  sibling, or in an untouched ancestor branch — always survives.

**Known wrinkle:** the canvas rect is measured during the UI pass, but the fit is
needed *before* it (to pick, and to build the vello scene), so the fit uses the
previous frame's rect. Stale only while a splitter or the window is actively
dragged, and self-correcting on the repaint a drag guarantees.

## The timeline panel: three views of one thing

The timeline is **one** dockable editor (`Editor::Timeline`) with a mode
selector, not three panels competing for the bottom of the window:

| Mode | Answers |
| --- | --- |
| **Layer Strips** | *When is this layer alive?* One bar per layer, its `LayerTiming` window. |
| **Dopesheet** | *When do its keys happen?* One row per animated property, keys as diamonds. |
| **Curve Editor** | *How do they move?* Every channel plotted as value over time, tangents editable. |

They share the row set, the keyframe selection, the time axis (`Axis` +
`TimelineView`), the label column width, and one `time_ruler()` — the ruler *is*
the panel's time axis made visible, and two implementations would eventually
disagree about where a frame is. Switching mode should feel like turning a card
over, not like opening a different tool.

`Editor::Timeline` carries `#[serde(alias = "Dopesheet")]`: the dock layout is
document data, and an unknown serde variant fails the **whole** `.pbc`, not just
the layout.

### Curves are sampled, never re-derived

`PropRef::channels()` views a property as one `Track<f64>` per numeric channel —
a `Vec2` becomes X and Y, a colour R/G/B, text none — each carrying the
*original* keys' timing (`Keyframe::shaped`). The plot samples those tracks, so
the drawn curve cannot drift from what plays back, and `Interp::Hold` renders
for free despite having no bezier form.

Every channel draws its own key knobs (a `Vec2` key has an X and a Y to grab),
but **tangents draw on channel 0 only**: one keyframe has one pair of handles,
and two sets of arms could be dragged into disagreeing.

### Easing: `Interp`, broken tangents, and the ease library

- `Interp::{Bezier, Hold}` sits on the **outgoing** side of a key. A hold is not
  a timing curve — no control points make a bezier stay flat then jump — so
  `Track::sample` checks it *before* solving the handles.
- `Keyframe.broken` unlocks a key's two tangents. Presentation, not evaluation,
  but it must survive a save. Re-locking mirrors the *outgoing* handle onto the
  incoming one (`mirror_handle`, `(1-x, 1-y)`) rather than averaging, which
  would move an arm nobody touched.
- `EasePreset { name, out, into }` — `BUILT_IN` is a `const` table (never
  serialized, so old files pick up new built-ins) and `Project.eases` is the
  per-project library, beside `modules` and `graph`: an ease is a house style
  and has to travel with the `.pbc`.

## Undo / redo: snapshots, not inverse operations

`live/src/history.rs`. The history stores **whole-document snapshots**. That is
a deliberate choice over an op-based (inverse-command) history, and it works
because of a property the editor already had: **every panel reports its intent
and all document mutation happens in one contiguous phase after the UI pass**.
So one copy taken before that phase and one comparison after it covers every
edit site there is:

```rust
let before = self.project.clone();
// ... the whole apply phase: props, dopesheet, tree, comp, node graph, aids ...
if self.project != before {
    self.history.record(before, edit_label, pointer_down);
}
```

The consequences are the point:

- **A new edit site gets undo for free.** Nothing has to opt in, and nothing can
  be forgotten. An op-based history fails the other way: a missing inverse is
  silent and *corrupts* the stack rather than losing one step.
- **`PartialEq` is derived across the document types** (`Project`, `Comp`,
  `Node`, `Transform`, `Shape`, `Value`, `Track`, `Expr`, `Mask`, `Camera`, …)
  purely to make that comparison cheap — it early-outs at the first difference.
- The cost is a clone per frame. Acceptable because the document is a *recipe*:
  shapes, tracks, expressions. **Footage is a path and a size, never pixels**
  (the asset registry's rule), so importing a 4K clip does not make a snapshot
  any bigger. `MAX_STEPS = 128`; old steps fall off the bottom.

**Coalescing is per pointer-down session.** A `Step` carries `open: bool`, true
while the pointer that started it is down, and a record that lands on an open
step is merged into it — keeping the *oldest* `before` of the run. So a gizmo
drag, a `DragValue` drag or a curve-handle drag is **one** undo, rewinding to
where the gesture began rather than to its previous frame. `end_interaction()`
is called every frame the pointer is up, which is what closes the run. A step
pushed by *redo* is closed on arrival — otherwise the next drag would merge into
it and one undo would rewind both.

Three ordering rules in `App::update`, each load-bearing:

1. The snapshot and the **label** are taken at the *top* of the apply phase,
   because several edit intents are `take`n as they are applied. `edit_label`
   is only a nicety (it names the tooltip); an unrecognised edit is `"Edit"`,
   and nothing in it may gate whether a step is *taken*.
2. **Undo/redo are applied after the record**, so the swap they perform is never
   itself mistaken for an edit and pushed back onto the stack.
3. **Loading a project clears the history.** Opening a different document is not
   an edit *of* the open one, and undoing across that boundary would resurrect a
   document the user has moved on from.

A restored snapshot is internally consistent on its own — it carries the graph
*and* the properties that graph lowered into — so **there is no recompile after
an undo**. What does need re-deriving is everything pointing *at* the document
from outside it, which is `App::after_history_jump`: a missing open comp falls
back to the root, `next_id` is raised (never lowered — a redo can bring back
nodes that still hold their ids), the timeline window is re-clamped, a selection
naming a deleted layer is dropped, and the keyframe selection is cleared
(a `KeyRef` is an *index*, so a step that adds or merges keys shifts what it
names).

Keys are Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y, read off egui's input and suppressed
while a text field has focus (same rule as keyframe copy/paste — Ctrl+Z in a
name field belongs to the field). The comp bar carries Undo/Redo buttons whose
tooltips name the step; they are words rather than icons because a glyph would
mean regenerating the icon font subset.

## Stacking order

**There is no z-index.** `eval::walk` pushes a node's own `RenderItem` and *then*
recurses into `children` in order, so `scene.items` is depth-first document
order and the renderer plays it back in order — **later in the list draws on
top**. Everything else follows from that one rule:

- A parent's own shape always draws **behind** its children.
- A group occupies **one slot** in its parent's order; nothing interleaves into
  a group and nothing leaves it.
- A precomp folds inline (no isolated rasterization), which is also why there
  are no blend modes yet.
- `pick` iterates `.rev()`, so clicking agrees with drawing by construction.
- `Node::replace` keeps a node's index, so pre-composing never restacks.

The **panel lists layers front-most first** (`tree_rows` / `strip_rows` iterate
`children.iter().rev()`), matching After Effects, Figma and Illustrator; a
parent row still sits above its children, Figma-style. That is display only —
the document is untouched — so the up/down buttons invert through
**`reorder_delta(up) -> +1`**, since `Node::reorder_child` speaks document
indices. The helper is named precisely so the sign flip doesn't get "fixed" back
into a bug by someone reading one side of it.

### Wanting a child in front of its parent

It already is: children always draw in front of the parent's own shape. The
inverse — artwork *behind* a child, or a child escaping its parent's slot — is
impossible by design, because a PBC node may carry a shape **and** children
(AE/Figma groups are pure containers, so the question never arises there).

The fix is structural rather than a second ordering concept: **Split shape into
child layer** (right-click a layers row) moves `shape`/`fill`/`stroke` into a
new child at **index 0** with an identity transform, so the frame is unchanged
and the artwork becomes an ordinary layer you can restack. The transform stays
on the parent — it governs the whole subtree, and moving it would change what
the other children do. Graph drivers on the moved properties are re-pointed at
the new layer; transform drivers stay. **Limitation:** expression refs by *name*
(`value("box", "size")`) still point at the parent, which no longer has those
properties.

## Editing a module body (the Blender-standard graph UI step)

The next roadmap step past pre-comps, made concrete (2026-07-20). A module's
**body** is now edited on the same node canvas a property is, so a module is
authored in one place rather than only inherited from the property it was
extracted with.

The seam is a `GraphTarget`: every tree-editing op (`SetKind` / `SetLit` /
`SetRef` / `SetScript` / `SetParam` / `SetWaveform` / `SetOverride` /
`SetModule`) now carries `GraphTarget::Prop(kind)` **or**
`GraphTarget::Module(id)` instead of a bare `PropKind`, and `edit_expr`
resolves the tree root from it — the node's property, or `module.body`. The
canvas, the box layout, the kind picker, and every in-box editor are byte-for-
byte the same for both; only the address differs. That is what makes this cheap:
the module body reuses the whole property-canvas machinery.

- **No selection required.** A module body isn't any node's property, so
  `apply_graph_op` takes `selected: Option<NodeId>`; the node-scoped ops
  (promote/bake/extract/link and any `Prop`-targeted edit) no-op without one,
  while module edits go through regardless. Which module is open is **view
  state** (`App::editing_module`), reported as a `GraphEdits::edit_module`
  intent and applied beside the document op, never in it — the same discipline
  as the canvas' box positions.
- **A module's knobs are editable too**, through the same parameters surface a
  node has: `ParamOwner::{Node(id), Module(id)}` says whose knobs an add/remove
  touches. A body full of `param("…")` nodes is useless without knobs to point
  them at, so the two ship together; removing a knob leaves the body's
  `param()` warning and falling back, like any dangling reference.
- **Deleting the open module closes it** (the app clears `editing_module`), so
  the panel can't keep editing a body that no longer exists.
- **Left for later, deliberately:** a link's *override* can be a whole
  sub-expression, but that's still shown-not-edited — it would want its own
  nested canvas at the call site, which the body canvas doesn't provide. And a
  module-body script's `param("x")` previews as a fallback (the module scope
  isn't pushed for the preview) though it resolves correctly at render time
  through the link.

## The node canvas zooms, it doesn't magnify

Added 2026-07-22, after the UI was reported to lose quality when zoomed in. The
canvas used to ride inside `egui::containers::Scene`, which draws into a
**transformed layer**: egui lays the contents out at 1× and scales the finished
shapes afterwards. Vector shapes survive that (they're re-tessellated from
transformed geometry), but a text galley is *rasterized at its layout size and
then stretched* — so every label, field and icon went soft the moment you passed
1:1. egui's own `Scene` caps its zoom range at 1.0 for exactly this reason; we
had raised it to 2.0 and inherited the blur. There is no per-layer rasterization
scale to turn on: `Context::set_zoom_factor` is global to the whole app.

So the canvas owns its zoom now (`nodegraph.rs`):

- **`View { zoom, pan }`** maps graph space to screen space — `to_screen` /
  `to_graph`, `s(len)` for a scaled length, `font(size)` for a font at its
  *on-screen* size. Node positions stay in graph units in the model (they're
  saved); only the drawing is in screen units. A drag delta arrives in screen
  pixels and is divided by the zoom on its way back into the document.
- **Every geometry constant goes through `View::s`.** A constant that doesn't is
  a constant that won't zoom, which is why the socket-hit radius, the wire
  thickness and the drop tolerance are all scaled too — the drop target should
  be as big as it looks.
- **The in-node widgets are scaled through their `Style`**, not through a
  transform: `View::scale_style` multiplies the text styles and the spacing, so
  a combo or a `DragValue` on a zoomed node is laid out at the size it is drawn.
  That is the whole trick — nothing is laid out small and stretched.
- **Navigation is ours**: middle-drag pans, wheel pans, ctrl-wheel/pinch zooms
  about the cursor (`View::zoom_about`, unit-tested to keep the point under the
  cursor fixed, *including when the zoom clamps* — the pan has to follow the
  applied factor, not the requested one). Zoom range is 0.15–4.0; zooming in
  costs sharpness nothing now.
- **"Frame all" is `fit_view(graph_bounds(..), area)`**, both free functions and
  both unit-tested without a window. It never magnifies past 1:1 — a two-node
  graph should frame as two normal nodes, not two billboards. The view is stored
  per scope as an `Option<View>`, and `None` means "frame on the next draw", so
  the toolbar button is just *forgetting where we were*.
- The canvas rect is measured with `available_rect_before_wrap()`, not
  `max_rect()` — the same egui trap the preview canvas hit — and remembered in
  memory so the toolbar (drawn *before* the canvas) can zoom about its centre.
  One frame stale by construction, like the preview's.

**Right-click the canvas for the palette.** The Add menu's contents live in
`palette_items`, shared by the toolbar button and the canvas context menu so the
two can't drift into offering different nodes. The right-click form passes the
click position, so a node lands **where you asked for it** rather than in the
staggered corner the toolbar still uses (it has no canvas position to speak of).
The position is captured when the menu *opens*, not when an entry is clicked —
by then the pointer is over the menu, and reading it there would drop every node
under the menu instead of under the click.

## Property In: the read half of the scene seam

Also 2026-07-22. The `ref` node had been filed under Input as "Reference", which
hid the only node that answers *"what is that layer doing right now?"* from
anyone looking for the counterpart of Property Out. It's now **`Property In`** in
the Layer category beside its sink, and a configured one titles itself
`boxA → Position` against the sink's `Position → boxA` — arrow pointing the way
the value travels in both cases.

The real bug behind the reframing: **a ref's output socket was always `Number`**,
whatever it read. `GraphCtx::descriptor_for` now retypes it from the target
property exactly as it does an `out` node's *input*, so reading a `position`
hands down a Vector wire and a `fill` a Colour one, and the canvas refuses the
wires that never made sense. Retargeting across types sweeps the wires that no
longer fit (`retarget_ref` + the new `NodeGraph::disconnect_output` — a read can
feed many inputs, so unlike the sink's single incoming wire it has to sweep them
all); a same-kind move (Rotation → Opacity) keeps them.

## Colours

Three surfaces, deliberately distinct, and they live in three different places
for a reason:

| Surface | Value | Where it lives |
| --- | --- | --- |
| UI chrome (panels, headers, widgets) | `#2d2d2d` | `theme::UI_BASE` → `egui::Visuals` |
| Preview backdrop (the letterbox) | `#23262d` | `theme::PREVIEW_BACKDROP`, vello's `base_color` |
| Composition area (inside the frame) | `#5d677e` **default** | `Comp::bg` — **document data** |

The backdrop is vello's, not egui's, because the canvas is a GPU hole with no
egui behind it. The composition colour is a **per-comp setting** rather than a
theme constant: it is what the frame renders against, so it belongs to the
document and is saved with it (`#[serde(default)]`, so pre-`bg` `.pbc` files load
on the default rather than rendering transparent). Edit it in the composition bar
under **BG**. Widget states are `shade()` offsets from `UI_BASE`, so re-tinting
the editor is one constant.

## Icons (`live/src/icon.rs`)

Tabler Icons as a font (MIT, subsetted to 8 KB — see `live/assets/NOTICE.md`).
Registered in its **own** family rather than as a fallback on the proportional
one: a fallback would let any missing character silently resolve to an icon
glyph, turning a text bug into a baffling picture. `icon::text` / `icon::button`
ask for the family explicitly, so an icon is always deliberate.

Every glyph is a **named const** — a raw `"\u{ea62}"` at a call site is
unsearchable, and you can't tell a chevron from a trash can by reading the diff.
Adding an icon means adding a const *and* re-running the subset with its
codepoint; miss the second step and it renders as tofu, which is a visible
failure rather than a blank button.
