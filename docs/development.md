# Development

How to build it, where things live, and the procedures that have a right answer.
Read [`gotchas.md`](gotchas.md) before fighting the UI framework, and
[`invariants.md`](invariants.md) before changing anything structural.

> Part of the PBC documentation set — see [`README.md`](README.md) for the map.

## Run it

```bash
cargo test --workspace     # engine + hit-test unit tests (all green)
cargo run -p motion-live   # THE EDITOR (opens a window) — this is the app
cargo run --bin motion     # offline: writes out/frame_00.svg .. frame_08.svg
```

Windows note: the running `pbc.exe` locks the binary, so `cargo build` can't
replace it while open. Kill it first: `taskkill //F //IM pbc.exe`.

Before pushing: `cargo test --workspace` and `cargo clippy --workspace
--all-targets`. The test suite is fast (well under a second) — there is no
excuse for not running it.

## Key code locations

- `core/src/timebase.rs` — `Timebase`: the **only** place that converts between
  seconds and frames, plus `timecode()` → `hh:mm:ss.ff` (non-drop-frame).
  Reach for `doc.timebase()`; never divide by `fps` by hand.
- `core/src/value.rs` — `Value<T>`, `Track<T>`, `Keyframe` (`.frame: i64`),
  `Handle`, easing solver. Keyframe ops: `set_at`, `insert_key` (const→track),
  `move_key` (neighbour-clamped, ±1 frame), `remove_key`, `key_frames`,
  `segment_handles` / `set_segment_handles`.
  Multi-key ops: `move_keys_limits` / `move_keys` (rigid block, clamped against
  the *block's* outer neighbours — see the doc comment for why per-key clamping
  collapses a selection), and `keys_at` / `insert_keys` for copy/paste.
- `core/src/node.rs` — `Node`, `Transform`, `Shape` (parametric Rect/Ellipse/
  Path), `Document`. Tree ops: `find`, `find_mut`, `reorder_child`, `remove`.
  Also `Document::timebase()`, `duration_frames()`, and **`migrate()`**.
- `core/src/eval.rs` — `evaluate(doc, frame) -> Scene`, `RenderItem`
  (+provenance).
- `core/src/expr.rs` — expressions: `EvalCtx` (the resolve context: frame, doc,
  cache, warnings), `ExprValue` + `From`/`ToExpr`, the `Expr` IR, `PropPath`,
  `eval_expr` with the memo + cycle-detecting `ResolveCache`, and `eval_script`
  (Rhai, on a thread-local engine) for `Expr::Script` nodes.
- `core/src/demo.rs` — the demo document loaded on launch.
- `live/src/app.rs` — the per-frame heart. `App::render` is:
  evaluate → hit-test → gather snapshots → run egui → apply `*Edits` → GPU. Panel
  fns: `comp_ui`, `tree_ui`, `transport_ui`, `dopesheet_ui`, `properties_ui`,
  `graph_ui` (the expression editor; `apply_graph_op` applies its deferred
  edits), `ease_editor`, `key_button`. Each panel fn renders into a `&mut Ui` it is
  handed — it does **not** create its own `egui::Panel`; placement is the
  layout tree's job (see [`editor.md`](editor.md)).
  Timeline mapping: `TimelineView` (the visible frame window) + `Axis`
  (frame↔pixel), built once by the ruler and reused by every row so they cannot
  drift out of alignment.
  Keyframe selection: `KeyRef` = `(PropKind, index)`, `KeySelection` =
  `BTreeSet<KeyRef>` (ordered so `group_selection_by_prop` can bucket it in one
  pass), `KeyClipboard`/`ClipTrack` for copy/paste — `ClipTrack` is the
  type-erasure boundary that keeps `Vec2` keys off a scalar property.
  **`prop_of` / `prop_of_mut`** are the single place `PropKind` is matched:
  they hand back a `PropRef`/`PropRefMut` (Vec2 | Num | Color) and every
  keyframe op goes through that.
  Graph canvas: `layout_expr` (tidy-tree placement, `box_height` per kind) +
  `expr_canvas`/`expr_box` draw it; every edit is one deferred `GraphOp` keyed by
  `(property, tree-path)` applied by `apply_graph_op` (a free fn over
  `&mut Document`, so it's unit-tested). Node positions are ephemeral egui-memory
  view state, not saved with the doc.

## Procedures

These three come up constantly and each has one correct answer.

### Adding an animatable property

`PropKind` names every animatable property; `prop_of`/`prop_of_mut` borrow one
off a `Node` as a type-erased `PropRef`/`PropRefMut`. Everything else — dopesheet
rows, retiming, delete, copy/paste, easing, the stopwatch — is written against
that pair, so **adding a property is a `PropKind` variant, an entry in
`PropKind::ALL`, a `label()` arm, and one arm in each of the two `prop_of`
functions.** No other match statement should have to grow.

`PropRef` returns `Option` because not every node has every property (a group
has no fill; an `Ellipse` no radius; a `Path` no parametric size) — callers skip
`None` rather than branching on shape kind, which is what keeps the "does this
node have it" question in exactly one place. The two functions must agree on
which properties exist, or reads and writes silently target different things;
there's a test pinning that.

### Loading a `.pbc`: always call `migrate()`

Pre-frame-grid documents stored keyframe times as float **seconds**. A
`Keyframe` can't convert itself (it has no timebase), so deserializing parks the
old value in a serde-only `legacy_seconds` field and `Document::migrate()`
converts it using the document's own `fps`. **Any new load path must call it**;
it's a no-op on an already-migrated doc. The legacy field is never
re-serialized, so a file is permanently migrated on its first save. Keys that
round onto the same frame collapse to one.

### Saving the layout: the `Project` wrapper

The `.pbc` is a `Project { document, layout }` (both in `live/`), **not** a bare
`Document` — the UI layout can't live in `core::Document` without breaking the
headless-engine split, so the app wraps the two on the way to disk. `layout`
holds the active `Dock` and the user presets (built-ins are code, so they're
never stored; `Preset::builtin` is `#[serde(skip)]` and reconstructs as `false`).

Two rules keep this safe:

- **Reading is backward-compatible.** `load` tries `Project` first; a pre-layout
  file is a bare `Document` with no `document` field, so that parse fails and the
  loader falls back to deserializing a plain `Document` (with the default
  layout). Distinguishing the two is exactly the absent/present `document` key —
  don't give `Project::document` a serde default or the fallback stops firing.
- **A loaded layout is validated.** `Dock::is_valid` requires the invariants the
  render path assumes (one canvas, innermost; comp + transport present). A file
  that fails is dropped for `default_layout()` rather than wedging the editor
  with, say, a layout that has no way back to the comp bar. User presets are
  filtered the same way.

## Writing a panel

A panel function renders into a `&mut Ui` it is handed — it does **not** create
its own `egui::Panel`; placement is the layout tree's job. Every leaf must
either fill its area exactly or scroll inside it, never allocate past it
(`Editor::scroll_wrapped`, pinned by a test). See [`editor.md`](editor.md) for
the layout tree and [`gotchas.md`](gotchas.md) for the egui traps that will
otherwise cost you an afternoon each.

## Conventions worth knowing

- **Structural edits are deferred ops.** The dock (`Dock` rewrites) and the node
  graph (`GraphOp` / `apply_graph_op`) both collect edits during the UI pass and
  apply them after it, as free functions over `&mut Document` — which is why
  both are unit-testable without a window. Follow the pattern; do not mutate
  mid-pass.
- **Tests are named as sentences.** `a_real_ffmpeg_stream_yields_whole_frames_in_order`
  reads as a claim about behaviour, and a failure names the broken claim.
- **A comment explains why, not what.** The codebase is unusually well
  commented and the comments have paid for themselves repeatedly; the ones that
  earn their place record a decision or a trap, not a restatement of the line
  below.
- **Don't round-trip source files through PowerShell.** See
  [`gotchas.md`](gotchas.md) — PS 5.1 mojibakes every non-ASCII character.
