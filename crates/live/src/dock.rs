//! The dockable panel layout tree, its editors, and the composition bar.
//!
//! Moved verbatim out of `main.rs` when it was split by concern; the
//! only edit was widening visibility to `pub(crate)`.

use crate::*;

/// Default panel sizes, in logical points (egui space). These now seed the
/// layout tree rather than being read back by the canvas fit — see [`Dock`].
pub(crate) const TRANSPORT_H: f32 = 56.0;
pub(crate) const PROPS_W: f32 = 260.0;
pub(crate) const TREE_W: f32 = 190.0;
pub(crate) const COMP_H: f32 = 34.0;

/// Which editor an area of the layout shows.
///
/// The canvas is one of these even though vello draws it, not egui: it has to
/// occupy a leaf so the layout tree knows where the leftover space is. Its leaf
/// draws nothing and merely reports its rect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Editor {
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
pub(crate) const SWAPPABLE: [Editor; 4] =
    [Editor::Layers, Editor::Properties, Editor::Dopesheet, Editor::Graph];

impl Editor {
    /// Human name shown in the area-header picker.
    pub(crate) fn label(self) -> &'static str {
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
    pub(crate) fn is_swappable(self) -> bool {
        SWAPPABLE.contains(&self)
    }

    /// Whether [`show_dock`] should wrap this leaf in a fill-and-scroll region
    /// so its content can't resize the panel it lives in. See the note on
    /// `show_dock` for why that matters.
    ///
    /// Excluded: **Graph** runs its own `ScrollArea::both` (nesting two would
    /// fight over the scroll delta); **Canvas** must measure an exact rect for
    /// the vello target; **Comp** and **Transport** are single fixed-height
    /// bars in non-resizable panels, so they have nothing to overflow.
    pub(crate) fn scroll_wrapped(self) -> bool {
        matches!(self, Editor::Layers | Editor::Properties | Editor::Dopesheet)
    }
}

/// Which edge of an area a split pins its first child to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum DockSide {
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
pub(crate) enum Branch {
    First,
    Second,
}

/// A pending change to the layout tree, produced by an area header during the
/// UI pass and applied once egui is done — the same defer-then-apply discipline
/// the rest of the panels use for their `*Edits`.
pub(crate) enum DockCmd {
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
pub(crate) enum Dock {
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
    pub(crate) fn split(side: DockSide, size: f32, resizable: bool, first: Editor, second: Dock) -> Dock {
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
    pub(crate) fn default_layout() -> Dock {
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
    pub(crate) fn animation_layout() -> Dock {
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
    pub(crate) fn design_layout() -> Dock {
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
    pub(crate) fn node_at_mut(&mut self, path: &[Branch]) -> &mut Dock {
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
    pub(crate) fn apply(&mut self, cmd: DockCmd) {
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
    pub(crate) fn is_valid(&self) -> bool {
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
pub(crate) struct Preset {
    pub(crate) name: String,
    pub(crate) dock: Dock,
    #[serde(skip)]
    pub(crate) builtin: bool,
}

/// The layouts offered out of the box. `Default` reproduces the fixed pre-dock
/// arrangement; `Animation` and `Design` re-weight the same panels for two
/// common modes of work. Adding another is one more entry here plus a `Dock`
/// constructor — which is exactly the extensibility the tree was built for.
pub(crate) fn builtin_presets() -> Vec<Preset> {
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
pub(crate) struct SaveFile {
    /// The multi-comp project. `None` in files written before comps existed —
    /// those carry `document` instead.
    #[serde(default)]
    pub(crate) project: Option<MProject>,
    /// A single composition, as every `.pbc` stored it before the project
    /// registry. Read into a one-comp project; never written any more.
    #[serde(default)]
    pub(crate) document: Option<Document>,
    #[serde(default)]
    pub(crate) layout: LayoutState,
}

/// The persisted UI layout: the active arrangement and any user-made presets.
/// Built-in presets are code, not data, so they're never stored.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct LayoutState {
    /// The active layout. Absent → fall back to [`Dock::default_layout`].
    #[serde(default)]
    pub(crate) dock: Option<Dock>,
    /// Presets saved via the Layout menu (built-ins omitted).
    #[serde(default)]
    pub(crate) user_presets: Vec<Preset>,
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
pub(crate) fn show_dock(
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
            if editor.scroll_wrapped() {
                // Vertical only: the dopesheet maps frames across the panel's
                // *width*, so a horizontal scroll would desync every track from
                // the ruler. `auto_shrink` off in both directions — a short list
                // must still fill the area, or the panel shrinks to fit it and
                // we're back to the same bug from the other side.
                // No `id_salt`: the enclosing panel `Ui` already carries the
                // split's unique id, so two areas showing the same editor still
                // get distinct scroll states.
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| draw(*editor, ui));
            } else {
                draw(*editor, ui);
            }
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
            //
            // **This rect is content-driven**, and that is a trap. egui returns
            // the panel's *inner response* rect, which grows (and shrinks) to
            // whatever the content allocated, clamped only at `max_size` — and
            // it stores that same rect as the panel's `PanelState`, so the next
            // frame starts from it. A leaf whose content changes height
            // therefore resizes its own panel and shoves every other leaf
            // around, including the canvas.
            //
            // That is exactly what selecting a layer used to do: the dopesheet
            // grows a row per animatable property, so each select/deselect
            // resized the timeline and moved the preview under the pointer.
            // The invariant that keeps this honest is `Editor::scroll_wrapped`
            // — every leaf must either fill its area exactly or scroll inside
            // it, never allocate past it.
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
pub(crate) fn area_header(ui: &mut egui::Ui, editor: Editor, path: &[Branch], cmd: &mut Option<DockCmd>) {
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
            if icon::button(ui, icon::CLOSE, "Close this area").clicked() {
                *cmd = Some(DockCmd::Close { path: path.to_vec() });
            }
            if icon::button(ui, icon::SPLIT_H, "Split top / bottom").clicked() {
                *cmd = Some(DockCmd::Split {
                    path: path.to_vec(),
                    side: DockSide::Top,
                    size: area.height() * 0.5,
                });
            }
            if icon::button(ui, icon::SPLIT_V, "Split left / right").clicked() {
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
pub(crate) struct CompEdits {
    pub(crate) width: Option<f64>,
    pub(crate) height: Option<f64>,
    pub(crate) fps: Option<f64>,
    /// The FPS drag began this frame — snapshot the comp before retiming it.
    pub(crate) fps_drag_started: bool,
    /// The FPS drag ended this frame — the rate is settled, drop the snapshot.
    pub(crate) fps_drag_stopped: bool,
    pub(crate) duration: Option<f64>,
    /// A new composition background colour, as egui's `[f32; 3]`.
    pub(crate) bg: Option<[f32; 3]>,
    /// Open a different composition. Everything comp-scoped (selection, the id
    /// counter, the timeline window) is rebuilt when this is applied.
    pub(crate) open: Option<CompId>,
    /// Rename the open comp.
    pub(crate) rename: Option<String>,
}

/// One entry in the comp switcher: its id and the label to show.
pub(crate) struct CompEntry {
    pub(crate) id: CompId,
    pub(crate) label: String,
}

/// Layout-preset intent from the top bar's Layout menu. At most one per frame.
#[derive(Default)]
pub(crate) struct LayoutEdits {
    /// Index into the preset list to switch the whole layout to.
    pub(crate) apply: Option<usize>,
    /// Save the current layout as a user preset under this (trimmed) name.
    pub(crate) save_as: Option<String>,
}

/// Top composition bar: editable resolution, fps, and duration, plus the Layout
/// menu (switch preset / save current). These drive the canvas fit, the
/// playback clock, the frame step, and the timeline mapping — so editing them
/// here reshapes the whole comp. Reports edits into `out` / `layout`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn comp_ui(
    ui: &mut egui::Ui,
    width: f64,
    height: f64,
    fps: f64,
    duration: f64,
    bg: MColor,
    out: &mut CompEdits,
    presets: &[String],
    name_buf: &mut String,
    layout: &mut LayoutEdits,
    warnings: &[(u64, String)],
    comps: &[CompEntry],
    current: CompId,
    comp_name_buf: &mut String,
) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.strong("Composition");

        // The comp switcher. Only worth the space once there's more than one —
        // a single-comp project should look exactly as it did before comps.
        if comps.len() > 1 {
            let open_label = comps
                .iter()
                .find(|c| c.id == current)
                .map(|c| c.label.clone())
                .unwrap_or_else(|| "?".into());
            egui::ComboBox::from_id_salt("comp_switcher")
                .selected_text(open_label)
                .show_ui(ui, |ui| {
                    for c in comps {
                        if ui.selectable_label(c.id == current, &c.label).clicked() {
                            out.open = Some(c.id);
                        }
                    }
                });
        }
        // Renaming the open comp. Edits a buffer so a half-typed name doesn't
        // rewrite the document on every keystroke.
        let resp = ui.add(
            egui::TextEdit::singleline(comp_name_buf)
                .desired_width(110.0)
                .hint_text("comp name"),
        );
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            out.rename = Some(comp_name_buf.clone());
        }
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
                icon::text(icon::WARNING),
            );
            ui.colored_label(
                egui::Color32::from_rgb(220, 160, 60),
                format!("{}", warnings.len()),
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
        // The drag edges bracket the retime: dragging the rate up or down
        // resolves live on every delta, but each delta is applied to the
        // pre-drag comp rather than stacked on the previous one.
        let fps_res = ui.add(egui::DragValue::new(&mut f).speed(0.5).range(1.0..=240.0));
        out.fps_drag_started = fps_res.drag_started();
        out.fps_drag_stopped = fps_res.drag_stopped();
        if fps_res.changed() {
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

        // Composition background. A per-comp setting rather than a theme
        // constant: it is what the frame renders against, so it belongs to the
        // document and is saved with it.
        ui.label("BG");
        let mut rgb = [bg.r as f32, bg.g as f32, bg.b as f32];
        if ui
            .color_edit_button_rgb(&mut rgb)
            .on_hover_text("Composition background")
            .changed()
        {
            out.bg = Some(rgb);
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
