# 0015. The workspace is a layout tree edited by deferred ops

- **Status:** Accepted — implemented
- **Recorded:** 2026-09-08, extracted from the README design journal.

## Context

Splittable, dockable panels can be built as ad-hoc panel juggling or as rewrites
of an explicit tree. Only one of those is testable, and egui's own panel
splitters make the literal drag-a-corner gesture far more fragile than header
controls for the same capability.

## Decision

The workspace is a `Dock` tree. Every structural change — split, join, retype —
is a pure tree rewrite, expressed as a deferred op and applied *after* the UI
pass, never in the middle of one. Structural guarantees (one innermost canvas;
the comp and transport toolbars always present) are pinned by tests over every
built-in preset.

The tree's shape, the panel-writing contract, and `Editor::scroll_wrapped` are
documented in [`../editor.md`](../editor.md).

## Consequences

This is the same discipline the node graph uses (`GraphOp` / `apply_graph_op`)
and the reason both are unit-tested as free functions. It is also what makes a
plugin panel a one-variant change rather than a new system: `Editor` gains a
`Plugin(..)` arm and plugin panels dock, split and save into presets for free.
