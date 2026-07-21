# Prior art & design boundaries

Where PBC sits in the free/open-source motion-design landscape, and — more
importantly — where it must **deliberately differ** so it doesn't become an
existing tool with a Rust engine swapped in. This is a design-boundary doc:
read it before adding a feature that a rival already owns, to decide whether
you're extending PBC's thesis or drifting into someone else's product.

## PBC's thesis in one paragraph

Animation is a **lazy `Value<T>` recipe**, never a baked result: a constant, a
keyframe track, or an `Expr`. `evaluate(&doc, frame)` is a **pure function** that
resolves the whole scene graph at a fractional frame into a flat `Scene`.
Expressions, the node graph, and project-wide **Modules** (`Expr::Use`) are three
front-ends that lower to **one small typed IR** (`Lit`, `Ref{node,prop,time_offset}`,
`Add`/`Mul`/`Neg`, `Script`, `Use`). The engine is **headless and unit-testable**
(`core` knows nothing about GPUs or windows), **frames are the native time
domain** (seconds are a presentation unit), and every drawn item carries
**provenance** (`source: NodeId`). Non-destructive editing, non-linear scrubbing,
and determinism all fall out of that single design choice.

That is an **author-time computed-graph** tool with a **scrub clock**. Almost no
FOSS project occupies exactly that point — and each neighbour misses it in an
instructive way.

## The two axes that separate these tools

- **How animation is stored** — *baked timelines* (keyframes are ground truth)
  vs. a *recipe/graph* (values are computed each frame).
- **What drives it at play time** — a *scrubbed clock*, an *interactive state
  machine*, or *host code*.

PBC = **recipe/graph + scrub clock**. Keep that corner honest.

## Landscape at a glance

| Tool | FOSS? | Core engine model | Driven by | Renderer | Nearest to PBC on |
| --- | --- | --- | --- | --- | --- |
| **Rive** | Runtime OSS, editor freemium | Baked timelines + **state machine** + data binding | Interactive inputs | Custom GPU vector | Reactivity (but input-driven, not IR) |
| **Cavalry** | Proprietary (free tier) | Node graph, attribute stack, **Behaviours / Duplicators / Falloffs** | Scrub + procedural | GPU | Procedural reuse — our closest **conceptual** rival |
| **Theatre.js** | MIT | Project→Sheet→Object→**Sequence**, animates host objects | Scrub / host code | None (renderer-agnostic) | "any property is animatable" model |
| **enve** | GPL, ~dormant | Monolithic Qt app, keyframes + expressions | Scrub | Skia | Closest FOSS **app**; cautionary tale |
| **Lottie / ThorVG** | OSS | **Baked** keyframe JSON + player | Scrub / host | rlottie / ThorVG / web | An **export target**, not a rival |
| **Motion Canvas** | MIT | Animation **as TypeScript** + timeline preview | Code | Own canvas | Script-as-substrate (but we keep it a graph leaf) |
| **Remotion** | Source-available | React components → video frames | Code | DOM/canvas | Programmatic video (licence caveat) |
| **Blender (Grease Pencil)** | GPL | 3D scene + GP strokes + F-curves | Scrub | Own | Onion-skin/graph-editor conventions only |
| **OpenToonz / Synfig / Krita / Pencil2D** | GPL | Frame/drawing-centric 2D (ink & paint, tweening) | Scrub | Own | Little overlap — different engine |

## Per-rival engine breakdown

### Rive — the interactive-runtime tool
- **Model:** artboards + baked timeline animations orchestrated by a **state
  machine** (inputs: bool/number/trigger → transitions between states); recent
  **data binding** wires view-model properties to design properties. Custom GPU
  vector renderer, tiny `.riv` binary, runtimes on every platform.
- **Nails:** the runtime. Deliverable is an interactive micro-app you push values
  into.
- **PBC diverges:** Rive's timelines are **baked**; the state machine blends
  pre-made clips. It has no expression algebra — no "this property is a pure
  function of that property at frame t−3." PBC's `Expr::Ref{time_offset}` is
  exactly that.
- **Trap:** don't chase the state machine. That's an interactivity feature for a
  runtime product and drags in an SDK on N platforms. PBC's reactivity story is
  the **expression graph**, not input-driven states.

### Cavalry — the procedural tool (our real conceptual rival)
- **Model:** node-based "anything connects to anything"; non-destructive attribute
  stack. Power tools: **Duplicators/Cloners** driven by **Falloffs + Effectors**
  (influence fields), and **Behaviours** (keyframeless animation drivers attached
  to an attribute). Data-driven (CSV → attributes). See the deep-dive below.
- **Nails:** procedural generation at scale, and reusable animation logic attached
  without keyframing.
- **PBC overlaps & diverges:** PBC **Modules** (`Expr::Use` + per-link overrides +
  free retiming) are conceptually Cavalry's Behaviours. But Cavalry is proprietary,
  layer/attribute-centric, and IR-less; PBC is timeline+recipe-centric with a
  **single typed IR** underneath, plus frames-as-native-time and a pure headless
  core Cavalry has no equivalent of.
- **Flag to plant:** be the tool that has the **compiled IR + provenance +
  determinism** Cavalry's surface-level node system lacks. The **one** Cavalry
  idea worth adopting deliberately is **instancing** (Duplicator/Cloner +
  Falloff/Effector) — the obvious next thing the `Expr` substrate can express, and
  the biggest gap in FOSS.

### Theatre.js — sequence the host's objects
- **Model:** Project → Sheet → Sheet Object → **Sequence**. Doesn't own the render;
  animates *your* JS/Three.js object properties on a timeline via a Studio overlay
  that writes JSON. Reactive "Dataverse" pointer/derivation system underneath.
- **Nails:** being a *library*, not an app — rides on whatever renderer you have.
  Its "any object property is animatable, sequenced non-destructively" rhymes with
  `Value<T>`.
- **PBC diverges:** PBC **owns the document and renderer** (vello) and has a vector
  scene model; Theatre is renderer-agnostic with no vector concept, and its
  sequences are keyframe tracks — no IR, no cross-property refs-at-a-time-offset.
- **Trap:** "animate anyone's objects" is a different product. If ever wanted, it's
  a **second front-end on `core`**, not a redirection of it.

### enve — the closest FOSS app, and the cautionary tale
- **Model:** Qt/C++ GPL desktop app; vector+raster+video, keyframed, had an
  expressions feature; Skia renderer. Effectively one-person, now largely dormant.
- **Nails:** proved the niche is real — then stalled.
- **PBC diverges:** **architecture.** enve is a monolithic Qt app with renderer and
  app fused; PBC's whole bet is the **headless, unit-testable `core`** with GPU/UI
  confined to `live/`. That layering is the durability story enve lacked.
- **Lesson (not a trap):** the differentiation vs. enve isn't a feature, it's the
  engine discipline. Protect `core` purity ruthlessly.

### Lottie / ThorVG — the format + delivery layer
- **Model:** Lottie is **baked keyframe JSON** (exported from AE via Bodymovin);
  runtimes (lottie-web, rlottie, **ThorVG**) just play it. ThorVG is a lean C++
  vector *renderer*, not an authoring model.
- **PBC diverges:** Lottie is the opposite of PBC's thesis — fully baked,
  non-parametric; expressions don't survive export. It's an **output**, not a rival.
- **Move:** treat Lottie as an **export target** ("author parametrically, bake to
  Lottie for delivery"); keep ThorVG in mind as a reference for a lean non-wgpu
  renderer path.

### Motion Canvas / Remotion — animation-as-code
- **Model:** you *write* the animation. Motion Canvas = TypeScript generators with
  a timeline; Remotion = React → video frames (source-available; licensed for
  teams — non-FOSS caveat).
- **PBC diverges:** code-first with a visual preview vs. PBC's visual-first with a
  code/expression substrate. Rhai script nodes touch the same idea but as a *leaf
  in a graph*, not the authoring paradigm.
- **Trap:** don't let the expression editor drift toward "write the whole animation
  in Rhai." The moment the graph is just a text editor, you're Motion Canvas
  without the ecosystem.

### Blender GP & traditional 2D (OpenToonz / Synfig / Krita / Pencil2D)
- Frame/drawing-centric — hand-drawn, ink-and-paint, tweening. PBC is
  parametric-vector + motion-graphics centric. Overlap is small; only worth
  borrowing UI **conventions** (onion-skin warm-past/cool-future tint, graph-editor
  ergonomics — already adopted). Don't wander into drawing tools; different engine.

## Where to plant the flag

**Protect — the moat (no FOSS tool has the combination):**
1. **One typed `Expr` IR under everything** — expressions + node graph + Modules
   all lower to it; IR is data, evaluation is a dumb tree-walk. Cavalry has nodes
   but no compiled IR; Rive has data-binding but no expression algebra; Theatre has
   neither.
2. **`evaluate(doc, frame)` as a pure, headless, testable function** with per-item
   provenance and determinism by construction. The anti-enve.
3. **Frames as the native time domain**, seconds as presentation — retime without
   keyframe drift; room for sub-frame motion blur.
4. **Modules: project-wide, override-layered, auto-retimed** reusable graphs — the
   Behaviours/components answer, better-founded because it's the same `Value`
   layering all the way down.

**Adopt deliberately (the few good ideas from rivals):**
- Cavalry's **Duplicator/Cloner + Falloff/Effector** instancing — the natural next
  thing the IR can express, and the biggest FOSS gap.
- **Lottie export** as a delivery target; **ThorVG** as a lean-renderer reference.

**Do NOT build (these turn PBC into someone else):**
- A **runtime state machine / input system** (→ Rive). Reactivity is the graph.
- A **renderer-agnostic "animate anyone's objects" library** (→ Theatre.js) as the
  *main* product — at most a later second front-end.
- **Code-as-the-authoring-paradigm** (→ Motion Canvas/Remotion). Script stays a
  graph leaf.
- **Hand-drawn / ink-and-paint tooling** (→ OpenToonz/Synfig/Krita). Different
  engine.

**One-line positioning that keeps us honest:**
> Cavalry's procedural reuse + After Effects' non-linear timeline, expressed
> through a single typed IR, in a headless testable Rust engine.

## Appendix — how Cavalry's node system works

Cavalry is **node-based behind the scenes**: the friendly layer list and the raw
**Dependency Graph** are two views of the same thing. Worth understanding because
it's the closest existing model to PBC's graph — and the differences show where
PBC's IR earns its keep.

- **A connection is a data pipe between two attributes.** You drag from a
  *Connection Anchor* (anything that turns blue on rollover) on one layer and drop
  it on another; a popup lets you pick the target attribute (row). From then on,
  the source attribute's value drives the target's — "data from one layer's
  attribute is passed directly to another's."
- **Everything is an attribute with ports.** In the Dependency Graph a layer is a
  block listing its attributes; each attribute has an **input port (left)** and an
  **output port (right)**. Ports/nodes are **colour-coded by data type** (int,
  double, string, …).
- **Connections are type-checked.** Compatible types only — a string can't drive a
  number; incompatible attributes are dimmed in the editor or hidden in the picker.
  (PBC's analogue: `ExprValue{Num,Vec2,Color}` with `FromExpr`/`ToExpr` pinning the
  type at the property, and a kind mismatch resolving to `T::fallback()`.)
- **One source, many targets.** The same behaviour can drive position, radius,
  colour, and deform a shape — and power several attributes on several objects at
  once by dragging more connections. (PBC's analogue: a controller node `Ref`'d by
  many properties, now first-classed as a **Module**.)
- **Node/layer families:**
  - **Behaviours** — keyframeless animation drivers you attach to any attribute
    (bounce/float/oscillate/react-to-data); can also deform shapes. ≈ PBC Modules.
  - **Utilities** — fundamental building blocks (math, remap, time, noise) you wire
    into custom "recipes." ≈ PBC's `Add`/`Mul`/`Neg`/`Script`/`Ref` IR nodes.
  - **Duplicators / Cloners** — instance an object many times, controlled by
    **Falloffs** (fields of influence) and **Effectors** (what the field changes) —
    procedural, not per-instance keyframes. **PBC has no equivalent yet.**
  - **Deformers** — a stack of connected deformers on a shape.
  - **Effects** — shaders and image-based filters.
  - **JavaScript layers / scripting** — custom nodes in JS. ≈ PBC's Rhai `Script`
    node (but Cavalry scripts are whole layers; PBC scripts are IR leaves).
- **Evaluation:** it's a dependency graph — outputs pull from inputs, non-destructive,
  changes propagate downstream automatically. Conceptually the same pull model as
  PBC's DFS resolve, but Cavalry exposes it as **connected nodes**, where PBC
  compiles the same relationships into a **small serialisable IR** that a dumb
  tree-walk evaluates (and can cycle-detect + memoise per `(node,prop,frame)`).

**The takeaway for PBC:** Cavalry validates the procedural-reuse thesis and the
"anything drives anything, type-checked" connection model — but it stops at
*nodes as the substrate*. PBC's differentiator is that the graph is a **front-end
over a compiled IR** with provenance, determinism, frames-native time, and a
headless testable core. Adopt Cavalry's **instancing** (Duplicator/Falloff/
Effector); do not adopt its lack of an underlying IR.

## Sources

- Rive: <https://help.rive.app/runtimes/state-machines> ·
  <https://dev.to/uianimation/engineering-interactive-mascots-with-rives-state-machine-and-runtime-architecture-4e2h>
- Theatre.js: <https://deepwiki.com/theatre-js/theatre/2-core-systems> ·
  <https://github.com/theatre-js/theatre>
- Cavalry: <https://docs.cavalry.scenegroup.co/user-interface/menus/window-menu/dependency-graph/> ·
  <https://cavalry.studio/docs/getting-started/key-concepts/connections/> ·
  <https://schoolofmotion.com/blog/getting-started-with-cavalry-5-things-every-beginner-should-know>
- enve: <https://maurycyliebner.github.io/> · <https://github.com/MaurycyLiebner/enve>
- Lottie/ThorVG: <https://github.com/thorvg/thorvg>
- Motion Canvas: <https://motioncanvas.io/> · Remotion: <https://www.remotion.dev/>
