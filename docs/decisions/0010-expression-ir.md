# 0010. Expressions and the node graph lower to one IR

- **Status:** Accepted — implemented
- **Recorded:** 2026-09-08, extracted verbatim from the README design journal.

## Context

Two front-ends (a text expression and a node canvas) over one evaluator, or two
evaluators that will drift apart.

## Decision

`Value::Expr` is another `Value<T>` recipe; `evaluate` runs it instead of
sampling keyframes. Expressions and the node graph are two front-ends that lower
to the **same IR** (the EBN IR + dumb-printer discipline).
> **Status:** the core of this is now built — see *Expressions* in [`../architecture.md`](../architecture.md) and
> `core/src/expr.rs`. The bullets below record the reasoning; ✅ marks what's
> implemented, and what's still ahead (Rhai, `wiggle`, stroke/shape refs).

- **Signature ripple:** ✅ `resolve(&self, t)` → `resolve(&self, ctx: &mut
  EvalCtx)` carrying `{ frame, doc, cache, warnings }`. A single-`t` "bake first"
  pre-pass can't work because `valueAtTime(t')` samples at *other* times, which
  is the whole reason the context — not a bare frame — is threaded.
- **Dynamic↔typed boundary:** ✅ `ExprValue { Num, Vec2, Color, Str }` + `FromExpr` /
  `ToExpr`, implemented only for scriptable `T` (not `BezPath` — enforced by the
  trait bound). Mismatch → `fallback()`.
- **Dependency graph is implicit** in pull-based DFS (a dependency resolves
  before its dependent because you recurse into it first — no separate topo
  sort). ✅ `ResolveCache`: a `visiting` set for cycle detection (a cycle →
  `fallback` + a `scene.warnings` entry, reusing the provenance channel) and a
  `(node, prop, frame)` memo (the frame in the key matters — off-time samples
  must not poison the primary value).
- **Determinism:** ✅ expressions are pure functions of (frame, inputs) — no IO,
  no wall clock. `wiggle()` (seeded from node, prop, frame) is still to come with
  the script engine. This is the *same sandbox* WASM plugins need — build it once.
- Engine: start with **Rhai** (pure-Rust, easy, safe) — *not yet wired*; the IR
  and evaluator are in place for it to lower into. Swap behind the IR later if
  AE-JS compatibility (`boa`/v8) or Lua (`mlua`) is wanted.
