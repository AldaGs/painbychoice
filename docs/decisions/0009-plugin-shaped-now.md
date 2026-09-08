# 0009. Plugin-shaped now, stable SDK later

- **Status:** Accepted — partially implemented
- **Recorded:** 2026-09-08, extracted verbatim from the README design journal.

## Context

"Expose an API from the beginning" and "publish a frozen ABI on an unstable
core" are not the same promise, and only the second one is a trap.

## Decision

"Expose from the beginning" ≠ "publish a frozen ABI on an unstable core."
- **Now (cheap, good regardless):** make effects, generator nodes, importers,
  exporters trait objects behind registries; dogfood our built-ins through the
  same seams. A third-party plugin is then "another registered impl."
- **Later:** the third-party boundary is **WASM via `wasmtime`** (sandboxed,
  hot-reloadable, language-agnostic) for logic/generator/expression plugins;
  C-ABI (`abi_stable`) only if a per-pixel effect needs native speed.
- Ship the stable SDK when the node/expression IR settles (Roadmap #5), not
  before. Promise "plugin-ready architecture" now, "stable plugin SDK" later.

## Consequences

`core/src/registry.rs` exists and is explicit that a built-in and a plugin
register descriptors through the same call. The full extensibility design — the
four contribution kinds, the manifest, why panels get a declarative spec and the
op layer rather than egui and `&mut Document` — is §4 of
[`../production-plan.md`](../production-plan.md).
