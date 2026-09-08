# Road to production: state of PBC and the plan to a shippable v1

Written 2026-09-08. This is a **status audit plus a build order**, aimed at one
goal: *a person can sit down in PBC, cut a piece, and hand over a finished video
file.* The README is the design record — it says what was decided and why. This
doc says what is missing between here and a deliverable, and in what order to
close it.

## 1. Where the project actually is

Measured, not remembered: ~38k lines of Rust across four crates, `cargo test
--workspace` green (628 tests: 317 core, 272 live, 31 render, 8 CLI). The render
and live counts include tests that need a real GPU or a real `ffmpeg`; both skip
cleanly where those are absent, so a headless CI box sees fewer.

| Area | State |
| --- | --- |
| Document model (`core`) | Solid. Frames-native timebase, `Value<T>` keyframes with easing, 2.5D `Transform` (Vec3 + Mat4), camera, groups, parenting, per-layer timing, pre-comps, params, vector paths, text, footage refs. |
| Evaluation | Solid. `evaluate(doc, frame)` is the single pure entry point, with memoization, cycle guard, and warnings as a provenance channel. |
| Expressions / graph | Deep. Closed IR (`Bin`/`Un`/`Ref`/`Param`/`Gen`/`Script`/`Use`), Rhai bridge with `value()`/`wiggle()`, generators, reusable modules, a node canvas. |
| Editor (`live`) | Broad. Dockable panels, timeline + dopesheet + curves, gizmo, pen tool, snapping, guides, onion skins, motion path, layer strips, font picker, undo/redo. |
| Compositing | Model only. Blend modes, masks, track mattes exist in `Scene` and both backends. No effect stack, no GPU effect passes. |
| Footage | Import works: stills (incl. HEIC/RAW), video via an `ffmpeg` sidecar, threaded decode cache with a warm frame stream. |
| Export | Works, from the CLI *and* the GUI. Encoder trait with PNG-sequence and ffmpeg-sidecar impls, a CPU rasterizer for headless renders, an offscreen vello target so the editor exports through its own preview renderer, and the two-button render queue. Gaps: no frame range and no frame-parallelism. |
| **Audio** | **Does not exist.** No decode, no playback, no waveform, no master clock. |
| Effects | Does not exist. `NodeCategory::Effect` is a registry slot with nothing in it. |
| Motion blur | Does not exist. |
| Project robustness | No autosave, no crash recovery, no asset relink UI, no "collect files". |

**The honest summary:** PBC is a strong *animation authoring engine* with a
capable editor on top, and as of 2026-09-08 **things come out of it** — a `.pbc`
becomes an mp4, a ProRes master or a PNG sequence, from a button or from the
command line. The remaining gap to production is **sound**, the **finishing
pass** (effects), and **not losing work**. Everything on the roadmap before this
point was engine depth; delivery is no longer the blocker.

## 2. What "ready to make production videos" requires

A minimum bar, stated as things a user must be able to do:

1. **Get a file out** — H.264/ProRes/PNG-sequence at full quality, from a range
   they choose, without the editor freezing.
2. **Hear the piece** — import audio, hear it in sync while scrubbing and
   playing, see a waveform, and have it land in the exported file.
3. **Finish the look** — at least blur, glow, levels/curves, and a keyer, as an
   animatable effect stack on a layer.
4. **Not lose work** — autosave, crash recovery, relinkable footage.
5. **Trust the output** — the render matches the preview, frame for frame.

Items 1, 2 and 4 are hard blockers. Item 3 is what separates "renders" from
"usable for real work". Item 5 is a property the architecture already almost
guarantees (`evaluate` is pure and frame-native) and must be defended by tests.

## 3. Build order

Sequenced so each phase ends with something a user can *do*, and so the
expensive shared subsystem (the compositor) is built once, late, with its
clients known.

### Phase 0 — Two fixes that must land before the first exported file ✅

**Done, 2026-09-08.** Both were latent until something was rendered to disk, and
both were cheaper to fix before saved projects and export code multiplied.

- ~~**`Comp::duration` is stored in seconds**~~ ✅ The comp now stores
  `duration_frames: i64`, with `duration_seconds()` derived and
  `set_duration_seconds()` for the one edge (the comp bar) where a user types a
  time. The frame count is no longer the output of a `.round()`: at 23.976fps a
  five-second comp used to be 119.88 frames, and whether an exported file got
  119 or 120 was a rounding decision. A pre-frames `.pbc` still opens — the old
  seconds field is read into a private `legacy_duration` and folded by
  `migrate()`, which is where it has to happen because serde gives no ordering
  guarantee that `fps` is read first, and it is never written back out.
  `set_fps` now re-grids the length along with the animation, so a rate change
  keeps a comp the same number of seconds long. Five tests pin it.
- ~~**The SVG backend silently drops matte layers**~~ ✅ Implemented as a
  **luminance** mask rather than demoting the backend — exact for both matte
  modes, and without depending on `mask-type="alpha"` (which has no inverse for
  `DestOut`). The backend also grew a way to say "I could not draw this
  exactly": `scene_to_svg_reporting` returns notes for the one case that is
  still inexact — footage used as a matte, whose luminance is not its alpha —
  and the offline binary prints them. See
  [`decisions/0016-svg-mattes-are-luminance-masks.md`](decisions/0016-svg-mattes-are-luminance-masks.md).

The second fix matters more than it looks: the SVG backend is the headless way
to verify compositing semantics without a GPU, which is what Phase 1's
preview-equals-export tests will assert against.

### Phase 1 — Export (the unblocker) — *in progress, nearly closed*

The single highest-value change in the project. Nothing else here matters if a
piece cannot leave the app.

**Landed 2026-09-08 — a project can now become files:**

- ✅ `render/src/encode.rs`: the `Encoder` trait — `push(rgba)` / `finish()` —
  with two implementations. **PNG sequence** (pure Rust, always available) and
  an **ffmpeg sidecar** fed raw RGBA over stdin, mirroring `decode.rs`'s process
  discipline including its named-tool error. Broadcast rates stay exact ratios
  (`24000/1001`, not `23.976`) because the decimal writes timestamps that drift.
- ✅ `render/src/raster.rs`: a **CPU rasterizer** — `Scene` → RGBA — so a frame
  can be rendered, and asserted on, with no GPU. Draws fills, strokes, opacity,
  isolation groups, blend modes, masks and track mattes; reports footage rather
  than silently dropping it. See
  [`decisions/0017-offline-cpu-rasterizer.md`](decisions/0017-offline-cpu-rasterizer.md)
  for why parity with the preview is *structural* rather than per-pixel.
- ✅ `Project::from_pbc` moved into `core`. The `.pbc` reader lived in the
  editor, where a headless renderer could not reach it; one format with two
  readers is how a format grows two dialects.
- ✅ **`motion` is the headless renderer**, not a demo:
  `motion render project.pbc --out film.mp4`, with `--comp`, `--start/--end`,
  `--scale`, `--fps`, ffmpeg pass-through, and `--demo` for rendering the
  built-in document on a machine with no project to hand.

**Landed 2026-09-08 — the GUI half:**

- ✅ **An offscreen vello render target** (`live/src/offscreen.rs`). The editor's
  export renders through **the same vello renderer as the preview**, into a
  texture instead of a surface, and reads the pixels back. There is no second
  rasterizer in this path and therefore no parity *question* — the export is the
  preview, minus the editor's furniture. That last clause is now enforced rather
  than hoped for: the onion skins, passepartout, frame border and selection
  outline were four separate arguments to `to_vello` and are now one `Chrome`
  parameter, of which export passes `Chrome::none()`. The frame border in
  particular was drawn *unconditionally*, so before this every rendered frame
  would have carried a 1.5px grey rectangle exactly on the crop.
- ✅ **A render queue in the GUI** (`live/src/renderqueue.rs`), hosting the two
  buttons under the composition bar. A job is **stepped from the redraw loop**
  rather than threaded — the device, the vello renderer and the footage cache all
  belong to the preview, and the alternatives cost either a second adapter or a
  lock that freezes the editor anyway. See
  [`decisions/0020`](decisions/0020-the-render-job-is-stepped-not-threaded.md).
- ✅ **Named render presets saved in the `.pbc`** (`Project::render_presets`,
  `#[serde(default)]`, so no migration). Master renders from the project's
  preset, which is what makes two people on one project produce the same file.
- ✅ **Blurred layers export blurred.** The GUI export runs the preview's own
  `rasterize_effect_layers` readback, because vello has no layer-filter
  primitive and a blur can only be done by rendering the layer alone, reading it
  back, filtering and drawing the result in. Skipping it would have dropped
  every blur from a delivered file *silently* — the frame still renders, still
  sizes right, and looks broadly correct. Pinned by a pixel test.
- ✅ **`Encoder::abort`**, which the Cancel button forced into existence:
  dropping a process-backed encoder closes ffmpeg's stdin, and that is precisely
  the signal meaning *finalize the container* — so a cancelled render would have
  produced a complete, playable, wrong-length video with nothing to mark it as
  partial.

**Still ahead in this phase:**

- **Frame-parallel rendering.** The loop is single-threaded and cleanly
  pixel-bound; frames are independent and `evaluate` is pure, so this is the
  largest easy win available. It composes with the stepped job rather than
  replacing it: the parallel half is `evaluate`, the GPU half stays serial
  because there is one device. See [`performance.md`](performance.md).
- **A range control.** A job renders the whole comp; the work area exists in the
  timeline and does not reach the render yet.
**Done when:** a `.pbc` becomes an `.mp4` and a PNG sequence, from the GUI *and*
from the command line, and the exported frame equals the preview frame.
**True today**, for both halves.

### Phase 2 — Audio and the master clock

Playback is a wall clock today. Sound forces the correct model, and every timing
bug is easier to see once something is audible.

- Decode with `symphonia`, output through `cpal`. Audio is an asset in the same
  registry as footage — references in the `.pbc`, samples in the shell.
- **The clock inverts:** audio becomes the time source and the frame is derived
  from the sample position, with a fallback to the wall clock when a comp has no
  audio. This is the one change that makes long-form playback stay in sync.
- An audio layer with in/out riding the existing `LayerTiming`, plus level and
  pan as ordinary `Value<T>` (so they animate for free).
- A **waveform** drawn in the timeline — the actual reason sync is editable.
- Export mixes the comp's audio and muxes it in the same ffmpeg invocation.

**Done when:** a cut can be edited to music and the exported file carries it.

### Phase 3 — Finishing: the compositor stage and effects

The subsystem the README already identifies as shared by effects, keying,
masking and 2.5D placement. Build it once, now that its clients are real.

- Each isolated layer renders to its own offscreen target; an ordered stack of
  wgpu passes processes it; the result composites into the parent with blend,
  opacity and matte. The existing `composite.rs` semantics are the contract —
  the GPU stage must reproduce them, verified against the SVG backend.
- First effects, chosen because they are what a finished piece actually needs:
  **gaussian blur, glow, levels/curves, colour balance, chroma key**, and
  **drop shadow**. Every parameter is a `Value<T>`, so animation and the graph
  come along at no cost.
- Effects register as **descriptors** through `registry.rs`, and the built-ins
  go through the same seam a plugin would — the plugin-shaped-now decision,
  cashed in.
- **Motion blur** belongs here: sub-frame sampling of `evaluate` accumulated in
  the compositor. Cheap to state, expensive to render, gated per comp and per
  layer like AE's.

**Done when:** a layer carries an animatable effect stack that renders the same
in the preview and the export.

### Phase 4 — Trust and finish

Small, unglamorous, and the difference between a demo and a tool.

- **Autosave** to a sidecar on a timer and before risky ops; **crash recovery**
  on next launch.
- **Asset relink** UI and a "collect files" action — `core` already treats a
  relink as recoverable; expose it.
- Missing-footage and missing-font states that are *visible* in the timeline,
  not just a warning in the scene.
- Export presets (YouTube 1080p/4K, ProRes master, PNG sequence).
- A first-run demo project, and a real README split: user docs out of the design
  record.

### Deferred, deliberately

The **Nuke-style image graph** (README: *the composition node graph*), the
**published plugin SDK** (see §4 — the architecture is settled below, the
*promise* is what waits), and **Level B 3D**. All three are architecturally
spec'd and none of them is on the path to a finished video. The image graph in
particular should wait for Phase 3 — it needs the compositor stage that phase
builds, exactly as the README argues.

## 4. Extensibility: how other people build on PBC

This is not on the critical path to a first exported video, and it is on the
critical path to PBC being worth using. It is written now because **the choices
are cheap today and unpayable later** — an extension surface is a promise you
cannot withdraw, and every one of them is decided by code that already exists.

### Why After Effects has an SDK, and why we need only one system

AE's extensibility is three unrelated machines, and the reason is history rather
than design:

| AE surface | What it does | Why it exists |
| --- | --- | --- |
| **Effect SDK** (C++, `PF_*`) | Pixel filters — `PF_EffectWorld` in, out | 1993-era C ABI; the only way to reach native speed then |
| **ExtendScript / `.jsx`** | Automation, batch, document edits | A scripting layer bolted on a decade later |
| **CEP / UXP panels** (HTML+JS) | Custom UI panels | A *third* layer, because ExtendScript had no UI |

The result is famous: a panel is written in JavaScript, talks to ExtendScript
over a message bridge, which drives a document model the C++ effect API cannot
see. Three languages, three lifetimes, three sets of documentation, and a plugin
author who wants a filter *with* a panel writes both halves twice.

We do not have to inherit that, because we have something AE never had: a
**closed IR with a single pure evaluator**, a **descriptor registry**
(`core/src/registry.rs`), and an editor that already routes every mutation
through deferred, unit-tested ops. The design goal is therefore: **one plugin
model, four contribution kinds.**

### The four things a user will actually want to build

1. **A filter** — operate on pixels. (Your AE question.)
2. **A node / generator** — produce or transform *values*, not pixels: a custom
   easing, a rig, a physics driver.
3. **A panel** — custom UI that automates something for *their* studio: batch
   rename, an asset check, a shot-tracker, a one-click deliverable.
4. **An importer / exporter** — a format we don't ship.

Each maps onto a seam that either exists or is already specified.

### The unit of distribution: a folder with a manifest

A plugin is a directory containing `plugin.toml` plus its payloads:

```toml
name = "studio-tools"
version = "1.0.0"
api = "1"                       # the contract version, not the app version

[[contributes.effect]]          # a filter
id    = "studio.halation"
shader = "halation.wgsl"
params = [
  { id = "amount", type = "number", default = 0.5, range = [0.0, 1.0] },
  { id = "tint",   type = "color",  default = "#ff8844" },
]

[[contributes.panel]]           # a UI panel
id     = "studio.shotcheck"
title  = "Shot Check"
script = "shotcheck.rhai"

[capabilities]                  # declared, and consented to on install
filesystem = ["read:project-dir"]
network    = false
process     = false
```

One manifest, one install, one uninstall, regardless of how many kinds a plugin
contributes. A filter with a companion panel is *one* plugin — the thing AE
makes hardest.

### Kind 1 — Filters (pixels)

Covered in the compositor discussion above; restated here as the contract.
**The primary path is a shader plugin**: the plugin ships WGSL plus its param
descriptor, and PBC runs it as a pass in the compositor stage. No FFI, no ABI to
freeze, no crash surface (an invalid shader fails validation instead of taking
the process down), hot-reloadable while animating, and it is the *fast* path
rather than a compatibility path.

The contract a filter author is handed:

```
fn filter(input: texture, params: resolved at this frame, roi: rect) -> texture
```

Three properties fall out of the architecture for free, and are worth stating in
the docs as guarantees:

- **Params animate without the author doing anything.** A declared param becomes
  a `Value<T>` like any other, so it gets a stopwatch, a dopesheet row, easing,
  and the expression graph. In AE this is `PF_ADD_PARAM` plumbing per parameter.
- **Determinism is enforced, not requested.** No wall clock, no IO from a filter.
  This is what makes preview-equals-export provable and frame caching sound.
- **One working format, fixed before the first filter ships:** linear,
  premultiplied, **f16 RGBA**. AE's 8/16/32-bpc split and its straight-vs-
  premultiplied confusion are scar tissue we can decline to inherit — but only
  by choosing before a plugin signature exists, never after.

Native C-ABI stays the escape hatch for someone who must run existing native
code, and is not the recommended path. **AE plugins cannot be loaded** — the AE
SDK is a C++ ABI over its own suites, and a `.aex` has no meaning here. What
ports is the author's mental model, not their binary.

### Kind 2 — Nodes and generators (values)

Already solved, and this is the part that is genuinely ahead of AE. The registry
(`core/src/registry.rs`) is explicit that a descriptor is *pure metadata* —
category, label, typed sockets — and that "a built-in registers a descriptor at
startup, a plugin registers one at load, and the descriptor-driven canvas draws
either without knowing which it is."

So the work here is not architecture, it is **discipline**: keep dogfooding
built-ins through the same registration path, and never let a built-in reach a
seam a plugin cannot. The registry already carries `is_buildable_now()` to stop
us shipping a promise the evaluator can't honour — that instinct is the right
one and should be kept.

### Kind 3 — Panels and automation (the question with the least obvious answer)

> **The first step here is a console panel**, not a plugin format. It is the
> same commitment — commands in, no `&mut Document` — minus packaging, and it
> proves the vocabulary before anyone can depend on it. Critically, its log
> should record what the *UI* did, so dragging a layer prints the command that
> would have done it: that is how Blender users learn the Python API, and it
> turns the UI into its own documentation. See
> [`decisions/0019-the-console.md`](decisions/0019-the-console.md).

"How do we give access to the UI and the internals?" splits into two questions
that need opposite answers.

**Internals: expose the command layer, not the data model.** The editor already
routes edits through deferred ops applied after the UI pass — `GraphOp` /
`apply_graph_op`, the `Dock` tree rewrites — deliberately, so the flow is
unit-testable as free functions. That layer *is* the plugin API. A panel submits
the same ops a mouse click submits, which means it inherits undo (snapshot
history), validation, and the existing test coverage on day one. What a plugin
must **never** get is `&mut Document`: a plugin mutating the tree directly would
bypass undo, break the migration contract, and make every future refactor a
breaking change. The rule to write down now: **plugins read a projection, and
write only ops.**

**UI: a declarative panel spec, not our widget library.** Handing plugins `egui`
directly would be the AE mistake in a new costume — egui is pinned at 0.35 and
churns, so every upgrade would break every plugin. Instead the plugin describes
its panel (a widget tree: rows, buttons, fields, lists, a table) and receives
events back; the host renders it in our theme. Plugins get version independence
and sandboxing; users get panels that look like the app instead of an embedded
browser. `Editor` in `live/src/dock.rs:21` is a closed `pub(crate)` enum today —
it gains one `Plugin(PluginPanelId)` variant, and plugin panels then dock,
split, save into layout presets, and appear in every picker for free, because
the dock system already treats every editor uniformly.

**Language: Rhai now, WASM later.** Rhai is already in-tree driving expressions,
already sandboxed, already deterministic. It is the zero-new-dependency
automation language, and a studio scripting their own pipeline can start the day
the op layer is exposed. WASM (the README's stated choice) is for *distributable*
compiled plugins and can follow once the op surface has settled. Note that this
is also why WASM should be scoped to logic and not per-pixel work: a 4K RGBA
frame is ~33 MB per copy across the module boundary, per effect, per frame.

### Kind 4 — Importers and exporters

The `Encoder` trait from Phase 1 and the existing `Decoder` split in
`render/src/decode.rs` are already the right shape. Register them like
descriptors and a third-party format is another registered impl. Nothing new is
needed beyond honouring the seam.

### What the documentation actually has to be

An extension surface is documentation-shaped, and this is where AE's is weakest.
The minimum, per contribution kind:

- **The manifest schema** and the capability list, exhaustively.
- **A stability contract**: what `api = "1"` guarantees, what may change in a
  minor version, and how deprecation is signalled. Written *before* the first
  external plugin exists, or it will be written by accident afterwards.
- **A template repo per kind** — `cargo generate`-able, building and loading on
  first try. This matters more than reference docs; most plugin authors start by
  copying something that works.
- **One worked example per kind, shipped in-tree**, and ideally *used* by the
  app: our own halation filter, our own batch-rename panel. A seam we don't
  depend on is a seam that will quietly rot.
- **A determinism and colour-space page** — the two contracts a filter author can
  violate invisibly, producing a render that doesn't match the preview.

### Staging

- **Now (free, and only discipline):** keep every built-in going through the
  registry; keep every editor mutation going through an op; do not grow a second
  path.
- **With Phase 3 (the compositor):** shader filters, since that is the first
  moment a pixel contract can exist at all.
- **After Phase 4 (once the app is trusted):** the panel spec and the Rhai
  automation API, then WASM packaging and a published SDK.

The README's call stands and this refines it: **plugin-shaped now, stable SDK
later** — and specifically, do not publish `api = "1"` until an exported video,
an effect stack, and one first-party panel have all been built through the seams
we intend to hand over.

## 5. Risks worth naming now

- **ffmpeg as a dependency of delivery.** It is already the import path, so the
  precedent is set, but export makes it non-optional. Ship a bundled binary or
  detect-and-guide on first run; `PBC_FFMPEG` already exists for the former.
- **vello 0.9 is pinned and young.** Phase 3 puts real weight on it. The
  `render/` backend boundary is the escape hatch — keep it honest, and do not
  let compositor code reach around it.
- **The README is 2,400 lines and is the only documentation.** It has been an
  excellent design record; it is now also the onboarding doc, the user manual
  and the roadmap, and it cannot be all three. Phase 4 splits it.
- **`live/src/app.rs` is 3,900 lines.** Not urgent, but the render queue and the
  effect stack both want to live somewhere, and neither should land in there.

## 6. The one-line version

The engine is ready and **export now works from both the CLI and the GUI**. Next
is **audio and the master clock**, then the **compositor and effects**, then
**autosave and relink** — and still no new engine depth until those land.
Extensibility (§4) costs nothing today but discipline: keep every built-in going
through the registry, and every edit through an op.
