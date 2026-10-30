# 0022. Floating windows share the main window's frame

- **Status:** Accepted — phase 1 implemented (content panels only)
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
- Shortcuts (undo, delete, copy/paste) read the main context's input, so they
  don't fire while a floating window has focus. Fixing that means merging key
  events from every context; it was left out of phase 1.
- Positions are physical pixels on the virtual desktop. Wayland ignores them,
  so the OS places the window there.
- A second canvas (a full-screen preview on another monitor) needs a second
  vello target and is phase 2.
