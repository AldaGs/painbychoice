# Pain By Choice (PBC)

A hybrid vector motion / animation tool — non-destructive, non-linear,
parametric. A blend of After Effects, Figma, Animate and Cavalry, on a Rust
engine.

**Status: working editor, no export yet.** You can build a composition from
scratch, animate it with frame-accurate keyframes and editable easing, drive any
property with expressions or a node graph (including Rhai script nodes), nest
pre-comps, import footage, scrub, play, and save. You cannot yet render it to a
video file — that is the top priority, and the plan is
[`docs/production-plan.md`](docs/production-plan.md).

## Run it

```bash
cargo test --workspace     # engine + editor unit tests
cargo run -p motion-live   # THE EDITOR (opens a window) — this is the app
cargo run --bin motion     # offline: writes out/frame_00.svg .. frame_08.svg
```

Rust stable, no system dependencies to build. Footage import shells out to
`ffmpeg` at runtime if you use it (`PBC_FFMPEG` overrides the binary).

## The shape of it

Four crates, deliberately layered. `core` is headless and knows nothing about
GPUs or windows — the engine must be testable by evaluating a frame in a unit
test, not a window. This separation is the whole design.

```
crates/
  core/    document model + evaluation engine. No GPU, no windowing.
  render/  evaluated Scene -> pixels. SVG backend; vello lives in live/.
  app/     offline binary `motion`
  live/    the editor `pbc`: winit + vello (wgpu) + egui over the engine
```

Animation is a lazy `Value<T>` recipe — a constant, a keyframe track, or an
expression — and `evaluate(&doc, frame)` is a **pure function** that resolves the
whole tree at a frame into a flat `Scene`. Non-destructive editing, non-linear
scrubbing and determinism all fall out of that one choice.

## Documentation

Start at **[`docs/README.md`](docs/README.md)** — it maps the set and tells you
where new writing belongs.

- [Architecture](docs/architecture.md) — how the engine works
- [Development](docs/development.md) — build, code map, procedures
- [Invariants](docs/invariants.md) — the rules that do not bend
- [Gotchas](docs/gotchas.md) — traps, mostly egui's
- [Decisions](docs/decisions/) — ADRs: what was decided, and what it cost
- [Production plan](docs/production-plan.md) — the road to a shippable v1

## License

MIT.
