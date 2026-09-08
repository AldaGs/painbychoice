# 0018. Two render buttons: Draft and Master

- **Status:** Accepted — partially implemented (the model and the CLI; the
  buttons wait on the GUI render queue)
- **Decided:** 2026-09-08

## Context

Every editing tool eventually grows an export dialog, and every export dialog
eventually becomes something users dread: a modal full of codecs, profiles,
bitrates and colour options that must be re-answered every time, where a wrong
answer is discovered an hour later in the file.

After Effects has the Render Queue and Output Modules. It gets one thing right —
settings are saved — and one thing badly wrong: the settings are opaque
templates, edited through nested dialogs, and the fast path and the deliverable
path are the *same* path with different values typed into it.

Build tooling solved this years ago, and the solution is not a better dialog.
`cargo run` and `cargo build --release` are **different verbs**. Nobody
configures `cargo run`; nobody ships what it produces.

## Decision

**Two render actions, not one with a mode.**

- **Draft** — "let me see it move." No dialog, ever. One keystroke, a predictable
  destination, fast encoder settings. The button pressed twenty times an hour.
- **Master** — "this is the deliverable." Full settings, and *reproducible*:
  the settings belong to the **project**, not to whatever was last typed into a
  dialog, so two people on one project produce identical files.

Three rules that make the split hold, each of which is a way it usually fails:

1. **Draft renders every pixel.** It is cheaper to *encode*, never cheaper to
   *render*. A draft that silently halved resolution would be the fastest
   possible way to ship the wrong file, and a draft you cannot trust is a draft
   nobody uses. (Pinned by a test: the draft preset may not resize or filter.)
2. **Draft never writes the master's output path.** Overwriting a deliverable
   with a preview is unrecoverable and entirely avoidable.
3. **The preset table stays small.** One sensible default per quality per
   container, and ffmpeg's own flags for everything else — a user's arguments
   are appended *after* the preset's, so anything can be overridden without
   growing a codec matrix. The moment this table needs a UI of its own we are
   maintaining what [0007](0007-never-implement-codecs.md) says not to maintain.

Container-aware where it genuinely differs: a `.mov` master is **ProRes**,
because that is what an editorial hand-off means by a `.mov`, and H.264 in a
`.mov` wrapper is the wrong answer wearing the right extension.

## Consequences

- `Quality` lives in `render/src/encode.rs` and is exercised by the CLI
  (`--quality draft|master`) before any button exists. The GUI buttons become a
  thin call rather than a new mechanism, and the behaviour is already tested.
- The default is **Draft**, on the command line and on the buttons, for the same
  reason: it is the one reached for constantly.
- Still to build: **named presets saved in the `.pbc`** so a team renders
  identically, and the render queue that hosts both buttons. Presets are
  project data (`#[serde(default)]`, so no migration), not app settings — an
  export spec is part of how a piece is delivered, and it has to travel with it.
- Deliberately *not* built: a general output-module editor. If Master needs an
  option often enough to deserve a control, it earns one; otherwise it is an
  ffmpeg flag in a saved preset.
