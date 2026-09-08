# 0019. A console panel, and why it is the automation API's first surface

- **Status:** Accepted — not yet built
- **Decided:** 2026-09-08

## Context

Professional tools in this space all have one: Blender's Python console and Info
editor, Nuke's Script Editor, Maya's command line, Houdini's textport. They are
not a power-user garnish. They are how a serious workflow gets automated, how a
studio adapts a tool to its pipeline, and — most underrated — **how people learn
the API without reading documentation**.

PBC has the two ingredients already: Rhai is in-tree driving expressions
(sandboxed, deterministic), and the editor already routes every structural edit
through deferred ops applied after the UI pass, as free functions over
`&mut Document`, precisely so they are testable without a window.

The extensibility plan (§4 of [`../production-plan.md`](../production-plan.md))
commits to plugins reading a projection and writing only ops. A console is that
same commitment, minus packaging — which makes it the cheapest possible first
version of the automation API, and the one that proves the seam before any
third party depends on it.

## Decision

**Build a console panel, as an ordinary `Editor` in the dock**, running Rhai
against a **command vocabulary** — never against the document directly.

Three calls that decide whether it is useful or merely present:

1. **It executes commands, not internals.** The console gets the same ops a
   mouse click submits: it inherits undo, validation, and the existing test
   coverage for free, and it cannot reach past the snapshot boundary. A console
   with `&mut Document` would be a console that corrupts projects, and every
   future refactor would be a breaking change to it.

2. **It is a log *and* an input, and the log records what the UI did.** This is
   the feature that makes a console teach rather than merely obey: drag a layer,
   see `layer("Title").move(12, 0)` appear in the log, copy it into a script.
   Blender's Info editor is the model, and it is how most Blender users learn
   the Python API — not from the reference. Without this the console is a
   second, worse way to do things you can already do with the mouse; with it,
   the UI becomes self-documenting.

3. **The vocabulary is the plugin API.** Anything the console can say, a plugin
   panel can say later, in the same words. One surface designed once, rather
   than a scripting language and a plugin API that drift apart — which is
   exactly the trap AE fell into with ExtendScript and the C++ SDK.

## Consequences

- **This must come after the command layer is a real thing**, not before. Today's
  ops are shaped for the UI that submits them (`GraphOp` is addressed by
  `(property, tree-path)`); a console needs commands addressed by *names* a
  person can type. That naming pass is the actual work here, and it is worth
  doing carefully — those names are the API, and they are hard to change once
  anyone has scripted against them.
- Rhai means no new dependency, and the same sandbox expressions already run in.
- It should land alongside or just after the render queue, so `render()` is one
  of the first verbs — batch rendering from the console is the most immediately
  useful thing a console can do, and it is the shape of every studio pipeline.
- It supersedes nothing, but it makes §4's panel work concrete: the declarative
  panel spec and the console consume the same vocabulary, so building the
  console first de-risks the plugin API rather than competing with it.
- A console is also the honest answer to "how do I do the thing the UI does not
  have a button for", which is otherwise answered by a feature request.
