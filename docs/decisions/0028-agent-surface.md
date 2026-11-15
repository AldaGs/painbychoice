# 0028. The agent surface: query, apply, describe

- **Status:** Accepted — implemented (`crates/app/src/ops.rs`, `core/src/edit.rs`, `docs/AGENTS.template.md`, `CLAUDE.md`)
- **Decided:** 2026-11-15

## Context

The brief (sections 2.3, 2.6) asks for a compact read (`query`), a cheap
atomic write (`apply`), self-description (`describe`), and an entry point
agents find on their own.

## Decision

1. **`motion query`** prints top-level layers per comp, with frame timing,
   kind, bus, sync, tags and subtitles (word frames with `--words`), plus
   buses, outputs and assets. A typical 85 KB project summarizes to ~3 KB.
2. **`motion apply`** runs ops in order on the parsed project: `set` (a JSON
   pointer, through serde, so any field and its validation come for free),
   `trim`, `move`, `insert`, `delete`, `razor`, `cut`, `preset`. All ops must
   succeed and the result must pass the live link's checks (unique ids per
   comp, a root that exists) before anything is written. The write keeps every
   other key in the file (layout), bumps `rev`, and goes through a temp file
   plus rename, so the editor's poll never reads half a file. An open editor
   takes it as one undo step (ADR 0024).
3. **Timeline edits moved to `motion_core::edit`.** Razor, ripple, roll,
   sync and range removal are pure functions over a comp root, so the CLI and
   the editor share one implementation. The editor keeps only its `Op` enum.
   Deleting a layer is `Project::delete_layer` for both, which also raises the
   id floor and untargets node-graph sinks and refs that named it.
4. **`motion describe` is generated, not written**: edit ops (with an
   example), every effect (`EffectType::ALL` with its seed as the example),
   every graph node (`NodeRegistry`), and the caption presets, including a
   project's own when you pass the `.pbc`. `describe <id>` prints one entry, or
   every entry under a prefix (`describe op`).
5. **AGENTS.md**: the editor writes `docs/AGENTS.template.md` beside a project
   on save if none exists, and the repo has a `CLAUDE.md`. Both teach the loop
   (describe, query, apply or write, check, preview) in about 20 lines.

## Consequences

- Descriptors are not `schemars` JSON Schemas yet, and there is no plugin
  manifest: there are no plugins to load. Add both when the first plugin does.
- `schema` stays the authoring reference; it is not renamed to `describe`.
- Ops address comp frames, not seconds; `query` reports both.
