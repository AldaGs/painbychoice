# Invariants

The rules this codebase does not bend. Most are enforced by a test; the rest are
enforced by review. **If a change requires breaking one of these, that is not a
refactor — it is an architecture decision, and it needs an
[ADR](decisions/).**

This page exists so that a year from now, nobody has to reconstruct which
constraints were load-bearing and which were taste.

## The engine

1. **`core` never touches a GPU, a window, or a file.** A frame must be
   evaluatable in a unit test. ([0001](decisions/0001-headless-core.md))
2. **`evaluate(doc, frame)` is the only way to ask what the document looks like
   at a time.** No second path, ever.
   ([0002](decisions/0002-evaluate-is-the-only-entry-point.md))
3. **Evaluation is pure and deterministic** — no wall clock, no IO, no hidden
   state. `wiggle()` is seeded, not random. This is what makes preview-equals-
   export provable and frame caching sound.
4. **`Timebase` is the only place seconds and frames convert.** Never divide by
   `fps` by hand. ([0003](decisions/0003-frames-are-native-time.md))
5. **The closed IR enums are the evaluation substrate.** Descriptors are
   metadata; a registry must never become a second evaluator.
   ([0013](decisions/0013-composition-node-graph.md))
6. **The `Node` / `Comp` tree is the structural spine.** Graphs are authoring
   front-ends that lower to it.
7. **No operator may produce a NaN or an infinity.** Divide by zero, modulo by
   zero, a fractional power of a negative all resolve to 0 — a NaN reaching a
   transform blanks the layer with no clue why, which is the least debuggable
   failure this engine has.
8. **Loading a `.pbc` always calls `migrate()`.** Old documents must keep
   opening. Serialization changes ride a migration.
9. **The document stores references, never pixels.** Footage and fonts are
   looked up; the decoded bytes live in the shell.
   ([0008](decisions/0008-asset-registry.md))

## The editor

10. **A panel function renders into a `&mut Ui` it is handed.** It never creates
    its own `egui::Panel` — placement belongs to the layout tree.
    ([0015](decisions/0015-dock-tree.md))
11. **Every leaf either fills its area exactly or scrolls inside it, never
    allocates past it** (`Editor::scroll_wrapped`). Violating this makes a panel
    resize its neighbours, canvas included. Pinned by a test.
12. **Structural edits are deferred ops applied after the UI pass**, as free
    functions over `&mut Document`, so they are testable without a window.
13. **Every layout preset keeps one innermost canvas, and the comp and transport
    toolbars.** Those are headerless and cannot be re-added if a preset drops
    them. Pinned by a test.
14. **Undo is a whole-document snapshot.** A new feature adds no undo code — and
    must not acquire a mutation path that bypasses the snapshot boundary.
    ([0014](decisions/0014-undo-is-snapshots.md))
15. **One keyframe per frame per track.** `sample` depends on it; paste replaces
    rather than stacks.
16. **A render draws the composition and nothing else.** Everything the editor
    adds around a comp — onion skins, passepartout, frame border, selection
    outline — is `scene::Chrome`, and an export passes `Chrome::none()`. The
    frame border is the one that bites: it is drawn unconditionally in the
    preview and sits exactly on the crop, so a render that forwarded the
    editor's chrome would put a grey rectangle around every delivered frame.
    Pinned by a test.
17. **An encoder is finished or aborted, never merely dropped.** Closing a
    process-backed encoder's stdin is the signal that means *finalize the
    container*, so dropping a cancelled ffmpeg encoder yields a complete,
    playable, wrong-length video. `Encoder::finish` reports muxer failure;
    `Encoder::abort` kills the process and removes the fragment. Both are pinned
    by tests. ([0020](decisions/0020-the-render-job-is-stepped-not-threaded.md))

## The boundaries we are deliberately keeping open

18. **`render/` abstracts `Scene → pixels` with more than one backend.** That
    boundary is the escape hatch if vello's maturity bites; compositor code must
    not reach around it. ([0004](decisions/0004-vector-first-raster-compositor.md))
19. **We never implement a codec.** Frames out, encoder in.
    ([0007](decisions/0007-never-implement-codecs.md))
20. **Built-ins register through the same seam a plugin would.** A seam we do
    not dogfood is a seam that will rot.
    ([0009](decisions/0009-plugin-shaped-now.md))
21. **Plugins read a projection and write only ops.** No plugin ever gets
    `&mut Document` — that would bypass undo, migration, and every test.
22. **We do not ship a promise the evaluator cannot honour.**
    `NodeCategory::is_buildable_now()` exists for exactly this, and a test pins
    it.
