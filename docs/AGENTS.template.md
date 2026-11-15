# Editing this PBC project

The `.pbc` files here are Pain By Choice projects. Edit them with the `motion` CLI; every command prints one JSON value and exits non-zero on error.

The loop:

1. `motion describe` lists what the engine can do (edit ops, effects, nodes, caption presets). `motion describe <id>` shows one with an example.
2. `motion query <file>.pbc [--words]` gives a compact summary: comps, layers with timing (comp frames), buses, outputs, subtitles.
3. Edit: `motion apply <file>.pbc ops.json` runs a batch of ops atomically, the cheap path. Writing the file yourself is also fine; read it first and write `rev` + 1.
4. `motion check <file>.pbc` validates.
5. `motion preview <file>.pbc` writes a contact sheet PNG and an audio report (LUFS and peak per bus).

If the editor has the file open, it reloads your write as one undo step and never overwrites it silently. Ids are stable: address layers by `id`, never by position.
`motion schema` is the full authoring reference.
