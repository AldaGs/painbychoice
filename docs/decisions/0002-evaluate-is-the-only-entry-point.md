# 0002. `evaluate(doc, frame)` is the single pure entry point

- **Status:** Accepted — implemented
- **Recorded:** 2026-09-08, extracted from the README design journal.

## Context

Expressions, plugins, effect parameters, media sampling and export each need
"the state of the document at time t". If each grows its own path they will
disagree, and the disagreement surfaces as a render that does not match the
preview — the least debuggable class of bug this project can have.

## Decision

One pure function, `evaluate(doc, frame) -> Scene`, is the only way to ask.
Purity is load-bearing rather than stylistic: no wall clock, no IO, no hidden
state. `resolve` threads an `EvalCtx` (frame, doc, memo cache, warnings) rather
than a bare frame, because `valueAtTime(t')` samples at *other* times and a
"bake once at t" pre-pass therefore cannot work.

The mechanics — the `EvalCtx` seam, `Scene::places` and why a node's place is
separate from its drawing — are documented in
[`../architecture.md`](../architecture.md).

## Consequences

Determinism is a property we can test rather than hope for, which is what makes
frame caching sound, preview-equals-export provable, and the plugin sandbox the
same sandbox expressions already run in. Anything resolving a node's properties
outside `evaluate` (the properties readout, the script preview) must go through
`EvalCtx::in_node` or it will show a fallback where the canvas shows the truth.

Protect this. Every large feature on the roadmap hangs off it staying true.
