# Architecture decision records

One file per architectural call: the context that forced it, the decision, and
what it costs. These were extracted from the README's design journal on
2026-09-08 — the reasoning is original, the format is new.

**An accepted ADR is not edited to say something different.** If a decision
changes, write a new one that supersedes it and mark the old one superseded.
The wrong turns are as useful as the right ones; a record that quietly rewrites
itself teaches nothing.

## Implemented

| # | Decision |
| --- | --- |
| [0001](0001-headless-core.md) | A headless `core` the engine can be tested without a window |
| [0002](0002-evaluate-is-the-only-entry-point.md) | `evaluate(doc, frame)` is the single pure entry point |
| [0003](0003-frames-are-native-time.md) | Frames are the native time domain |
| [0008](0008-asset-registry.md) | Footage import: an asset registry, references never pixels |
| [0010](0010-expression-ir.md) | Expressions and the node graph lower to one IR |
| [0011](0011-precomps-and-layer-time.md) | Pre-comps and the per-layer time model |
| [0012](0012-reusable-modules.md) | Reusable animation modules — shared, auto-retimed, overridable |
| [0014](0014-undo-is-snapshots.md) | Undo/redo is whole-document snapshots, not inverse operations |
| [0015](0015-dock-tree.md) | The workspace is a layout tree edited by deferred ops |

## Decided, not yet built

| # | Decision |
| --- | --- |
| [0004](0004-vector-first-raster-compositor.md) | Vector-first substrate, raster compositor on top |
| [0005](0005-compositor-stage.md) | One compositor stage, shared by effects, keying, masking and 2.5D |
| [0006](0006-25d-level-a.md) | 2.5D: target Level A (flat layers in 3D), not Level B |
| [0007](0007-never-implement-codecs.md) | Export: never implement codecs |
| [0009](0009-plugin-shaped-now.md) | Plugin-shaped now, stable SDK later |
| [0013](0013-composition-node-graph.md) | The composition node graph — one node system, three scopes |

## The two unifying insights

Why this is one project rather than N, and the reason several ADRs above keep
pointing at each other:

1. **One deterministic, sandboxed eval-with-dependency-graph** serves
   expressions, WASM plugins, effect params, and media sampling. Build it once.
2. **One compositor stage** serves effects, keying, masking, blend modes, and
   2.5D card placement. Build it once.

Everything else (ffmpeg export, asset registry) hangs off `evaluate` staying the
single pure entry point. Protect that.
