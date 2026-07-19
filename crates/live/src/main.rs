//! pbc — the live GPU shell with an egui overlay.
//!
//! Every frame: read the wall clock, compute a looped time `t`, call
//! `motion_core::evaluate(doc, t)`, rasterize the resulting `Scene` with vello,
//! then draw an egui transport bar (play/pause, restart, and a scrubbable
//! playhead) on top. Dragging the playhead just seeks — i.e. evaluates at a
//! different `t` — which is the whole non-linear model made interactive.
//!
//! Rendering order per frame:
//!   1. vello renders the scene into its offscreen target texture,
//!   2. we blit that target onto the swapchain surface,
//!   3. egui renders the UI on top (LoadOp::Load, so it composites over).
//!
//! The engine (`motion-core`) has no idea any of this exists.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Instant;

use kurbo::{Affine, Point, Shape as _, Stroke as KurboStroke, Vec2};
use motion_core::{
    demo::demo_document, evaluate, node::ParamValue, Color as MColor, Document, EvalCtx, Expr,
    ExprKind, ExprValue, Handle, Keyframe, Node as MNode, NodeId, PropPath, Scene as MScene,
    Shape as MShape, Transform, Value,
};
use serde::{Deserialize, Serialize};
use vello::peniko::{Color, Fill};
use vello::util::{RenderContext, RenderSurface};
use vello::wgpu;
use vello::{AaConfig, AaSupport, Renderer, RendererOptions, Scene as VScene};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// Convert one core `Color` into a vello/peniko color, folding in an opacity.
fn to_peniko(c: MColor, opacity: f64) -> Color {
    Color::new([c.r as f32, c.g as f32, c.b as f32, (c.a * opacity) as f32])
}

/// Convert an evaluated engine `Scene` into a `vello::Scene`, prepending a
/// global transform that fits the composition into the window. The composition
/// bounds are drawn first (so the editable frame is visible), then the shapes,
/// then the selection outline on top.
fn to_vello(scene: &MScene, fit: Affine, comp: (f64, f64), selected: Option<NodeId>) -> VScene {
    let mut vs = VScene::new();

    // Composition frame: a slightly lighter fill plus a border, so the comp
    // bounds stand out from the letterbox and resolution changes are visible.
    let comp_rect = kurbo::Rect::new(0.0, 0.0, comp.0, comp.1);
    vs.fill(Fill::NonZero, fit, Color::new([0.14, 0.15, 0.18, 1.0]), None, &comp_rect);
    let scale = fit.as_coeffs()[0].abs().max(1e-6);
    vs.stroke(
        &KurboStroke::new(1.5 / scale),
        fit,
        Color::new([0.35, 0.37, 0.42, 1.0]),
        None,
        &comp_rect,
    );

    for item in &scene.items {
        let xf = fit * item.transform;
        if let Some(fill) = item.fill {
            vs.fill(Fill::NonZero, xf, to_peniko(fill, item.opacity), None, &item.path);
        }
        if let Some((color, width)) = item.stroke {
            vs.stroke(
                &KurboStroke::new(width),
                xf,
                to_peniko(color, item.opacity),
                None,
                &item.path,
            );
        }
    }
    // Selection outline on top of everything.
    if let Some(sel) = selected {
        if let Some(item) = scene.items.iter().find(|i| i.source == sel) {
            let xf = fit * item.transform;
            // Width is in the item's local space; keep it visible but modest.
            vs.stroke(
                &KurboStroke::new(4.0),
                xf,
                Color::new([1.0, 0.85, 0.2, 1.0]),
                None,
                &item.path,
            );
        }
    }
    vs
}

/// Pick the front-most scene item under a point given in physical pixels.
/// Returns the `NodeId` that produced it, or `None` for empty space.
fn pick(scene: &MScene, fit: Affine, px: (f64, f64)) -> Option<NodeId> {
    let comp_point = fit.inverse() * Point::new(px.0, px.1);
    // Iterate back-to-front: the last item drawn is on top.
    scene.items.iter().rev().find_map(|item| {
        let local = item.transform.inverse() * comp_point;
        if item.fill.is_some() && item.path.contains(local) {
            Some(item.source)
        } else {
            None
        }
    })
}

/// "Contain" fit into the canvas area: scale the doc uniformly to fit `area`
/// and center it there. `area` is in **physical pixels** — the canvas leaf's
/// rect from the layout tree, scaled by pixels-per-point.
///
/// This used to subtract hardcoded panel sizes from the window. It couldn't
/// survive dockable panels: the moment a splitter moves, constants and reality
/// disagree and the canvas drifts out from under the cursor (which also breaks
/// click-picking, since `pick` inverts this very transform).
fn fit_transform(doc: &Document, area: kurbo::Rect) -> Affine {
    let avail_w = area.width().max(1.0);
    let avail_h = area.height().max(1.0);
    let scale = (avail_w / doc.width).min(avail_h / doc.height);
    let dx = area.x0 + (avail_w - doc.width * scale) * 0.5;
    let dy = area.y0 + (avail_h - doc.height * scale) * 0.5;
    Affine::translate((dx, dy)) * Affine::scale(scale)
}

/// Default panel sizes, in logical points (egui space). These now seed the
/// layout tree rather than being read back by the canvas fit — see [`Dock`].
const TRANSPORT_H: f32 = 56.0;
const PROPS_W: f32 = 260.0;
const TREE_W: f32 = 190.0;
const COMP_H: f32 = 34.0;

/// Which editor an area of the layout shows.
///
/// The canvas is one of these even though vello draws it, not egui: it has to
/// occupy a leaf so the layout tree knows where the leftover space is. Its leaf
/// draws nothing and merely reports its rect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Editor {
    Canvas,
    Comp,
    Layers,
    Properties,
    Transport,
    Dopesheet,
    /// The expression/node-graph editor: builds a `Value::Expr` on the selected
    /// node's properties. Not in the default layout — summon it into any content
    /// area with the area-header picker.
    Graph,
}

/// The editors a user may freely place, split, and close in a dockable area.
///
/// Deliberately excludes the three structural leaves. **Canvas** is a single
/// vello target measured from its one leaf ([`App::canvas_rect`]) and must stay
/// the tree's innermost leaf — duplicating or losing it breaks both. **Comp**
/// and **Transport** are fixed chrome. So those three carry no area header:
/// they can't be swapped away, split, or closed, which is exactly what keeps
/// the canvas invariants intact while the content panels rearrange around them.
const SWAPPABLE: [Editor; 4] =
    [Editor::Layers, Editor::Properties, Editor::Dopesheet, Editor::Graph];

impl Editor {
    /// Human name shown in the area-header picker.
    fn label(self) -> &'static str {
        match self {
            Editor::Canvas => "Canvas",
            Editor::Comp => "Composition",
            Editor::Layers => "Layers",
            Editor::Properties => "Properties",
            Editor::Transport => "Transport",
            Editor::Dopesheet => "Dopesheet",
            Editor::Graph => "Graph",
        }
    }

    /// Whether this area gets a header (picker + split/close) — see [`SWAPPABLE`].
    fn is_swappable(self) -> bool {
        SWAPPABLE.contains(&self)
    }
}

/// Which edge of an area a split pins its first child to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum DockSide {
    Left,
    Right,
    Top,
    Bottom,
}

/// A step down the layout tree: into a split's `first` or `second` child. A
/// sequence of these names a leaf. Area-header clicks record a target leaf as a
/// path so the edit can be applied *after* the egui pass — restructuring the
/// tree mid-render would desync egui's live panels and their ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Branch {
    First,
    Second,
}

/// A pending change to the layout tree, produced by an area header during the
/// UI pass and applied once egui is done — the same defer-then-apply discipline
/// the rest of the panels use for their `*Edits`.
enum DockCmd {
    /// Show a different editor in the area at `path`.
    Retype { path: Vec<Branch>, editor: Editor },
    /// Split the area at `path` in two along `side`; the new area clones the
    /// editor. `size` is the first child's start size (half the area) in points.
    Split { path: Vec<Branch>, side: DockSide, size: f32 },
    /// Close the area at `path`; its sibling absorbs the freed space.
    Close { path: Vec<Branch> },
}

/// The panel layout: a binary tree of splits with editors at the leaves.
///
/// Borrowed from the EBN project's `layoutTree`. A split pins its `first` child
/// to `side` at `size` points and gives `second` everything left over, so the
/// nesting order *is* the outermost-to-innermost panel order — which is exactly
/// how egui's panels already compose, letting the whole tree render by
/// recursion into a plain `Ui`.
///
/// This shape is deliberately serialization-ready: `size` lives here rather than
/// only in egui's panel memory, so a future "save this layout" (per-project
/// layouts, roadmap #4) is a `serde` derive away and needs no new plumbing.
/// `Clone` is what lets a saved preset be re-applied without rebuilding it;
/// `serde` is what lets the active layout and user presets ride in the `.pbc`.
#[derive(Clone, Serialize, Deserialize)]
enum Dock {
    Leaf(Editor),
    Split {
        side: DockSide,
        /// Outer size of `first` along the split axis, in points. Written back
        /// from the real panel rect each frame so a splitter drag sticks.
        size: f32,
        /// Toolbars (comp bar, transport) are fixed; content panels resize.
        resizable: bool,
        first: Box<Dock>,
        second: Box<Dock>,
    },
}

impl Dock {
    fn split(side: DockSide, size: f32, resizable: bool, first: Editor, second: Dock) -> Dock {
        Dock::Split {
            side,
            size,
            resizable,
            first: Box::new(Dock::Leaf(first)),
            second: Box::new(second),
        }
    }

    /// The stock arrangement, reproducing the pre-dock fixed layout: comp bar on
    /// top, layers left, transport and dopesheet stacked at the bottom,
    /// properties right, canvas in what remains.
    ///
    /// The first of what will be several named layouts; keeping it a plain
    /// constructor (rather than the only possible tree) is what makes adding
    /// presets later a matter of writing more of these.
    fn default_layout() -> Dock {
        use DockSide::*;
        Dock::split(
            Top,
            COMP_H,
            false,
            Editor::Comp,
            Dock::split(
                Left,
                TREE_W,
                true,
                Editor::Layers,
                Dock::split(
                    Bottom,
                    TRANSPORT_H,
                    false,
                    Editor::Transport,
                    Dock::split(
                        Bottom,
                        DOPESHEET_H,
                        true,
                        Editor::Dopesheet,
                        Dock::split(Right, PROPS_W, true, Editor::Properties, Dock::Leaf(Editor::Canvas)),
                    ),
                ),
            ),
        )
    }

    /// Timeline-forward layout for keyframe-heavy work: the dopesheet spans the
    /// full width at the bottom and is given far more height than the stock
    /// arrangement, with layers and properties flanking a smaller canvas above.
    fn animation_layout() -> Dock {
        use DockSide::*;
        Dock::split(
            Top,
            COMP_H,
            false,
            Editor::Comp,
            Dock::split(
                Bottom,
                TRANSPORT_H,
                false,
                Editor::Transport,
                Dock::split(
                    Bottom,
                    320.0,
                    true,
                    Editor::Dopesheet,
                    Dock::split(
                        Left,
                        TREE_W,
                        true,
                        Editor::Layers,
                        Dock::split(Right, PROPS_W, true, Editor::Properties, Dock::Leaf(Editor::Canvas)),
                    ),
                ),
            ),
        )
    }

    /// Design layout: no dopesheet, so the canvas gets the whole middle for
    /// vector/layout work. The transport stays (you still scrub), and the
    /// dopesheet is one picker-click away on any content area if it's wanted.
    fn design_layout() -> Dock {
        use DockSide::*;
        Dock::split(
            Top,
            COMP_H,
            false,
            Editor::Comp,
            Dock::split(
                Bottom,
                TRANSPORT_H,
                false,
                Editor::Transport,
                Dock::split(
                    Left,
                    TREE_W,
                    true,
                    Editor::Layers,
                    Dock::split(Right, PROPS_W, true, Editor::Properties, Dock::Leaf(Editor::Canvas)),
                ),
            ),
        )
    }

    /// Borrow the subtree named by `path`. If the path outruns the tree (a stale
    /// edit against a since-changed layout) it stops at the deepest node reached,
    /// which the callers then no-op on.
    fn node_at_mut(&mut self, path: &[Branch]) -> &mut Dock {
        let mut cur = self;
        for &b in path {
            cur = match cur {
                Dock::Split { first, second, .. } => match b {
                    Branch::First => first.as_mut(),
                    Branch::Second => second.as_mut(),
                },
                Dock::Leaf(_) => return cur,
            };
        }
        cur
    }

    /// Apply a deferred layout edit. Each op is a local tree rewrite; every path
    /// is re-resolved here (never held across the UI pass), so a command that no
    /// longer matches the tree simply finds a leaf where it expected a split, or
    /// vice versa, and does nothing.
    fn apply(&mut self, cmd: DockCmd) {
        match cmd {
            DockCmd::Retype { path, editor } => {
                let leaf = self.node_at_mut(&path);
                if matches!(leaf, Dock::Leaf(_)) {
                    *leaf = Dock::Leaf(editor);
                }
            }
            DockCmd::Split { path, side, size } => {
                let leaf = self.node_at_mut(&path);
                if let Dock::Leaf(e) = leaf {
                    let e = *e;
                    // Both halves start on the cloned editor; the picker then
                    // lets the user retype either. New splits are always
                    // resizable — only the stock toolbars are fixed.
                    *leaf = Dock::Split {
                        side,
                        size: size.max(48.0),
                        resizable: true,
                        first: Box::new(Dock::Leaf(e)),
                        second: Box::new(Dock::Leaf(e)),
                    };
                }
            }
            DockCmd::Close { path } => {
                // Replace the parent split with the *kept* sibling. The closed
                // leaf is always a content leaf (the canvas has no close button),
                // so the canvas — living in the sibling or an untouched ancestor
                // branch — always survives.
                let Some((&last, parent_path)) = path.split_last() else {
                    return; // the root has no sibling to fall back to.
                };
                let parent = self.node_at_mut(parent_path);
                if let Dock::Split { .. } = parent {
                    let old = std::mem::replace(parent, Dock::Leaf(Editor::Canvas));
                    if let Dock::Split { first, second, .. } = old {
                        *parent = match last {
                            Branch::First => *second,
                            Branch::Second => *first,
                        };
                    }
                }
            }
        }
    }

    /// Whether this tree is safe to drive the UI. A layout loaded from a `.pbc`
    /// (which may have been hand-edited or written by a newer/older build) has
    /// to hold the same guarantees the code paths lean on, or it's discarded for
    /// the default: exactly one canvas — the single vello target and the tree's
    /// innermost leaf — plus the two headerless toolbars, which no picker can
    /// re-add if a bad layout dropped them.
    fn is_valid(&self) -> bool {
        fn tally(d: &Dock, canvas: &mut u32, comp: &mut u32, transport: &mut u32) {
            match d {
                Dock::Leaf(Editor::Canvas) => *canvas += 1,
                Dock::Leaf(Editor::Comp) => *comp += 1,
                Dock::Leaf(Editor::Transport) => *transport += 1,
                Dock::Leaf(_) => {}
                Dock::Split { first, second, .. } => {
                    tally(first, canvas, comp, transport);
                    tally(second, canvas, comp, transport);
                }
            }
        }
        let (mut canvas, mut comp, mut transport) = (0, 0, 0);
        tally(self, &mut canvas, &mut comp, &mut transport);
        let mut cur = self;
        let innermost_is_canvas = loop {
            match cur {
                Dock::Split { second, .. } => cur = second,
                Dock::Leaf(e) => break *e == Editor::Canvas,
            }
        };
        canvas == 1 && comp == 1 && transport == 1 && innermost_is_canvas
    }
}

/// A named layout the user can switch to. Built-ins ship with the app and can't
/// be renamed or removed; user presets are made by "Save current" and are saved
/// into the `.pbc`. Only user presets are serialized — `builtin` is skipped and
/// so defaults to `false` on load, which is what every loaded preset is.
#[derive(Clone, Serialize, Deserialize)]
struct Preset {
    name: String,
    dock: Dock,
    #[serde(skip)]
    builtin: bool,
}

/// The layouts offered out of the box. `Default` reproduces the fixed pre-dock
/// arrangement; `Animation` and `Design` re-weight the same panels for two
/// common modes of work. Adding another is one more entry here plus a `Dock`
/// constructor — which is exactly the extensibility the tree was built for.
fn builtin_presets() -> Vec<Preset> {
    [
        ("Default", Dock::default_layout as fn() -> Dock),
        ("Animation", Dock::animation_layout),
        ("Design", Dock::design_layout),
    ]
    .into_iter()
    .map(|(name, make)| Preset { name: name.to_string(), dock: make(), builtin: true })
    .collect()
}

/// The on-disk `.pbc` bundle: the headless document plus this shell's UI layout.
/// The layout lives here in `live/`, never in `core::Document` — the engine
/// stays UI-agnostic (the whole point of the crate split). Files written before
/// layouts were saved are a *bare* `Document`; [`App::load`] falls back to that
/// and defaults the layout, so old projects keep opening.
#[derive(Serialize, Deserialize)]
struct Project {
    document: Document,
    #[serde(default)]
    layout: LayoutState,
}

/// The persisted UI layout: the active arrangement and any user-made presets.
/// Built-in presets are code, not data, so they're never stored.
#[derive(Default, Serialize, Deserialize)]
struct LayoutState {
    /// The active layout. Absent → fall back to [`Dock::default_layout`].
    #[serde(default)]
    dock: Option<Dock>,
    /// Presets saved via the Layout menu (built-ins omitted).
    #[serde(default)]
    user_presets: Vec<Preset>,
}

/// Render a layout tree into `ui`, calling `draw` for each leaf.
///
/// `next_id` just hands every panel a distinct egui id; egui keys its persistent
/// panel state (including the size a user dragged a splitter to) off that, so
/// the ids must stay stable frame to frame — which they are, since the walk
/// order is the tree's own structure.
///
/// `path` tracks the current leaf's address as the walk descends, and `cmd`
/// collects at most one area-header interaction (split/join/retype) to be
/// applied after the pass — the tree must not be restructured while its panels
/// are still being laid out this frame.
fn show_dock(
    node: &mut Dock,
    ui: &mut egui::Ui,
    next_id: &mut u32,
    path: &mut Vec<Branch>,
    draw: &mut dyn FnMut(Editor, &mut egui::Ui),
    cmd: &mut Option<DockCmd>,
) {
    match node {
        Dock::Leaf(editor) => {
            // Content areas get a header (picker + split/close); the three
            // structural leaves (canvas, comp, transport) don't — see `SWAPPABLE`.
            if editor.is_swappable() {
                area_header(ui, *editor, path, cmd);
            }
            draw(*editor, ui);
        }
        Dock::Split { side, size, resizable, first, second } => {
            let id = egui::Id::new(("dock", *next_id));
            *next_id += 1;
            let panel = match side {
                DockSide::Left => egui::Panel::left(id),
                DockSide::Right => egui::Panel::right(id),
                DockSide::Top => egui::Panel::top(id),
                DockSide::Bottom => egui::Panel::bottom(id),
            };
            let panel = if *resizable {
                panel.resizable(true).default_size(*size).min_size(48.0)
            } else {
                panel.exact_size(*size)
            };
            let resp = panel.show(ui, |ui| {
                path.push(Branch::First);
                show_dock(first, ui, next_id, path, draw, cmd);
                path.pop();
            });
            // Read the size back so the tree — not egui's private panel memory —
            // stays the source of truth for what the layout currently is.
            let r = resp.response.rect;
            *size = match side {
                DockSide::Left | DockSide::Right => r.width(),
                DockSide::Top | DockSide::Bottom => r.height(),
            };
            // The remaining space is the second child's area. Recursing here
            // (rather than into a sibling panel) is what makes the nesting
            // depth-first and the geometry exact.
            path.push(Branch::Second);
            show_dock(second, ui, next_id, path, draw, cmd);
            path.pop();
        }
    }
}

/// The control strip atop a dockable area: an editor picker plus split and
/// close buttons. It never mutates the tree (egui is mid-layout); a click just
/// records a [`DockCmd`] against this leaf's `path`, applied once the frame's
/// UI is done. At most one command survives a frame, which is all a click can
/// produce anyway.
fn area_header(ui: &mut egui::Ui, editor: Editor, path: &[Branch], cmd: &mut Option<DockCmd>) {
    // Measured before any content is drawn, so a split starts at half the area.
    let area = ui.max_rect();
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("area", path))
            .selected_text(editor.label())
            .show_ui(ui, |ui| {
                for e in SWAPPABLE {
                    if ui.selectable_label(e == editor, e.label()).clicked() && e != editor {
                        *cmd = Some(DockCmd::Retype { path: path.to_vec(), editor: e });
                    }
                }
            });
        // Split/close sit at the right edge. Plain ASCII glyphs on purpose — the
        // egui default font tofus most box-drawing characters (see README).
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("x").on_hover_text("Close this area").clicked() {
                *cmd = Some(DockCmd::Close { path: path.to_vec() });
            }
            if ui.small_button("-").on_hover_text("Split top / bottom").clicked() {
                *cmd = Some(DockCmd::Split {
                    path: path.to_vec(),
                    side: DockSide::Top,
                    size: area.height() * 0.5,
                });
            }
            if ui.small_button("|").on_hover_text("Split left / right").clicked() {
                *cmd = Some(DockCmd::Split {
                    path: path.to_vec(),
                    side: DockSide::Left,
                    size: area.width() * 0.5,
                });
            }
        });
    });
    ui.separator();
}

/// Composition-settings edits from the top bar. Any `Some` is a new value.
#[derive(Default)]
struct CompEdits {
    width: Option<f64>,
    height: Option<f64>,
    fps: Option<f64>,
    duration: Option<f64>,
}

/// Layout-preset intent from the top bar's Layout menu. At most one per frame.
#[derive(Default)]
struct LayoutEdits {
    /// Index into the preset list to switch the whole layout to.
    apply: Option<usize>,
    /// Save the current layout as a user preset under this (trimmed) name.
    save_as: Option<String>,
}

/// Top composition bar: editable resolution, fps, and duration, plus the Layout
/// menu (switch preset / save current). These drive the canvas fit, the
/// playback clock, the frame step, and the timeline mapping — so editing them
/// here reshapes the whole comp. Reports edits into `out` / `layout`.
#[allow(clippy::too_many_arguments)]
fn comp_ui(
    ui: &mut egui::Ui,
    width: f64,
    height: f64,
    fps: f64,
    duration: f64,
    out: &mut CompEdits,
    presets: &[String],
    name_buf: &mut String,
    layout: &mut LayoutEdits,
    warnings: &[(u64, String)],
) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.strong("Composition");
        // A broken script or an ambiguous name resolves to a neutral value, so
        // the frame looks deliberate. Say so here rather than only on stderr.
        if !warnings.is_empty() {
            let summary = warnings
                .iter()
                .map(|(id, msg)| format!("node {id}: {msg}"))
                .collect::<Vec<_>>()
                .join("
");
            ui.colored_label(
                egui::Color32::from_rgb(220, 160, 60),
                format!("! {}", warnings.len()),
            )
            .on_hover_text(summary);
        }
        ui.separator();

        ui.label("Size");
        let mut w = width;
        if ui.add(egui::DragValue::new(&mut w).speed(1.0).range(1.0..=16384.0)).changed() {
            out.width = Some(w);
        }
        ui.label("×");
        let mut h = height;
        if ui.add(egui::DragValue::new(&mut h).speed(1.0).range(1.0..=16384.0)).changed() {
            out.height = Some(h);
        }
        ui.separator();

        ui.label("FPS");
        let mut f = fps;
        if ui.add(egui::DragValue::new(&mut f).speed(0.5).range(1.0..=240.0)).changed() {
            out.fps = Some(f);
        }
        ui.separator();

        ui.label("Duration");
        let mut dur = duration;
        if ui
            .add(egui::DragValue::new(&mut dur).speed(0.1).range(0.1..=3600.0).suffix(" s"))
            .changed()
        {
            out.duration = Some(dur);
        }
        ui.separator();

        // Layout presets. A menu keeps the bar tidy: pick a preset to apply it,
        // or name and save the current arrangement as a session preset.
        ui.menu_button("Layout", |ui| {
            for (i, name) in presets.iter().enumerate() {
                if ui.button(name).clicked() {
                    layout.apply = Some(i);
                    ui.close();
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(name_buf)
                        .hint_text("preset name")
                        .desired_width(120.0),
                );
                let named = !name_buf.trim().is_empty();
                if ui.add_enabled(named, egui::Button::new("Save current")).clicked() {
                    layout.save_as = Some(name_buf.trim().to_string());
                    name_buf.clear();
                    ui.close();
                }
            });
        });
    });
}

/// What the transport UI reports back after a frame's interaction.
#[derive(Default)]
struct Transport {
    toggle: bool,
    restart: bool,
    /// Frame to scrub to. Snapped by the slider's integer step.
    scrub_to: Option<i64>,
}

/// A snapshot of the selected node's resolved properties at the current time,
/// gathered before the egui closure so the UI never borrows `App`. The `*_anim`
/// flags mark properties backed by a keyframe track (edits auto-key those).
struct NodeInfo {
    name: String,
    id: u64,
    pos: (f64, f64),
    rot: f64,
    scale: (f64, f64),
    opacity: f64,
    fill: Option<[f32; 3]>,
    /// Parametric geometry, `None` for a group or a hand-drawn `Path`.
    size: Option<(f64, f64)>,
    /// Corner radius — `Some` only for a Rect.
    radius: Option<f64>,
    /// Stroke color + width, `None` when the node has no stroke.
    stroke: Option<([f32; 3], f64)>,
    pos_anim: bool,
    rot_anim: bool,
    scale_anim: bool,
    opacity_anim: bool,
    fill_anim: bool,
    size_anim: bool,
    radius_anim: bool,
    stroke_color_anim: bool,
    stroke_width_anim: bool,
}

/// egui's color buttons speak `[f32; 3]`; the document speaks `Color`.
fn rgb_color(rgb: [f32; 3]) -> MColor {
    MColor::rgb(rgb[0] as f64, rgb[1] as f64, rgb[2] as f64)
}

/// Whether `kind` exists on this node *and* is keyframed.
fn is_anim(node: &MNode, kind: PropKind) -> bool {
    prop_of(node, kind).is_some_and(|p| p.is_animated())
}

impl NodeInfo {
    fn resolve(node: &motion_core::Node, doc: &Document, t: f64) -> Self {
        let mut ctx = EvalCtx::new(doc, t);
        // Mark the node, as `evaluate`'s walk does: a `param("x")` with no
        // explicit owner reads this node's knobs, so the panel would otherwise
        // show a fallback where the canvas shows the real value.
        ctx.in_node(node.id, |ctx| Self::resolve_in(node, ctx))
    }

    fn resolve_in(node: &motion_core::Node, ctx: &mut EvalCtx) -> Self {
        let tr = &node.transform;
        let pos = tr.position.resolve(ctx);
        let scale = tr.scale.resolve(ctx);
        NodeInfo {
            name: node.name.clone(),
            id: node.id.0,
            pos: (pos.x, pos.y),
            rot: tr.rotation_deg.resolve(ctx),
            scale: (scale.x, scale.y),
            opacity: tr.opacity.resolve(ctx),
            fill: node.fill.as_ref().map(|f| {
                let c = f.resolve(ctx);
                [c.r as f32, c.g as f32, c.b as f32]
            }),
            size: match node.shape.as_ref() {
                Some(MShape::Rect { size, .. }) | Some(MShape::Ellipse { size }) => {
                    let s = size.resolve(ctx);
                    Some((s.x, s.y))
                }
                _ => None,
            },
            radius: match node.shape.as_ref() {
                Some(MShape::Rect { radius, .. }) => Some(radius.resolve(ctx)),
                _ => None,
            },
            stroke: node.stroke.as_ref().map(|s| {
                let c = s.color.resolve(ctx);
                ([c.r as f32, c.g as f32, c.b as f32], s.width.resolve(ctx))
            }),
            pos_anim: tr.position.is_animated(),
            rot_anim: tr.rotation_deg.is_animated(),
            scale_anim: tr.scale.is_animated(),
            opacity_anim: tr.opacity.is_animated(),
            // Whether each optional property is animated. `prop_of` already
            // encodes "does this node even have it", so ask it rather than
            // re-deriving the shape/stroke cases here and risking disagreement.
            fill_anim: is_anim(node, PropKind::Fill),
            size_anim: is_anim(node, PropKind::ShapeSize),
            radius_anim: is_anim(node, PropKind::ShapeRadius),
            stroke_color_anim: is_anim(node, PropKind::StrokeColor),
            stroke_width_anim: is_anim(node, PropKind::StrokeWidth),
        }
    }
}

/// Edits collected from the properties panel this frame. Any `Some` field is a
/// new value the user dialed in; `None` means untouched.
#[derive(Default)]
struct PropEdits {
    pos_x: Option<f64>,
    pos_y: Option<f64>,
    rot: Option<f64>,
    scale_x: Option<f64>,
    scale_y: Option<f64>,
    opacity: Option<f64>,
    fill: Option<[f32; 3]>,
    size_x: Option<f64>,
    size_y: Option<f64>,
    radius: Option<f64>,
    stroke_color: Option<[f32; 3]>,
    stroke_width: Option<f64>,
    /// Add a default stroke to a node that has none / drop the one it has.
    add_stroke: bool,
    remove_stroke: bool,
    // Insert-keyframe-at-playhead requests (the "stopwatch"). Keyed by
    // `PropKind` rather than one bool per property, so adding an animatable
    // property doesn't grow this struct.
    key: KeySelectionKinds,
}

/// The set of properties whose stopwatch was clicked this frame.
type KeySelectionKinds = std::collections::BTreeSet<PropKind>;

/// A "stopwatch" toggle: a filled dot when the property is animated, a hollow
/// ring when constant. Clicking it inserts a keyframe at the playhead
/// (promoting a constant to a track). The indicator is *painted* rather than
/// drawn from a glyph, since the circle/diamond glyphs are missing from egui's
/// default font and render as tofu boxes.
fn key_button(ui: &mut egui::Ui, animated: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
    let c = rect.center();
    let painter = ui.painter();
    if animated {
        painter.circle_filled(c, 4.0, egui::Color32::from_rgb(255, 216, 51));
    } else {
        let col = if resp.hovered() {
            egui::Color32::from_gray(200)
        } else {
            egui::Color32::from_gray(120)
        };
        painter.circle_stroke(c, 4.0, egui::Stroke::new(1.5, col));
    }
    resp.on_hover_text("Insert a keyframe at the playhead").clicked()
}

/// The two normalized cubic-bezier control points of the selected keyframe's
/// outgoing timing segment (`cubic-bezier(p1, p2)` with endpoints 0,0 and 1,1).
struct EaseInfo {
    p1: (f32, f32),
    p2: (f32, f32),
}

/// A CSS-style cubic-bezier editor. Draws the timing curve in a unit square and
/// lets the two control points be dragged. New handles are reported in `out`.
fn ease_editor(ui: &mut egui::Ui, ease: &EaseInfo, out: &mut Option<((f32, f32), (f32, f32))>) {
    let sz = (ui.available_width() - 8.0).clamp(80.0, 180.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(sz, sz), egui::Sense::hover());
    let painter = ui.painter_at(rect);

    // value (x right, y up) in [0,1] → screen (y is down).
    let map = |v: (f32, f32)| {
        egui::pos2(rect.left() + v.0 * rect.width(), rect.bottom() - v.1 * rect.height())
    };
    let unmap = |p: egui::Pos2| {
        (
            ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
            ((rect.bottom() - p.y) / rect.height()).clamp(0.0, 1.0),
        )
    };

    painter.rect_filled(rect, 3.0, egui::Color32::from_gray(28));
    // Reference diagonal (linear).
    painter.line_segment(
        [map((0.0, 0.0)), map((1.0, 1.0))],
        egui::Stroke::new(1.0, egui::Color32::from_gray(60)),
    );

    // Drag the control points first, so the curve draws with fresh values.
    let mut p1 = ease.p1;
    let mut p2 = ease.p2;
    let mut changed = false;
    for (i, hp) in [&mut p1, &mut p2].into_iter().enumerate() {
        let sp = map(*hp);
        let hit = egui::Rect::from_center_size(sp, egui::vec2(16.0, 16.0));
        let resp = ui.interact(hit, ui.id().with(("ease_handle", i)), egui::Sense::drag());
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                *hp = unmap(p);
                changed = true;
            }
        }
    }
    if changed {
        *out = Some((p1, p2));
    }

    // Handle guide lines.
    let accent = egui::Color32::from_rgb(255, 216, 51);
    painter.line_segment([map((0.0, 0.0)), map(p1)], egui::Stroke::new(1.0, accent));
    painter.line_segment([map((1.0, 1.0)), map(p2)], egui::Stroke::new(1.0, accent));

    // The timing curve itself.
    let bez = |a: f32, b: f32, s: f32| {
        let mt = 1.0 - s;
        3.0 * mt * mt * s * a + 3.0 * mt * s * s * b + s * s * s
    };
    let curve: Vec<egui::Pos2> = (0..=48)
        .map(|i| {
            let s = i as f32 / 48.0;
            map((bez(p1.0, p2.0, s), bez(p1.1, p2.1, s)))
        })
        .collect();
    painter.add(egui::Shape::line(curve, egui::Stroke::new(2.0, egui::Color32::WHITE)));

    // Control-point knobs.
    for hp in [p1, p2] {
        painter.circle_filled(map(hp), 4.0, accent);
    }
}

/// Right-hand properties panel. Reads a resolved `NodeInfo` and writes any user
/// changes into `edits`; it never touches `App`. `ease` is the selected key's
/// segment (if any) and edits go to `ease_out`.
fn properties_ui(
    ui: &mut egui::Ui,
    info: &Option<NodeInfo>,
    edits: &mut PropEdits,
    ease: &Option<EaseInfo>,
    ease_out: &mut Option<((f32, f32), (f32, f32))>,
) {
    ui.add_space(8.0);
    ui.heading("Properties");
    ui.separator();
    let Some(n) = info else {
        ui.add_space(8.0);
        ui.weak("Click a shape on the canvas to select it.");
        return;
    };

    egui::Grid::new("props").num_columns(3).striped(true).show(ui, |ui| {
        ui.label("Name");
        ui.strong(&n.name);
        ui.label("");
        ui.end_row();

        ui.label("Node id");
        ui.monospace(n.id.to_string());
        ui.label("");
        ui.end_row();

        // Position (x, y). DragValue gives both interactions: drag to
        // nudge, or click to type a value and commit with Enter.
        ui.label("Position");
        ui.horizontal(|ui| {
            let mut x = n.pos.0;
            let mut y = n.pos.1;
            if ui.add(egui::DragValue::new(&mut x).speed(0.5)).changed() {
                edits.pos_x = Some(x);
            }
            if ui.add(egui::DragValue::new(&mut y).speed(0.5)).changed() {
                edits.pos_y = Some(y);
            }
        });
        if key_button(ui, n.pos_anim) {
            edits.key.insert(PropKind::Position);
        }
        ui.end_row();

        ui.label("Rotation");
        let mut rot = n.rot;
        if ui
            .add(egui::DragValue::new(&mut rot).speed(0.5).suffix("°"))
            .changed()
        {
            edits.rot = Some(rot);
        }
        if key_button(ui, n.rot_anim) {
            edits.key.insert(PropKind::Rotation);
        }
        ui.end_row();

        ui.label("Scale");
        ui.horizontal(|ui| {
            let mut sx = n.scale.0;
            let mut sy = n.scale.1;
            if ui.add(egui::DragValue::new(&mut sx).speed(0.01)).changed() {
                edits.scale_x = Some(sx);
            }
            if ui.add(egui::DragValue::new(&mut sy).speed(0.01)).changed() {
                edits.scale_y = Some(sy);
            }
        });
        if key_button(ui, n.scale_anim) {
            edits.key.insert(PropKind::Scale);
        }
        ui.end_row();

        ui.label("Opacity");
        let mut op = n.opacity;
        if ui
            .add(egui::Slider::new(&mut op, 0.0..=1.0).show_value(false))
            .changed()
        {
            edits.opacity = Some(op);
        }
        if key_button(ui, n.opacity_anim) {
            edits.key.insert(PropKind::Opacity);
        }
        ui.end_row();

        ui.label("Fill");
        if let Some(mut rgb) = n.fill {
            if ui.color_edit_button_rgb(&mut rgb).changed() {
                edits.fill = Some(rgb);
            }
            if key_button(ui, n.fill_anim) {
                edits.key.insert(PropKind::Fill);
            }
        } else {
            ui.weak("none");
            ui.label("");
        }
        ui.end_row();

        // --- Stroke. Optional, so the row doubles as its add/remove
        // control: a node without one gets a "+ add" button rather
        // than disabled widgets. ---
        ui.label("Stroke");
        if let Some((mut rgb, _)) = n.stroke {
            ui.horizontal(|ui| {
                if ui.color_edit_button_rgb(&mut rgb).changed() {
                    edits.stroke_color = Some(rgb);
                }
                if ui.small_button("✕").on_hover_text("Remove stroke").clicked() {
                    edits.remove_stroke = true;
                }
            });
            if key_button(ui, n.stroke_color_anim) {
                edits.key.insert(PropKind::StrokeColor);
            }
        } else {
            if ui.small_button("+ add").clicked() {
                edits.add_stroke = true;
            }
            ui.label("");
        }
        ui.end_row();

        if let Some((_, w)) = n.stroke {
            ui.label("Stroke W");
            let mut w = w;
            if ui
                .add(egui::DragValue::new(&mut w).speed(0.1).range(0.0..=f64::MAX))
                .changed()
            {
                edits.stroke_width = Some(w);
            }
            if key_button(ui, n.stroke_width_anim) {
                edits.key.insert(PropKind::StrokeWidth);
            }
            ui.end_row();
        }

        // --- Parametric geometry. Absent for groups and for imported
        // `Path` shapes, whose geometry isn't expressed as parameters. ---
        if let Some((w, h)) = n.size {
            ui.label("Size");
            ui.horizontal(|ui| {
                let (mut w, mut h) = (w, h);
                if ui
                    .add(egui::DragValue::new(&mut w).speed(0.5).range(0.0..=f64::MAX))
                    .changed()
                {
                    edits.size_x = Some(w);
                }
                if ui
                    .add(egui::DragValue::new(&mut h).speed(0.5).range(0.0..=f64::MAX))
                    .changed()
                {
                    edits.size_y = Some(h);
                }
            });
            if key_button(ui, n.size_anim) {
                edits.key.insert(PropKind::ShapeSize);
            }
            ui.end_row();
        }

        if let Some(r) = n.radius {
            ui.label("Radius");
            let mut r = r;
            if ui
                .add(egui::DragValue::new(&mut r).speed(0.5).range(0.0..=f64::MAX))
                .changed()
            {
                edits.radius = Some(r);
            }
            if key_button(ui, n.radius_anim) {
                edits.key.insert(PropKind::ShapeRadius);
            }
            ui.end_row();
        }
    });

    ui.add_space(6.0);
    ui.weak("Drag a field to nudge, or click to type; Enter commits.");
    ui.weak("The dot button inserts a keyframe at the playhead (hollow ring = start animating).");

    // Easing editor for the selected keyframe's outgoing segment.
    if let Some(e) = ease {
        ui.separator();
        ui.strong("Easing");
        ui.weak("Timing of the selected key's outgoing segment.");
        ui.horizontal(|ui| {
            if ui.small_button("Linear").clicked() {
                *ease_out = Some(((1.0 / 3.0, 1.0 / 3.0), (2.0 / 3.0, 2.0 / 3.0)));
            }
            if ui.small_button("Smooth").clicked() {
                *ease_out = Some(((0.42, 0.0), (0.58, 1.0)));
            }
            if ui.small_button("Ease In").clicked() {
                *ease_out = Some(((0.42, 0.0), (1.0, 1.0)));
            }
            if ui.small_button("Ease Out").clicked() {
                *ease_out = Some(((0.0, 0.0), (0.58, 1.0)));
            }
        });
        ease_editor(ui, e, ease_out);
    }
}

/// Build the bottom transport bar. Reads the current time / playing state and
/// writes user intent into `out`; it never touches `App` directly, so it can't
/// collide with the borrows in `render`.
fn transport_ui(
    ui: &mut egui::Ui,
    frame: i64,
    last_frame: i64,
    tb: motion_core::Timebase,
    playing: bool,
    out: &mut Transport,
) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        if ui.button(if playing { "Pause" } else { "Play" }).clicked() {
            out.toggle = true;
        }
        if ui.button("Restart").clicked() {
            out.restart = true;
        }
        // Frame-domain readout: hh:mm:ss.ff plus the raw frame number,
        // monospaced so the digits don't jitter during playback.
        ui.label(
            egui::RichText::new(format!(
                "{}  [{frame}/{last_frame}]",
                tb.timecode(frame as f64)
            ))
            .monospace(),
        );

        // Full-width playhead scrubber. An integer slider, so dragging
        // it can only produce whole frames — snapping for free.
        let mut val = frame.clamp(0, last_frame);
        ui.spacing_mut().slider_width = (ui.available_width() - 16.0).max(60.0);
        let resp = ui.add(
            egui::Slider::new(&mut val, 0..=last_frame.max(1))
                .show_value(false)
                .trailing_fill(true),
        );
        if resp.dragged() || resp.changed() {
            out.scrub_to = Some(val);
        }
    });
}

/// Which animated property a dopesheet row refers to. Lets the UI report a
/// keyframe drag back to `App` without knowing the property's value type.
///
/// Declaration order is meaningful twice over: it's the dopesheet's row order,
/// and — because `KeySelection` is a `BTreeSet` keyed on this — it's what makes
/// a selection's entries for one property contiguous (see
/// `group_selection_by_prop`). Transform first, then paint, then geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum PropKind {
    Position,
    Rotation,
    Scale,
    Opacity,
    Fill,
    StrokeColor,
    StrokeWidth,
    ShapeSize,
    ShapeRadius,
}

impl PropKind {
    /// Every property that can be animated, in row order.
    const ALL: [PropKind; 9] = [
        PropKind::Position,
        PropKind::Rotation,
        PropKind::Scale,
        PropKind::Opacity,
        PropKind::Fill,
        PropKind::StrokeColor,
        PropKind::StrokeWidth,
        PropKind::ShapeSize,
        PropKind::ShapeRadius,
    ];

    fn label(self) -> &'static str {
        match self {
            PropKind::Position => "Position",
            PropKind::Rotation => "Rotation",
            PropKind::Scale => "Scale",
            PropKind::Opacity => "Opacity",
            PropKind::Fill => "Fill",
            PropKind::StrokeColor => "Stroke",
            PropKind::StrokeWidth => "Stroke W",
            PropKind::ShapeSize => "Size",
            PropKind::ShapeRadius => "Radius",
        }
    }
}

/// A borrowed animatable property, with its value type erased down to the three
/// the document actually uses.
///
/// This exists so the keyframe machinery — dopesheet rows, retiming, delete,
/// copy/paste, easing — matches on `PropKind` in exactly *one* place
/// ([`prop_of`] / [`prop_of_mut`]) instead of once per operation. Adding a new
/// animatable property is then a `PropKind` variant plus two match arms, rather
/// than an edit to eight call sites that all have to agree.
enum PropRef<'a> {
    Vec2(&'a Value<Vec2>),
    Num(&'a Value<f64>),
    Color(&'a Value<MColor>),
}

enum PropRefMut<'a> {
    Vec2(&'a mut Value<Vec2>),
    Num(&'a mut Value<f64>),
    Color(&'a mut Value<MColor>),
}

/// Call the same method on whichever `Value<T>` a `PropRef`/`PropRefMut` holds.
/// The body is written once and monomorphized per arm, which is the whole point
/// — every op below is identical apart from `T`.
macro_rules! on_prop {
    ($p:expr, $v:ident => $body:expr) => {
        match $p {
            PropRef::Vec2($v) => $body,
            PropRef::Num($v) => $body,
            PropRef::Color($v) => $body,
        }
    };
}

macro_rules! on_prop_mut {
    ($p:expr, $v:ident => $body:expr) => {
        match $p {
            PropRefMut::Vec2($v) => $body,
            PropRefMut::Num($v) => $body,
            PropRefMut::Color($v) => $body,
        }
    };
}

impl PropRef<'_> {
    fn is_animated(&self) -> bool {
        on_prop!(self, v => v.is_animated())
    }
    fn is_expr(&self) -> bool {
        on_prop!(self, v => v.is_expr())
    }
    /// The expression tree, if this property is expression-driven.
    fn expr(&self) -> Option<&Expr> {
        on_prop!(self, v => v.expr_ref())
    }
    fn key_frames(&self) -> Vec<i64> {
        on_prop!(self, v => v.key_frames())
    }
    fn move_keys_limits(&self, idxs: &[usize]) -> Option<(i64, i64)> {
        on_prop!(self, v => v.move_keys_limits(idxs))
    }
    fn segment_handles(&self, index: usize) -> Option<(Handle, Handle)> {
        on_prop!(self, v => v.segment_handles(index))
    }
    /// Copy the keys at `idxs` onto the clipboard, tagged with their type.
    fn keys_at(&self, idxs: &[usize]) -> ClipTrack {
        match self {
            PropRef::Vec2(v) => ClipTrack::Vec2(v.keys_at(idxs)),
            PropRef::Num(v) => ClipTrack::Num(v.keys_at(idxs)),
            PropRef::Color(v) => ClipTrack::Color(v.keys_at(idxs)),
        }
    }
}

impl PropRefMut<'_> {
    fn move_keys(&mut self, idxs: &[usize], delta: i64) {
        on_prop_mut!(self, v => { v.move_keys(idxs, delta); })
    }
    /// Seed an expression from the current value (see [`Value::promote_to_expr`]).
    fn promote_to_expr(&mut self, ctx: &mut EvalCtx) {
        on_prop_mut!(self, v => v.promote_to_expr(ctx))
    }
    /// Freeze an expression back to a constant (see [`Value::bake_to_const`]).
    fn bake_to_const(&mut self, ctx: &mut EvalCtx) {
        on_prop_mut!(self, v => v.bake_to_const(ctx))
    }
    /// The expression tree mutably, for structured editing by path.
    fn expr_mut(&mut self) -> Option<&mut Expr> {
        on_prop_mut!(self, v => v.expr_mut())
    }
    fn remove_key(&mut self, index: usize) {
        on_prop_mut!(self, v => v.remove_key(index))
    }
    fn insert_key(&mut self, frame: i64) {
        on_prop_mut!(self, v => v.insert_key(frame))
    }
    fn set_segment_handles(&mut self, index: usize, out: Handle, next_in: Handle) {
        on_prop_mut!(self, v => v.set_segment_handles(index, out, next_in))
    }
    /// Paste a clipboard track, but only onto a property of the same type — a
    /// `Vec2` clip must never land on a scalar. Mismatches can't happen through
    /// the UI (a clip is tagged at copy time) so they're simply ignored.
    fn insert_keys(&mut self, clip: &ClipTrack, offset: i64) -> Vec<usize> {
        match (self, clip) {
            (PropRefMut::Vec2(v), ClipTrack::Vec2(k)) => v.insert_keys(k, offset),
            (PropRefMut::Num(v), ClipTrack::Num(k)) => v.insert_keys(k, offset),
            (PropRefMut::Color(v), ClipTrack::Color(k)) => v.insert_keys(k, offset),
            _ => Vec::new(),
        }
    }
}

/// Borrow one of a node's animatable properties. `None` when the node doesn't
/// have it at all — a group has no fill, an ellipse has no corner radius, and a
/// hand-drawn `Path` has no parametric size.
fn prop_of(node: &MNode, kind: PropKind) -> Option<PropRef<'_>> {
    let tr = &node.transform;
    Some(match kind {
        PropKind::Position => PropRef::Vec2(&tr.position),
        PropKind::Rotation => PropRef::Num(&tr.rotation_deg),
        PropKind::Scale => PropRef::Vec2(&tr.scale),
        PropKind::Opacity => PropRef::Num(&tr.opacity),
        PropKind::Fill => PropRef::Color(node.fill.as_ref()?),
        PropKind::StrokeColor => PropRef::Color(&node.stroke.as_ref()?.color),
        PropKind::StrokeWidth => PropRef::Num(&node.stroke.as_ref()?.width),
        PropKind::ShapeSize => match node.shape.as_ref()? {
            MShape::Rect { size, .. } | MShape::Ellipse { size } => PropRef::Vec2(size),
            MShape::Path(_) => return None,
        },
        PropKind::ShapeRadius => match node.shape.as_ref()? {
            MShape::Rect { radius, .. } => PropRef::Num(radius),
            _ => return None,
        },
    })
}

/// Mutable twin of [`prop_of`]. Kept adjacent on purpose: the two must agree on
/// which properties exist, and they're only correct read together.
fn prop_of_mut(node: &mut MNode, kind: PropKind) -> Option<PropRefMut<'_>> {
    let tr = &mut node.transform;
    Some(match kind {
        PropKind::Position => PropRefMut::Vec2(&mut tr.position),
        PropKind::Rotation => PropRefMut::Num(&mut tr.rotation_deg),
        PropKind::Scale => PropRefMut::Vec2(&mut tr.scale),
        PropKind::Opacity => PropRefMut::Num(&mut tr.opacity),
        PropKind::Fill => PropRefMut::Color(node.fill.as_mut()?),
        PropKind::StrokeColor => PropRefMut::Color(&mut node.stroke.as_mut()?.color),
        PropKind::StrokeWidth => PropRefMut::Num(&mut node.stroke.as_mut()?.width),
        PropKind::ShapeSize => match node.shape.as_mut()? {
            MShape::Rect { size, .. } | MShape::Ellipse { size } => PropRefMut::Vec2(size),
            MShape::Path(_) => return None,
        },
        PropKind::ShapeRadius => match node.shape.as_mut()? {
            MShape::Rect { radius, .. } => PropRefMut::Num(radius),
            _ => return None,
        },
    })
}

/// One dopesheet row: an animated property and the frames of its keyframes.
struct DopeRow {
    label: &'static str,
    kind: PropKind,
    frames: Vec<i64>,
}

/// Gather the animated properties of a node into dopesheet rows.
fn dope_rows(node: &motion_core::Node) -> Vec<DopeRow> {
    PropKind::ALL
        .iter()
        .filter_map(|&kind| {
            let p = prop_of(node, kind)?;
            p.is_animated().then(|| DopeRow {
                label: kind.label(),
                kind,
                frames: p.key_frames(),
            })
        })
        .collect()
}

/// A keyframe's identity within a node: which property, which index.
type KeyRef = (PropKind, usize);

/// The dopesheet's keyframe selection. A `BTreeSet` so iteration order is
/// deterministic and indices come out sorted — which the group-move code
/// below relies on when it batches a selection per property.
type KeySelection = std::collections::BTreeSet<KeyRef>;

/// Bucket a selection into one `(property, sorted indices)` entry per property.
///
/// Relies on `BTreeSet<(PropKind, usize)>` ordering by property first: entries
/// for the same property are therefore *contiguous*, so a single pass that
/// extends the last bucket is enough. If `PropKind`'s `Ord` ever stops being
/// the primary key this silently starts producing duplicate buckets — hence
/// the test.
fn group_selection_by_prop(sel: &KeySelection) -> Vec<(PropKind, Vec<usize>)> {
    let mut out: Vec<(PropKind, Vec<usize>)> = Vec::new();
    for &(kind, index) in sel.iter() {
        match out.last_mut() {
            Some((k, idxs)) if *k == kind => idxs.push(index),
            _ => out.push((kind, vec![index])),
        }
    }
    out
}

/// One property's worth of copied keyframes. Two variants because `Value<T>` is
/// generic and the transform's properties are either `Vec2` or `f64` — the enum
/// is the type-erasure boundary, so paste can only ever put `Vec2` keys back on
/// a `Vec2` property.
#[derive(Clone)]
enum ClipTrack {
    Vec2(Vec<Keyframe<Vec2>>),
    Num(Vec<Keyframe<f64>>),
    Color(Vec<Keyframe<MColor>>),
}

impl ClipTrack {
    /// Frame of the earliest copied key, or `None` if nothing was copied.
    fn first_frame(&self) -> Option<i64> {
        match self {
            ClipTrack::Vec2(k) => k.first().map(|k| k.frame),
            ClipTrack::Num(k) => k.first().map(|k| k.frame),
            ClipTrack::Color(k) => k.first().map(|k| k.frame),
        }
    }
}

/// Keyframes on the clipboard, with the frame they were copied from.
///
/// Storing `origin` (the earliest copied frame) rather than pre-baked offsets is
/// what makes paste land the *block* at the playhead with its internal spacing
/// intact, regardless of where in the timeline it was copied from.
#[derive(Clone)]
struct KeyClipboard {
    origin: i64,
    tracks: Vec<(PropKind, ClipTrack)>,
}

/// Read the outgoing-segment handles for a given property + keyframe index.
fn segment_handles_of(node: &MNode, kind: PropKind, index: usize) -> Option<(Handle, Handle)> {
    prop_of(node, kind)?.segment_handles(index)
}

/// What the dopesheet reports after a frame: seek, keyframe move, and/or a
/// change to which keyframe is selected.
#[derive(Default)]
struct DopeEdits {
    /// Frame to seek to. Already snapped to the grid.
    seek_to: Option<i64>,
    /// Drag delta in frames, applied to the whole selection as a rigid block.
    move_by: Option<i64>,
    /// A diamond was clicked → make it the selection.
    select_key: Option<KeyRef>,
    /// A diamond was ctrl/shift-clicked → add or remove it from the selection.
    toggle_key: Option<KeyRef>,
    /// Empty track was clicked → clear the keyframe selection.
    clear_selection: bool,
    /// A marquee is being dragged: every key inside it, this frame. Reported
    /// live (not on release) so the selection previews as the box is drawn.
    box_select: Option<KeySelection>,
    /// Zoom / pan produced a new visible window.
    set_view: Option<TimelineView>,
}

/// The visible frame window of the timeline. Zoom and pan only ever change
/// this; every frame↔pixel mapping reads it, so the ruler, the keyframes, and
/// the playhead cannot drift out of agreement.
#[derive(Clone, Copy, Debug)]
struct TimelineView {
    /// Leftmost visible frame (fractional — panning is continuous).
    start: f64,
    /// How many frames fit across the track.
    visible: f64,
}

impl TimelineView {
    fn full(last_frame: i64) -> Self {
        Self { start: 0.0, visible: last_frame.max(1) as f64 }
    }

    /// Keep the window inside `0..=last_frame` and never narrower than a few
    /// frames (past that the diamonds are wider than their spacing anyway).
    fn clamped(self, last_frame: i64) -> Self {
        let total = last_frame.max(1) as f64;
        let visible = self.visible.clamp(4.0, total);
        let start = self.start.clamp(0.0, (total - visible).max(0.0));
        Self { start, visible }
    }
}

/// Maps frames to pixels across one track's inset width. Built once from the
/// ruler's rect and shared by every row below it.
#[derive(Clone, Copy)]
struct Axis {
    x0: f32,
    span: f32,
    view: TimelineView,
}

impl Axis {
    fn new(track: egui::Rect, view: TimelineView) -> Self {
        // Inset so keys on the first/last visible frame sit fully inside the
        // track rather than clipped at the edge.
        const PAD: f32 = 8.0;
        let x0 = track.left() + PAD;
        let span = ((track.right() - PAD) - x0).max(1.0);
        Self { x0, span, view }
    }

    fn px_per_frame(&self) -> f32 {
        self.span / self.view.visible as f32
    }

    fn frame_to_x(&self, f: f64) -> f32 {
        self.x0 + ((f - self.view.start) as f32) * self.px_per_frame()
    }

    fn x_to_frame_exact(&self, x: f32) -> f64 {
        self.view.start + ((x - self.x0) / self.px_per_frame()) as f64
    }

    /// Snapped to the grid — this is where clicking and dragging become
    /// frame-exact, regardless of zoom.
    fn x_to_frame(&self, x: f32) -> i64 {
        self.x_to_frame_exact(x).round() as i64
    }
}

/// Choose a ruler tick interval (in frames) that leaves at least `min_px`
/// between labels. Candidates are the 1-2-5-10 frame steps plus whole-second
/// multiples, so labels land on round timecodes once you zoom out.
fn tick_step(px_per_frame: f32, fps: f64, min_px: f32) -> i64 {
    let f = fps.round().max(1.0) as i64;
    let mut cands: Vec<i64> = vec![1, 2, 5, 10];
    for secs in [1i64, 2, 5, 10, 15, 30, 60, 120, 300, 600, 1800, 3600] {
        cands.push(secs * f);
    }
    cands.sort_unstable();
    cands.dedup();
    *cands
        .iter()
        .find(|c| px_per_frame * (**c as f32) >= min_px)
        .unwrap_or_else(|| cands.last().unwrap())
}

/// How hard the timeline should auto-pan for a pointer at `x`, given the
/// track's `left`/`right` edges and the width of the sensitive zone.
///
/// Returns -1..0 in the left zone, 0..1 in the right zone, 0 in the middle;
/// magnitude ramps linearly with depth so a nudge scrolls slowly and pinning
/// the pointer to the edge scrolls fast. Past the edge it saturates at ±1
/// rather than accelerating without bound.
fn edge_pan_intensity(x: f32, left: f32, right: f32, edge: f32) -> f32 {
    if edge <= 0.0 || right <= left {
        return 0.0;
    }
    if x < left + edge {
        -(((left + edge - x) / edge).min(1.0))
    } else if x > right - edge {
        ((x - (right - edge)) / edge).min(1.0)
    } else {
        0.0
    }
}

const DOPESHEET_H: f32 = 178.0;
const RULER_H: f32 = 20.0;
/// Width of the auto-pan zone at each end of the track, in points.
const EDGE_PAN_W: f32 = 36.0;

/// Bottom dopesheet: one row per animated property, keyframes drawn as diamonds
/// along a shared time axis with a playhead line. Click a row's track to seek;
/// click a diamond to select it (Delete removes); drag a diamond to move it.
fn dopesheet_ui(
    ui: &mut egui::Ui,
    rows: &[DopeRow],
    frame: f64,
    last_frame: i64,
    tb: motion_core::Timebase,
    view: TimelineView,
    selected_keys: &KeySelection,
    out: &mut DopeEdits,
) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.strong("Timeline");
        ui.weak(
            "— ctrl+click or drag a box to multi-select, drag to move them \
             together, ctrl+C/V copies, Del removes",
        );
    });
    ui.separator();

    const LABEL_W: f32 = 80.0;
    const ROW_H: f32 = 22.0;
    let accent = egui::Color32::from_rgb(255, 216, 51);
    let playhead_col = egui::Color32::from_rgb(240, 90, 90);
    // Set by any drag on the timeline (ruler scrub or keyframe drag).
    // Gates the edge auto-pan below.
    let mut dragging = false;

    // --- Ruler. Allocated with the same layout as a property row, so
    // its axis geometry is exactly the rows' axis geometry. ---
    let mut axis = None;
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            egui::vec2(LABEL_W, RULER_H),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.weak("Frame");
            },
        );
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width() - 8.0, RULER_H),
            egui::Sense::click_and_drag(),
        );
        let a = Axis::new(rect, view);
        axis = Some(a);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, egui::Color32::from_gray(28));

        // Ticks. Minor ticks appear only once frames are far enough
        // apart to be legible as individual frames.
        let step = tick_step(a.px_per_frame(), tb.fps(), 58.0);
        let minor = if a.px_per_frame() >= 6.0 { 1 } else { 0 };
        let first = view.start.floor() as i64;
        let last = (view.start + view.visible).ceil() as i64;

        if minor > 0 {
            let mut f = first;
            while f <= last {
                if f % step != 0 {
                    let x = a.frame_to_x(f as f64);
                    painter.line_segment(
                        [
                            egui::pos2(x, rect.bottom() - 4.0),
                            egui::pos2(x, rect.bottom()),
                        ],
                        egui::Stroke::new(1.0, egui::Color32::from_gray(58)),
                    );
                }
                f += 1;
            }
        }

        let mut f = (first.div_euclid(step)) * step;
        while f <= last {
            if f >= 0 {
                let x = a.frame_to_x(f as f64);
                painter.line_segment(
                    [egui::pos2(x, rect.top() + 3.0), egui::pos2(x, rect.bottom())],
                    egui::Stroke::new(1.0, egui::Color32::from_gray(110)),
                );
                painter.text(
                    egui::pos2(x + 3.0, rect.top() + 1.0),
                    egui::Align2::LEFT_TOP,
                    tb.timecode(f as f64),
                    egui::FontId::monospace(9.0),
                    egui::Color32::from_gray(165),
                );
            }
            f += step;
        }

        // Playhead marker on the ruler.
        let px = a.frame_to_x(frame);
        painter.line_segment(
            [egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())],
            egui::Stroke::new(1.5, playhead_col),
        );

        // Dragging or clicking the ruler scrubs.
        if resp.clicked() || resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                out.seek_to = Some(a.x_to_frame(p.x).clamp(0, last_frame));
            }
        }
        dragging |= resp.dragged();
    });

    let axis = axis.expect("ruler always allocates the axis");

    // --- Zoom / pan. Scroll anywhere over the panel; zoom keeps the
    // frame under the cursor pinned, which is what makes it feel like
    // zooming rather than jumping. ---
    let panel_rect = ui.max_rect();
    let (scroll, hover) =
        ui.input(|i| (i.smooth_scroll_delta, i.pointer.hover_pos()));
    if let Some(p) = hover.filter(|p| panel_rect.contains(*p)) {
        // egui rewrites a shift+wheel gesture into a *horizontal*
        // scroll, so the shift modifier is already gone by the time we
        // see it — a nonzero x delta is the pan signal, not `shift`.
        // (Trackpad sideways swipes land here too, which is right.)
        let next = if scroll.x != 0.0 {
            // Pan: one notch moves a tenth of the window.
            Some(TimelineView {
                start: view.start - (scroll.x as f64 / 120.0) * view.visible * 0.1,
                visible: view.visible,
            })
        } else if scroll.y != 0.0 {
            let factor = (0.9f64).powf(scroll.y as f64 / 120.0);
            let anchor = axis.x_to_frame_exact(p.x);
            let visible = view.visible * factor;
            // Keep `anchor` under the cursor at the new scale.
            let ratio = (anchor - view.start) / view.visible.max(1e-9);
            Some(TimelineView { start: anchor - ratio * visible, visible })
        } else {
            None
        };
        if let Some(next) = next {
            out.set_view = Some(next.clamped(last_frame));
        }
    }

    // --- Box-select. A drag that *starts* on empty track (rather than
    // on a diamond, which grabs the press first) draws a marquee; every
    // key inside it becomes the selection.
    //
    // The rect has to be known before the rows loop, but only a row's
    // response can tell us the drag began on a track — so the "a
    // marquee is live" flag round-trips through egui memory and is read
    // on the following frame. The one-frame lag is invisible: the
    // marquee has no area worth hit-testing until the pointer has
    // actually moved. ---
    let marquee_id = ui.id().with("marquee");
    let mut marquee_active: bool =
        ui.ctx().data(|d| d.get_temp(marquee_id).unwrap_or(false));
    let (press, latest, any_down) = ui.input(|i| {
        (i.pointer.press_origin(), i.pointer.latest_pos(), i.pointer.any_down())
    });
    if marquee_active && !any_down {
        // Released: the last live report already produced the selection.
        marquee_active = false;
        ui.ctx().data_mut(|d| d.insert_temp(marquee_id, false));
    }
    let marquee = match (marquee_active, press, latest) {
        (true, Some(a), Some(b)) => Some(egui::Rect::from_two_pos(a, b)),
        _ => None,
    };
    let mut marquee_hits = KeySelection::new();

    // No early return: the rows loop is a no-op on an empty slice, and
    // returning here would skip the edge auto-pan below (which should
    // still work while scrubbing the ruler with nothing selected).
    if rows.is_empty() {
        ui.add_space(8.0);
        ui.weak("Select a node with animated properties to see its keyframes.");
    }

    for (row_idx, row) in rows.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.allocate_ui_with_layout(
                egui::vec2(LABEL_W, ROW_H),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(row.label);
                },
            );

            // The track: full remaining width, fixed height.
            let (track, track_resp) = ui.allocate_exact_size(
                egui::vec2(ui.available_width() - 8.0, ROW_H),
                egui::Sense::click_and_drag(),
            );
            if track_resp.drag_started() {
                ui.ctx().data_mut(|d| d.insert_temp(marquee_id, true));
            }
            let painter = ui.painter_at(track);
            painter.rect_filled(track, 3.0, egui::Color32::from_gray(32));

            let frame_to_x = |f: f64| axis.frame_to_x(f);
            let x_to_frame = |x: f32| axis.x_to_frame(x);

            // Playhead line.
            let px = frame_to_x(frame);
            painter.line_segment(
                [egui::pos2(px, track.top()), egui::pos2(px, track.bottom())],
                egui::Stroke::new(1.5, playhead_col),
            );

            // Click on empty track → seek and clear the key selection.
            if track_resp.clicked() {
                if let Some(p) = track_resp.interact_pointer_pos() {
                    out.seek_to = Some(x_to_frame(p.x).clamp(0, last_frame));
                    out.clear_selection = true;
                }
            }

            // Keyframe diamonds (interactive, drawn on top).
            let cy = track.center().y;
            for (key_idx, &kf) in row.frames.iter().enumerate() {
                let kx = frame_to_x(kf as f64);
                // Skip keys scrolled out of the window — otherwise
                // their hit rects stay live outside the visible track.
                if kx < track.left() - 2.0 || kx > track.right() + 2.0 {
                    continue;
                }
                if let Some(m) = marquee {
                    if m.contains(egui::pos2(kx, cy)) {
                        marquee_hits.insert((row.kind, key_idx));
                    }
                }
                let is_sel = selected_keys.contains(&(row.kind, key_idx));
                let r = if is_sel { 6.5 } else { 5.0 };
                let hit = egui::Rect::from_center_size(
                    egui::pos2(kx, cy),
                    egui::vec2(r * 2.4, r * 2.4),
                );
                let id = ui.id().with((row_idx, key_idx));
                let resp = ui.interact(hit, id, egui::Sense::click_and_drag());

                let col = if is_sel || resp.dragged() || resp.hovered() {
                    egui::Color32::WHITE
                } else {
                    accent
                };
                let border = if is_sel {
                    egui::Stroke::new(2.0, playhead_col)
                } else {
                    egui::Stroke::new(1.0, egui::Color32::from_gray(16))
                };
                // Diamond = a rotated square.
                let d = [
                    egui::pos2(kx, cy - r),
                    egui::pos2(kx + r, cy),
                    egui::pos2(kx, cy + r),
                    egui::pos2(kx - r, cy),
                ];
                painter.add(egui::Shape::convex_polygon(d.to_vec(), col, border));

                if resp.clicked() {
                    // Ctrl/⌘ or shift extends; a plain click replaces.
                    let mods = ui.input(|i| i.modifiers);
                    if mods.command || mods.shift {
                        out.toggle_key = Some((row.kind, key_idx));
                    } else {
                        out.select_key = Some((row.kind, key_idx));
                    }
                }
                if resp.dragged() {
                    dragging = true;
                    if let Some(p) = resp.interact_pointer_pos() {
                        // Dragging an unselected key selects it first,
                        // so the drag acts on what's under the cursor.
                        if !is_sel {
                            out.select_key = Some((row.kind, key_idx));
                        }
                        // Report a *delta* from this key's current
                        // frame, so the whole selection can move as a
                        // block. Recomputed each frame, so a clamped
                        // drag catches up once room appears.
                        let target = x_to_frame(p.x).clamp(0, last_frame);
                        let delta = target - kf;
                        if delta != 0 {
                            out.move_by = Some(delta);
                        }
                    }
                }
            }
        });
    }

    // Report and draw the marquee. Reported even when empty, so
    // dragging a box over nothing clears the selection like a click on
    // empty track does.
    if let Some(m) = marquee {
        dragging = true;
        out.box_select = Some(std::mem::take(&mut marquee_hits));
        let painter = ui.painter_at(ui.max_rect());
        painter.rect_filled(m, 2.0, egui::Color32::from_white_alpha(18));
        painter.rect_stroke(
            m,
            2.0,
            egui::Stroke::new(1.0, accent),
            egui::StrokeKind::Inside,
        );
    }

    // --- Edge auto-pan. While dragging (scrubbing the ruler or moving
    // a keyframe), holding the pointer near either end of the track
    // scrolls the window that way — so you can drag a key past the
    // visible range without letting go. Deliberately drag-only: doing
    // this on plain hover would scroll the timeline out from under the
    // pointer whenever it drifted near an edge. ---
    if dragging {
        if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
            let intensity = edge_pan_intensity(
                p.x,
                axis.x0,
                axis.x0 + axis.span,
                EDGE_PAN_W,
            );
            if intensity != 0.0 {
                // Time-based so the speed doesn't depend on frame rate;
                // clamped in case a slow frame produces a huge dt.
                let dt = (ui.input(|i| i.stable_dt) as f64).min(0.05);
                let delta = intensity as f64 * view.visible * 0.8 * dt;
                out.set_view = Some(
                    TimelineView { start: view.start + delta, visible: view.visible }
                        .clamped(last_frame),
                );
                // Redraw is event-driven, so without this the pan stops
                // the moment the pointer stops moving.
                ui.ctx().request_repaint();
            }
        }
    }
}

/// A flattened scene-tree row for the layers panel.
struct TreeRow {
    id: NodeId,
    name: String,
    depth: usize,
    is_group: bool,
}

/// Flatten the scene graph depth-first into indented rows.
fn tree_rows(node: &motion_core::Node, depth: usize, out: &mut Vec<TreeRow>) {
    out.push(TreeRow {
        id: node.id,
        name: node.name.clone(),
        depth,
        is_group: node.shape.is_none(),
    });
    for c in &node.children {
        tree_rows(c, depth + 1, out);
    }
}

/// A shape the "add" tools can create.
#[derive(Clone, Copy)]
enum NewShape {
    Rect,
    Ellipse,
    Group,
}

/// What the layers panel reports: selection, reorder, add, and/or delete.
#[derive(Default)]
struct TreeEdits {
    select: Option<NodeId>,
    /// (node, delta) — move among siblings (-1 up, +1 down).
    reorder: Option<(NodeId, i32)>,
    add: Option<NewShape>,
    delete: Option<NodeId>,
    save: bool,
    load: bool,
}

/// Left layers panel: the scene graph as a clickable, indented list. Clicking a
/// row selects that node; the ▲/▼ buttons restack it among its siblings.
fn tree_ui(ui: &mut egui::Ui, rows: &[TreeRow], selected: Option<NodeId>, out: &mut TreeEdits) {
    ui.add_space(8.0);
    ui.heading("Layers");
    ui.horizontal(|ui| {
        if ui.button("Save…").clicked() {
            out.save = true;
        }
        if ui.button("Load…").clicked() {
            out.load = true;
        }
    });
    ui.horizontal(|ui| {
        if ui.button("+ Rect").clicked() {
            out.add = Some(NewShape::Rect);
        }
        if ui.button("+ Ellipse").clicked() {
            out.add = Some(NewShape::Ellipse);
        }
        if ui.button("+ Group").clicked() {
            out.add = Some(NewShape::Group);
        }
    });
    ui.weak("Adds into the selected node, else the root.");
    ui.separator();
    for row in rows {
        ui.horizontal(|ui| {
            ui.add_space(6.0 + row.depth as f32 * 14.0);
            let icon = if row.is_group { "▶" } else { "•" };
            let label = format!("{icon} {}", row.name);
            if ui
                .selectable_label(selected == Some(row.id), label)
                .clicked()
            {
                out.select = Some(row.id);
            }
            // Reorder + delete (not meaningful for the root).
            if row.depth > 0 {
                if ui.small_button("▲").clicked() {
                    out.reorder = Some((row.id, -1));
                }
                if ui.small_button("▼").clicked() {
                    out.reorder = Some((row.id, 1));
                }
                if ui.small_button("✕").clicked() {
                    out.delete = Some(row.id);
                }
            }
        });
    }
}

/// The `PropPath`s an expression can reference, with labels. Core owns the list
/// (`PropPath::ALL`) so a new property shows up in the picker automatically.
const PROP_PATHS: [PropPath; PropPath::ALL.len()] = PropPath::ALL;

fn prop_path_label(p: PropPath) -> &'static str {
    match p {
        PropPath::Position => "Position",
        PropPath::Rotation => "Rotation",
        PropPath::Scale => "Scale",
        PropPath::Opacity => "Opacity",
        PropPath::Anchor => "Anchor",
        PropPath::Fill => "Fill",
        PropPath::StrokeColor => "Stroke Color",
        PropPath::StrokeWidth => "Stroke Width",
        PropPath::ShapeSize => "Size",
        PropPath::ShapeRadius => "Radius",
    }
}

/// Which kind of parameter an "add" button creates. The UI's counterpart to
/// core's `ParamValue`, without carrying a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParamKind {
    Num,
    Vec,
    Color,
}

impl ParamKind {
    /// A fresh parameter of this kind, at a neutral value.
    fn seed(self) -> ParamValue {
        match self {
            ParamKind::Num => ParamValue::Num(Value::constant(0.0)),
            ParamKind::Vec => ParamValue::Vec(Value::constant(Vec2::ZERO)),
            ParamKind::Color => {
                ParamValue::Color(Value::constant(MColor::rgba(1.0, 1.0, 1.0, 1.0)))
            }
        }
    }
}

/// Every script node's current result, keyed by its `(property, tree-path)`
/// address. `Ok` is the formatted value, `Err` the first line of the error.
type ScriptResults = HashMap<(PropKind, Vec<usize>), Result<String, String>>;

/// One property in the graph panel: its kind, and — if it's expression-driven —
/// a clone of the tree to render and its printed form.
struct GraphProp {
    kind: PropKind,
    label: &'static str,
    is_expr: bool,
    expr: Option<Expr>,
    printed: Option<String>,
}

/// What the graph panel renders: the selected node, its editable properties, and
/// every node (for a reference target picker). Cloned before the UI pass so the
/// panel never borrows `App`.
struct GraphInfo {
    node_id: NodeId,
    node_name: String,
    props: Vec<GraphProp>,
    /// (id, name) of every node in the document.
    nodes: Vec<(u64, String)>,
    /// The selected node's parameter names and kinds, in display order.
    params: Vec<(String, &'static str)>,
    /// Each script node's result at the current frame, addressed the same way
    /// an edit is: `(property, tree-path)`. Evaluated **here**, against the
    /// document, because `value()`/`wiggle()` need a real `EvalCtx` — a
    /// doc-less preview would report "no node named …" for a script that in
    /// fact resolves fine at render time.
    script_results: ScriptResults,
}

impl GraphInfo {
    fn gather(doc: &Document, selected: Option<NodeId>, frame: f64) -> Option<GraphInfo> {
        let id = selected?;
        let node = doc.root.find(id)?;
        let props: Vec<GraphProp> = PropKind::ALL
            .iter()
            .filter_map(|&kind| {
                let p = prop_of(node, kind)?;
                Some(GraphProp {
                    kind,
                    label: kind.label(),
                    is_expr: p.is_expr(),
                    expr: p.expr().cloned(),
                    printed: p.expr().map(|e| e.to_string()),
                })
            })
            .collect();
        let mut nodes = Vec::new();
        collect_nodes(&doc.root, &mut nodes);

        let mut script_results = ScriptResults::new();
        for prop in &props {
            if let Some(expr) = &prop.expr {
                let mut ctx = EvalCtx::new(doc, frame);
                // In this node's context, so a script's `param("x")` previews
                // the same value it resolves to at render time.
                ctx.in_node(id, |ctx| {
                    collect_scripts(expr, prop.kind, &mut Vec::new(), ctx, &mut script_results)
                });
            }
        }
        let params =
            node.params.iter().map(|p| (p.name.clone(), p.value.kind_name())).collect();
        Some(GraphInfo {
            node_id: id,
            node_name: node.name.clone(),
            props,
            nodes,
            params,
            script_results,
        })
    }
}

/// Walk `expr`, evaluating every `Script` node it contains against `ctx` and
/// recording the result under its `(property, tree-path)` address — the address
/// the editor looks it up by.
fn collect_scripts(
    expr: &Expr,
    kind: PropKind,
    path: &mut Vec<usize>,
    ctx: &mut EvalCtx,
    out: &mut ScriptResults,
) {
    if let Expr::Script(src) = expr {
        let result = motion_core::eval_script_ctx(src, ctx)
            .map(|v| v.to_string())
            .map_err(|e| e.lines().next().unwrap_or("error").to_string());
        out.insert((kind, path.clone()), result);
    }
    for slot in 0..expr.arity() {
        if let Some(child) = child_ref(expr, slot) {
            path.push(slot);
            collect_scripts(child, kind, path, ctx, out);
            path.pop();
        }
    }
}

fn collect_nodes(node: &MNode, out: &mut Vec<(u64, String)>) {
    out.push((node.id.0, node.name.clone()));
    for c in &node.children {
        collect_nodes(c, out);
    }
}

/// One deferred graph edit. Like the dock, the panel records at most one per
/// frame against a `(property, tree-path)` address and `App` applies it after
/// the UI pass, so the tree isn't restructured mid-render.
enum GraphOp {
    /// Make a property expression-driven (seeded from its current value).
    Promote(PropKind),
    /// Freeze an expression back to a constant.
    Bake(PropKind),
    /// Replace the node at `path` with a fresh node of `new` kind.
    SetKind { kind: PropKind, path: Vec<usize>, new: ExprKind },
    /// Set the literal at `path`.
    SetLit { kind: PropKind, path: Vec<usize>, value: ExprValue },
    /// Set the reference at `path`.
    SetRef { kind: PropKind, path: Vec<usize>, node: NodeId, prop: PropPath, offset: f64 },
    /// Set the script source at `path`.
    SetScript { kind: PropKind, path: Vec<usize>, src: String },
    /// Point a `param` node at a parameter of the node that owns the property.
    SetParam { kind: PropKind, path: Vec<usize>, name: String },
    /// Add a parameter to the selected node, seeded with a neutral value.
    AddParam { name: String, kind: ParamKind },
    /// Remove a parameter. Expressions reading it aren't rewritten — they warn.
    RemoveParam { name: String },
}

#[derive(Default)]
struct GraphEdits {
    op: Option<GraphOp>,
}

// Canvas geometry (logical points). A node box sits at (depth·COL_W, y) inside
// the scrolled content; its *height* varies by kind (see `box_height`).
const GRAPH_COL_W: f32 = 172.0;
const GRAPH_BOX_W: f32 = 152.0;
const GRAPH_V_GAP: f32 = 12.0;
const GRAPH_MARGIN: f32 = 10.0;

/// How tall a node's box needs to be, by kind — enough for its controls. A
/// `ref` node stacks three pickers plus an offset, a `script` a field and its
/// result line, a `value` its editor, and an operator just its kind picker.
fn box_height(expr: &Expr) -> f32 {
    match expr {
        Expr::Ref { .. } => 100.0,
        Expr::Script(_) => 66.0,
        Expr::Param { .. } => 66.0,
        Expr::Lit(_) => 50.0,
        Expr::Add(..) | Expr::Mul(..) | Expr::Neg(..) => 30.0,
    }
}

/// A node's place in the auto-laid-out expression tree: its `path`, tree `depth`
/// (its column), and its box's `y` top and `height` (in content-local points).
/// The layout is a pure function of the tree, so it's unit-tested.
struct ExprBox {
    path: Vec<usize>,
    depth: usize,
    y: f32,
    height: f32,
}

#[cfg(test)]
impl ExprBox {
    /// The vertical centre of the box (wires attach here). Used by the layout
    /// tests; the canvas derives the same point from each box's rect.
    fn center_y(&self) -> f32 {
        self.y + self.height / 2.0
    }
}

/// Lay an expression tree out as a tidy tree: root on the left, children to the
/// right, leaves stacked top-to-bottom (each reserving its own height + a gap)
/// and every parent centred on the span of its children.
fn layout_expr(expr: &Expr) -> Vec<ExprBox> {
    // Returns the laid-out node's centre-y, so a parent can centre on its kids.
    fn rec(
        expr: &Expr,
        path: &mut Vec<usize>,
        depth: usize,
        cursor_y: &mut f32,
        out: &mut Vec<ExprBox>,
    ) -> f32 {
        let height = box_height(expr);
        if expr.arity() == 0 {
            let y = *cursor_y;
            *cursor_y += height + GRAPH_V_GAP;
            out.push(ExprBox { path: path.clone(), depth, y, height });
            y + height / 2.0
        } else {
            let (mut first, mut last) = (0.0, 0.0);
            for slot in 0..expr.arity() {
                path.push(slot);
                let c = rec(child_ref(expr, slot).unwrap(), path, depth + 1, cursor_y, out);
                path.pop();
                if slot == 0 {
                    first = c;
                }
                last = c;
            }
            let center = (first + last) / 2.0;
            out.push(ExprBox { path: path.clone(), depth, y: center - height / 2.0, height });
            center
        }
    }
    let mut out = Vec::new();
    rec(expr, &mut Vec::new(), 0, &mut 0.0, &mut out);
    out
}

/// The expression/node-graph panel: for the selected node, promote a property to
/// an expression, edit its tree on a node canvas, or bake it back to a constant.
fn graph_ui(ui: &mut egui::Ui, info: &Option<GraphInfo>, frame: f64, out: &mut GraphEdits) {
    ui.add_space(8.0);
    ui.heading("Graph");
    let Some(info) = info else {
        ui.weak("Select a node to drive its properties with expressions.");
        return;
    };
    ui.weak(format!("Node: {}  ·  drag a node to arrange", info.node_name));
    ui.separator();
    let param_names = params_ui(ui, info, out);
    ui.separator();
    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for prop in &info.props {
                ui.horizontal(|ui| {
                    ui.strong(prop.label);
                    if prop.is_expr {
                        if ui.small_button("bake").on_hover_text("Freeze to a constant").clicked() {
                            out.op = Some(GraphOp::Bake(prop.kind));
                        }
                        if let Some(printed) = &prop.printed {
                            ui.weak(format!("= {printed}"));
                        }
                    } else if ui
                        .small_button("= fx")
                        .on_hover_text("Drive with an expression")
                        .clicked()
                    {
                        out.op = Some(GraphOp::Promote(prop.kind));
                    }
                });
                if let Some(expr) = &prop.expr {
                    expr_canvas(
                        ui,
                        expr,
                        info.node_id,
                        prop.kind,
                        frame,
                        &info.nodes,
                        &param_names,
                        &info.script_results,
                        out,
                    );
                    ui.separator();
                }
            }
        });
}

/// Manually-placed node positions for one property's canvas, in content-local
/// points. Absent entries fall back to the auto-layout; this is ephemeral view
/// state (egui memory), not saved with the document.
type GraphPositions = std::collections::HashMap<Vec<usize>, egui::Vec2>;

/// Draw one property's expression as a node canvas: boxes at their positions
/// (auto-laid-out, or wherever the user has dragged them), wires from each
/// node's output (right) to its parent's input (left), and a compact editor
/// inside each box. Every editor control reports a [`GraphOp`] against the
/// node's tree-path; dragging updates only the (ephemeral) position memory —
/// neither mutates the document here.
#[allow(clippy::too_many_arguments)]
fn expr_canvas(
    ui: &mut egui::Ui,
    expr: &Expr,
    node_id: NodeId,
    kind: PropKind,
    frame: f64,
    nodes: &[(u64, String)],
    params: &[String],
    results: &ScriptResults,
    out: &mut GraphEdits,
) {
    let boxes = layout_expr(expr);

    // Positions are remembered per (node, property) in egui memory; a box with
    // no stored position falls back to its auto-layout slot (column × its y).
    let mem_id = ui.id().with(("graphpos", node_id.0, kind.label()));
    let mut positions: GraphPositions =
        ui.data(|d| d.get_temp::<GraphPositions>(mem_id)).unwrap_or_default();
    let pos_of = |b: &ExprBox, positions: &GraphPositions| {
        positions
            .get(&b.path)
            .copied()
            .unwrap_or_else(|| egui::vec2(b.depth as f32 * GRAPH_COL_W, b.y))
    };
    // A box's rect at content-local top-left `p`, using its own (kind-based) height.
    let rect_of = |b: &ExprBox, p: egui::Vec2, origin: egui::Pos2| {
        egui::Rect::from_min_size(origin + p, egui::vec2(GRAPH_BOX_W, b.height))
    };

    // Content bounds cover every box (including dragged-out ones) so the scroll
    // area can reach them.
    let mut extent = egui::vec2(0.0, 0.0);
    for b in &boxes {
        let p = pos_of(b, &positions);
        extent.x = extent.x.max(p.x + GRAPH_BOX_W);
        extent.y = extent.y.max(p.y + b.height);
    }
    let (area, _) = ui.allocate_exact_size(extent + egui::vec2(GRAPH_MARGIN, GRAPH_MARGIN), egui::Sense::hover());
    let origin = area.min + egui::vec2(GRAPH_MARGIN, GRAPH_MARGIN);

    // Wires under the boxes: each node's left-centre to its parent's right-centre.
    let wire = ui.visuals().weak_text_color();
    for b in &boxes {
        if let Some((_, parent_path)) = b.path.split_last() {
            if let Some(pb) = boxes.iter().find(|x| x.path == parent_path) {
                let child_in = rect_of(b, pos_of(b, &positions), origin).left_center();
                let parent_out = rect_of(pb, pos_of(pb, &positions), origin).right_center();
                ui.painter().line_segment([parent_out, child_in], egui::Stroke::new(1.5, wire));
            }
        }
    }

    // Boxes on top: a drag response for the box body, then the editor widgets
    // (which take pointer priority where they sit, so dragging an empty part of
    // the box moves it while the controls stay usable).
    for b in &boxes {
        let mut p = pos_of(b, &positions);
        let drag_id = ui.id().with(("graphbox", node_id.0, kind.label(), b.path.as_slice()));
        let resp = ui.interact(rect_of(b, p, origin), drag_id, egui::Sense::drag());
        if resp.dragged() {
            p = (p + resp.drag_delta()).max(egui::vec2(0.0, 0.0));
            positions.insert(b.path.clone(), p);
        }
        let rect = rect_of(b, p, origin);
        let node = expr.at(&b.path).unwrap_or(expr);
        ui.painter().rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
        ui.painter().rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            egui::StrokeKind::Inside,
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(5.0)));
        expr_box(&mut child, node, kind, frame, &b.path, nodes, params, results, out);
    }

    ui.data_mut(|d| d.insert_temp(mem_id, positions));
}

/// The controls inside one canvas box: a kind picker, and a compact editor for a
/// `Lit`/`Ref`. Operators (`Add`/`Mul`/`Neg`) show only their kind — their
/// inputs are separate boxes wired in.
#[allow(clippy::too_many_arguments)]
fn expr_box(
    ui: &mut egui::Ui,
    expr: &Expr,
    kind: PropKind,
    frame: f64,
    path: &[usize],
    nodes: &[(u64, String)],
    params: &[String],
    results: &ScriptResults,
    out: &mut GraphEdits,
) {
    let cur = expr.kind();
    egui::ComboBox::from_id_salt(("ek", kind.label(), path))
        .width(60.0)
        .selected_text(cur.label())
        .show_ui(ui, |ui| {
            for k in ExprKind::ALL {
                if ui.selectable_label(k == cur, k.label()).clicked() && k != cur {
                    out.op = Some(GraphOp::SetKind { kind, path: path.to_vec(), new: k });
                }
            }
        });
    match expr {
        Expr::Lit(v) => lit_editor(ui, *v, kind, path, out),
        Expr::Ref { node, prop, time_offset } => {
            ref_editor(ui, *node, *prop, *time_offset, kind, path, nodes, out)
        }
        Expr::Param { name, .. } => param_editor(ui, name, params, kind, path, out),
        Expr::Script(src) => {
            let result = results.get(&(kind, path.to_vec()));
            script_editor(ui, src, frame, result, kind, path, out)
        }
        _ => {}
    }
}

/// The selected node's parameters: what exists, plus add/remove. Returns the
/// names, which the `param` nodes below use to populate their picker.
///
/// Parameters live here rather than in the properties panel because they only
/// mean anything to expressions — a knob nothing reads is noise in a list of
/// real properties.
fn params_ui(ui: &mut egui::Ui, info: &GraphInfo, out: &mut GraphEdits) -> Vec<String> {
    let names: Vec<String> = info.params.iter().map(|(n, _)| n.clone()).collect();
    ui.horizontal_wrapped(|ui| {
        ui.strong("Parameters");
        // The new parameter's name is typed into egui memory, so the panel
        // stays a pure function of the document (the same rule as the canvas'
        // box positions).
        let buf_id = egui::Id::new(("param_new", info.node_id.0));
        let mut buf: String = ui.data_mut(|d| d.get_temp(buf_id).unwrap_or_default());
        ui.add(
            egui::TextEdit::singleline(&mut buf).hint_text("new name").desired_width(90.0),
        );
        for (label, kind) in
            [("+num", ParamKind::Num), ("+vec", ParamKind::Vec), ("+col", ParamKind::Color)]
        {
            let taken = names.contains(&buf);
            let ok = !buf.trim().is_empty() && !taken;
            if ui
                .add_enabled(ok, egui::Button::new(label).small())
                .on_disabled_hover_text(if taken {
                    "that name is taken"
                } else {
                    "type a name first"
                })
                .clicked()
            {
                out.op = Some(GraphOp::AddParam { name: buf.trim().to_string(), kind });
                buf.clear();
            }
        }
        ui.data_mut(|d| d.insert_temp(buf_id, buf));
    });
    if info.params.is_empty() {
        ui.weak("None. A parameter is a named knob expressions can read.");
    }
    for (name, kind) in &info.params {
        ui.horizontal(|ui| {
            if ui.small_button("x").on_hover_text("Remove").clicked() {
                out.op = Some(GraphOp::RemoveParam { name: name.clone() });
            }
            ui.label(name);
            ui.weak(*kind);
        });
    }
    names
}

/// Pick which of the owning node's parameters this node reads. A combo rather
/// than a free-text field: the parameters that exist are knowable, and a typo
/// would only surface as a warning at render time.
fn param_editor(
    ui: &mut egui::Ui,
    name: &str,
    params: &[String],
    kind: PropKind,
    path: &[usize],
    out: &mut GraphEdits,
) {
    if params.is_empty() {
        ui.weak("no parameters");
        ui.small("add one above");
        return;
    }
    egui::ComboBox::from_id_salt(("pn", kind.label(), path))
        .width(120.0)
        .selected_text(if name.is_empty() { "(pick)" } else { name })
        .show_ui(ui, |ui| {
            for p in params {
                if ui.selectable_label(p == name, p).clicked() && p != name {
                    out.op = Some(GraphOp::SetParam {
                        kind,
                        path: path.to_vec(),
                        name: p.clone(),
                    });
                }
            }
        });
    // A name that no longer matches any parameter (renamed or removed) would
    // otherwise look like a valid pick.
    if !name.is_empty() && !params.iter().any(|p| p == name) {
        ui.colored_label(egui::Color32::from_rgb(220, 90, 90), format!("'{name}' is gone"));
    }
}

/// A one-line Rhai editor with live feedback: below the field, the value the
/// script currently evaluates to, or the error (in red) if it doesn't compile.
fn script_editor(
    ui: &mut egui::Ui,
    src: &str,
    frame: f64,
    result: Option<&Result<String, String>>,
    kind: PropKind,
    path: &[usize],
    out: &mut GraphEdits,
) {
    let mut text = src.to_string();
    let resp = ui
        .add(
            egui::TextEdit::singleline(&mut text)
                .hint_text("frame * 2.0")
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        )
        .on_hover_text(SCRIPT_HELP);
    if resp.changed() {
        out.op = Some(GraphOp::SetScript { kind, path: path.to_vec(), src: text.clone() });
    }
    // The result was computed in `GraphInfo::gather`, against the document, so
    // `value()`/`wiggle()` resolve here exactly as they do at render time.
    // While the field is being edited the snapshot is one frame behind the
    // text, so fall back to a doc-less eval for that frame only — it can't
    // resolve `value()`, and says so rather than showing a stale result.
    match result.map(|r| r.as_ref()) {
        Some(Ok(v)) => {
            ui.weak(format!("= {v}"));
        }
        Some(Err(e)) => {
            ui.colored_label(egui::Color32::from_rgb(220, 90, 90), e);
        }
        None => match motion_core::eval_script(&text, frame) {
            Ok(v) => {
                ui.weak(format!("= {v}"));
            }
            Err(e) => {
                let msg = e.lines().next().unwrap_or("error").to_string();
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), msg);
            }
        },
    }
}

/// What a script can call, shown on hover over the field. Kept short — it's a
/// reminder of the vocabulary, not documentation.
const SCRIPT_HELP: &str = "\
Rhai. Return a number, or an array: [x, y] or [r, g, b(, a)].

In scope:
  frame, time          the current frame
  value(\"node\", \"prop\")            another node's property
  value_at(\"node\", \"prop\", frame)  …at another frame
  wiggle(freq, amp)               smooth noise, deterministic
  wiggle(freq, amp, seed)         an independent stream

prop: position, rotation, scale, opacity, anchor, fill,
      stroke_color, stroke_width, size, radius

Nodes are named, not id'd; a vec/colour comes back as an array.";

/// The child expression at `slot` (0/1 for `Add`/`Mul`, 0 for `Neg`).
fn child_ref(expr: &Expr, slot: usize) -> Option<&Expr> {
    match (expr, slot) {
        (Expr::Add(a, _) | Expr::Mul(a, _) | Expr::Neg(a), 0) => Some(a),
        (Expr::Add(_, b) | Expr::Mul(_, b), 1) => Some(b),
        _ => None,
    }
}

fn lit_editor(ui: &mut egui::Ui, v: ExprValue, kind: PropKind, path: &[usize], out: &mut GraphEdits) {
    let set = |value| Some(GraphOp::SetLit { kind, path: path.to_vec(), value });
    match v {
        ExprValue::Num(n) => {
            let mut n = n;
            if ui.add(egui::DragValue::new(&mut n).speed(0.1)).changed() {
                out.op = set(ExprValue::Num(n));
            }
        }
        ExprValue::Vec2(vec) => {
            let (mut x, mut y) = (vec.x, vec.y);
            let cx = ui.add(egui::DragValue::new(&mut x).speed(0.5)).changed();
            let cy = ui.add(egui::DragValue::new(&mut y).speed(0.5)).changed();
            if cx || cy {
                out.op = set(ExprValue::Vec2(Vec2::new(x, y)));
            }
        }
        ExprValue::Color(c) => {
            let mut rgb = [c.r as f32, c.g as f32, c.b as f32];
            if ui.color_edit_button_rgb(&mut rgb).changed() {
                out.op = set(ExprValue::Color(rgb_color(rgb)));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn ref_editor(
    ui: &mut egui::Ui,
    node: NodeId,
    prop: PropPath,
    offset: f64,
    kind: PropKind,
    path: &[usize],
    nodes: &[(u64, String)],
    out: &mut GraphEdits,
) {
    let mut chosen_node = node;
    let cur_name = nodes
        .iter()
        .find(|(id, _)| *id == node.0)
        .map(|(_, n)| n.clone())
        .unwrap_or_else(|| format!("#{}", node.0));
    egui::ComboBox::from_id_salt(("rn", kind.label(), path))
        .selected_text(cur_name)
        .show_ui(ui, |ui| {
            for (id, name) in nodes {
                if ui.selectable_label(*id == node.0, format!("{name} (#{id})")).clicked() {
                    chosen_node = NodeId(*id);
                }
            }
        });
    let mut chosen_prop = prop;
    egui::ComboBox::from_id_salt(("rp", kind.label(), path))
        .width(80.0)
        .selected_text(prop_path_label(prop))
        .show_ui(ui, |ui| {
            for p in PROP_PATHS {
                if ui.selectable_label(p == prop, prop_path_label(p)).clicked() {
                    chosen_prop = p;
                }
            }
        });
    let mut off = offset;
    let off_changed = ui
        .add(egui::DragValue::new(&mut off).speed(0.5).prefix("t"))
        .on_hover_text("Frame offset")
        .changed();
    if chosen_node != node || chosen_prop != prop || off_changed {
        out.op = Some(GraphOp::SetRef { kind, path: path.to_vec(), node: chosen_node, prop: chosen_prop, offset: off });
    }
}

/// Apply a deferred graph-panel edit to `selected`'s property in `doc`. A free
/// function (not an `App` method) so the whole promote/edit/bake flow is unit-
/// testable against a plain `Document`, no window required.
fn apply_graph_op(doc: &mut Document, selected: NodeId, op: GraphOp, frame: i64) {
    let t = frame as f64;
    match op {
        GraphOp::Promote(kind) => {
            // Promoting a constant/keyframed value only reads its own current
            // value, so a document-less context is enough.
            let mut ctx = EvalCtx::at(t);
            if let Some(node) = doc.root.find_mut(selected) {
                if let Some(mut p) = prop_of_mut(node, kind) {
                    p.promote_to_expr(&mut ctx);
                }
            }
        }
        GraphOp::Bake(kind) => {
            // Baking resolves the *expression*, which may reference other nodes —
            // so it needs the document. Resolve against a clone so the read
            // context doesn't alias the node being mutated.
            let snapshot = doc.clone();
            let mut ctx = EvalCtx::new(&snapshot, t);
            if let Some(node) = doc.root.find_mut(selected) {
                if let Some(mut p) = prop_of_mut(node, kind) {
                    p.bake_to_const(&mut ctx);
                }
            }
        }
        GraphOp::SetKind { kind, path, new } => {
            edit_expr(doc, selected, kind, &path, |e| *e = Expr::seed(new));
        }
        GraphOp::SetLit { kind, path, value } => {
            edit_expr(doc, selected, kind, &path, |e| *e = Expr::Lit(value));
        }
        GraphOp::SetRef { kind, path, node, prop, offset } => {
            edit_expr(doc, selected, kind, &path, |e| {
                *e = Expr::Ref { node, prop, time_offset: offset }
            });
        }
        GraphOp::AddParam { name, kind } => {
            if let Some(node) = doc.root.find_mut(selected) {
                node.set_param(name, kind.seed());
            }
        }
        GraphOp::RemoveParam { name } => {
            if let Some(node) = doc.root.find_mut(selected) {
                node.remove_param(&name);
            }
        }
        GraphOp::SetParam { kind, path, name } => {
            edit_expr(doc, selected, kind, &path, |e| *e = Expr::Param { node: None, name });
        }
        GraphOp::SetScript { kind, path, src } => {
            edit_expr(doc, selected, kind, &path, |e| *e = Expr::Script(src));
        }
    }
}

/// Mutate the expression subtree at `path` on `selected`'s `kind` property.
/// No-op if the property isn't an expression or the path is stale.
fn edit_expr(
    doc: &mut Document,
    selected: NodeId,
    kind: PropKind,
    path: &[usize],
    f: impl FnOnce(&mut Expr),
) {
    if let Some(node) = doc.root.find_mut(selected) {
        if let Some(mut p) = prop_of_mut(node, kind) {
            if let Some(target) = p.expr_mut().and_then(|e| e.at_mut(path)) {
                f(target);
            }
        }
    }
}

enum RenderState {
    Active {
        surface: RenderSurface<'static>,
        window: Arc<Window>,
    },
    Suspended(Option<Arc<Window>>),
}

struct App {
    context: RenderContext,
    /// One vello renderer per wgpu device, indexed by `RenderSurface::dev_id`.
    renderers: Vec<Option<Renderer>>,
    state: RenderState,
    vscene: VScene,
    doc: Document,

    // egui (created lazily in `resumed`, once we have a window + device).
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,

    // Playback clock.
    playing: bool,
    anchor: Instant,
    paused_t: f64,

    // Selection / picking (physical-pixel coordinates).
    cursor: (f64, f64),
    pending_pick: Option<(f64, f64)>,
    selected: Option<NodeId>,
    /// The last frame's evaluation warnings (node id + message), kept so the
    /// comp bar can show them and stderr only prints when the set changes.
    warnings: Vec<(u64, String)>,
    /// The keyframes selected in the dopesheet. Empty = nothing selected.
    selected_keys: KeySelection,
    /// Copied keyframes, pasteable onto any node's matching properties.
    key_clipboard: Option<KeyClipboard>,
    /// The timeline's visible frame window (zoom / pan).
    view: TimelineView,
    /// The panel layout.
    dock: Dock,
    /// Named layouts (built-ins + session-made user presets) offered in the
    /// Layout menu. Applying one replaces `dock` with a clone of its tree.
    presets: Vec<Preset>,
    /// The Layout menu's "save current as" name field, kept across frames.
    preset_name_buf: String,
    /// Canvas area in physical pixels, measured from the layout tree's canvas
    /// leaf during the last UI pass. `None` until the first pass has run.
    canvas_rect: Option<kurbo::Rect>,
    /// Next unused node id, for shapes created in-app.
    next_id: u64,
}

/// The largest node id in a subtree, for seeding the id counter.
fn max_id(node: &MNode) -> u64 {
    node.children.iter().fold(node.id.0, |m, c| m.max(max_id(c)))
}

impl App {
    fn new(doc: Document) -> Self {
        let next_id = max_id(&doc.root) + 1;
        let view = TimelineView::full(doc.duration_frames());
        Self {
            context: RenderContext::new(),
            renderers: Vec::new(),
            state: RenderState::Suspended(None),
            warnings: Vec::new(),
            vscene: VScene::new(),
            doc,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            egui_renderer: None,
            playing: true,
            anchor: Instant::now(),
            paused_t: 0.0,
            cursor: (0.0, 0.0),
            pending_pick: None,
            selected: None,
            selected_keys: KeySelection::new(),
            key_clipboard: None,
            view,
            dock: Dock::default_layout(),
            presets: builtin_presets(),
            preset_name_buf: String::new(),
            canvas_rect: None,
            next_id,
        }
    }

    /// Current looped position on the wall clock, in seconds. Continuous — this
    /// is the clock, not the frame grid. Use `current_frame` / `current_time`
    /// for anything that evaluates or displays.
    fn raw_time(&self) -> f64 {
        let raw = if self.playing {
            self.anchor.elapsed().as_secs_f64()
        } else {
            self.paused_t
        };
        if self.doc.duration > 0.0 {
            raw.rem_euclid(self.doc.duration)
        } else {
            raw
        }
    }

    /// The frame the playhead currently sits on.
    ///
    /// Floors rather than rounds: a frame must be *held* for its full duration,
    /// the way a projector does. Rounding would show frame N starting half a
    /// frame early and is the classic off-by-half in playback code.
    fn current_frame(&self) -> i64 {
        let tb = self.doc.timebase();
        tb.seconds_to_frames_exact(self.raw_time()).floor() as i64
    }

    /// Current document time in seconds, **snapped to the frame grid**. This is
    /// what the canvas evaluates at, so playback actually steps at `doc.fps`
    /// instead of running at the monitor's refresh rate.
    fn current_time(&self) -> f64 {
        self.doc.timebase().frames_to_seconds(self.current_frame() as f64)
    }

    /// Seek to a frame, wrapping around the composition length. All seeking
    /// goes through here, so the playhead can only ever land on the grid.
    fn seek_frame(&mut self, frame: i64) {
        let total = self.doc.duration_frames().max(1);
        let frame = frame.rem_euclid(total);
        self.seek(self.doc.timebase().frames_to_seconds(frame as f64));
    }

    fn seek(&mut self, t: f64) {
        let t = t.rem_euclid(self.doc.duration.max(f64::MIN_POSITIVE));
        self.paused_t = t;
        self.anchor = Instant::now() - std::time::Duration::from_secs_f64(t);
    }

    fn toggle_play(&mut self) {
        if self.playing {
            self.paused_t = self.current_time();
            self.playing = false;
        } else {
            self.anchor = Instant::now() - std::time::Duration::from_secs_f64(self.paused_t);
            self.playing = true;
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let RenderState::Suspended(cached) = &mut self.state else {
            return;
        };
        let window = cached.take().unwrap_or_else(|| {
            let attrs = Window::default_attributes()
                .with_title("Pain By Choice")
                .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
            Arc::new(event_loop.create_window(attrs).unwrap())
        });

        let size = window.inner_size();
        let surface = pollster::block_on(self.context.create_surface(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            wgpu::PresentMode::AutoVsync,
        ))
        .expect("create surface");

        while self.renderers.len() <= surface.dev_id {
            self.renderers.push(None);
        }
        let device = &self.context.devices[surface.dev_id].device;
        if self.renderers[surface.dev_id].is_none() {
            self.renderers[surface.dev_id] = Some(
                Renderer::new(
                    device,
                    RendererOptions {
                        use_cpu: false,
                        antialiasing_support: AaSupport::area_only(),
                        num_init_threads: NonZeroUsize::new(1),
                        pipeline_cache: None,
                    },
                )
                .expect("create renderer"),
            );
        }

        // egui: input plumbing + its own wgpu renderer targeting the swapchain.
        if self.egui_state.is_none() {
            self.egui_state = Some(egui_winit::State::new(
                self.egui_ctx.clone(),
                egui::ViewportId::ROOT,
                &window,
                Some(window.scale_factor() as f32),
                Some(winit::window::Theme::Dark),
                None,
            ));
        }
        self.egui_renderer = Some(egui_wgpu::Renderer::new(
            device,
            surface.format,
            egui_wgpu::RendererOptions::default(),
        ));

        self.state = RenderState::Active { surface, window };
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        if let RenderState::Active { window, .. } = &self.state {
            self.state = RenderState::Suspended(Some(window.clone()));
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let window = match &self.state {
            RenderState::Active { window, .. } => window.clone(),
            RenderState::Suspended(_) => return,
        };

        // Let egui see every event first; if it wants the event exclusively
        // (e.g. dragging the scrubber), don't also treat it as a canvas input.
        let consumed = self
            .egui_state
            .as_mut()
            .map(|st| st.on_window_event(&window, &event).consumed)
            .unwrap_or(false);

        // Whether the pointer is over any egui panel/widget. Combined with
        // `consumed` this decides if a click belongs to the UI rather than the
        // canvas. Both read egui's last frame, so we keep that frame fresh by
        // repainting on pointer motion (see CursorMoved below).
        let over_ui = consumed || self.egui_ctx.is_pointer_over_egui();

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let RenderState::Active { surface, .. } = &mut self.state {
                    self.context
                        .resize_surface(surface, size.width.max(1), size.height.max(1));
                }
                window.request_redraw();
            }

            WindowEvent::KeyboardInput { event, .. }
                if !consumed && event.state == ElementState::Pressed =>
            {
                match event.logical_key {
                    Key::Named(NamedKey::Space) => self.toggle_play(),
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Named(NamedKey::ArrowRight) => {
                        self.playing = false;
                        self.seek_frame(self.current_frame() + 1);
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        self.playing = false;
                        self.seek_frame(self.current_frame() - 1);
                    }
                    Key::Character(ref s) if s == "r" || s == "R" => self.seek_frame(0),
                    Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                        self.delete_selected_keys();
                    }
                    _ => {}
                }
                window.request_redraw();
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                // Repaint so egui's hover/consumed state stays current even
                // while paused — otherwise the next click is judged against a
                // stale frame and canvas picking fires over the UI.
                window.request_redraw();
            }

            WindowEvent::MouseInput { state, button, .. }
                if !over_ui
                    && state == ElementState::Pressed
                    && button == winit::event::MouseButton::Left =>
            {
                // Defer the hit-test to render(), where the evaluated scene and
                // fit transform for this exact frame are in hand.
                self.pending_pick = Some(self.cursor);
                window.request_redraw();
            }

            WindowEvent::RedrawRequested => {
                self.render(&window);
                // Keep animating while playing; when paused, egui still asks
                // for repaints while the pointer interacts with the UI.
                if self.playing || self.egui_ctx.has_requested_repaint() {
                    window.request_redraw();
                }
            }

            // Any other event (mouse move/click for egui) → repaint.
            _ => window.request_redraw(),
        }
    }
}

impl App {
    /// Write the panel's edits into the selected node. Returns whether anything
    /// changed. An edit to a constant overwrites it; an edit to an animated
    /// property sets a keyframe on `frame` (via `Value::set_at`).
    fn apply_edits(&mut self, frame: i64, e: &PropEdits) -> bool {
        let t = frame as f64;
        let mut ctx = EvalCtx::at(t);
        let Some(id) = self.selected else {
            return false;
        };
        let Some(node) = self.doc.root.find_mut(id) else {
            return false;
        };
        let tr = &mut node.transform;
        let mut changed = false;

        if e.pos_x.is_some() || e.pos_y.is_some() {
            let cur = tr.position.resolve(&mut ctx);
            let v = Vec2::new(e.pos_x.unwrap_or(cur.x), e.pos_y.unwrap_or(cur.y));
            tr.position.set_at(frame, v);
            changed = true;
        }
        if let Some(r) = e.rot {
            tr.rotation_deg.set_at(frame, r);
            changed = true;
        }
        if e.scale_x.is_some() || e.scale_y.is_some() {
            let cur = tr.scale.resolve(&mut ctx);
            let v = Vec2::new(e.scale_x.unwrap_or(cur.x), e.scale_y.unwrap_or(cur.y));
            tr.scale.set_at(frame, v);
            changed = true;
        }
        if let Some(o) = e.opacity {
            tr.opacity.set_at(frame, o);
            changed = true;
        }
        if let Some(rgb) = e.fill {
            if let Some(fill) = node.fill.as_mut() {
                fill.set_at(frame, rgb_color(rgb));
                changed = true;
            }
        }

        // Stroke add/remove first, so a stroke added this frame is immediately
        // editable by the value edits below rather than a frame later.
        if e.add_stroke && node.stroke.is_none() {
            node.stroke = Some(motion_core::Stroke {
                color: Value::constant(MColor::rgb(0.0, 0.0, 0.0)),
                width: Value::constant(2.0),
            });
            changed = true;
        }
        if e.remove_stroke {
            node.stroke = None;
            // Its keyframes go with it, so drop any selection pointing at them
            // — stale `(kind, index)` refs would otherwise address a track that
            // no longer exists.
            self.selected_keys
                .retain(|(k, _)| !matches!(k, PropKind::StrokeColor | PropKind::StrokeWidth));
            changed = true;
        }
        let node = self.doc.root.find_mut(id).expect("checked above");
        if let Some(rgb) = e.stroke_color {
            if let Some(s) = node.stroke.as_mut() {
                s.color.set_at(frame, rgb_color(rgb));
                changed = true;
            }
        }
        if let Some(w) = e.stroke_width {
            if let Some(s) = node.stroke.as_mut() {
                s.width.set_at(frame, w);
                changed = true;
            }
        }

        // Shape geometry. Size is a `Vec2` edited as two independent fields, so
        // the untouched axis has to be read back from the current value — same
        // pattern as position/scale above.
        if e.size_x.is_some() || e.size_y.is_some() {
            if let Some(MShape::Rect { size, .. }) | Some(MShape::Ellipse { size }) =
                node.shape.as_mut()
            {
                let cur = size.resolve(&mut ctx);
                let v = Vec2::new(e.size_x.unwrap_or(cur.x), e.size_y.unwrap_or(cur.y));
                size.set_at(frame, v);
                changed = true;
            }
        }
        if let Some(r) = e.radius {
            if let Some(MShape::Rect { radius, .. }) = node.shape.as_mut() {
                radius.set_at(frame, r);
                changed = true;
            }
        }

        // Stopwatch clicks: insert a keyframe at the playhead (promoting a
        // constant to a track the first time). Driven off `PropKind` so a new
        // animatable property needs no new branch here.
        for &kind in &e.key {
            if let Some(mut p) = prop_of_mut(node, kind) {
                p.insert_key(frame);
                changed = true;
            }
        }
        changed
    }

    /// Set the easing handles for the selected keyframe's outgoing segment.
    fn set_ease(&mut self, kind: PropKind, index: usize, p1: (f32, f32), p2: (f32, f32)) -> bool {
        let Some(id) = self.selected else {
            return false;
        };
        let Some(node) = self.doc.root.find_mut(id) else {
            return false;
        };
        let out = Handle::new(p1.0 as f64, p1.1 as f64);
        let next_in = Handle::new(p2.0 as f64, p2.1 as f64);
        let Some(mut p) = prop_of_mut(node, kind) else {
            return false;
        };
        p.set_segment_handles(index, out, next_in);
        true
    }

    /// Remove every dopesheet-selected keyframe (Delete). A track keeps at
    /// least one key, so this may be a partial no-op.
    fn delete_selected_keys(&mut self) -> bool {
        let Some(id) = self.selected else {
            return false;
        };
        if self.selected_keys.is_empty() {
            return false;
        }
        let Some(node) = self.doc.root.find_mut(id) else {
            return false;
        };
        // Descending index order: removing a key shifts every later index
        // down, so deleting from the back keeps the remaining ones valid.
        for &(kind, index) in self.selected_keys.iter().rev() {
            if let Some(mut p) = prop_of_mut(node, kind) {
                p.remove_key(index);
            }
        }
        self.selected_keys.clear();
        true
    }

    /// Copy the selected keyframes (Ctrl+C). Whole keys — value and easing —
    /// so a paste reproduces the curve, not just the timing.
    fn copy_selected_keys(&mut self) -> bool {
        let Some(node) = self.selected.and_then(|id| self.doc.root.find(id)) else {
            return false;
        };
        if self.selected_keys.is_empty() {
            return false;
        }
        let mut tracks = Vec::new();
        let mut origin = i64::MAX;
        for (kind, idxs) in group_selection_by_prop(&self.selected_keys) {
            let Some(p) = prop_of(node, kind) else { continue };
            let clip = p.keys_at(&idxs);
            let Some(first) = clip.first_frame() else { continue };
            origin = origin.min(first);
            tracks.push((kind, clip));
        }
        if tracks.is_empty() {
            return false;
        }
        self.key_clipboard = Some(KeyClipboard { origin, tracks });
        true
    }

    /// Paste the clipboard with its earliest key on the playhead (Ctrl+V), and
    /// select what landed — so the very next drag moves the paste, which is the
    /// motion the user almost always wants next.
    fn paste_keys(&mut self) -> bool {
        let Some(clip) = self.key_clipboard.clone() else {
            return false;
        };
        let Some(id) = self.selected else {
            return false;
        };
        let offset = self.current_frame() - clip.origin;
        let Some(node) = self.doc.root.find_mut(id) else {
            return false;
        };
        let mut landed = KeySelection::new();
        for (kind, track) in &clip.tracks {
            // Skipped when the paste target lacks the property entirely —
            // copying an ellipse's Size and pasting onto a group, say.
            let Some(mut p) = prop_of_mut(node, *kind) else { continue };
            for i in p.insert_keys(track, offset) {
                landed.insert((*kind, i));
            }
        }
        if landed.is_empty() {
            return false;
        }
        self.selected_keys = landed;
        true
    }

    /// Move every selected keyframe by `delta` frames as one rigid block.
    ///
    /// Each property is a separate `Track`, so the limits are intersected
    /// across all of them *before* anything moves — otherwise a track that
    /// clamps early would slide out of sync with the others and the selection
    /// would deform instead of translating.
    fn move_selected_keys(&mut self, delta: i64) -> bool {
        let Some(id) = self.selected else {
            return false;
        };
        if self.selected_keys.is_empty() || delta == 0 {
            return false;
        }
        let Some(node) = self.doc.root.find_mut(id) else {
            return false;
        };

        let per_prop = group_selection_by_prop(&self.selected_keys);

        // Intersect the allowed delta across every affected track.
        let (mut lo, mut hi) = (i64::MIN, i64::MAX);
        for (kind, idxs) in &per_prop {
            let Some(p) = prop_of(node, *kind) else { continue };
            if let Some((l, h)) = p.move_keys_limits(idxs) {
                lo = lo.max(l);
                hi = hi.min(h);
            }
        }
        if lo > hi {
            return false; // the block is boxed in somewhere
        }
        // Also keep the whole selection inside the composition.
        let last = self.doc.duration_frames().max(1);
        let node = self.doc.root.find_mut(id).expect("checked above");
        let mut min_frame = i64::MAX;
        let mut max_frame = i64::MIN;
        for (kind, idxs) in &per_prop {
            let Some(p) = prop_of(node, *kind) else { continue };
            let frames = p.key_frames();
            for &i in idxs {
                if let Some(&f) = frames.get(i) {
                    min_frame = min_frame.min(f);
                    max_frame = max_frame.max(f);
                }
            }
        }
        if min_frame <= max_frame {
            lo = lo.max(-min_frame);
            hi = hi.min(last - max_frame);
        }
        if lo > hi {
            return false;
        }

        let applied = delta.clamp(lo, hi);
        if applied == 0 {
            return false;
        }
        for (kind, idxs) in &per_prop {
            if let Some(mut p) = prop_of_mut(node, *kind) {
                p.move_keys(idxs, applied);
            }
        }
        true
    }

    /// Create a new shape/group, parent it under the selected node (or the
    /// root), select it, and return `true` (the doc changed).
    fn add_node(&mut self, kind: NewShape) -> bool {
        let id = self.next_id;
        self.next_id += 1;

        let center = Vec2::new(self.doc.width / 2.0, self.doc.height / 2.0);
        let at_center = Transform {
            position: Value::constant(center),
            ..Transform::default()
        };
        // A rotating palette so new shapes are visually distinct.
        let palette = [
            MColor::rgb(0.90, 0.25, 0.25),
            MColor::rgb(0.25, 0.65, 0.95),
            MColor::rgb(0.35, 0.80, 0.45),
            MColor::rgb(0.95, 0.75, 0.20),
            MColor::rgb(0.70, 0.45, 0.90),
        ];
        let fill = palette[(id as usize) % palette.len()];

        let node = match kind {
            NewShape::Rect => MNode::shape(
                id,
                format!("Rect {id}"),
                MShape::Rect {
                    size: Value::constant(Vec2::new(200.0, 200.0)),
                    radius: Value::constant(0.0),
                },
            )
            .with_fill(fill)
            .with_transform(at_center),
            NewShape::Ellipse => MNode::shape(
                id,
                format!("Ellipse {id}"),
                MShape::Ellipse { size: Value::constant(Vec2::new(200.0, 200.0)) },
            )
            .with_fill(fill)
            .with_transform(at_center),
            NewShape::Group => MNode::group(id, format!("Group {id}")).with_transform(at_center),
        };

        // Parent under the selected node if it still exists, else the root.
        let target = self.selected.filter(|sid| self.doc.root.find(*sid).is_some());
        let parent = match target {
            Some(sid) => self.doc.root.find_mut(sid).unwrap(),
            None => &mut self.doc.root,
        };
        parent.children.push(node);

        self.selected = Some(NodeId(id));
        self.selected_keys.clear();
        true
    }

    /// Serialize the document *and the current UI layout* to a `.pbc` (JSON)
    /// file chosen via a native save dialog. The layout (active dock + user
    /// presets) rides in a [`Project`] wrapper alongside the document; built-in
    /// presets are code, so only user ones are written.
    fn save(&self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Pain By Choice", &["pbc", "json"])
            .set_file_name("project.pbc")
            .save_file()
        else {
            return;
        };
        let project = Project {
            document: self.doc.clone(),
            layout: LayoutState {
                dock: Some(self.dock.clone()),
                user_presets: self.presets.iter().filter(|p| !p.builtin).cloned().collect(),
            },
        };
        match serde_json::to_string_pretty(&project) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    eprintln!("save failed: {e}");
                }
            }
            Err(e) => eprintln!("serialize failed: {e}"),
        }
    }

    /// Load a `.pbc` via a native open dialog, replacing the current document
    /// *and* layout. Returns whether the document changed. Selection and the id
    /// counter are reset to match the loaded tree.
    ///
    /// Reads both the current [`Project`] format and the older bare-`Document`
    /// files (which carry no layout): the wrapper is tried first, and a bare doc
    /// fails it — it has no `document` field — so it falls through to the plain
    /// parse and keeps the default layout.
    fn load(&mut self) -> bool {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Pain By Choice", &["pbc", "json"])
            .pick_file()
        else {
            return false;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("read failed: {e}");
                return false;
            }
        };
        let (mut doc, layout) = match serde_json::from_str::<Project>(&text) {
            Ok(p) => (p.document, Some(p.layout)),
            Err(_) => match serde_json::from_str::<Document>(&text) {
                Ok(d) => (d, None),
                Err(e) => {
                    eprintln!("parse failed: {e}");
                    return false;
                }
            },
        };
        // Pre-frame-grid docs stored keyframes as float seconds; this converts
        // them using the doc's own fps. No-op on new files.
        doc.migrate();
        self.next_id = max_id(&doc.root) + 1;
        self.view = TimelineView::full(doc.duration_frames());
        self.doc = doc;
        self.selected = None;
        self.selected_keys.clear();

        // Restore the layout. Built-ins are always rebuilt from code; loaded user
        // presets (and the active dock) are validated, so a corrupt or edited
        // file can never wedge the editor with an unusable arrangement.
        self.presets = builtin_presets();
        let restored = match layout {
            Some(l) => {
                self.presets.extend(l.user_presets.into_iter().filter(|p| p.dock.is_valid()));
                l.dock
            }
            None => None,
        };
        self.dock = match restored {
            Some(d) if d.is_valid() => d,
            Some(_) => {
                eprintln!("ignoring invalid saved layout; using default");
                Dock::default_layout()
            }
            None => Dock::default_layout(),
        };
        self.seek_frame(0);
        true
    }

    /// Evaluate + rasterize the current frame, then composite the egui overlay.
    fn render(&mut self, window: &Window) {
        // The whole render path is in the frame domain; seconds only ever
        // appear in the timecode string.
        let frame = self.current_frame();
        let t = frame as f64;
        let last_frame = self.doc.duration_frames().max(1);
        let scene = evaluate(&self.doc, t);
        // Warnings are re-derived every frame, so print only when the set
        // actually changes — a broken script would otherwise spam stderr at the
        // refresh rate. The current set is kept for the comp bar's indicator.
        let warnings: Vec<(u64, String)> =
            scene.warnings.iter().map(|(id, m)| (id.0, m.clone())).collect();
        if warnings != self.warnings {
            for (id, msg) in &warnings {
                eprintln!("warning [node {id}]: {msg}");
            }
        }
        self.warnings = warnings;
        // Cloned for the UI closure, which must not borrow `self`.
        let warnings = self.warnings.clone();

        let size = window.inner_size();
        // egui works in points; the canvas fit works in physical pixels.
        let ppp = window.scale_factor();
        // The canvas area comes from the layout tree's canvas leaf, measured
        // during *last* frame's UI pass — the rect isn't known until the panels
        // have laid out, and the fit is needed before this frame's UI runs (to
        // pick and to build the vello scene). One frame stale only while a
        // splitter or the window is actively being dragged, and it self-corrects
        // on the next repaint, which a drag guarantees.
        let canvas = self.canvas_rect.unwrap_or_else(|| {
            // First frame: nothing measured yet, so fill the window.
            kurbo::Rect::new(0.0, 0.0, size.width as f64, size.height as f64)
        });
        let fit = fit_transform(&self.doc, canvas);

        // Resolve any pending click into a selection (or a deselect). Changing
        // the selected node invalidates any keyframe selection.
        if let Some(px) = self.pending_pick.take() {
            let picked = pick(&scene, fit, px);
            if picked != self.selected {
                self.selected = picked;
                self.selected_keys.clear();
            }
        }

        self.vscene = to_vello(&scene, fit, (self.doc.width, self.doc.height), self.selected);

        // Snapshot the selected node's properties before the UI closure so the
        // egui code borrows a plain struct, never `self`.
        let sel_node = self.selected.and_then(|id| self.doc.root.find(id));
        // Pass the doc so an expression-driven property resolves against the
        // scene (a doc-less context would show its fallback instead).
        let sel_info = sel_node.map(|node| NodeInfo::resolve(node, &self.doc, t));
        let rows = sel_node.map(dope_rows).unwrap_or_default();
        // Snapshot for the graph panel (clones the selected node's expressions).
        let graph_info = GraphInfo::gather(&self.doc, self.selected, t);

        // The selected keyframe's outgoing easing segment, if it has one.
        // Only meaningful for a single key — a segment belongs to one key, and
        // there's no sensible "the" curve for a multi-key selection.
        let single_key = if self.selected_keys.len() == 1 {
            self.selected_keys.iter().next().copied()
        } else {
            None
        };
        let ease_info = match (sel_node, single_key) {
            (Some(node), Some((kind, idx))) => {
                segment_handles_of(node, kind, idx).map(|(p1, p2)| EaseInfo {
                    p1: (p1.x as f32, p1.y as f32),
                    p2: (p2.x as f32, p2.y as f32),
                })
            }
            _ => None,
        };

        // Flatten the scene tree for the layers panel.
        let mut tree = Vec::new();
        tree_rows(&self.doc.root, 0, &mut tree);

        // --- Run egui for this frame (no `self` borrow leaks into the UI). ---
        let raw_input = self.egui_state.as_mut().unwrap().take_egui_input(window);
        let duration = self.doc.duration;
        let timebase = self.doc.timebase();
        let view = self.view;
        let playing = self.playing;
        let mut transport = Transport::default();
        let mut edits = PropEdits::default();
        let mut dope = DopeEdits::default();
        let mut tree_edits = TreeEdits::default();
        let selected_keys = std::mem::take(&mut self.selected_keys);
        let selected_node = self.selected;
        let mut ease_out: Option<((f32, f32), (f32, f32))> = None;
        let mut comp = CompEdits::default();
        let (doc_w, doc_h, doc_fps) = (self.doc.width, self.doc.height, self.doc.fps);
        // Layout-preset menu: the names to list, the save-field buffer (taken so
        // the UI never borrows `self`, restored after), and the reported intent.
        let preset_names: Vec<String> = self.presets.iter().map(|p| p.name.clone()).collect();
        let mut preset_name_buf = std::mem::take(&mut self.preset_name_buf);
        let mut layout = LayoutEdits::default();
        // Panels are drawn by walking the layout tree; each leaf dispatches to
        // the matching editor. Nothing here knows *where* a panel is — that's
        // the tree's business, which is the whole point of the refactor.
        let dock = &mut self.dock;
        let mut canvas_pts: Option<egui::Rect> = None;
        // At most one layout edit (split/join/retype) from an area header this
        // frame; applied to the tree after the UI pass, never during it.
        let mut dock_cmd: Option<DockCmd> = None;
        let mut graph_edits = GraphEdits::default();
        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            let mut next_id = 0;
            let mut path = Vec::new();
            show_dock(
                dock,
                ui,
                &mut next_id,
                &mut path,
                &mut |editor, ui| match editor {
                    Editor::Comp => comp_ui(
                        ui,
                        doc_w,
                        doc_h,
                        doc_fps,
                        duration,
                        &mut comp,
                        &preset_names,
                        &mut preset_name_buf,
                        &mut layout,
                        &warnings,
                    ),
                    Editor::Layers => tree_ui(ui, &tree, selected_node, &mut tree_edits),
                    Editor::Transport => {
                        transport_ui(ui, frame, last_frame, timebase, playing, &mut transport)
                    }
                    Editor::Dopesheet => dopesheet_ui(
                        ui,
                        &rows,
                        t,
                        last_frame,
                        timebase,
                        view,
                        &selected_keys,
                        &mut dope,
                    ),
                    Editor::Properties => {
                        properties_ui(ui, &sel_info, &mut edits, &ease_info, &mut ease_out)
                    }
                    Editor::Graph => graph_ui(ui, &graph_info, t, &mut graph_edits),
                    // vello paints here; egui only measures the hole.
                    Editor::Canvas => canvas_pts = Some(ui.max_rect()),
                },
                &mut dock_cmd,
            );
        });
        // Apply a graph edit (promote/bake/tree change) after the UI pass.
        if let Some(op) = graph_edits.op.take() {
            if let Some(id) = self.selected {
                apply_graph_op(&mut self.doc, id, op, frame);
                window.request_redraw();
            }
        }
        // Now that egui has finished, restructure the layout tree if an area
        // header asked to. Doing it here (not mid-pass) keeps the panels and
        // their egui ids stable for the frame that was just drawn.
        if let Some(cmd) = dock_cmd {
            self.dock.apply(cmd);
            window.request_redraw();
        }
        // Restore the save-field buffer taken for the UI pass.
        self.preset_name_buf = preset_name_buf;
        // Layout presets: switch to one, or save the current arrangement as a
        // session preset. Both re-lay out the panels, so a redraw is due.
        if let Some(i) = layout.apply {
            if let Some(preset) = self.presets.get(i) {
                self.dock = preset.dock.clone();
                window.request_redraw();
            }
        }
        if let Some(name) = layout.save_as {
            let current = self.dock.clone();
            // Overwrite a user preset of the same name; never clobber a built-in.
            match self.presets.iter_mut().find(|p| !p.builtin && p.name == name) {
                Some(existing) => existing.dock = current,
                None => self.presets.push(Preset { name, dock: current, builtin: false }),
            }
        }
        // Points → physical pixels for the next frame's fit.
        self.canvas_rect = canvas_pts.map(|r| {
            kurbo::Rect::new(
                r.min.x as f64 * ppp,
                r.min.y as f64 * ppp,
                r.max.x as f64 * ppp,
                r.max.y as f64 * ppp,
            )
        });

        // Composition settings.
        if let Some(w) = comp.width {
            self.doc.width = w.max(1.0);
        }
        if let Some(h) = comp.height {
            self.doc.height = h.max(1.0);
        }
        if let Some(f) = comp.fps {
            self.doc.fps = f.max(1.0);
        }
        if let Some(d) = comp.duration {
            self.doc.duration = d.max(0.1);
        }
        // fps/duration changes resize the frame axis under the view, so the
        // window may now hang past the end of the comp.
        if comp.fps.is_some() || comp.duration.is_some() {
            self.view = self.view.clamped(self.doc.duration_frames());
        }

        // Layers panel: selection + reorder.
        if let Some(id) = tree_edits.select {
            if Some(id) != self.selected {
                self.selected = Some(id);
                self.selected_keys.clear();
            }
        }

        // Zoom / pan from the timeline.
        if let Some(v) = dope.set_view {
            self.view = v;
        }

        // Keyframe selection changes from the dopesheet. The set was moved out
        // of `self` before the UI ran (so the closure couldn't borrow `App`);
        // put it back, then apply this frame's changes to it.
        self.selected_keys = selected_keys;
        if let Some(hits) = dope.box_select {
            // A live marquee owns the selection outright while it is being
            // dragged — shrinking the box has to deselect, so this replaces
            // rather than merges.
            self.selected_keys = hits;
        } else if let Some(k) = dope.select_key {
            // Plain click: this key becomes the whole selection.
            self.selected_keys.clear();
            self.selected_keys.insert(k);
        } else if let Some(k) = dope.toggle_key {
            // Ctrl/shift click: add, or remove if already in.
            if !self.selected_keys.remove(&k) {
                self.selected_keys.insert(k);
            }
        } else if dope.clear_selection {
            self.selected_keys.clear();
        }
        // Apply the UI's intent to the playback clock.
        if transport.toggle {
            self.toggle_play();
        }
        if transport.restart {
            self.seek_frame(0);
        }
        if let Some(nf) = transport.scrub_to.or(dope.seek_to) {
            self.playing = false;
            self.seek_frame(nf);
        }

        // Apply property edits + keyframe drags to the selected node, then
        // re-evaluate so the change is visible on this very frame.
        let mut dirty = self.apply_edits(frame, &edits);
        if let Some(delta) = dope.move_by {
            dirty |= self.move_selected_keys(delta);
        }

        // Keyframe copy/paste. Read off egui's input rather than the winit
        // handler because that one never sees a modifier state, and suppressed
        // while a text field has focus so Ctrl+V in a numeric box still pastes
        // text instead of keyframes.
        if !self.egui_ctx.egui_wants_keyboard_input() {
            let (copy, paste) = self.egui_ctx.input(|i| {
                (
                    i.modifiers.command && i.key_pressed(egui::Key::C),
                    i.modifiers.command && i.key_pressed(egui::Key::V),
                )
            });
            if copy {
                self.copy_selected_keys();
            }
            if paste {
                dirty |= self.paste_keys();
            }
        }
        // Easing edits target the single selected key (the editor only appears
        // when exactly one is selected).
        let single_key = if self.selected_keys.len() == 1 {
            self.selected_keys.iter().next().copied()
        } else {
            None
        };
        if let (Some((kind, idx)), Some((p1, p2))) = (single_key, ease_out) {
            dirty |= self.set_ease(kind, idx, p1, p2);
        }
        if let Some((id, delta)) = tree_edits.reorder {
            dirty |= self.doc.root.reorder_child(id, delta);
        }
        if let Some(kind) = tree_edits.add {
            dirty |= self.add_node(kind);
        }
        if let Some(id) = tree_edits.delete {
            self.doc.root.remove(id);
            if self.selected == Some(id) {
                self.selected = None;
                self.selected_keys.clear();
            }
            dirty = true;
        }
        if tree_edits.save {
            self.save();
        }
        if tree_edits.load {
            dirty |= self.load();
        }
        if dirty {
            let scene = evaluate(&self.doc, t);
            self.vscene = to_vello(&scene, fit, (self.doc.width, self.doc.height), self.selected);
        }

        self.egui_state
            .as_mut()
            .unwrap()
            .handle_platform_output(window, full_output.platform_output);
        let ppp = self.egui_ctx.pixels_per_point();
        let paint_jobs = self.egui_ctx.tessellate(full_output.shapes, ppp);
        let tex_delta = full_output.textures_delta;

        // --- GPU (disjoint field borrows only past this point). ---
        let RenderState::Active { surface, .. } = &mut self.state else {
            return;
        };

        use wgpu::CurrentSurfaceTexture as Cst;
        let surface_texture = match surface.surface.get_current_texture() {
            Cst::Success(tx) | Cst::Suboptimal(tx) => tx,
            _ => {
                window.request_redraw();
                return;
            }
        };

        let device_handle = &self.context.devices[surface.dev_id];
        let vrenderer = self.renderers[surface.dev_id].as_mut().unwrap();
        vrenderer
            .render_to_texture(
                &device_handle.device,
                &device_handle.queue,
                &self.vscene,
                &surface.target_view,
                &vello::RenderParams {
                    base_color: Color::new([0.08, 0.09, 0.11, 1.0]),
                    width: surface.config.width,
                    height: surface.config.height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .expect("render");

        let egui_renderer = self.egui_renderer.as_mut().unwrap();
        for (id, delta) in &tex_delta.set {
            egui_renderer.update_texture(&device_handle.device, &device_handle.queue, *id, delta);
        }
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [surface.config.width, surface.config.height],
            pixels_per_point: ppp,
        };

        let mut encoder = device_handle
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        let user_buffers = egui_renderer.update_buffers(
            &device_handle.device,
            &device_handle.queue,
            &mut encoder,
            &paint_jobs,
            &screen,
        );

        let surface_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // 1) vello target -> swapchain (overwrites the whole surface).
        surface
            .blitter
            .copy(&device_handle.device, &mut encoder, &surface.target_view, &surface_view);

        // 2) egui overlay composited on top.
        {
            let mut rpass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &surface_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            egui_renderer.render(&mut rpass, &paint_jobs, &screen);
        }

        for id in &tex_delta.free {
            egui_renderer.free_texture(id);
        }

        device_handle
            .queue
            .submit(user_buffers.into_iter().chain([encoder.finish()]));
        surface_texture.present();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_axis(view: TimelineView) -> Axis {
        // 8px pad each side → a 400px usable span.
        Axis::new(
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(416.0, 20.0)),
            view,
        )
    }

    #[test]
    fn axis_round_trips_frames_through_pixels() {
        let a = test_axis(TimelineView { start: 0.0, visible: 100.0 });
        for f in [0i64, 1, 37, 99, 100] {
            assert_eq!(a.x_to_frame(a.frame_to_x(f as f64)), f, "frame {f}");
        }
    }

    #[test]
    fn axis_round_trips_when_panned_and_zoomed() {
        let a = test_axis(TimelineView { start: 240.0, visible: 12.0 });
        for f in [240i64, 243, 251, 252] {
            assert_eq!(a.x_to_frame(a.frame_to_x(f as f64)), f, "frame {f}");
        }
    }

    #[test]
    fn x_to_frame_snaps_to_the_nearest_frame() {
        let a = test_axis(TimelineView { start: 0.0, visible: 10.0 });
        // 40px per frame here, so a third of the way past frame 2 still snaps
        // back to 2, and two-thirds snaps up to 3.
        let x2 = a.frame_to_x(2.0);
        assert_eq!(a.x_to_frame(x2 + 13.0), 2);
        assert_eq!(a.x_to_frame(x2 + 27.0), 3);
    }

    #[test]
    fn zoom_keeps_the_anchored_frame_under_the_cursor() {
        // This is the property that makes zooming feel like zooming: whatever
        // frame is under the pointer must not move while the scale changes.
        let view = TimelineView { start: 0.0, visible: 120.0 };
        let a = test_axis(view);
        let cursor_x = a.frame_to_x(90.0);
        let anchor = a.x_to_frame_exact(cursor_x);

        for factor in [0.5f64, 0.8, 1.25, 2.0] {
            let visible = view.visible * factor;
            let ratio = (anchor - view.start) / view.visible;
            let next = TimelineView { start: anchor - ratio * visible, visible };
            let moved = test_axis(next).frame_to_x(anchor);
            assert!(
                (moved - cursor_x).abs() < 0.5,
                "factor {factor}: anchor drifted {cursor_x} -> {moved}"
            );
        }
    }

    #[test]
    fn view_clamp_keeps_the_window_inside_the_comp() {
        let v = TimelineView { start: -50.0, visible: 5000.0 }.clamped(120);
        assert_eq!(v.start, 0.0);
        assert_eq!(v.visible, 120.0, "cannot show more than the comp");

        // Panned past the end: slides back so the window ends at the last frame.
        let v = TimelineView { start: 900.0, visible: 20.0 }.clamped(120);
        assert!((v.start + v.visible - 120.0).abs() < 1e-9, "start = {}", v.start);

        // Zoomed in absurdly far: floored, not zero or negative.
        let v = TimelineView { start: 10.0, visible: 0.0001 }.clamped(120);
        assert!(v.visible >= 4.0, "visible = {}", v.visible);
    }

    #[test]
    fn tick_step_grows_as_you_zoom_out() {
        // Zoomed in: every frame is far apart, so a 1-frame step fits.
        assert_eq!(tick_step(80.0, 24.0, 58.0), 1);
        // Zoomed out: steps must land on whole seconds at 24fps.
        let wide = tick_step(0.5, 24.0, 58.0);
        assert!(wide % 24 == 0, "expected a whole-second step, got {wide}");
        // And it must actually satisfy the spacing it was asked for.
        assert!(0.5 * wide as f32 >= 58.0);
    }

    #[test]
    fn selection_groups_into_one_bucket_per_property() {
        let mut sel = KeySelection::new();
        // Inserted interleaved and out of order on purpose.
        sel.insert((PropKind::Rotation, 5));
        sel.insert((PropKind::Position, 3));
        sel.insert((PropKind::Rotation, 1));
        sel.insert((PropKind::Position, 0));
        sel.insert((PropKind::Opacity, 2));

        let grouped = group_selection_by_prop(&sel);
        assert_eq!(grouped.len(), 3, "one bucket per property: {grouped:?}");

        // Every property appears exactly once...
        let mut kinds: Vec<PropKind> = grouped.iter().map(|(k, _)| *k).collect();
        let before = kinds.len();
        kinds.dedup();
        assert_eq!(kinds.len(), before, "a property was split across buckets");

        // ...and each bucket's indices are sorted ascending, which is what
        // Track::move_keys and the descending-delete both assume.
        for (kind, idxs) in &grouped {
            assert!(idxs.windows(2).all(|w| w[0] < w[1]), "{kind:?} unsorted: {idxs:?}");
        }
    }

    #[test]
    fn empty_selection_groups_to_nothing() {
        assert!(group_selection_by_prop(&KeySelection::new()).is_empty());
    }

    #[test]
    fn edge_pan_is_dead_in_the_middle_and_signed_at_the_ends() {
        let (l, r, e) = (100.0f32, 500.0f32, 40.0f32);
        assert_eq!(edge_pan_intensity(300.0, l, r, e), 0.0, "middle is dead");
        assert_eq!(edge_pan_intensity(145.0, l, r, e), 0.0, "just inside the zone");
        assert!(edge_pan_intensity(120.0, l, r, e) < 0.0, "left zone pans left");
        assert!(edge_pan_intensity(480.0, l, r, e) > 0.0, "right zone pans right");
    }

    #[test]
    fn edge_pan_ramps_with_depth_and_saturates() {
        let (l, r, e) = (100.0f32, 500.0f32, 40.0f32);
        // Deeper into the zone → stronger.
        let shallow = edge_pan_intensity(130.0, l, r, e).abs();
        let deep = edge_pan_intensity(105.0, l, r, e).abs();
        assert!(deep > shallow, "{deep} should exceed {shallow}");
        // At and beyond the edge it saturates rather than running away.
        assert!((edge_pan_intensity(l, l, r, e) + 1.0).abs() < 1e-6);
        assert!((edge_pan_intensity(-9999.0, l, r, e) + 1.0).abs() < 1e-6);
        assert!((edge_pan_intensity(9999.0, l, r, e) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn edge_pan_handles_degenerate_tracks() {
        // A collapsed or inverted track must not produce a pan (or a NaN).
        assert_eq!(edge_pan_intensity(50.0, 100.0, 100.0, 36.0), 0.0);
        assert_eq!(edge_pan_intensity(50.0, 500.0, 100.0, 36.0), 0.0);
        assert_eq!(edge_pan_intensity(50.0, 100.0, 500.0, 0.0), 0.0);
    }

    #[test]
    fn tick_step_never_returns_zero() {
        // A degenerate/huge zoom-out must still yield a usable positive step,
        // since it's used as a modulus when drawing the ruler.
        for pxf in [1e-6f32, 0.0, 1000.0] {
            assert!(tick_step(pxf, 24.0, 58.0) > 0, "px/frame {pxf}");
        }
    }

    /// A click on the demo square (centered at t=0) should select it, and a
    /// click far outside should deselect. Fit is identity here so physical
    /// pixels equal composition coordinates.
    #[test]
    fn pick_hits_shape_and_misses_empty_space() {
        let doc = demo_document();
        let scene = evaluate(&doc, 0.0);
        let fit = Affine::IDENTITY;

        // The square sits at (300, 540) at t=0 with a 200x200 body.
        assert_eq!(pick(&scene, fit, (300.0, 540.0)), Some(NodeId(1)));
        // Empty corner — nothing there.
        assert_eq!(pick(&scene, fit, (5.0, 5.0)), None);
    }

    #[test]
    fn pick_prefers_front_most_item() {
        // The dot is a child drawn after the square, so where they overlap the
        // dot (front-most) wins. At t=0 the dot is above the square center.
        let doc = demo_document();
        let scene = evaluate(&doc, 0.0);
        let fit = Affine::IDENTITY;
        // Dot center: square pos (300,540) + child offset (0,-120) = (300,420).
        assert_eq!(pick(&scene, fit, (300.0, 420.0)), Some(NodeId(2)));
    }

    #[test]
    fn fit_centers_and_letterboxes_inside_the_canvas_rect() {
        let doc = Document::new(100.0, 100.0, MNode::group(0, "root"));
        // A wide area: the square doc is limited by height and centered on x.
        let fit = fit_transform(&doc, kurbo::Rect::new(0.0, 0.0, 300.0, 100.0));
        assert_eq!(fit * Point::new(0.0, 0.0), Point::new(100.0, 0.0));
        assert_eq!(fit * Point::new(100.0, 100.0), Point::new(200.0, 100.0));
    }

    #[test]
    fn fit_respects_a_canvas_rect_that_does_not_start_at_the_origin() {
        // The regression the layout tree exists to prevent: with panels around
        // it the canvas is an *offset* rect, and picking inverts this transform
        // — an ignored origin puts every click in the wrong place.
        let doc = Document::new(100.0, 100.0, MNode::group(0, "root"));
        let area = kurbo::Rect::new(190.0, 34.0, 290.0, 134.0);
        let fit = fit_transform(&doc, area);
        assert_eq!(fit * Point::new(0.0, 0.0), Point::new(190.0, 34.0));
        assert_eq!(fit * Point::new(50.0, 50.0), Point::new(240.0, 84.0));
        // And a click there inverts back to the doc's center.
        let back = fit.inverse() * Point::new(240.0, 84.0);
        assert!((back.x - 50.0).abs() < 1e-9 && (back.y - 50.0).abs() < 1e-9);
    }

    #[test]
    fn the_default_layout_shows_every_editor_exactly_once() {
        // A leaf reachable in the tree is a panel that renders; one that isn't
        // is a panel that silently vanished. Cheap invariant, easy to break
        // while rearranging the default.
        fn walk(d: &Dock, out: &mut Vec<Editor>) {
            match d {
                Dock::Leaf(e) => out.push(*e),
                Dock::Split { first, second, .. } => {
                    walk(first, out);
                    walk(second, out);
                }
            }
        }
        let mut found = Vec::new();
        walk(&Dock::default_layout(), &mut found);
        for e in [
            Editor::Comp,
            Editor::Layers,
            Editor::Transport,
            Editor::Dopesheet,
            Editor::Properties,
            Editor::Canvas,
        ] {
            assert_eq!(found.iter().filter(|f| **f == e).count(), 1, "{e:?}");
        }
        assert_eq!(found.len(), 6, "no extra leaves");
    }

    #[test]
    fn the_canvas_is_the_innermost_leaf() {
        // It has to be: every other panel takes a fixed edge, and the canvas is
        // whatever is left. If some editor ends up nested *inside* the canvas
        // leaf's remainder, the canvas rect measures the wrong hole.
        let mut d = Dock::default_layout();
        loop {
            match d {
                Dock::Split { second, .. } => d = *second,
                Dock::Leaf(e) => {
                    assert_eq!(e, Editor::Canvas, "innermost leaf must be the canvas");
                    break;
                }
            }
        }
    }

    /// Flatten a layout tree to the editors it shows, in walk order.
    fn dock_editors(d: &Dock) -> Vec<Editor> {
        fn go(d: &Dock, out: &mut Vec<Editor>) {
            match d {
                Dock::Leaf(e) => out.push(*e),
                Dock::Split { first, second, .. } => {
                    go(first, out);
                    go(second, out);
                }
            }
        }
        let mut out = Vec::new();
        go(d, &mut out);
        out
    }

    /// The address of the first leaf showing `target`, or `None`.
    fn path_to(d: &Dock, target: Editor) -> Option<Vec<Branch>> {
        match d {
            Dock::Leaf(e) => (*e == target).then(Vec::new),
            Dock::Split { first, second, .. } => path_to(first, target)
                .map(|mut p| {
                    p.insert(0, Branch::First);
                    p
                })
                .or_else(|| {
                    path_to(second, target).map(|mut p| {
                        p.insert(0, Branch::Second);
                        p
                    })
                }),
        }
    }

    fn count_editor(d: &Dock, e: Editor) -> usize {
        dock_editors(d).into_iter().filter(|x| *x == e).count()
    }

    /// Following `.second` from the root always lands on the canvas — the
    /// invariant the canvas-rect measurement depends on.
    fn innermost_is_canvas(d: &Dock) -> bool {
        let mut cur = d;
        loop {
            match cur {
                Dock::Split { second, .. } => cur = second,
                Dock::Leaf(e) => return *e == Editor::Canvas,
            }
        }
    }

    #[test]
    fn retype_swaps_the_editor_in_place() {
        let mut d = Dock::default_layout();
        let path = path_to(&d, Editor::Layers).unwrap();
        d.apply(DockCmd::Retype { path, editor: Editor::Properties });
        assert_eq!(count_editor(&d, Editor::Layers), 0, "old editor gone");
        assert_eq!(count_editor(&d, Editor::Properties), 2, "now shown twice");
        // Structure is otherwise untouched.
        assert_eq!(dock_editors(&d).len(), 6);
        assert!(innermost_is_canvas(&d));
    }

    #[test]
    fn split_then_close_the_new_half_round_trips() {
        let mut d = Dock::default_layout();
        let before = dock_editors(&d);
        let path = path_to(&d, Editor::Properties).unwrap();
        d.apply(DockCmd::Split {
            path: path.clone(),
            side: DockSide::Left,
            size: 120.0,
        });
        // The leaf became a split of two Properties.
        assert_eq!(count_editor(&d, Editor::Properties), 2);
        assert!(innermost_is_canvas(&d), "a split must not dislodge the canvas");
        // Close the newly-made second half; its sibling (the original) absorbs it.
        let mut new_half = path.clone();
        new_half.push(Branch::Second);
        d.apply(DockCmd::Close { path: new_half });
        assert_eq!(dock_editors(&d), before, "join undoes the split exactly");
    }

    #[test]
    fn closing_an_area_keeps_the_canvas() {
        // Properties sits beside the canvas; closing it must leave the canvas as
        // the surviving sibling, never remove it.
        let mut d = Dock::default_layout();
        let path = path_to(&d, Editor::Properties).unwrap();
        d.apply(DockCmd::Close { path });
        assert_eq!(count_editor(&d, Editor::Properties), 0, "area closed");
        assert_eq!(count_editor(&d, Editor::Canvas), 1, "canvas survives");
        assert!(innermost_is_canvas(&d));
    }

    #[test]
    fn every_builtin_preset_is_a_valid_layout() {
        // A preset a user can switch to must keep the structural guarantees the
        // rest of the app leans on: exactly one canvas as the innermost leaf, and
        // the two headerless toolbars present (there's no way to bring back a
        // Comp or Transport that a preset dropped — they carry no picker).
        for preset in builtin_presets() {
            let d = &preset.dock;
            assert_eq!(count_editor(d, Editor::Canvas), 1, "{}: one canvas", preset.name);
            assert!(innermost_is_canvas(d), "{}: canvas innermost", preset.name);
            assert_eq!(count_editor(d, Editor::Comp), 1, "{}: comp bar", preset.name);
            assert_eq!(count_editor(d, Editor::Transport), 1, "{}: transport", preset.name);
        }
    }

    #[test]
    fn presets_offer_more_than_one_arrangement() {
        // The whole point of presets: the Design layout drops the dopesheet the
        // default keeps, so switching visibly changes the panels.
        let presets = builtin_presets();
        assert!(presets.len() >= 3);
        assert!(presets.iter().all(|p| p.builtin));
        let default = &presets.iter().find(|p| p.name == "Default").unwrap().dock;
        let design = &presets.iter().find(|p| p.name == "Design").unwrap().dock;
        assert_eq!(count_editor(default, Editor::Dopesheet), 1);
        assert_eq!(count_editor(design, Editor::Dopesheet), 0);
    }

    #[test]
    fn is_valid_accepts_layouts_and_rejects_broken_ones() {
        for p in builtin_presets() {
            assert!(p.dock.is_valid(), "{} should be valid", p.name);
        }
        // No toolbars, no comp: a lone canvas leaves the user no way back.
        assert!(!Dock::Leaf(Editor::Canvas).is_valid());
        // Two canvases — the vello target measurement expects exactly one.
        let two = Dock::split(DockSide::Top, 10.0, false, Editor::Canvas, Dock::default_layout());
        assert!(!two.is_valid());
        // Canvas present but not the innermost leaf (it's a split's `first`).
        let off = Dock::split(
            DockSide::Top,
            COMP_H,
            false,
            Editor::Comp,
            Dock::split(
                DockSide::Bottom,
                TRANSPORT_H,
                false,
                Editor::Transport,
                Dock::split(DockSide::Left, 10.0, true, Editor::Canvas, Dock::Leaf(Editor::Properties)),
            ),
        );
        assert!(!off.is_valid());
    }

    #[test]
    fn project_round_trips_document_and_layout() {
        // A non-default arrangement plus a user preset must survive save/load.
        let mut dock = Dock::default_layout();
        let path = path_to(&dock, Editor::Properties).unwrap();
        dock.apply(DockCmd::Split { path, side: DockSide::Top, size: 60.0 });
        let project = Project {
            document: demo_document(),
            layout: LayoutState {
                dock: Some(dock.clone()),
                user_presets: vec![Preset {
                    name: "Mine".into(),
                    dock: Dock::design_layout(),
                    builtin: true, // deliberately set — serialization must drop it
                }],
            },
        };
        let json = serde_json::to_string(&project).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(dock_editors(&back.layout.dock.unwrap()), dock_editors(&dock));
        assert_eq!(back.layout.user_presets.len(), 1);
        assert_eq!(back.layout.user_presets[0].name, "Mine");
        assert!(!back.layout.user_presets[0].builtin, "builtin is skipped → false on load");
    }

    #[test]
    fn a_bare_document_file_still_loads() {
        // Old `.pbc` files are a bare Document with no layout wrapper; the loader
        // tries Project first (which lacks a `document` field here and fails),
        // then falls back to the plain document parse.
        let json = serde_json::to_string(&demo_document()).unwrap();
        assert!(serde_json::from_str::<Project>(&json).is_err(), "no `document` field");
        assert!(serde_json::from_str::<Document>(&json).is_ok(), "parses as a bare doc");
    }

    #[test]
    fn only_content_areas_carry_a_header() {
        // The three structural leaves must never offer split/close/retype, or the
        // canvas-rect and innermost-canvas invariants could be broken from the UI.
        assert!(Editor::Layers.is_swappable());
        assert!(Editor::Properties.is_swappable());
        assert!(Editor::Dopesheet.is_swappable());
        assert!(Editor::Graph.is_swappable());
        assert!(!Editor::Canvas.is_swappable());
        assert!(!Editor::Comp.is_swappable());
        assert!(!Editor::Transport.is_swappable());
    }

    /// A one-node doc (id 1) with the given opacity, under a root.
    fn graph_doc(opacity: Value<f64>) -> (Document, NodeId) {
        let n = MNode::group(1, "n")
            .with_transform(Transform { opacity, ..Transform::default() });
        let doc = Document::new(100.0, 100.0, MNode::group(0, "root").with_child(n));
        (doc, NodeId(1))
    }

    fn resolved_opacity(doc: &Document, id: NodeId) -> f64 {
        let node = doc.root.find(id).unwrap();
        let mut ctx = EvalCtx::new(doc, 0.0);
        ctx.in_node(node.id, |ctx| node.transform.opacity.resolve(ctx))
    }

    #[test]
    fn gather_lists_the_selected_nodes_properties() {
        let (doc, id) = graph_doc(Value::constant(0.5));
        let info = GraphInfo::gather(&doc, Some(id), 0.0).unwrap();
        let opacity = info.props.iter().find(|p| p.kind == PropKind::Opacity).unwrap();
        assert!(!opacity.is_expr, "starts as a plain value");
        assert!(opacity.expr.is_none());
        // The reference-target list includes every node.
        assert!(info.nodes.iter().any(|(nid, _)| *nid == 1));
        assert!(info.nodes.iter().any(|(nid, _)| *nid == 0), "root too");
    }

    #[test]
    fn the_script_preview_resolves_against_the_document() {
        // The panel's result line must use a doc-backed context: a doc-less
        // preview would report "no node named 'root'" for a script that
        // resolves fine at render time.
        let (mut doc, id) = graph_doc(Value::expr(Expr::Script(
            "value(\"root\", \"opacity\") + wiggle(0.0, 0.0)".into(),
        )));
        // Give the root a distinctive opacity to read back.
        doc.root.transform.opacity = Value::constant(0.25);
        let info = GraphInfo::gather(&doc, Some(id), 0.0).unwrap();
        let result = info.script_results.get(&(PropKind::Opacity, vec![])).unwrap();
        assert_eq!(result.as_ref().unwrap(), "0.25");
    }

    #[test]
    fn the_script_preview_is_addressed_by_tree_path() {
        // A script nested under an operator is keyed by its path, so two
        // scripts in one tree can't show each other's result.
        let (mut doc, id) = graph_doc(Value::constant(0.0));
        apply_graph_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
        apply_graph_op(
            &mut doc,
            id,
            GraphOp::SetKind { kind: PropKind::Opacity, path: vec![], new: ExprKind::Add },
            0,
        );
        for (slot, src) in [(0usize, "1.0"), (1, "2.0")] {
            apply_graph_op(
                &mut doc,
                id,
                GraphOp::SetKind {
                    kind: PropKind::Opacity,
                    path: vec![slot],
                    new: ExprKind::Script,
                },
                0,
            );
            apply_graph_op(
                &mut doc,
                id,
                GraphOp::SetScript {
                    kind: PropKind::Opacity,
                    path: vec![slot],
                    src: src.into(),
                },
                0,
            );
        }
        let info = GraphInfo::gather(&doc, Some(id), 0.0).unwrap();
        let at = |path: Vec<usize>| {
            info.script_results.get(&(PropKind::Opacity, path)).unwrap().clone().unwrap()
        };
        assert_eq!(at(vec![0]), "1");
        assert_eq!(at(vec![1]), "2");
        assert!(
            !info.script_results.contains_key(&(PropKind::Opacity, vec![])),
            "the root is an Add, not a script"
        );
    }

    #[test]
    fn a_bad_script_shows_one_line_of_error_in_the_preview() {
        let (doc, id) =
            graph_doc(Value::expr(Expr::Script("value(\"nope\", \"opacity\")".into())));
        let info = GraphInfo::gather(&doc, Some(id), 0.0).unwrap();
        let err = info.script_results[&(PropKind::Opacity, vec![])].clone().unwrap_err();
        assert!(err.contains("nope"), "{err}");
        assert!(!err.contains('\n'), "one line, so it fits under the field");
    }

    #[test]
    fn add_param_then_remove_it_through_the_graph_ops() {
        let (mut doc, id) = graph_doc(Value::constant(0.5));
        apply_graph_op(
            &mut doc,
            id,
            GraphOp::AddParam { name: "gain".into(), kind: ParamKind::Num },
            0,
        );
        assert!(doc.root.find(id).unwrap().param("gain").is_some());
        // The panel lists it, so a `param` node can pick it.
        let info = GraphInfo::gather(&doc, Some(id), 0.0).unwrap();
        assert_eq!(info.params, vec![("gain".to_string(), "number")]);

        apply_graph_op(&mut doc, id, GraphOp::RemoveParam { name: "gain".into() }, 0);
        assert!(doc.root.find(id).unwrap().param("gain").is_none());
    }

    #[test]
    fn a_param_node_drives_a_property_end_to_end() {
        // The whole flow through the panel's ops: add a knob, point the
        // property's expression at it, and see the property take its value.
        let (mut doc, id) = graph_doc(Value::constant(0.0));
        apply_graph_op(
            &mut doc,
            id,
            GraphOp::AddParam { name: "gain".into(), kind: ParamKind::Num },
            0,
        );
        doc.root
            .find_mut(id)
            .unwrap()
            .set_param("gain", ParamValue::Num(Value::constant(0.8)));
        apply_graph_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
        apply_graph_op(
            &mut doc,
            id,
            GraphOp::SetKind { kind: PropKind::Opacity, path: vec![], new: ExprKind::Param },
            0,
        );
        apply_graph_op(
            &mut doc,
            id,
            GraphOp::SetParam { kind: PropKind::Opacity, path: vec![], name: "gain".into() },
            0,
        );
        assert_eq!(resolved_opacity(&doc, id), 0.8);
    }

    #[test]
    fn promote_edit_then_bake_round_trips_a_property() {
        let (mut doc, id) = graph_doc(Value::constant(0.5));
        // Promote seeds a literal of the current value — unchanged.
        apply_graph_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
        assert!(doc.root.find(id).unwrap().transform.opacity.is_expr());
        assert_eq!(resolved_opacity(&doc, id), 0.5);
        // Edit the literal.
        apply_graph_op(
            &mut doc,
            id,
            GraphOp::SetLit { kind: PropKind::Opacity, path: vec![], value: ExprValue::Num(0.9) },
            0,
        );
        assert_eq!(resolved_opacity(&doc, id), 0.9);
        // Bake back to a constant, freezing the value.
        apply_graph_op(&mut doc, id, GraphOp::Bake(PropKind::Opacity), 0);
        assert!(!doc.root.find(id).unwrap().transform.opacity.is_expr());
        assert_eq!(resolved_opacity(&doc, id), 0.9);
    }

    #[test]
    fn set_kind_grows_a_tree_that_evaluates() {
        // Promote, turn the root into Add, then set its two children: 0.2 + 0.3.
        let (mut doc, id) = graph_doc(Value::constant(0.0));
        apply_graph_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
        apply_graph_op(
            &mut doc,
            id,
            GraphOp::SetKind { kind: PropKind::Opacity, path: vec![], new: ExprKind::Add },
            0,
        );
        apply_graph_op(
            &mut doc,
            id,
            GraphOp::SetLit { kind: PropKind::Opacity, path: vec![0], value: ExprValue::Num(0.2) },
            0,
        );
        apply_graph_op(
            &mut doc,
            id,
            GraphOp::SetLit { kind: PropKind::Opacity, path: vec![1], value: ExprValue::Num(0.3) },
            0,
        );
        assert!((resolved_opacity(&doc, id) - 0.5).abs() < 1e-9);
    }

    fn box_at_path<'a>(boxes: &'a [ExprBox], path: &[usize]) -> &'a ExprBox {
        boxes.iter().find(|b| b.path == path).expect("box for path")
    }

    #[test]
    fn layout_stacks_leaves_and_centres_parents() {
        // A single leaf: one box at the top-left column.
        let single = layout_expr(&Expr::num(1.0));
        assert_eq!(single.len(), 1);
        assert_eq!((single[0].depth, single[0].y), (0, 0.0));

        // Add(Lit, Lit): two leaves stacked (the second below the first by at
        // least the first's height), the operator centred between them.
        let add = layout_expr(&Expr::Add(Box::new(Expr::num(1.0)), Box::new(Expr::num(2.0))));
        assert_eq!(add.len(), 3);
        let (root, a, b) = (
            box_at_path(&add, &[]),
            box_at_path(&add, &[0]),
            box_at_path(&add, &[1]),
        );
        assert_eq!((root.depth, a.depth), (0, 1), "inputs one column right");
        assert_eq!(a.y, 0.0);
        assert!(b.y >= a.y + a.height, "second leaf clears the first");
        assert!(
            (root.center_y() - (a.center_y() + b.center_y()) / 2.0).abs() < 1e-4,
            "operator centred over its inputs"
        );
    }

    #[test]
    fn layout_handles_a_nested_tree() {
        // Add(Lit, Mul(Lit, Lit)): three leaves stacked top-to-bottom; each
        // parent centred on its children's span.
        let e = Expr::Add(
            Box::new(Expr::num(1.0)),
            Box::new(Expr::Mul(Box::new(Expr::num(2.0)), Box::new(Expr::num(3.0)))),
        );
        let boxes = layout_expr(&e);
        assert_eq!(boxes.len(), 5);
        let cy = |p: &[usize]| box_at_path(&boxes, p).center_y();
        assert!(cy(&[0]) < cy(&[1, 0]) && cy(&[1, 0]) < cy(&[1, 1]), "leaves stacked in order");
        assert!((cy(&[1]) - (cy(&[1, 0]) + cy(&[1, 1])) / 2.0).abs() < 1e-4, "Mul centred");
        assert!((cy(&[]) - (cy(&[0]) + cy(&[1])) / 2.0).abs() < 1e-4, "root centred on its span");
        assert_eq!(box_at_path(&boxes, &[1, 1]).depth, 2, "deepest column");
    }

    #[test]
    fn taller_nodes_reserve_more_vertical_room() {
        // A ref node is taller than a value node, and the leaf stacked below it
        // clears its full height (so their boxes don't overlap).
        let e = Expr::Add(
            Box::new(Expr::reference(NodeId(1), PropPath::Position)),
            Box::new(Expr::num(0.0)),
        );
        let boxes = layout_expr(&e);
        let refb = box_at_path(&boxes, &[0]);
        let litb = box_at_path(&boxes, &[1]);
        assert!(refb.height > litb.height, "a ref box is taller than a value box");
        assert!(litb.y >= refb.y + refb.height, "the box below clears the taller one");
    }

    #[test]
    fn set_script_drives_a_property_from_the_frame() {
        let (mut doc, id) = graph_doc(Value::constant(0.0));
        apply_graph_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
        apply_graph_op(
            &mut doc,
            id,
            GraphOp::SetKind { kind: PropKind::Opacity, path: vec![], new: ExprKind::Script },
            0,
        );
        apply_graph_op(
            &mut doc,
            id,
            GraphOp::SetScript { kind: PropKind::Opacity, path: vec![], src: "frame + 0.25".into() },
            0,
        );
        // resolved_opacity samples at frame 0, so the script yields 0.25.
        assert!((resolved_opacity(&doc, id) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn set_ref_links_one_property_to_another() {
        // Two nodes: a (id 1) opacity 0.4, b (id 2) empty; b.opacity references a.
        let a = MNode::group(1, "a")
            .with_transform(Transform { opacity: Value::constant(0.4), ..Transform::default() });
        let b = MNode::group(2, "b");
        let mut doc =
            Document::new(100.0, 100.0, MNode::group(0, "root").with_child(a).with_child(b));
        apply_graph_op(&mut doc, NodeId(2), GraphOp::Promote(PropKind::Opacity), 0);
        apply_graph_op(
            &mut doc,
            NodeId(2),
            GraphOp::SetRef {
                kind: PropKind::Opacity,
                path: vec![],
                node: NodeId(1),
                prop: PropPath::Opacity,
                offset: 0.0,
            },
            0,
        );
        assert_eq!(resolved_opacity(&doc, NodeId(2)), 0.4, "b now mirrors a");
    }

    /// A rect with every optional property present.
    fn full_node() -> MNode {
        let mut n = MNode::shape(
            1,
            "rect",
            MShape::Rect {
                size: Value::constant(Vec2::new(100.0, 50.0)),
                radius: Value::constant(4.0),
            },
        )
        .with_fill(MColor::rgb(1.0, 0.0, 0.0));
        n.stroke = Some(motion_core::Stroke {
            color: Value::constant(MColor::rgb(0.0, 0.0, 1.0)),
            width: Value::constant(2.0),
        });
        n
    }

    #[test]
    fn prop_of_and_prop_of_mut_agree_on_what_exists() {
        // The two are separate matches over the same 9 variants, and every
        // keyframe operation trusts them to describe the same node. If they
        // ever disagree, reads and writes silently target different properties.
        for node in [full_node(), MNode::group(1, "g")] {
            for kind in PropKind::ALL {
                let mut m = node.clone();
                assert_eq!(
                    prop_of(&node, kind).is_some(),
                    prop_of_mut(&mut m, kind).is_some(),
                    "{kind:?} disagrees on {}",
                    node.name
                );
            }
        }
    }

    #[test]
    fn optional_properties_are_absent_when_the_node_lacks_them() {
        // A group has no paint and no geometry...
        let g = MNode::group(1, "g");
        for kind in [
            PropKind::Fill,
            PropKind::StrokeColor,
            PropKind::StrokeWidth,
            PropKind::ShapeSize,
            PropKind::ShapeRadius,
        ] {
            assert!(prop_of(&g, kind).is_none(), "group should not have {kind:?}");
        }
        // ...but it still transforms.
        assert!(prop_of(&g, PropKind::Position).is_some());

        // An ellipse has a size but no corner radius.
        let e = MNode::shape(2, "e", MShape::Ellipse { size: Value::constant(Vec2::new(10.0, 10.0)) });
        assert!(prop_of(&e, PropKind::ShapeSize).is_some());
        assert!(prop_of(&e, PropKind::ShapeRadius).is_none(), "ellipse has no radius");

        // A hand-drawn path has neither: its geometry isn't parametric.
        let p = MNode::shape(3, "p", MShape::Path(kurbo::BezPath::new()));
        assert!(prop_of(&p, PropKind::ShapeSize).is_none());
        assert!(prop_of(&p, PropKind::ShapeRadius).is_none());
    }

    #[test]
    fn dope_rows_lists_animated_shape_and_stroke_properties() {
        let mut n = full_node();
        // Nothing animated yet → no rows, even though every property exists.
        assert!(dope_rows(&n).is_empty());

        prop_of_mut(&mut n, PropKind::ShapeRadius).unwrap().insert_key(5);
        prop_of_mut(&mut n, PropKind::StrokeWidth).unwrap().insert_key(7);
        prop_of_mut(&mut n, PropKind::Fill).unwrap().insert_key(9);

        let rows = dope_rows(&n);
        let kinds: Vec<_> = rows.iter().map(|r| r.kind).collect();
        // Row order follows PropKind's declaration order, not insertion order.
        assert_eq!(
            kinds,
            vec![PropKind::Fill, PropKind::StrokeWidth, PropKind::ShapeRadius]
        );
        assert_eq!(rows[2].frames, vec![5], "radius keyed at frame 5");
    }

    #[test]
    fn a_color_clip_will_not_paste_onto_a_scalar_property() {
        // The type tag on ClipTrack is the only thing standing between a fill
        // copy and a width track full of nonsense.
        let mut n = full_node();
        prop_of_mut(&mut n, PropKind::Fill).unwrap().insert_key(0);
        let clip = prop_of(&n, PropKind::Fill).unwrap().keys_at(&[0]);
        assert!(matches!(clip, ClipTrack::Color(_)));

        let landed = prop_of_mut(&mut n, PropKind::StrokeWidth).unwrap().insert_keys(&clip, 0);
        assert!(landed.is_empty(), "color keys must not land on a width track");
        assert!(!is_anim(&n, PropKind::StrokeWidth), "width stays constant");
    }
}

fn main() {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(demo_document());
    println!("Pain By Choice — live. Space=play/pause  ←/→=step  R=restart  Esc=quit");
    event_loop.run_app(&mut app).unwrap();
}
