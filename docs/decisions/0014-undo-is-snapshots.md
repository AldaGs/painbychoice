# 0014. Undo/redo is whole-document snapshots, not inverse operations

- **Status:** Accepted — implemented
- **Recorded:** 2026-09-08, extracted from the README design journal.

## Context

Inverse operations are the textbook answer: each mutation ships its own undo.
The cost is that *every* future feature must implement one correctly, forever,
and a single missing or wrong inverse corrupts the document silently.

## Decision

Snapshot the whole document. The engine's data is a plain serializable tree, so
a snapshot is a clone — cheap enough at real document sizes, and impossible to
get subtly wrong.

The implementation, including what counts as one undoable step and how a drag
coalesces into a single entry, is in [`../editor.md`](../editor.md).

## Consequences

Adding a feature costs no undo code at all, which is the entire point. The price
is memory proportional to history depth times document size — measured and
acceptable — and it is the reason the plugin API hands out *ops* rather than
`&mut Document`: an op goes through the snapshot boundary, a direct mutation
would slip underneath it.
