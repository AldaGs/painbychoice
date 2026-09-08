# Architecture

How the engine is put together and why. This is the **reference for the code as
it exists** — decisions about work not yet built live in
[`decisions/`](decisions/), and the build order lives in
[`roadmap.md`](roadmap.md).

> Part of the PBC documentation set — see [`docs/README.md`](README.md) for the map.

Four crates, deliberately layered. `core` is headless and knows nothing about
GPUs or windows — the engine must be testable by rendering a frame in a unit
test, not a window. This separation is the whole design; keep it.

```
crates/
  core/    document model + evaluation engine. No GPU, no windowing. (unit-tested)
  render/  evaluated Scene -> pixels. SVG backend (offline). vello lives in live/.
  app/     offline binary `motion`: evaluate the demo doc -> out/frame_*.svg
  live/    the real editor `pbc`: winit + vello (wgpu) + egui over the engine
```

## The core idea

Every animatable property is a `Value<T>` — a *recipe*, never a baked result:
a constant or a keyframe track today; an expression / parametric-node IR later.
`evaluate(&doc, t)` is a pure function that resolves the whole scene graph at
time `t` into a flat `Scene` of draw items. Non-destructive editing and
non-linear scrubbing both fall out of that single design choice.

A resolve takes an **`EvalCtx`** rather than a bare frame:
`Value::resolve(&self, ctx: &mut EvalCtx)`. `EvalCtx` carries the frame, the
document, a resolve cache, and a warnings sink — one struct threaded through the
whole walk so nothing needs re-plumbing as the engine grows. `evaluate` builds
one context and shares it down the walk (`&mut`, because resolving an expression
mutates the cache).

Every evaluated item carries a `source: NodeId` (provenance) so a frame traces
back to the node that produced it — used for click-to-select and debugging.

## `Scene::places` — why a node's place is separate from its drawing

`eval.rs`'s walk records a `Placement { world, pivot }` for every live node,
alongside the draw list. A node need not draw anything — a group or a null has
no shape and so no `RenderItem` — but it still has a place, and it is exactly
the sort of layer you parent things to and animate. **Both** editor overlays
read it: the motion path takes `pivot`, the transform gizmo takes `world`, and
so both work on a bare group.

Three properties are load-bearing:

- `pivot` is the **anchor**, not the local origin. `local` maps the anchor point
  to `position` by construction, so this is the point the layer rotates and
  scales about, and the point the gizmo centres on. An overlay drawn anywhere
  else sits away from where the layer visibly turns.
- `world` is the whole **parent chain** multiplied out, which is what an overlay
  needs to know which way the layer's axes point — not merely where it is. A
  pivot alone would place the gizmo correctly but aim its arrows wrong.
- A node outside its time window is **absent, not zeroed**. The walk returns
  before reaching it, so "the layer isn't here on this frame" is expressible —
  which is what lets the motion path break its polyline instead of drawing a
  line to the origin, and what stops the gizmo lingering over an off-screen
  layer.
- `bounds` is the subtree's extent, **filled after the children are walked**,
  because a group has no geometry of its own. It is `None` when nothing in the
  subtree draws — better than a zero-size box, which would act as a phantom snap
  target sitting in every composition's corner. Having it here rather than in
  the editor is what lets snapping read *every* sibling's extent from one pass
  instead of walking per candidate.

## Frames are the native time domain

`core` thinks in **frames, not seconds**. `Keyframe.frame` is an `i64`, and
`evaluate(&doc, frame)` takes a *fractional* frame: keys sit on the grid, the
playhead need not (which leaves room for sub-frame sampling — motion blur —
later). Seconds are a **presentation unit**, converted only at the edges by
`core/src/timebase.rs`.

The payoff is that `fps` never has to be threaded into the value engine: a
track is a function of frames, and only the composition knows what a frame is
worth in wall-clock time. Changing fps therefore re-times the document without
drifting keyframes off their frames — the same thing After Effects does.
Integer frames also killed two float-epsilon fudges (key matching, and
neighbour clamping when dragging).

## Expressions (`core/src/expr.rs`)

A third `Value` arm beside `Const`/`Keyframed`: `Value::Expr(Expr)` computes a
property from *other* values. This is the shared substrate roadmap #5 is built
on — expressions and (later) a node graph are two front-ends that lower to the
same `Expr` IR (the EBN "IR + dumb-printer" split: the IR is data, evaluation is
a pure tree-walk).

- **Dynamic↔typed edge.** An expression works in
  `ExprValue { Num, Vec2, Color, Str }` and pins the type down only at the
  property, via `FromExpr`/`ToExpr` (impl'd for exactly the scriptable types —
  never `BezPath`). A kind mismatch resolves to `T::fallback()` (a neutral zero,
  or the empty string), never a failed frame. Every conversion is **strict**,
  including text: mixing kinds is something you ask for with `Add`, not
  something the property edge does behind you.
- **The IR** is deliberately tiny: `Lit`, `Ref { node, prop, time_offset }`, and
  `Add`/`Mul`/`Neg` (`a - b` lowers to `Add(a, Neg(b))`). `Ref`'s `time_offset`
  is the `valueAtTime(t')` case — sampling another property at a *shifted* frame.
- **Dependency resolution is pull-based DFS** — a dependency resolves because you
  recurse into it, so there's no separate topo sort. `EvalCtx`'s `ResolveCache`
  adds a `visiting` set (a back-edge is a **cycle** → a `scene.warnings` entry +
  a neutral fallback, so a self-referential doc warns instead of hanging) and a
  `(node, prop, frame)` **memo** (the frame is in the key, so an off-time sample
  can't poison the primary value's slot).
- **Determinism** is by construction: every node is a pure function of the frame
  and the values it reads — no IO, no clock. That's the same sandbox a script
  engine (Rhai, next) and WASM plugins will reuse.

No authoring UI yet — expressions are built in code or a hand-edited `.pbc`
(`Value::Expr` serializes like any other value). The node-graph panel that lets
you *build* them is the next #5 step.

## Shared animation modules (`Module` + `Expr::Use`)

The document-wide property graph, made concrete (2026-07-19), with UI.

A `Module { name, params, body }` lives on the `Project`, not a node: one
definition, addressable from every comp. A property links it with
`Expr::Use { module, overrides }`. Editing the definition edits every link.

This is less "new engine" than promoting a pattern the expression graph already
supported by convention (park the animation on a controller node and `Ref` it)
into a first-class object with a real definition site, per-link overrides, and
automatic retiming.

- **Overrides are call-by-value, evaluated in the caller's scope**, before the
  module's scope is pushed. That is what lets a link feed the module its own
  `t01` or a node param, and it keeps the body a pure function of its knobs.
  A module body has no state to read back, so laziness would buy nothing and
  cost a borrow-checker fight over storing `&Expr` in the context.
- **Override is a layering, not a fork.** A link stores *only* the knobs it wants
  different; the rest inherit, so a later edit to the module still reaches an
  overridden instance. Same shape as `Value`'s const→keyframe→expr layering.
- **`param("x")` inside a body means the module's knob**, not the owning node's —
  a module is closed over its own scope, which is what makes it reusable. An
  explicit `Param { node: Some(id) }` still reaches that node deliberately.
- **Retiming is free**: a body reading `t01`/`localTime` reads *whichever layer
  is resolving*, so one module fits itself to every clip. That is the subtitle
  story, and it is a unit test.
- **Third cycle guard, same discipline** as the property and comp ones: a module
  that links itself warns and falls back rather than recursing. An override
  naming a knob the module lacks warns too — a silent no-op would be a typo trap.

**The UI**, in the graph panel: a **Modules** list (rename / delete / **edit**),
a **`-> module`** button on any expression-driven property, and a **`link`**
picker on any property that isn't one yet. A link's box shows a module picker
and one row per knob reading either `inherit` or the overridden value.

The **edit** button is the graph-UI step: it opens the module's *body*
on the same node canvas a property uses, plus the module's own knobs — see
*Editing a module body* in [`editor.md`](editor.md). Before it, a module could be *made* (by
extracting a property) but its body edited nowhere; you could only relink or
tweak knobs at a call site.

- **Extract is a no-op on the frame.** The recipe moves to the module and the
  property links it, so pressing `-> module` on work you care about is safe.
  Unit-tested by evaluating either side and comparing.
- **`inherit` is spelled out, not implied by a blank field.** The point of a
  module is that unset knobs keep following the definition; a UI that can't show
  the difference between "inheriting 0.4" and "overridden to 0.4" hides it.
- Repointing a link keeps overrides whose knob names still exist, and deleting a
  module leaves its links warning rather than silently reverting.
- `apply_graph_op` takes the whole `Project` now, since modules are project-wide.
  The tests that predate projects go through an `apply_op` shim.

- `ExprKind::Use` is deliberately **not** in `ExprKind::ALL`: that list is the
  in-box kind picker, and seeding a link needs a *module* picker, not a bare
  kind. A property links a module through `-> module` / `link`; a module body's
  own boxes can't spawn a nested link from the kind menu (which keeps a module
  from accidentally linking itself while you edit it). Repointing an existing
  link still uses the module picker inside `use_editor`.

## Text

`Shape::Text { content, family, size, align, max_width }` — `core/src/text.rs`.
Two decisions, both load-bearing:

**Text resolves to glyph *outlines*, not glyph runs.** `Shape::to_path(ctx)` is
the seam every renderer consumes, so shaping into a `BezPath` means a text layer
fills, strokes, transforms, keyframes, and animates through the *existing*
pipeline — the SVG backend and the offline `motion` binary render text without
knowing it exists, and **no renderer changed at all** to add this. Handing vello
glyph runs (the obvious route, and what an earlier note here assumed) would have
been live-only. parley does the real work — bidi, script segmentation, font
fallback, line breaking, alignment — and skrifa pulls outlines for the shaped
glyph ids; the only coordinate work here is the y-flip from font space (y-up,
from the baseline) into layout space (y-down). Text is centred on the origin, the
convention `Rect`/`Ellipse` already follow, so anchor/rotation/scale behave.

**Families resolve against the system font set** — so `family` stores a *name*,
never font bytes. **This is the one place the engine is not deterministic:**
everywhere else `evaluate(doc, t)` is pure, so a render matches the preview and
tests pin output; a `.pbc` naming "Futura" instead draws Futura where it's
installed and a fallback where it isn't. An unknown or blank family falls back
through the generic sans-serif stack rather than failing, so a project from
another machine still draws. The tests in `text.rs` therefore assert *structure*
(non-empty path, bigger size ⇒ bigger box, wrapping ⇒ taller and narrower,
unknown family still draws) and never exact coordinates.

`size` **and `content`** are both `Value`s, so both keyframe and take
expressions (`PropKind::TextSize`/`TextContent`, `PropPath::TextSize`/
`TextContent` → `text_size` / `content` in scripts). `content` became one when
`ExprValue` grew `Str` — see *The string value type* on this page, which is what makes
the typewriter effect an expression rather than a built-in.

Shaping contexts (parley's `FontContext`/`LayoutContext`) are `thread_local` and
reused, like `expr.rs`'s script engine — enumerating system fonts is far too
expensive to redo per frame.

### The geometry fold, both directions

Added 2026-07-21. The graph could already *author* geometry — `lower_geometry`
compiles a shape node's `geometry` output into a whole `Shape`, and a
`ShapeBinding` makes a layer's shape be that output. What it couldn't do was
bring geometry into or out of itself: a binding needed a layer that already
existed, and an existing `Shape` couldn't be pulled onto the canvas. Both ends
are now closed, so the node canvas is a place you can work rather than a panel
that decorates layers made elsewhere.

- **Create layer** (on any node with a `geometry` output) adds a layer whose
  shape *is* that output and binds the two. The shape is lowered immediately
  rather than left for the next recompile, so the layer is correct the instant
  it appears instead of flashing a placeholder. It parents at the **root**, not
  under the layers-panel selection — a graph-created layer shouldn't inherit
  whatever happens to be selected in a panel that had nothing to do with the
  click. `App::new_layer_look` / `push_layer` are shared with the toolbar's
  `add_node`, so the two kinds of layer are indistinguishable.
- **Import ▸ Shape** is `raise_geometry` (`core/src/raise.rs`), the inverse of
  `lower_geometry` and tested as one: lowering a raised shape reproduces it, the
  geometry counterpart of `lower(raise(e)) == e`. Each param is filled by what
  its `Value` *is* — a constant becomes the socket's stored literal, an
  expression is raised through the existing `raise` and **wired in** (so you can
  keep editing the recipe, which is the point), and a keyframe track is refused.
- **Refusing a keyframed param is the design, not a limitation.** A raised shape
  is bound straight away, and a graph-authored param lowers to `Value::Expr` —
  so going ahead would *replace* the track and the animation would simply be
  gone. `RaiseShapeError::Keyframed` names the offending param so "bake Radius
  first" is actionable; "something is keyframed" wouldn't be, on a shape with
  several. Same discipline as the property fold asking you to promote first.
  The check runs **before** any node is built, so a refusal can't leave orphans.
- **The refusal needed somewhere to be read.** `App::import_property` had been
  reporting failure with `eprintln!` since it was written — invisible to anyone
  running a GUI, which makes a refusal indistinguishable from a dead button.
  `App::ng_status` (view state, not saved) now carries one line into the Nodes
  panel in the usual warning amber, cleared by the next successful graph edit so
  a stale complaint can't outlive what it was about.
- The document work sits in **free functions** (`create_layer_from_geometry`,
  `import_shape`) with the `App` methods as thin wrappers over id allocation,
  selection, and status — the same split `compile_drivers` follows, and the
  reason both directions are unit-tested without a window.

This deliberately stops short of nodes-*being*-the-scene. The layer tree is
still the structural spine; the graph authors and drives it. Making a node's
existence create a layer is the composition-graph step, still gated behind the
compositor.

### The string value type

Added 2026-07-21. `ExprValue` gained a fourth kind, `Str(String)`, so text is a
*value* like every other — keyframable, scriptable, wirable — rather than a
plain field bolted to the side of a text layer. The typewriter effect falls out
of it as an expression; nothing in the engine knows the word "typewriter".

- **`ExprValue` is no longer `Copy`.** One heap variant demotes the whole enum.
  Interning the strings to keep `Copy` was considered and rejected: `Expr::Lit`
  is *serialized into the `.pbc`*, so an interner would have to round-trip
  through the document format too. The ripple was 8 call sites, all mechanical.
- **A string track holds, it doesn't interpolate.** `impl Animatable for String`
  returns the starting key until the segment ends — there is no halfway point
  between two strings, and inventing one (a cross-fade through character codes,
  a progressive reveal) would be an *effect* masquerading as interpolation. So a
  text track is a step sequence, which is exactly what titles and subtitles
  want. Easing handles are still stored and are simply inert, so the dopesheet
  needs no special case.
- **`Add` concatenates, and it's contagious**: if either operand is text the sum
  is text, so `"take " + n` reads the way it does in any scripting language.
  This lives in `eval_expr`, not in `ExprValue::zip` — `zip` only knows how to
  combine two numbers component-wise. `Mul`/`Neg` pass text through untouched.
- **Conversion at the property edge stays strict.** A lenient `FromExpr for
  String` (stringify whatever arrives) was tried and reverted: a script that
  errors resolves to `Num(0.0)` as its universal "nothing", and a lenient
  conversion renders that as the text **"0"** — a broken expression putting a
  plausible-looking zero on the canvas instead of reading as broken. Empty +
  the existing warning is honest. Mixing is still available, just *explicit*,
  through `Add`.
- **`SocketType::Text` interchanges with `Number` in `feeds`.** Note the
  asymmetry with the rule above: wire legality is not a promise the value
  converts. The math nodes' sockets are declared `Number` but are really "any
  scalar value" — a `value` node's output is `Number` whatever literal it holds,
  which is how a `Vec2` already flows through an `add` today. Refusing text
  there would make concatenation unbuildable on the canvas even between two
  strings.
- **Rhai needed almost nothing**: it has a native string type, so text crosses
  the bridge as itself and the whole Rhai string library (`sub_string`, `+`,
  `len`) is available to a text property for free. A script returning a string
  used to be an *error*; that assertion was inverted.
- **A `string` input node** joins `value`, rather than `value` growing a text
  mode: the socket type is what the canvas colours a wire by, and one node that
  changed its output type under you would make a graph unreadable. `raise` picks
  it for a `Str` literal, so `lower(raise(e)) == e` still holds.
- **`content` left `TextConfig`** and became a real `Text` input socket on the
  text node — that is what makes a typewriter *a wire from a script node*
  instead of a built-in. `family` stayed config: it names a system font, so it's
  a lookup key, not a value. Knobs gained `ParamValue::Str` / `ParamKind::Str`
  to match.
- **Old `.pbc` files still open.** `content` was a bare JSON string before this
  and is a tagged `Value` now, so `de_text_content` accepts either. Resolvable
  on the spot (a plain string *is* a `Value::Const`), so unlike the frames
  migration there's no deferred `migrate()` step — and the next save rewrites it
  in the current form.

### Picking a font, and when one is missing

A missing font is **invisible by construction**: parley substitutes and the text
draws perfectly well in the wrong face, so nothing about the frame reveals it.
Hence `text::font_exists` and two places that report it:

- **`scene.warnings`** — `Shape::to_path` warns through `EvalCtx::warn_here`
  (widened to `pub(crate)` so a *shape* can warn, not just an expression), which
  puts it behind the comp bar's existing amber indicator for free. Same channel
  as a broken script, same "fall back to something drawable but say so" rule as a
  dangling `Ref`.
- **The Font row** — an amber warning glyph beside the picker, naming the font
  and explaining that the project still stores the name, so it will look right on
  a machine that has it.

A **blank family is never "missing"**: it means "use the default" deliberately, so
it reports as existing and never warns — otherwise every new text layer would
ship with a warning on it.

The picker itself follows the modern-editor shape: a searchable list of every
installed family with recently-applied fonts pinned on top, and **hover to
preview, click to apply**. Hovering only reports
`PropEdits::text_family_preview`; `App::preview_project` then renders *that*
frame from a throwaway clone with the family swapped in, so browsing hundreds of
fonts never touches the document and needs no undo. The clone only happens while
a row is hovered, so the common path pays nothing. Recents are session-only
(`remember_font`, a free function so its most-recently-used ordering is
unit-tested) — they're app state, not project state, so they stay out of the
`.pbc`.
