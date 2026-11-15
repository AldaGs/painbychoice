# 0024. The live link: one file, two writers

- **Status:** Accepted — implemented (`crates/live/src/livelink.rs`)
- **Decided:** 2026-11-14

## Context

An agent edits a project by writing its `.pbc` and running `motion check` /
`preview`. Until now that only worked with the editor closed: the editor never
looked at the file again after opening it, and its next save wrote over
whatever the agent had done, silently.

The brief for PBC Edit (Phase 1) asks for the opposite: the editor open, an
agent writing the same file, and nothing lost on either side.

## Decision

1. **The editor watches its file.** With a project open the event loop wakes
   every 250ms (`ControlFlow::WaitUntil`) and compares the file's mtime, then
   its text, with what the editor last read or wrote. An mtime poll on one
   file, not the `notify` crate: no dependency, no platform quirks with atomic
   renames, and 250ms is under the ~150ms debounce plus a redraw the brief had
   in mind for a human watching.
2. **A foreign write is one undo step.** It is parsed, migrated (invariant 8)
   and validated, then swapped in through `History::record` labelled
   "External edit". Undo takes the agent's change back out. Snapshot undo
   (ADR 0014) is what makes this nearly free; no invariant moves.
3. **Unsaved edits ask.** If the editor's document differs from what it last
   synced with the file, the reload asks first. Accepted, the editor's edits
   stay one Undo away. Declined, the file is left alone — and the next save
   asks before overwriting it.
4. **A save never clobbers unseen.** Before writing the open file, the editor
   re-reads it; if it differs from what the editor last synced, it asks.
5. **`rev`.** Every `.pbc` carries `rev`, the save count. Each writer writes
   the `rev` it read plus one. The editor detects foreign writes **by content**,
   not by `rev`, so an agent that forgets to bump it is still never clobbered;
   `rev` is for the agent, which re-reads before writing and re-derives its
   change if `rev` moved.
6. **Invalid writes are refused, not loaded.** A file that fails to parse (a
   half-written one included) or has two layers with one id in a comp keeps the
   editor's document, logs why, and is picked up again by the next write. That
   is also the debounce.

## Stable ids

Already true of the `.pbc`: layers carry a `NodeId`, comps are keyed by
`CompId`, and graph bindings name those ids. Nothing to migrate. An agent adding
a layer must give it an id unused in its comp (max + 1 is safe); a collision is
refused per rule 6. The authoring JSON still addresses by name and index — it is
an import format, not the live file, so it is left alone.

## Consequences

- An external change clears the footage cache and reloads sounds, since asset
  ids may now name other files. Correct, and rare enough not to optimise.
- The editor wakes four times a second while a project is open, even idle.
  A stat and nothing more when the file is unchanged.
- Layout (dock, windows) in an externally written file is ignored on reload:
  it belongs to the person at the screen, not the writer.
- Not done: replaying the editor's unsaved ops *on top of* the agent's change.
  Snapshot undo has no ops to replay; "ask, and keep yours in Undo" is the
  honest version. Revisit if merging becomes a real need.
