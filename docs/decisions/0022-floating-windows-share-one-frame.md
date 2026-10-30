# 0022. Floating windows share the main window's frame

- **Status:** Accepted — implemented (phase 1: floating panels; phase 2: preview window)
- **Recorded:** 2026-10-30

## Context

Motion work wants two monitors: the canvas on one, the timeline and properties
on the other. The layout was a single `Dock` tree in one OS window (ADR 0015).
egui supports several windows, but its multi-viewport support lives in eframe,
and this shell drives winit and wgpu itself.

## Decision

A content area can be **popped out** into its own OS window. Each floating
window owns its own window, wgpu surface, egui context and egui renderer, and
holds a `Dock` of content editors. It owns no editor logic: the main window's
`render` runs every window's egui pass in turn with one shared leaf-drawing
closure, then applies edits once. Events for a floating window go to its egui
state and request a redraw of the main window.

Phase 1 limits: the canvas, comp bar and transport stay in the main window
(`Dock::is_floatable`). Closing a floating window docks its panels back beside
the canvas instead of discarding them. The open windows (tree, position, size)
are saved in the `.pbc` layout.

## Consequences

- One snapshot, one apply phase, one undo record per frame, however many
  windows are open. No cross-window synchronisation exists, so none can drift.
- Floating windows repaint at the main window's rate, which is already the
  rate the document changes.
- Shortcuts work from any window. The egui-read ones (undo, delete,
  copy/paste, file) check every window's context; only the focused one sees a
  key, so a shortcut fires once. The bare winit keys share `App::plain_key`.
- Positions are physical pixels on the virtual desktop. Wayland ignores them,
  so the OS places the window there.

## Phase 2: the preview window

A second canvas, for a client monitor or a projector: Layout ▸ Preview window
opens the composition alone in its own window. It is the *export* picture
(`Chrome::none`, the comp's own camera, no zoom, pan or orbit) fitted to that
window, not a mirror of the editor's canvas. It is one window with a vello
target, built and presented by the main frame like a floating window
(`App::paint_preview`), so it can never show a different frame than the editor.
F or F11 toggles borderless fullscreen on the monitor it's on; Escape there
leaves fullscreen and never quits. Other bare keys (Space, arrows) work from
it. Its position, size and fullscreen state are saved in the layout.

Costs: while it's open, every frame builds a second vello scene, and blurred
layers and motion blur are rasterized a second time at the preview's size. The
canvas's evaluation is reused unless the canvas is orbited.
