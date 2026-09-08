# 0013. The composition node graph — one node system, three scopes

- **Status:** Accepted — partially built
- **Recorded:** 2026-09-08, extracted verbatim from the README design journal.

## Context

"Nodes to drive animation" and today's property graph are different machines,
and building the second without deciding how it relates to the first produces
two unrelated panels.

## Decision

The differentiator past today's per-property expression editor, and the home for
"drive animation with nodes." Decided 2026-07-21. **Nothing here is built yet**;
this records the model so the reasoning survives, like every other decision record.

**Two graphs, named.** What `graph.rs` renders today is a *property* graph: pick
a node, promote one property, edit its `Expr` tree (`Lit`/`Ref`/`Add·Mul·Neg`/
`Gen`/`Script`/`Param`/`Use`) as an auto-laid-out **tree** (`layout_expr`) whose
wires only run parent→child. Values flow *into one property*. What "nodes to
drive animation" asks for is a *scene/composition* graph: a free-form **DAG** of
whole things — a rectangle with output sockets, math with input+result, comps
wiring mattes/masks/effectors/adjustment-layers/parenting — where outputs are
*shared*, not owned by one property. These are different machines (the Roadmap
already draws the line: property graph ✅ vs. the Nuke-style image graph, unbuilt).
This doc is the plan for the second, built so both become **one node system at
different scopes** rather than two unrelated panels.

**The model decision: alongside, lowering to the IR.** The `Node`/`Comp` tree
stays the **structural spine** — layers, groups, parenting, precomp instancing.
The node graph is an **authoring front-end that lowers to today's `Node`/`Expr`
IR**; `evaluate(doc, t)` stays the single pure entry point and is not touched.
This is the EBN "IR + dumb-printer" discipline applied once more (graph = source,
`Node`/`Expr` = IR, `evaluate` = the tree-walk), and it is also what Blender and
Nuke actually do — an outliner/tree *and* node editors, not one replacing the
other. Rejected: making the graph the primary document model. It reads more
unified on a slide, but it rewrites the doc model, serialization, and every panel
and test built on the tree — to buy nothing `evaluate` can't already do, since
the memo + `visiting` cycle guard in `EvalCtx` already evaluate a shared-output
DAG.

**Multi-level = three scopes of the same node system.** "Make the nodes
multi-level / distinguish layers and compositions" resolves to *where* a graph is
scoped, not three separate editors:
- **Object scope** — a layer's own driver/geometry network. Today's property
  graph, generalized: a `Rect` node's `size`/`radius` become **output sockets**,
  so math can read them, instead of a promoted-property tree hidden inside one
  value.
- **Composition scope** — layers/comps/effects/mattes as nodes; wiring expresses
  the compositing, matte/track-matte, effector, and parenting relationships that
  aren't a single value.
- **Document scope** — shared modules (already first-class: `Module` +
  `Expr::Use`).

A **composition node is a group you can enter** — Blender's node groups / Nuke's
Groups — which is the same act as opening a precomp today. "Layer vs composition"
is then just *leaf node vs enterable group node*, and nesting is the `precomp`
link that already exists.

**The keystone that gates everything else: a node-type descriptor + socket
registry.** This is the answer to "how does adding a new object/layer/effect/
plugin already integrate itself as a node." Today `Shape`, `ExprKind`, and
`Generator` are **closed enums matched exhaustively**, with **per-kind editors
hand-written in `graph.rs`** (`expr_box`, `ref_editor`, `wave_editor`, …). That
is the *opposite* of auto-integration: a new type means editing the enum, the
`apply_graph_op` arms, and the UI. The fix is a metadata layer — headless,
`core`, unit-tested, in the grain of the crate's discipline:
- **`SocketType`** — the port type system. The four `ExprValue` kinds (Number /
  Vec2 / Color / Text) plus the scene-graph kinds (Geometry/Path, Layer/Render,
  Matte/Alpha, Time). **Each type carries a colour** — that *is* Blender's
  colour-coded dots, defined in one place instead of scattered through the UI.
- **`NodeDescriptor`** — per node kind: id, category, label, input sockets
  (name, type, default), output sockets (name, type), and its eval behaviour.
- **A registry** `kind-id → descriptor`. Built-ins register at startup; WASM /
  native plugins register *identically* later. This is exactly the already-agreed
  plugin seam ("effects, generators, importers, exporters as trait objects behind
  registries; dogfood our built-ins through the same seams; a third-party plugin
  is just another registered impl") — the descriptor is that seam, made concrete.

The UI then draws **any** node from its descriptor (rounded box, input dots left /
output dots right coloured by `SocketType`, bezier wires), so a new node type
appears in the graph for free. The closed IR enums **stay** as the evaluation
substrate — the descriptor is *metadata that lowers to them*, never a second
evaluator. A new native primitive still adds its enum arm; what the registry buys
is that the *graph and its UI* need no change, and that a plugin can contribute a
kind the core enum doesn't have (lowering through a generic plugin/`Script` arm).

**What lowers to what** (the "alongside" contract, made concrete):

| Graph node | Lowers to |
| --- | --- |
| Rectangle / shape (with output sockets) | `Node { shape: Shape::Rect… }`; sockets read its `Value`s |
| Math (input + result) | an `Expr` subtree (`Add`/`Mul`/`Neg`/`Gen`/`Script`) |
| Reference / driver wire | `Expr::Ref` / `Expr::Param` — the memo collapses shared reads |
| Module / reuse | `Expr::Use { module, overrides }` |
| Group / parent | tree nesting (`Node.children` + the transform compose) |
| Composition instance | `Node.precomp: CompId` |
| Effect / matte / mask / adjustment layer | **the compositor stage — not built** |

**The honest gate: half the wish-list needs the compositor stage.** Effects (and
their properties-panel UI), mattes, masks, adjustment layers, and pixel-level
effectors are all facets of the *one* unbuilt subsystem this README already
specifies (offscreen layer target → ordered effect passes → composite with blend
+ opacity + mask). They can exist as **descriptors/stubs** in the graph
immediately — a node with the right sockets and colour — but they cannot *do*
anything until that stage lands. Effect params are `Value<T>`, so once the stage
exists they animate through the same graph for free. So the node UI and the
value/geometry/math/parenting/module nodes are buildable now; the *effect* half is
buildable only behind the compositor. Don't promise the effect nodes before that
gate.

**UI.** Blender-style, descriptor-driven. Reuse the geometry already in
`graph.rs` — `box_height`, the rect/stroke draw, drag-to-move, the wire
`line_segment` (upgraded to a bezier) — but drive it from a stored
`Graph { nodes, edges }` and node descriptors, not a per-property `layout_expr`
tree re-derived each frame. Same `*Edits`-struct discipline every panel follows
(the egui closure never borrows `App`; report ops, apply after).

**Build order.** (1) ✅ `SocketType` + `NodeDescriptor` + registry, headless and
tested (`core/src/socket.rs`, `core/src/registry.rs`). (2) ✅ The generic graph
model + descriptor-driven panel — `NodeGraph { nodes, edges }` in
`core/src/graph.rs` (registry-validated: output→input, socket types match, DAG
kept by a forward-reachability check, one wire per input, serde), and the
Blender-style `Editor::NodeGraph` panel in `live/src/nodegraph.rs` (rounded
boxes, category-tinted headers, type-coloured socket dots, bezier wires,
drag-to-wire with the model rejecting an illegal drop, palette grouped by
category). It's authoring state on `App` for now — not yet lowered or saved.
(3) ◐ Lowering to the `Node`/`Expr` IR, and folding today's property graph in as
the object-scope case. *Started:* `core/src/lower.rs` — `lower_output(graph, reg,
endpoint)` compiles the value / math / generator subset to an `Expr` (a pure
tree-walk, cycle-guarded, defaults from the descriptor so a fresh `osc` already
oscillates); nodes carry sparse `values` (literal overrides / a `value` node's
constant) and sockets a `default`. **Driving a property now works**: a *driver*
(`live::Binding`) binds a graph output to a scene layer's property; `App::
recompile_graph` lowers each and hands the property a `Value::Expr` via the
existing `prop_of_mut(...).set_expr(...)`, so `evaluate` runs it and the picture
moves — dropping a driver bakes the property to a constant at its current value.
The Nodes panel gained a **Drivers** section (bind output→layer→property) and a
selected-node **inspector** (drag editors for a `value` constant and unwired
numeric inputs), with node selection on the canvas. **The graph now persists**:
`NodeGraph` and its drivers (`Vec<Binding>`, keyed by core `PropPath`) are
project-level fields on `motion_core::Project`, so they ride in the `.pbc`
serialization already used for the document, with `#[serde(default)]` so older
files load with an empty graph; load re-syncs the driven properties. **`ref` /
`param` / time-source lowering now works**: a `GraphNode` carries a sparse
`config` (a `ref`'s `(NodeId, PropPath, offset)` target, a `param`'s knob name);
`ref` lowers to `Expr::Ref`, `param` to a node-relative `Expr::Param { node:
None, .. }` (so it reads whichever layer a driver points at), and
`localTime`/`inPoint`/`outPoint`/`t01` to `Expr::Time`. The inspector gained a
ref target picker (layer / property / frame offset) and a param name field.
**The property-graph fold is underway**: `raise` (`core/src/raise.rs`) is the
inverse of `lower` — it lays an existing property `Expr` out as nodes + wires and
returns the output endpoint, so `lower(raise(e)) == e` on the lowerable subset
(tested). The Nodes panel's **Import** row pulls an expression-driven property
onto the canvas (`App::import_property` raises its `Expr` and binds the result
back), so a recipe built in the old per-property editor becomes editable on the
unified canvas — two views of one substrate. (A small type-system consequence:
`SocketType::feeds` lets a `Time` output feed a `Number` input, since a layer
clock reads as a number.) **`script` and `use` node kinds now exist**, so the
canvas covers what the old per-property editor does: `script` is a Rhai leaf
(`config.script` → `Expr::Script`, edited in the inspector), `use` links a shared
module (`config.module` → `Expr::Use`, picked from the project's modules) — both
raise/lower round-trip.
**Per-node descriptor resolution ✅ — the registry's one real limitation, lifted.**
`kind → descriptor` is enough only while every node of a kind has the same
sockets, and a `use` node breaks that: its inputs are the *linked module's*
knobs, so two `use` nodes in one graph have different shapes. Resolution is now
per **placed node** through `GraphCtx { reg, modules }` (`core/src/graph.rs`),
whose `descriptor_for(node) -> Option<Cow<NodeDescriptor>>` returns the kind's
static descriptor borrowed (free) for everything else and an owned, specialized
one for a linked `use`. Everything that reads sockets — `connect`, `validate`,
`lower_*`, `raise`, and the panel's drawing — goes through it rather than the
registry, so the *canvas* needed no special case: a module's knobs appear as
ordinary typed, colour-coded input sockets and an **override is a wire from any
node**, not a literal in a side panel. Consequences: a knob is type-checked like
any socket; **unwired and unset means *inherit*** (the resting state has no
default, because a module default is resolved lazily in the caller's scope and a
literal copy of it would stop retiming), so lowering emits an override only for
a knob that is wired or has a stored literal; and `raise` now reproduces a
link's overrides, closing the last lossy case — **`lower(raise(e)) == e` now
holds across the whole `Expr` enum**. The inspector shows each knob in one of
three states (wired / overridden / inheriting) with `override` and `×` to move
between the last two — the old link box's two-state toggle plus the third state
only a canvas can offer. An `osc` node also carries its **waveform** in config
now (it selects which function the generator *is*, so nothing wires into it),
which fixes the same class of bug: a square oscillator imported onto the canvas
used to come back a sine.
**Shape/geometry lowering now works, so the graph can *author* geometry and not
only drive values.** Two halves. (a) A shape node's **scalar outputs echo its
resolved params** — `rect.size`/`rect.radius`/`ellipse.size`/`text.size` hand
back whatever feeds the input of the same id, so math chains off a rectangle's
size (the object-scope case above). The echo is read structurally from the
socket-id pairing, pinned by a registry test, so a new shape primitive needs no
arm in `lower`. (b) `lower_geometry(graph, reg, endpoint) -> Option<Shape>`
compiles a `geometry` output to a whole `Shape` whose every param is a
`Value::Expr` of the lowered input — so a wired `size` animates through
`evaluate` exactly like a hand-written expression, and an unwired one rests on
the descriptor's default (which now matches the layers panel's add-shape seeds,
so a canvas rect *is* the toolbar's rect). A **geometry driver**
(`motion_core::ShapeBinding`, a project field beside `bindings`, serde-defaulted)
binds such an output to a layer and replaces its `Shape` on every recompile;
removing one bakes the params to constants, like a value driver. Drivers run
**shapes first, properties second**, so a geometry driver decides the shape
*kind* and a value driver on `size`/`radius` can still override that one param.
Text's non-animatable half (family / align / wrap) rides in `NodeConfig::text`
(a `TextConfig`); `content` **left** that struct when `ExprValue` grew `Str` and
is now an ordinary wirable `Text` input socket. The Nodes panel gained a
**Geometry** driver section and vector + text inspector fields; the driver
pickers are now type-filtered, so a geometry output is never offered to a
property driver. `App::recompile_graph`'s body moved to the free fn
`compile_drivers(project, reg, comp)` so it's testable without a window.
**Retiring `Editor::Graph` is gated on three capabilities that still live only
there** — audited before touching it, because deleting the panel would delete
working features:
  1. ~~a link's module **overrides**~~ ✅ (the descriptor seam above);
  2. ~~an oscillator's **waveform**~~ ✅;
  3. ~~**exposed params**~~ ✅ — one owner-agnostic `NgKnobOp` and one `knobs_ui`
     serve both scopes, because a knob *is* the same idea at both: a named value
     `param("x")` reads from whatever owns the expression. Project scope gained
     a **Layer knobs** section (pick a layer, add num/vec/col knobs, remove
     them); module scope's knob editor is now the same widget, so module knobs
     gained the vector and colour types they lacked. The `param` node's editor
     is a picker over every knob exposed anywhere in the scene (labelled with
     which layers expose it) **plus** a free-text field, because a `param` node
     lowers to a node-*relative* `Expr::Param { node: None, .. }` — there is no
     single owner whose knobs are *the* candidate list, and naming a knob that
     doesn't exist yet is legitimate. A name nothing exposes is flagged in
     amber, not refused. This is what lets one graph output fit many layers: one
     `osc × param("gain")` drives five layers at five gains, no five graphs.
     **This pass also closed a hole older than the node graph:** nothing in the
     app had ever *set* a knob's value — `ParamValue` was constructed by
     `ParamKind::seed()` and never edited again — so every `param("x")` read
     resolved to its seed of 0 and any knob-driven recipe silently did nothing.
     `ParamValue::as_const`/`set_const` (core, typed: a literal of the wrong
     shape is ignored rather than retyping the knob) plus a per-row editor in
     `knobs_ui` fix it. A keyframed or expression-driven knob shows "animated"
     and no field, since one number can't stand for a track and writing it back
     would flatten it.
  4. ~~**module authoring + body scope**~~ ✅ — resolved in favour of giving
     `Module` its own `NodeGraph` + `output: Option<Endpoint>`, the same
     "alongside, lowers to the IR" shape `Project.graph` has with the properties
     it drives. `compile_modules(modules, reg)` lowers each module whose
     `output` is set into its `body` (which stays the IR `eval_use` runs); a
     module with **no** `output` is left alone, so an empty canvas can't blank a
     body authored elsewhere. The Nodes panel gained an `NgScope`
     (Project | Module) — same panel, same ops, different graph, which is the
     three-scopes design rather than three editors — with a Modules list
     (new/edit), rename/delete, an output picker, and knob add/remove (a knob
     added here becomes an input socket on every `use` node linking the module).
     **Opening a module seeds its canvas by raising its `body`**, so a module
     built in the old editor becomes node-editable with no migration and its
     layout persists from then on. Recompile order is modules-then-drivers, since
     a driver may link one. Guard against the two editors fighting over one body:
     a body edit made in the *old* panel clears the module's `output`, handing
     ownership back, because the editor you just typed into should win.
**`Editor::Graph` is retired** (all four blockers closed, plus the script live
result ported: the selected `script` node now shows the value it evaluates to,
or its error in red, computed before the UI pass against a layer the script
actually drives — found by walking forward from the node to the first driver
that reads it, so a `param`/`value` read previews the number it will really
resolve to). `live/src/graph.rs` is gone (-1335 lines); its surviving shared
vocabulary (`PROP_PATHS`, `prop_path_label`, `ParamOwner`, `ParamKind`,
`SCRIPT_HELP`) moved into `nodegraph.rs`, the only remaining user. **One
capability was consciously dropped:** `ExtractModule`, which lifted a property's
expression into a new module in one click — with module scope and Import both on
the canvas, it was a shortcut, not a capability.

One thing worth knowing about the retirement. The `Editor::Graph` enum variant
was briefly kept for loadability — the dock layout is *document data*, and an
unknown serde variant is a hard error that fails an entire `.pbc` rather than
just its layout — with a `migrate_retired()` pass rewriting such leaves. **That
shim is gone**: no `.pbc` predating the retirement exists, so it was
back-compat for a population of zero. If a save file ever does need to outlive a
panel, that is the shape the migration takes. **29 tests went with the panel** — the ones covering its box
geometry (`layout_expr`, `box_height`) were genuinely dead, but the ones covering
*semantics* that merely moved (module rename/delete, body edits reaching every
link, overrides surviving a re-point) are re-pinned against the node path that
owns them now.

**Drivers are nodes now, and derived rather than stored.** The panel had grown
list sections above the canvas — Drivers and Geometry — whose rows were combo
boxes binding an output to a layer's property. That made *the one edit that gives
a graph any effect* the one edit you couldn't make on the canvas, and it kept two
representations of a single fact: `Project::bindings` beside the graph could
disagree with it (a wire deleted under a binding, a binding naming a node that's
gone). Both sections are gone. Two **sink** descriptors replace them —
`out` (Property Out) and `shapeOut` (Shape Out), the only built-ins with no
outputs, because a driver *ends* the dataflow — added from the palette like any
node and targeted in the inspector like any node. `out`'s input socket is
**typed by the property it targets** (`PropPath::socket_type` +
`GraphCtx::descriptor_for`, the same per-placed-node specialization a `use`
node's knobs already used), so the canvas refuses a number wire into a fill at
authoring time; changing the target across a kind boundary drops the wire that no
longer fits. A sink's header reads what it drives (`Rotation → Star`), which is
the point of it being a node at all: the binding is legible without selecting
anything. `NodeGraph::bindings()` / `shape_bindings()` derive the drivers from
the `out` nodes' config plus the wires feeding them, so the disagreeing-state
simply can't be represented; `Project`'s two `Vec`s are now private, load-only,
and drained into sink nodes by `Project::migrate` (dropping the fields outright
would make serde skip the keys silently — data loss wearing a compatible face).
Unbinding got *harder* and better for it: a driver can now end half a dozen ways
(delete the sink, pull its wire, retarget it, delete its source), so rather than
teach each op about baking, `bake_unbound(project, comp, frame, before, before_shapes)`
diffs the drivers either side of an edit and freezes every target that lost one —
skipping any property another driver still writes, since baking there would
freeze a value the next recompile immediately overwrites. Free-standing, like
`compile_drivers`, because it is the one piece of this that silently rewrites the
document.

**The panel's last three lists went the same way.** With the drivers on the
canvas, what remained above it — Modules, Import, Layer knobs — were the sections
that made the Nodes panel a panel *with* a graph in it rather than a graph.
Each had a different right answer:
  - **Import** is an action, not a list, and it always named a layer and a
    property that a sink node already names. It moved onto the sinks: an `out`
    node that's targeted but unwired offers "Import its expression", a `shapeOut`
    offers "Import its shape". One object now means "this graph and that property
    are the same thing", in both directions, and the duplicate layer/property
    combos are gone. `import_property` joined `import_shape` as a free function
    taking the sink, so both are testable without a window and both report a
    refusal the same way.
  - **Modules** was a list whose two jobs were *create* and *navigate*. Creating
    moved to the palette (`Module ▸ New module…`) — the one entry that isn't a
    registered kind, because it makes a document object as well as a node — and
    it now places a `use` node linked to the new module, so a module is never
    unreachable from the canvas. Navigating moved onto the `use` node: the link
    is a better front door than a list, because a link is a thing you can see.
  - **Layer knobs** was the odd one out: a layer's knobs are *that layer's own
    data*, and editing them through a panel-wide "pick a layer" combo meant
    choosing the selection twice. They moved to the **properties panel**, where
    the rest of a layer's data lives. `knobs_ui` now takes the op channel rather
    than the whole `NgEdits`, so the module-scope editor and the properties-panel
    editor are the same widget writing the same owner-agnostic `NgKnobOp` through
    the same applier — one knob concept, one code path, two callers.

In project scope the panel is now a header, a status line, the selected node's
inspector, and the canvas. Nothing is bound, made, imported, or exposed anywhere
but on a node.

**And the values moved onto the nodes.** A node's own numbers were still being
edited in a "Values —" block above the canvas that described whichever node
happened to be selected — so a `value` node showed a bare box with its number
somewhere else, and reading a graph meant clicking each node in turn. Every
socket literal now draws its editor **in its own row on the box** (`socket_field`,
a scoped `Ui` over the row so a vector's two drags and a colour button fit the
same slot), which is what makes a node self-contained. Which rows get a field is
**structural** — `row_literal` asks whether there is a literal there, not what
kind the node is — and three behaviours fall out of that rather than being
special-cased: a wired input has no field (the wire is the value), a `use` node's
override socket has none while inheriting (unset means *inherit*, and a field
seeded to zero would state the opposite), and geometry/layer/matte sockets have
none because they have no `ExprValue` at all. The one thing that *is* per-kind is
`const_socket`: which nodes keep their constant on an output socket, since that's
their only socket — one function, so the canvas and the inspector can't disagree.
`NODE_W` grew to 208 to hold a label column plus a field.

The inspector kept exactly what isn't a socket — a `ref`'s target, a `param`'s
name, a script's source, a `use`'s module, an `osc`'s waveform, a text node's
typography, a sink's property, Create-layer and Import — which is a far clearer
remit than "some of the values", and it now says so when a node has nothing left
for it. `string` is the one deliberate exception: its inline field is one line
and its inspector one is multi-line, because a caption with an embedded newline
would otherwise be invisible. Same value, two views.

The panel had no tests at all before this (it's UI, and the repo's rule is that
document work is a free function). It has some now, because this change put real
widgets on the canvas: the four `row_literal` rules are pinned directly, and a
headless `Context::run_ui` lays out one node of every registered kind, which is
the only way an id clash or a font that isn't bound shows up before runtime.

**Then the node roster, in four passes. Pass 1: one Math node, and an IR that can
carry it.** `add`, `mul` and `neg` were three kinds, and every operator worth
having next — subtract, divide, power, min, sqrt, trig — would have been another,
each with its own palette entry and its own lowering arm, all differing only in
which `f64 → f64 → f64` they apply. They are now one **`math`** node whose
operator is config, like an oscillator's waveform.

That merge is safe in a way a `value`/`string` merge would not be, and the
distinction is worth keeping straight: the rule those two obey is that a node
must not change its **output type** under you, because the output type is what
colours a wire. Every Math mode is Number → Number. What *does* change is the
**arity** — `Sqrt` takes one operand — and `GraphCtx::descriptor_for` drops the
`b` socket for a unary op, the same per-placed-node specialization that grows a
`use` node's knobs and types an `out` node's input. Dropping the socket rather
than hiding it is what makes a wire into it impossible instead of merely
invisible; `App` pulls any existing wire when the op goes unary, as it does when
a retargeted sink's type stops fitting.

The IR grew to match, and shrank in the process: `Expr::Add`/`Mul`/`Neg` became
`Expr::Bin { op: BinOp, .. }` and `Expr::Un { op: UnOp, .. }`. Nine binary and
seven unary operators now cost *one* arm each in `arity`/`child`/`Display`/eval
instead of sixteen. `ExprValue::zip`/`map` already broadcast component-wise, so
every operator works on vectors and colours for free — `position / 2` means what
it looks like. Two deliberate calls: **trig is in degrees**, because every angle
a user touches in this app is (`rotation_deg`, the properties panel, the gizmo)
and a graph needing `× 57.2958` to aim one layer at another would be telling on
itself; and **no operator may produce a NaN or an infinity** — divide by zero,
modulo by zero, a fractional power of a negative, the root of a negative all
resolve to 0, because a NaN reaching a transform blanks the layer with no clue
why, which is the least debuggable failure this engine has. String concatenation
stayed pinned to `Add` alone: subtracting from text has no meaning worth
guessing, and inventing one would make the IR's strictness a lie.

`Project::migrate` folds the retired kinds (`add`/`mul`/`neg` → `math` + op). Not
strictly needed — no `.pbc` in existence carried them, the same population of
zero that retired `Editor::Graph`'s shim — but a node kind is a plain string, an
unrecognised one just draws red and lowers to nothing, and a graph going silently
inert is a bad way to discover that.

Passes 2–4, not yet built: **Vector** (`Separate XY` / `Combine XY`, needing the
IR's second addition — component read and construct); **Remap** (Map Range,
Clamp, Mix — pure *macro* nodes lowering to composed `Bin` trees, no IR change,
which is the EBN split holding: a node that looks like an operator needn't be an
arm of the IR); and **everything onto the node** (variable node height, per-kind
config drawn on the box, the inspector retired for good).
(4) The compositor stage — the real gate for effects/mattes/masks.
(5) Effect nodes + their properties-panel show, then plugins registering
descriptors like built-ins. Steps 1–3 need no new engine; step 4 is the large
separate track.

**Invariants to protect.** `evaluate` stays the one pure entry point; the closed
IR enums stay the evaluation substrate (descriptors are metadata, not a rival
evaluator); the tree stays the structural spine; a new node type is a registered
descriptor, not a UI edit.
