# PBC documentation

The map. Every page has one job; if you cannot tell which page something belongs
on, that is a sign the page list is wrong — fix it rather than appending to
whatever file is open.

## Start here

| Page | What it answers |
| --- | --- |
| [`architecture.md`](architecture.md) | How the engine works: crates, the document model, evaluation, expressions, time, text. |
| [`development.md`](development.md) | How to build it, where code lives, and the procedures with one right answer. |
| [`invariants.md`](invariants.md) | The rules that do not bend. Read before changing anything structural. |
| [`gotchas.md`](gotchas.md) | Traps, mostly egui's. Read before fighting the UI framework. |

## Reference

| Page | What it answers |
| --- | --- |
| [`editor.md`](editor.md) | The `live/` shell: module layout, the panel tree, timeline, undo, stacking, theming. |
| [`canvas-tools.md`](canvas-tools.md) | Viewport tools: preview camera, gizmo, grid/guides, snapping, onion skins, motion path. |
| [`features.md`](features.md) | What a user can actually do today — the capability inventory. |

## Direction

| Page | What it answers |
| --- | --- |
| [`production-plan.md`](production-plan.md) | What is missing before a finished video can leave the app, and in what order. **The current priority document.** |
| [`roadmap.md`](roadmap.md) | The agreed build order and what each completed item delivered. |
| [`decisions/`](decisions/) | ADRs: the architecture calls, why they were made, and what they cost. |
| [`prior-art.md`](prior-art.md) | What we are borrowing from, and from whom. |

## Where does this go?

- **A decision, with alternatives you rejected** → a new ADR in
  [`decisions/`](decisions/). Never edit an accepted one to say something
  different; supersede it.
- **How an implemented system works** → the matching topic page
  (`architecture` / `editor` / `canvas-tools`).
- **A trap that cost you an afternoon** → [`gotchas.md`](gotchas.md), while it
  is fresh.
- **A rule someone could plausibly break** → [`invariants.md`](invariants.md).
- **A new user-visible capability** → [`features.md`](features.md).
- **A change of plan** → [`production-plan.md`](production-plan.md) or
  [`roadmap.md`](roadmap.md).

The rule that keeps this from re-collapsing into one file: **an ADR owns the
"why", a topic page owns the "how", and no paragraph lives in two places.** Link
instead of copying.
