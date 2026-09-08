# Road to production: state of PBC and the plan to a shippable v1

Written 2026-09-08. This is a **status audit plus a build order**, aimed at one
goal: *a person can sit down in PBC, cut a piece, and hand over a finished video
file.* The README is the design record — it says what was decided and why. This
doc says what is missing between here and a deliverable, and in what order to
close it.

## 1. Where the project actually is

Measured, not remembered: 37k lines of Rust across four crates, `cargo test
--workspace` green (553 tests: 304 core, 239 live, 10 render).

| Area | State |
| --- | --- |
| Document model (`core`) | Solid. Frames-native timebase, `Value<T>` keyframes with easing, 2.5D `Transform` (Vec3 + Mat4), camera, groups, parenting, per-layer timing, pre-comps, params, vector paths, text, footage refs. |
| Evaluation | Solid. `evaluate(doc, frame)` is the single pure entry point, with memoization, cycle guard, and warnings as a provenance channel. |
| Expressions / graph | Deep. Closed IR (`Bin`/`Un`/`Ref`/`Param`/`Gen`/`Script`/`Use`), Rhai bridge with `value()`/`wiggle()`, generators, reusable modules, a node canvas. |
| Editor (`live`) | Broad. Dockable panels, timeline + dopesheet + curves, gizmo, pen tool, snapping, guides, onion skins, motion path, layer strips, font picker, undo/redo. |
| Compositing | Model only. Blend modes, masks, track mattes exist in `Scene` and both backends. No effect stack, no GPU effect passes. |
| Footage | Import works: stills (incl. HEIC/RAW), video via an `ffmpeg` sidecar, threaded decode cache with a warm frame stream. |
| **Export** | **Does not exist.** No encoder, no render queue, no PNG sequence. `motion` still writes the 9-frame SVG demo. |
| **Audio** | **Does not exist.** No decode, no playback, no waveform, no master clock. |
| Effects | Does not exist. `NodeCategory::Effect` is a registry slot with nothing in it. |
| Motion blur | Does not exist. |
| Project robustness | No autosave, no crash recovery, no asset relink UI, no "collect files". |

**The honest summary:** PBC is a strong *animation authoring engine* with a
capable editor on top, and it is **not yet a video tool**, because nothing
comes out of it. Everything on the roadmap past this point has been engine
depth; the gap to production is delivery, sound, and the finishing pass.

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

### Phase 1 — Export (the unblocker)

The single highest-value change in the project. Nothing else here matters if a
piece cannot leave the app.

- `render/src/encode.rs`: an `Encoder` trait — `begin(w, h, fps)`,
  `push(&[u8] /* RGBA */)`, `finish()`. Two impls: **ffmpeg sidecar** (raw
  `rgba` over stdin, mirroring `decode.rs`'s process discipline, including its
  "is the tool installed?" error) and **PNG sequence** (`image`, near-free).
- An **offscreen vello render target** in `render`, so a frame is rasterized at
  full comp resolution independent of the window and the preview camera. This is
  the piece that makes preview-vs-export parity a testable property rather than
  a hope.
- A **render queue** in `live`: comp, frame range (comp / work area / custom),
  scale, format, output path; runs on a worker with progress and cancel; the
  editor stays live throughout.
- Retire the demo in `crates/app` — make `motion` the **headless renderer**
  (`motion render project.pbc --comp X --out film.mp4`). That is also the CI
  and batch story, and it keeps the engine honest as a library.
- Tests: a fixed document renders byte-identical frames twice; the SVG and the
  GPU backend agree on blend/matte structure; a missing `ffmpeg` fails with the
  named-tool error, not a panic.

**Done when:** a `.pbc` becomes an `.mp4` and a PNG sequence, from the GUI and
from the command line, and the exported frame equals the preview frame.

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
**WASM plugin ABI**, and **Level B 3D**. All three are architecturally spec'd
and none of them is on the path to a finished video. The image graph in
particular should wait for Phase 3 — it needs the compositor stage that phase
builds, exactly as the README argues.

## 4. Risks worth naming now

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

## 5. The one-line version

The engine is ready. Build **export**, then **audio**, then the **compositor and
effects**, then **autosave and relink** — and stop adding engine depth until a
finished video can leave the application.
