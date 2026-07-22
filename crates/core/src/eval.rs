//! Evaluation: the pure function `(Document, t) -> Scene`. Scrubbing to any
//! time is just calling this with a different `t`; nothing is cached or baked,
//! which is what makes the whole thing non-linear and non-destructive.

use kurbo::{Affine, BezPath, Rect, Shape as _};

use crate::asset::ImagePaint;
use crate::composite::BlendMode;
use crate::expr::EvalCtx;
use crate::node::{CompId, Document, Node, NodeId, Project};
use crate::value::Color;

/// One flat, ready-to-draw item. `source` traces it back to the node that
/// produced it — provenance for selection and debugging.
#[derive(Clone, Debug)]
pub struct RenderItem {
    pub source: NodeId,
    pub transform: Affine,
    pub path: BezPath,
    pub fill: Option<Color>,
    pub stroke: Option<(Color, f64)>,
    /// The footage painted inside `path`, for a raster layer.
    ///
    /// `None` for every vector item, which is why adding footage cost the
    /// renderers a branch rather than a second draw list: an image layer is a
    /// rectangle that happens to name some pixels. A backend that can't draw
    /// images can ignore this field and still place the layer correctly.
    pub image: Option<ImagePaint>,
    /// Effective opacity after multiplying down the ancestor chain.
    pub opacity: f64,
}

/// The evaluated frame: a flat draw list plus any warnings gathered while
/// resolving (e.g. a value that came out non-finite, tagged with its node).
#[derive(Clone, Debug, Default)]
pub struct Scene {
    pub items: Vec<RenderItem>,
    pub warnings: Vec<(NodeId, String)>,
    /// Where every *live* node ended up, recorded as the walk passes through.
    ///
    /// Separate from `items` because a node need not draw anything: a group or
    /// a null has no shape and so no [`RenderItem`], but it still has a place,
    /// and it is exactly the sort of layer you parent things to and animate.
    /// Editor overlays — the motion path, the transform gizmo — need that
    /// place. Reading it here rather than re-deriving the parent chain outside
    /// is what keeps those overlays from drifting away from what the walk
    /// actually did with `LayerTiming`, pre-comps and expression-driven
    /// transforms.
    ///
    /// Nodes outside their time window are absent, not zeroed — the walk
    /// returns before reaching them, which is exactly the "layer isn't here on
    /// this frame" signal a motion path breaks its polyline on.
    pub places: Vec<(NodeId, Placement)>,
    /// Layers that must be composited **in isolation** — rendered into an image
    /// of their own, then combined with the backdrop as a unit.
    ///
    /// Expressed as index ranges into [`Scene::items`] rather than by nesting
    /// the draw list, because `walk` is depth-first and so a node's subtree is
    /// already contiguous. That keeps `items` a flat list in draw order, which
    /// is what picking, the gizmo, onion skins and the SVG backend all read —
    /// none of them had to change to gain compositing.
    ///
    /// Ranges nest but never partially overlap (they are a tree flattened), so
    /// a backend can walk them with a stack. Empty for a document that uses no
    /// blend modes, which is the ordinary case and costs nothing.
    pub groups: Vec<LayerGroup>,
}

impl Scene {
    /// Isolated layers in the order a renderer must **open** them: outermost
    /// first.
    ///
    /// [`walk`] emits groups in post-order — a child's range is known before
    /// its parent's, so it is pushed first — which is the reverse of the order
    /// they have to be opened in. Sorting by start ascending, and by length
    /// descending where two share a start, restores the nesting: an enclosing
    /// range always sorts before the one it contains.
    ///
    /// Lives here rather than in a backend because it is a property of how a
    /// `Scene` is laid out, and every renderer that supports isolation needs
    /// the same answer. An unbalanced or interleaved sequence would corrupt
    /// everything drawn after it, so there is one definition to get right.
    pub fn nesting_order(&self) -> Vec<&LayerGroup> {
        let mut groups: Vec<&LayerGroup> = self.groups.iter().collect();
        groups.sort_by_key(|g| (g.start, std::cmp::Reverse(g.end)));
        groups
    }
}

/// One isolated layer: which items belong to it, and how the result combines.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerGroup {
    pub source: NodeId,
    /// Half-open range into [`Scene::items`]. May be empty, in which case
    /// there is nothing to composite — `walk` skips emitting those.
    pub start: usize,
    pub end: usize,
    pub blend: BlendMode,
    /// The layer's own opacity, applied **once to the composited result**
    /// rather than to each item.
    ///
    /// That distinction is the whole reason isolation is visible to the user:
    /// two overlapping shapes in a 50% group show through each other when
    /// their opacities multiply individually, and don't when the group is
    /// composited first and faded as a unit. The second is what every
    /// compositing tool does, and what a blend mode requires anyway.
    pub alpha: f64,
    /// The layer's mask, already resolved to geometry: the outline, and the
    /// transform that places it in composition space.
    ///
    /// Resolved here rather than left as a `Shape` so that backends need no
    /// `EvalCtx` and no idea that masks are parametric — the same reason
    /// `RenderItem` carries a `BezPath` instead of a `Shape`.
    ///
    /// Already **inverted if the mask is**: the path is a donut of the clip
    /// bounds minus the shape, to be filled even-odd. Doing that here keeps
    /// every backend from re-deriving the same trick and disagreeing about it.
    pub clip: Option<MaskPath>,
}

/// A resolved mask: the outline to clip to, and how to fill it.
#[derive(Clone, Debug, PartialEq)]
pub struct MaskPath {
    pub path: BezPath,
    /// Local → composition space, the layer's own matrix. The mask lives in
    /// layer space, so it moves and rotates with what it masks.
    pub transform: Affine,
    /// Whether the path must be filled **even-odd** rather than non-zero.
    ///
    /// True for an inverted mask, where the path is an enclosing rectangle with
    /// the mask shape punched out of it — even-odd is what makes the hole a
    /// hole rather than a second solid region.
    pub even_odd: bool,
}

/// Where one node sits in composition space on this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Local → composition space: the node's own matrix with every ancestor's
    /// already applied. An overlay that needs to know which way the layer's
    /// axes point (not merely where it is) needs this, not just [`Self::pivot`].
    pub world: Affine,
    /// The node's anchor point in composition space.
    ///
    /// The *anchor*, not the local origin: the local matrix maps the anchor to
    /// `position` by construction, so this is the point the layer rotates and
    /// scales about. An overlay drawn anywhere else sits away from where the
    /// layer visibly turns.
    pub pivot: kurbo::Point,
    /// The node's extent in composition space: its own geometry unioned with
    /// every descendant's. `None` when nothing in the subtree draws.
    ///
    /// Filled in *after* the children are walked, because a group has no
    /// geometry of its own and its extent is entirely its contents'. Computing
    /// it here rather than outside is what makes it available for **every**
    /// node — an editor wanting to snap one layer's edge against its siblings'
    /// needs all of their bounds from the same pass, not a second walk per
    /// candidate.
    ///
    /// Axis-aligned in comp space, so a rotated layer reports the box of the
    /// rotated shape rather than a rotated box. That is the right answer for
    /// "how much room does this take up", which is what alignment needs.
    pub bounds: Option<kurbo::Rect>,
}

impl Scene {
    /// Where `node` ended up, if it was live on this frame.
    pub fn place(&self, node: NodeId) -> Option<Placement> {
        self.places.iter().find(|(id, _)| *id == node).map(|(_, p)| *p)
    }

    /// Shorthand for [`Placement::pivot`].
    pub fn pivot(&self, node: NodeId) -> Option<kurbo::Point> {
        self.place(node).map(|p| p.pivot)
    }
}

/// Evaluate a document at `frame` into a flat `Scene`.
///
/// The frame may be fractional — keys sit on the grid, the playhead need not.
/// Seconds never reach this layer; convert at the edges with
/// [`crate::timebase::Timebase`].
pub fn evaluate(doc: &Document, frame: f64) -> Scene {
    let mut scene = Scene::default();
    // The resolve context is built once here and shared (by `&mut`) down the
    // whole walk, so every property resolves against the same document, cache,
    // and warnings sink. Expression warnings gathered during the walk are folded
    // into the scene's provenance-tagged list afterward.
    let mut ctx = EvalCtx::new(doc, frame);
    walk(&doc.root, Affine::IDENTITY, 1.0, &mut ctx, None, &mut Vec::new(), &mut scene);
    scene.warnings.append(&mut ctx.take_warnings());
    scene
}

/// Evaluate one composition of a project at `frame`, recursing into any precomp
/// layers it instances.
///
/// A comp's contents resolve against **that comp** — expressions inside a
/// precomp reach its own nodes, not the parent's. Cross-comp references are
/// deliberately out of scope for v1.
pub fn evaluate_comp(project: &Project, comp: CompId, frame: f64) -> Scene {
    let mut scene = Scene::default();
    eval_comp(project, comp, frame, Affine::IDENTITY, 1.0, &mut Vec::new(), &mut scene);
    scene
}

/// Evaluate a project's root composition — what opening a `.pbc` shows.
pub fn evaluate_project(project: &Project, frame: f64) -> Scene {
    evaluate_comp(project, project.root, frame)
}

/// Walk one comp's tree, with `stack` recording which comps are already being
/// evaluated further up so a cycle can be caught rather than recursed into.
fn eval_comp(
    project: &Project,
    id: CompId,
    frame: f64,
    xf: Affine,
    opacity: f64,
    stack: &mut Vec<CompId>,
    scene: &mut Scene,
) {
    let Some(comp) = project.comp(id) else {
        // A dangling instance: the comp was deleted but a layer still points at
        // it. Warn against the *comp's* root id — there's no better provenance
        // here, and silently drawing nothing would look like a broken frame.
        scene.warnings.push((NodeId(0), format!("precomp {} no longer exists", id.0)));
        return;
    };
    // Each comp gets its own resolve context: its cache and name lookups are
    // scoped to its own tree, which is exactly what "a comp is a boundary" means.
    let mut ctx = EvalCtx::new(comp, frame);
    // Modules are project-wide: the same definition resolves from any comp.
    ctx.modules = Some(&project.modules);
    // So is footage — one import, shown from any comp.
    ctx.assets = Some(&project.assets);
    stack.push(id);
    walk(&comp.root, xf, opacity, &mut ctx, Some(project), stack, scene);
    stack.pop();
    scene.warnings.append(&mut ctx.take_warnings());
}

/// The extent of a run of items in composition space, strokes included.
///
/// Used to size an inverted mask's cut-out. Strokes straddle their path, so
/// half a width is added back — otherwise inverting a mask on an outlined shape
/// would clip the outer half of its own stroke away.
fn content_bounds(scene: &Scene, start: usize, end: usize) -> Option<kurbo::Rect> {
    scene.items[start..end].iter().fold(None, |acc, item| {
        let mut b = (item.transform * item.path.clone()).bounding_box();
        if let Some((_, width)) = item.stroke {
            b = b.inflate(width / 2.0, width / 2.0);
        }
        Some(match acc {
            Some(a) => a.union(b),
            None => b,
        })
    })
}

fn walk(
    node: &Node,
    parent_xf: Affine,
    parent_opacity: f64,
    ctx: &mut EvalCtx,
    project: Option<&Project>,
    stack: &mut Vec<CompId>,
    scene: &mut Scene,
) -> Option<kurbo::Rect> {
    // A trimmed layer outside its window contributes nothing — and neither do
    // its children, which live in its time. Checked before anything resolves,
    // so a hidden layer costs nothing.
    if let Some(timing) = &node.timing {
        if !timing.is_live(ctx.comp_frame) {
            return None;
        }
    }

    // Inside its window, the layer resolves at its *local* frame. Saved and
    // restored around the whole subtree (the same mechanism `resolve_target`
    // uses for off-time sampling) so a sibling can't inherit the shift.
    let prev_frame = ctx.frame;
    let prev_timing = ctx.timing;
    if let Some(timing) = &node.timing {
        ctx.frame = timing.local_frame(ctx.comp_frame);
        // Also publish the window, so `Expr::Time` (in/out/t01) reads *this*
        // layer's clock rather than an ancestor's.
        ctx.timing = Some(*timing);
    }

    // Everything resolved below belongs to this node, so a warning raised deep
    // in an expression (a bad script, an ambiguous name) is tagged with it.
    let prev_node = ctx.enter_node(node.id);
    let (local_xf, local_opacity) = node.transform.resolve(ctx);
    let xf = parent_xf * local_xf;
    // An isolated layer's opacity belongs to the *group*, applied once to the
    // finished image, so the subtree below it draws at full strength and the
    // fade happens on the way out. `full` is what accumulated down to here;
    // since entering an isolated layer resets the running value to 1.0, an
    // isolated layer nested inside another can't double-count its ancestors.
    let isolated = node.needs_isolation();
    let full = parent_opacity * local_opacity.clamp(0.0, 1.0);
    // Two opacities, because isolation covers the layer's *content* but not its
    // children. Content draws at full strength and the group fades the
    // composite; children are separate layers outside the group, so they take
    // the accumulated fade per-item exactly as they always have.
    let content_opacity = if isolated { 1.0 } else { full };
    let opacity = content_opacity;
    let group_at = scene.items.len();

    // Recorded for every node, drawable or not — see `Scene::places`. `bounds`
    // can't be known yet (a group's extent is its children's), so the slot is
    // remembered and filled once they've been walked.
    let anchor = node.transform.anchor.resolve(ctx);
    let place_at = scene.places.len();
    scene.places.push((
        node.id,
        Placement {
            world: xf,
            pivot: xf * kurbo::Point::new(anchor.x, anchor.y),
            bounds: None,
        },
    ));
    let mut bounds: Option<kurbo::Rect> = None;

    if let Some(shape) = &node.shape {
        let path = shape.to_path(ctx);
        let image = shape.image_paint(ctx);
        let fill = node.fill.as_ref().map(|f| f.resolve(ctx));
        let stroke = node
            .stroke
            .as_ref()
            .map(|s| (s.color.resolve(ctx), s.width.resolve(ctx)));

        // Provenance-tagged sanity check: surface non-finite geometry instead
        // of silently emitting a broken frame.
        if !xf.as_coeffs().iter().all(|c| c.is_finite()) {
            scene
                .warnings
                .push((node.id, "transform resolved to a non-finite value".into()));
        } else {
            bounds = Some((xf * path.clone()).bounding_box());
            scene.items.push(RenderItem {
                source: node.id,
                transform: xf,
                path,
                fill,
                stroke,
                image,
                opacity,
            });
        }
    }

    // A precomp layer renders another comp *into* this one, folded through this
    // layer's transform and opacity — the "vector paste-through" of the plan.
    // No isolated rasterization, so no blend modes or 2D/3D collapse yet; those
    // need the compositor stage.
    if let Some(id) = node.precomp {
        match project {
            // The nested comp's own comp-time is this layer's *local* frame, so
            // trimming or slipping a precomp retimes everything inside it. This
            // is also where nested timing becomes properly relative — a comp
            // boundary is what stage 1 left open.
            Some(project) if !stack.contains(&id) => {
                eval_comp(project, id, ctx.frame, xf, opacity, stack, scene);
            }
            // Comp-level cycle guard, mirroring the expression one: a comp that
            // contains itself warns and stops rather than recursing forever.
            Some(_) => {
                scene.warnings.push((
                    node.id,
                    format!("precomp {} contains itself; not expanded", id.0),
                ));
            }
            // Evaluated as a bare comp rather than through a project, so there
            // is no registry to look the instance up in.
            None => {
                scene
                    .warnings
                    .push((node.id, "precomp layer needs a project to resolve".into()));
            }
        }
    }

    // Everything emitted so far is this layer's own **content**: its artwork,
    // plus the composition it instances (a precomp layer's content genuinely is
    // the nested comp, which is why blending a precomp blends everything in
    // it). Its `children` come next and are deliberately *outside* — see the
    // group emitted below.
    let content_end = scene.items.len();

    // Children are walked *inside* this node's mark only in the sense that each
    // re-marks itself; restore ours first so a sibling can't inherit it.
    ctx.exit_node(prev_node);

    // A blended layer isolates its content, so that content is composited and
    // blended before any child draws. Emitted here rather than after the
    // children for exactly that reason.
    if isolated && content_end > group_at {
        // Resolved after the content, so the enclosing rectangle an inverted
        // mask needs can be measured from what is actually being masked rather
        // than guessed at.
        let clip = node.mask.as_ref().map(|mask| {
            let path = mask.shape.to_path(ctx);
            if mask.inverted {
                // Everything *but* the shape. Built as one path — the content's
                // extent with the shape appended — filled even-odd, so the
                // shape becomes a hole. Sized from the content's own bounds so
                // the cut-out covers exactly what could have drawn.
                // Measured in comp space, then brought back into layer space,
                // where the mask shape lives.
                let mut outer = content_bounds(scene, group_at, content_end)
                    .map(|b| xf.inverse().transform_rect_bbox(b))
                    .unwrap_or(Rect::ZERO);
                // A hair of margin: a rectangle exactly on the content's edge
                // can drop the outermost pixel to rounding.
                outer = outer.inflate(1.0, 1.0);
                let mut combined = outer.to_path(0.1);
                combined.extend(path.iter());
                MaskPath { path: combined, transform: xf, even_odd: true }
            } else {
                MaskPath { path, transform: xf, even_odd: false }
            }
        });
        scene.groups.push(LayerGroup {
            source: node.id,
            start: group_at,
            end: content_end,
            blend: node.blend,
            alpha: full,
            clip,
        });
    }

    for child in &node.children {
        // `full`, not `opacity`: a child is outside its parent's group, so the
        // parent's fade has to reach it the ordinary way. Taking the isolated
        // content's 1.0 here would make children of a faded layer draw at full
        // strength.
        let child_bounds = walk(child, xf, full, ctx, project, stack, scene);
        bounds = match (bounds, child_bounds) {
            (Some(a), Some(b)) => Some(a.union(b)),
            (a, b) => a.or(b),
        };
    }
    scene.places[place_at].1.bounds = bounds;

    ctx.frame = prev_frame;
    ctx.timing = prev_timing;
    bounds
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::BinOp;
    use crate::composite::Mask;
    use crate::node::{Comp, Node, Project, Shape, Transform};
    use crate::value::{Keyframe, Track, Value};
    use kurbo::Vec2;

    fn box_at(id: u64, x: f64) -> Node {
        let mut n = Node::group(id, format!("box{id}"));
        n.shape = Some(Shape::Rect {
            size: Value::constant(Vec2::new(10.0, 10.0)),
            radius: Value::constant(0.0),
        });
        n.fill = Some(Value::constant(Color::rgb(1.0, 1.0, 1.0)));
        n.transform.position = Value::constant(Vec2::new(x, 0.0));
        n
    }

    /// The ordinary document pays nothing. No blend modes means no groups,
    /// which means no backend ever allocates an offscreen target — and it is
    /// why every `.pbc` written before compositing renders exactly as it did.
    #[test]
    fn a_document_without_blend_modes_has_no_groups() {
        let doc = Document::new(
            100.0,
            100.0,
            Node::group(0, "root").with_child(box_at(1, 10.0)).with_child(box_at(2, 20.0)),
        );
        let scene = evaluate(&doc, 0.0);
        assert_eq!(scene.items.len(), 2);
        assert!(scene.groups.is_empty(), "Normal costs nothing");
    }

    /// **A blend mode is not inherited.** It covers the layer's own artwork and
    /// stops there; children are separate layers that composite on their own
    /// terms, and merely inherit the transform.
    #[test]
    fn a_blend_mode_covers_the_layers_own_artwork_not_its_children() {
        let mut parent = box_at(1, 0.0);
        parent.blend = BlendMode::Multiply;
        parent.children.push(box_at(2, 10.0));
        parent.children.push(box_at(3, 20.0));
        let doc = Document::new(
            100.0,
            100.0,
            Node::group(0, "root").with_child(box_at(9, 90.0)).with_child(parent),
        );

        let scene = evaluate(&doc, 0.0);
        assert_eq!(scene.items.len(), 4, "the untouched sibling plus the trio");
        let g = scene.groups.first().expect("the blended layer is isolated");
        assert_eq!(g.source, NodeId(1));
        assert_eq!(g.blend, BlendMode::Multiply);
        // Exactly one item — the parent's own artwork. The children follow it
        // in the draw list and are outside the range.
        assert_eq!((g.start, g.end), (1, 2));
        assert_eq!(scene.items[g.start].source, NodeId(1));
    }

    /// A group with a blend mode but no artwork of its own does nothing —
    /// exactly as a null does. Its children are not its content.
    #[test]
    fn a_blend_mode_on_a_bare_group_does_nothing() {
        let mut group = Node::group(1, "holder");
        group.blend = BlendMode::Multiply;
        group.children.push(box_at(2, 10.0));
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(group));

        let scene = evaluate(&doc, 0.0);
        assert_eq!(scene.items.len(), 1, "the child still draws");
        assert!(scene.groups.is_empty(), "but there is nothing of its own to blend");
    }

    /// A **precomp** layer's content genuinely is the nested comp, so blending
    /// one blends everything inside it. That is what a blend mode on a precomp
    /// is for, and it is why the group covers the precomp expansion even though
    /// it stops before the layer's own children.
    #[test]
    fn blending_a_precomp_layer_covers_the_whole_nested_comp() {
        let inner = Comp::new(
            100.0,
            100.0,
            Node::group(10, "inner root").with_child(box_at(11, 0.0)).with_child(box_at(12, 10.0)),
        );
        let mut project = Project::single(inner);
        let inner_id = project.root;

        let mut host = box_at(1, 0.0);
        host.blend = BlendMode::Screen;
        host.precomp = Some(inner_id);
        host.children.push(box_at(2, 50.0));
        let outer = Comp::new(100.0, 100.0, Node::group(0, "root").with_child(host));
        let outer_id = project.insert(outer);

        let scene = evaluate_comp(&project, outer_id, 0.0);
        let g = scene.groups.first().expect("the precomp layer is isolated");
        // Its own artwork plus both layers of the nested comp — but not the
        // child layer that follows.
        assert_eq!(g.end - g.start, 3);
        let sources: Vec<u64> = scene.items[g.start..g.end].iter().map(|i| i.source.0).collect();
        assert_eq!(sources, vec![1, 11, 12]);
        assert!(scene.items[g.end..].iter().any(|i| i.source == NodeId(2)));
    }

    /// An isolated layer's opacity moves to the group and off the items.
    ///
    /// This is the visible difference between isolating and not: faded as a
    /// unit, overlapping children don't show through each other. Getting it
    /// wrong would double-apply the fade — once per item and again on the
    /// composite.
    #[test]
    fn isolation_moves_opacity_from_the_content_to_the_group() {
        let mut parent = box_at(1, 0.0);
        parent.blend = BlendMode::Screen;
        parent.transform.opacity = Value::constant(0.5);
        parent.children.push(box_at(2, 10.0));
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(parent));

        let scene = evaluate(&doc, 0.0);
        let g = scene.groups.first().expect("isolated");
        assert_eq!(g.alpha, 0.5, "the fade rides on the composite");
        assert_eq!(scene.items[0].opacity, 1.0, "and not on the artwork as well");
        // The child is *outside* the group, so nothing else applies the
        // parent's fade to it — it has to arrive the ordinary way, per item.
        // Missing this would make children of a faded layer draw at full
        // strength.
        assert_eq!(scene.items[1].source, NodeId(2));
        assert_eq!(scene.items[1].opacity, 0.5, "a child still inherits the fade");
    }

    /// Ancestor opacity still reaches an isolated layer — it folds into the
    /// group's alpha rather than being dropped.
    #[test]
    fn an_isolated_layers_group_carries_its_ancestors_fade_too() {
        let mut parent = box_at(1, 0.0);
        parent.blend = BlendMode::Multiply;
        parent.transform.opacity = Value::constant(0.5);
        let mut outer = Node::group(7, "outer");
        outer.transform.opacity = Value::constant(0.5);
        outer.children.push(parent);
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(outer));

        let scene = evaluate(&doc, 0.0);
        assert_eq!(scene.groups[0].alpha, 0.25, "0.5 above times 0.5 of its own");
    }

    /// A blended child of a blended parent is a **sibling** in the draw list,
    /// not a nested layer — each blends against whatever is beneath it. Its
    /// alpha still accumulates the parent's fade, because it is outside the
    /// parent's group and nothing else would apply it.
    #[test]
    fn a_blended_child_composites_beside_its_parent_not_inside_it() {
        let mut inner = box_at(2, 10.0);
        inner.blend = BlendMode::Screen;
        inner.transform.opacity = Value::constant(0.5);
        let mut outer = box_at(1, 0.0);
        outer.blend = BlendMode::Multiply;
        outer.transform.opacity = Value::constant(0.5);
        outer.children.push(inner);
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(outer));

        let scene = evaluate(&doc, 0.0);
        let inner_g = scene.groups.iter().find(|g| g.source == NodeId(2)).expect("inner");
        let outer_g = scene.groups.iter().find(|g| g.source == NodeId(1)).expect("outer");
        assert_eq!(outer_g.alpha, 0.5);
        assert_eq!(inner_g.alpha, 0.25, "its own 0.5, under its parent's 0.5");
        // Disjoint, not nested: the parent's group closes before the child's
        // opens.
        assert!(outer_g.end <= inner_g.start);
    }

    /// Groups still nest — through a **precomp**, whose contents are inside the
    /// instancing layer's range. This is the case a backend's stack-based walk
    /// has to get right, and the reason `nesting_order` exists.
    #[test]
    fn groups_nest_through_a_precomp() {
        let mut nested = box_at(11, 0.0);
        nested.blend = BlendMode::Screen;
        let inner = Comp::new(100.0, 100.0, Node::group(10, "inner root").with_child(nested));
        let mut project = Project::single(inner);
        let inner_id = project.root;

        let mut host = box_at(1, 0.0);
        host.blend = BlendMode::Multiply;
        host.precomp = Some(inner_id);
        let outer = Comp::new(100.0, 100.0, Node::group(0, "root").with_child(host));
        let outer_id = project.insert(outer);

        let scene = evaluate_comp(&project, outer_id, 0.0);
        let host_g = scene.groups.iter().find(|g| g.source == NodeId(1)).expect("host");
        let nested_g = scene.groups.iter().find(|g| g.source == NodeId(11)).expect("nested");
        assert!(
            host_g.start <= nested_g.start && nested_g.end <= host_g.end,
            "the nested comp's layer sits inside the instancing layer's group"
        );
    }

    /// A layer that draws nothing gets no group. An offscreen target to
    /// composite zero pixels is pure cost.
    #[test]
    fn an_empty_isolated_layer_emits_no_group() {
        let mut empty = Node::group(1, "empty");
        empty.blend = BlendMode::Multiply;
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(empty));
        assert!(evaluate(&doc, 0.0).groups.is_empty());
    }

    /// A mask forces isolation even with no blend mode: a clip has to apply to
    /// the finished content, not to each item. Clipping a fill and a stroke
    /// separately would let the stroke survive where the fill was cut.
    #[test]
    fn a_mask_isolates_the_layer_on_its_own() {
        let mut n = box_at(1, 0.0);
        n.mask = Some(Mask::new(Shape::Ellipse { size: Value::constant(Vec2::new(6.0, 6.0)) }));
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(n));

        let scene = evaluate(&doc, 0.0);
        let g = scene.groups.first().expect("a mask needs an offscreen target");
        assert_eq!(g.blend, BlendMode::Normal, "isolated without a blend mode");
        let clip = g.clip.as_ref().expect("the mask resolved to geometry");
        assert!(!clip.even_odd, "an ordinary mask keeps what is inside it");
        // Resolved to the ellipse's own outline, in layer space.
        assert_eq!(clip.path.bounding_box().size(), kurbo::Size::new(6.0, 6.0));
    }

    /// The mask travels in the layer's space, so moving the layer moves its
    /// mask with it — what makes a mask feel attached rather than overlapping.
    #[test]
    fn a_mask_rides_the_layers_own_transform() {
        let mut n = box_at(1, 40.0);
        n.mask = Some(Mask::new(Shape::Rect {
            size: Value::constant(Vec2::new(4.0, 4.0)),
            radius: Value::constant(0.0),
        }));
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(n));

        let scene = evaluate(&doc, 0.0);
        let clip = scene.groups[0].clip.as_ref().unwrap();
        // Layer space is centred on the layer, so the mask's own path is
        // centred on the origin and the transform carries it out to x=40.
        assert_eq!(clip.path.bounding_box().center(), kurbo::Point::ZERO);
        assert_eq!(clip.transform.translation(), Vec2::new(40.0, 0.0));
    }

    /// An inverted mask is the same geometry with the fill rule changed: the
    /// content's extent with the shape punched out, filled even-odd. Built in
    /// core so no backend re-derives the trick and disagrees about it.
    #[test]
    fn an_inverted_mask_becomes_a_hole_in_the_contents_extent() {
        let mut n = box_at(1, 0.0);
        n.mask = Some(Mask {
            shape: Shape::Ellipse { size: Value::constant(Vec2::new(4.0, 4.0)) },
            inverted: true,
        });
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(n));

        let scene = evaluate(&doc, 0.0);
        let clip = scene.groups[0].clip.as_ref().unwrap();
        assert!(clip.even_odd, "the hole is a hole, not a second solid region");
        // The outline now spans the whole 10×10 box (plus the rounding margin)
        // rather than the 4×4 ellipse — the ellipse is the part removed.
        assert!(clip.path.bounding_box().width() >= 10.0);
    }

    /// A mask is scoped exactly like a blend mode: the layer's own content,
    /// never its children.
    #[test]
    fn a_mask_does_not_clip_the_layers_children() {
        let mut parent = box_at(1, 0.0);
        parent.mask = Some(Mask::new(Shape::Ellipse {
            size: Value::constant(Vec2::new(2.0, 2.0)),
        }));
        parent.children.push(box_at(2, 40.0));
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(parent));

        let scene = evaluate(&doc, 0.0);
        let g = &scene.groups[0];
        assert_eq!((g.start, g.end), (0, 1), "the child is outside the clip");
        assert_eq!(scene.items[1].source, NodeId(2));
    }

    /// Footage is a rectangle that names some pixels: it produces an ordinary
    /// render item with a path, so every overlay and hit-test in the editor
    /// keeps working, and the pixels ride alongside in `image`.
    #[test]
    fn a_footage_layer_is_a_rect_that_names_its_source_frame() {
        use crate::asset::{Asset, AssetId};
        use crate::node::{Comp, Project};

        let mut layer = Node::group(1, "clip");
        layer.shape = Some(Shape::Image {
            asset: AssetId(0),
            size: Value::constant(Vec2::new(320.0, 180.0)),
            time_remap: None,
        });
        layer.fill = Some(Value::constant(Color::rgb(1.0, 1.0, 1.0)));
        let mut comp = Comp::new(640.0, 360.0, Node::group(0, "root").with_child(layer));
        // Set directly, not through `set_fps`: this comp has no keys to re-grid.
        comp.fps = 24.0;
        let mut project = Project::single(comp);
        // 24fps footage in a 24fps comp: source frame == local frame.
        project.add_asset(Asset::video(AssetId(9), "clip.mp4", 320.0, 180.0, 48, 24.0));

        let scene = evaluate_project(&project, 7.0);
        let item = scene.items.first().expect("footage draws");
        assert_eq!(
            item.path.bounding_box().size(),
            kurbo::Size::new(320.0, 180.0),
            "the frame rectangle is what the gizmo and hit-test read"
        );
        assert_eq!(item.image.map(|i| i.source_frame), Some(7));
        assert!(scene.warnings.is_empty());
    }

    /// A layer pointing at footage that was removed still draws its rectangle,
    /// and warns — the same treatment a dangling precomp gets. Dropping the
    /// item instead would make a deleted import look like a broken renderer.
    #[test]
    fn footage_missing_from_the_project_warns_rather_than_vanishing() {
        use crate::asset::AssetId;
        use crate::node::{Comp, Project};

        let mut layer = Node::group(1, "clip");
        layer.shape = Some(Shape::Image {
            asset: AssetId(42),
            size: Value::constant(Vec2::new(100.0, 100.0)),
            time_remap: None,
        });
        layer.fill = Some(Value::constant(Color::rgb(1.0, 1.0, 1.0)));
        let mut comp = Comp::new(640.0, 360.0, Node::group(0, "root").with_child(layer));
        // Set directly, not through `set_fps`: this comp has no keys to re-grid.
        comp.fps = 24.0;
        let project = Project::single(comp);

        let scene = evaluate_project(&project, 0.0);
        assert_eq!(scene.items.len(), 1, "the layer still has a place on screen");
        assert!(scene.warnings.iter().any(|(id, m)| *id == NodeId(1) && m.contains("footage")));
    }

    /// Time remapping is authored in *source* frames, so it picks the frame
    /// directly rather than going through the rate conversion — but it is held
    /// to the same bounds, so a curve running past the end holds the last
    /// frame instead of asking for one that isn't there.
    #[test]
    fn time_remapping_picks_a_source_frame_directly_and_still_clamps() {
        use crate::asset::{Asset, AssetId};
        use crate::node::{Comp, Project};

        let mut layer = Node::group(1, "clip");
        layer.shape = Some(Shape::Image {
            asset: AssetId(0),
            size: Value::constant(Vec2::new(10.0, 10.0)),
            // Freeze on source frame 3 regardless of the comp's clock.
            time_remap: Some(Value::constant(3.0)),
        });
        layer.fill = Some(Value::constant(Color::rgb(1.0, 1.0, 1.0)));
        let mut comp = Comp::new(640.0, 360.0, Node::group(0, "root").with_child(layer));
        // Set directly, not through `set_fps`: this comp has no keys to re-grid.
        comp.fps = 48.0;
        let mut project = Project::single(comp);
        project.add_asset(Asset::video(AssetId(9), "clip.mp4", 10.0, 10.0, 5, 24.0));

        for frame in [0.0, 30.0, 200.0] {
            let scene = evaluate_project(&project, frame);
            assert_eq!(scene.items[0].image.map(|i| i.source_frame), Some(3));
        }
    }

    /// A group draws nothing, so it produces no `RenderItem` — but it is
    /// exactly the sort of layer you parent things to and animate, so it must
    /// still report a pivot. This is what lets the editor draw a motion path
    /// for a null.
    #[test]
    fn a_group_has_no_render_item_but_still_has_a_pivot() {
        let mut group = Node::group(1, "null");
        group.transform.position = Value::constant(Vec2::new(30.0, 40.0));
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(group));

        let scene = evaluate(&doc, 0.0);
        assert!(scene.items.is_empty(), "a bare group renders nothing");
        assert_eq!(scene.pivot(NodeId(1)), Some(kurbo::Point::new(30.0, 40.0)));
    }

    /// A group's `world` matrix must carry the whole parent chain, not just its
    /// own transform — an on-canvas gizmo reads it to work out which way the
    /// layer's axes point, so a missing ancestor rotation would draw the
    /// handles pointing the wrong way while the pivot still looked right.
    #[test]
    fn a_groups_world_matrix_carries_the_parent_chain() {
        let mut child = Node::group(2, "child");
        child.transform.position = Value::constant(Vec2::new(10.0, 0.0));

        let mut parent = Node::group(1, "parent");
        parent.transform.position = Value::constant(Vec2::new(100.0, 0.0));
        parent.transform.rotation_deg = Value::constant(90.0);

        let doc = Document::new(
            500.0,
            500.0,
            Node::group(0, "root").with_child(parent.with_child(child)),
        );
        let scene = evaluate(&doc, 0.0);
        assert!(scene.items.is_empty(), "still nothing drawable");

        let world = scene.place(NodeId(2)).expect("a group has a place").world;
        // The parent turns a quarter turn, so the child's local +X points along
        // comp +Y. Its origin lands at (100,0) + rot90*(10,0) = (100,10).
        let origin = world * kurbo::Point::ZERO;
        assert!((origin.x - 100.0).abs() < 1e-9, "x {}", origin.x);
        assert!((origin.y - 10.0).abs() < 1e-9, "y {}", origin.y);
        let x_axis = (world * kurbo::Point::new(1.0, 0.0)) - origin;
        assert!(x_axis.x.abs() < 1e-9 && (x_axis.y - 1.0).abs() < 1e-9, "{x_axis:?}");
    }

    /// The pivot is the *anchor* in comp space, not the local origin, and it
    /// composes through the parent chain. Anything else and an overlay drawn
    /// at the pivot would sit away from where the layer turns.
    #[test]
    fn a_pivot_is_the_anchor_through_the_whole_parent_chain() {
        let mut child = Node::group(2, "child");
        child.transform.position = Value::constant(Vec2::new(10.0, 0.0));
        child.transform.anchor = Value::constant(Vec2::new(5.0, 5.0));

        let mut parent = Node::group(1, "parent");
        parent.transform.position = Value::constant(Vec2::new(100.0, 100.0));
        let doc = Document::new(
            500.0,
            500.0,
            Node::group(0, "root").with_child(parent.with_child(child)),
        );

        let scene = evaluate(&doc, 0.0);
        // local maps anchor -> position, so the child's pivot is its position
        // in the parent's space, offset by the parent's own position.
        assert_eq!(scene.pivot(NodeId(2)), Some(kurbo::Point::new(110.0, 100.0)));
        assert_eq!(scene.pivot(NodeId(1)), Some(kurbo::Point::new(100.0, 100.0)));
    }

    /// A layer outside its time window is absent from the pivot table rather
    /// than reported at the origin — "the layer isn't here on this frame" is
    /// the signal the motion path breaks its polyline on, and a zero would
    /// draw a line to the corner instead.
    #[test]
    fn a_trimmed_layer_has_no_pivot_outside_its_window() {
        use crate::node::LayerTiming;
        let mut layer = Node::group(1, "clip");
        layer.transform.position = Value::constant(Vec2::new(20.0, 20.0));
        layer.timing = Some(LayerTiming { start: 0, in_: 10, out: 20 });
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(layer));

        assert_eq!(evaluate(&doc, 15.0).pivot(NodeId(1)), Some(kurbo::Point::new(20.0, 20.0)));
        assert_eq!(evaluate(&doc, 5.0).pivot(NodeId(1)), None, "before its in-point");
        assert_eq!(evaluate(&doc, 25.0).pivot(NodeId(1)), None, "after its out-point");
    }

    /// The canonical smoke test: a keyframed square whose position animates.
    /// Halfway between its two keys the evaluated render item's transform must
    /// reflect the midpoint.
    #[test]
    fn keyframed_square_is_halfway_at_midpoint() {
        let square = Node::shape(
            1,
            "square",
            Shape::Rect {
                size: Value::constant(Vec2::new(100.0, 100.0)),
                radius: Value::constant(0.0),
            },
        )
        .with_fill(Color::rgb(1.0, 0.0, 0.0))
        .with_transform(Transform {
            position: Value::Keyframed(Track::new(vec![
                Keyframe::linear(0, Vec2::new(0.0, 0.0)),
                Keyframe::linear(24, Vec2::new(200.0, 100.0)),
            ])),
            ..Transform::default()
        });

        let doc = Document::new(1920.0, 1080.0, Node::group(0, "root").with_child(square));

        let scene = evaluate(&doc, 12.0);
        assert_eq!(scene.items.len(), 1, "one drawable expected");
        assert!(scene.warnings.is_empty(), "no warnings expected");

        // The translation component of the resolved matrix = the eased position.
        let coeffs = scene.items[0].transform.as_coeffs();
        let (tx, ty) = (coeffs[4], coeffs[5]);
        assert!((tx - 100.0).abs() < 1e-3, "tx = {tx}");
        assert!((ty - 50.0).abs() < 1e-3, "ty = {ty}");
    }

    #[test]
    fn opacity_multiplies_down_the_tree() {
        let child = Node::shape(2, "c", Shape::Ellipse { size: Value::constant(Vec2::new(10.0, 10.0)) })
            .with_transform(Transform { opacity: Value::constant(0.5), ..Transform::default() });
        let parent = Node::group(1, "g")
            .with_transform(Transform { opacity: Value::constant(0.5), ..Transform::default() })
            .with_child(child);
        let doc = Document::new(100.0, 100.0, parent);

        let scene = evaluate(&doc, 0.0);
        assert!((scene.items[0].opacity - 0.25).abs() < 1e-6);
    }

    #[test]
    fn an_expression_drives_an_evaluated_property() {
        use crate::expr::{Expr, PropPath};
        // A driver node holds opacity 0.4; a visible square mirrors it via an
        // expression. The evaluated square's opacity should be the driver's.
        let driver = Node::group(1, "driver")
            .with_transform(Transform { opacity: Value::constant(0.4), ..Transform::default() });
        let square = Node::shape(
            2,
            "square",
            Shape::Rect { size: Value::constant(Vec2::new(10.0, 10.0)), radius: Value::constant(0.0) },
        )
        .with_fill(Color::rgb(1.0, 1.0, 1.0))
        .with_transform(Transform {
            opacity: Value::expr(Expr::reference(NodeId(1), PropPath::Opacity)),
            ..Transform::default()
        });
        let doc =
            Document::new(100.0, 100.0, Node::group(0, "root").with_child(driver).with_child(square));

        let scene = evaluate(&doc, 0.0);
        let item = scene.items.iter().find(|i| i.source == NodeId(2)).unwrap();
        assert!((item.opacity - 0.4).abs() < 1e-9, "opacity = {}", item.opacity);
        assert!(scene.warnings.is_empty());
    }

    #[test]
    fn a_broken_script_warns_against_the_node_that_owns_it() {
        use crate::expr::Expr;
        // A script that can't compile: the frame still renders (the property
        // falls back to a neutral value) but the scene says which node broke.
        let square = Node::shape(
            7,
            "square",
            Shape::Rect {
                size: Value::constant(Vec2::new(10.0, 10.0)),
                radius: Value::constant(0.0),
            },
        )
        .with_transform(Transform {
            opacity: Value::expr(Expr::Script("this is not rhai".into())),
            ..Transform::default()
        });
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(square));
        let scene = evaluate(&doc, 0.0);
        assert_eq!(scene.items.len(), 1, "the frame still renders");
        let (node, msg) = scene.warnings.first().expect("a warning reached the scene");
        assert_eq!(*node, NodeId(7), "attributed to the node with the script");
        assert!(msg.contains("script"), "{msg}");
    }

    #[test]
    fn an_ambiguous_name_warns_rather_than_silently_picking() {
        use crate::expr::Expr;
        // Two nodes named "dup": a script referencing that name has to pick
        // one, but the choice depends on tree order, so it says so.
        let dup = |id: u64| {
            Node::group(id, "dup")
                .with_transform(Transform { opacity: Value::constant(0.5), ..Transform::default() })
        };
        let reader = Node::shape(
            9,
            "reader",
            Shape::Rect {
                size: Value::constant(Vec2::new(10.0, 10.0)),
                radius: Value::constant(0.0),
            },
        )
        .with_transform(Transform {
            opacity: Value::expr(Expr::Script("value(\"dup\", \"opacity\")".into())),
            ..Transform::default()
        });
        let doc = Document::new(
            100.0,
            100.0,
            Node::group(0, "root").with_child(dup(1)).with_child(dup(2)).with_child(reader),
        );
        let scene = evaluate(&doc, 0.0);
        let msg = scene
            .warnings
            .iter()
            .find(|(_, m)| m.contains("named 'dup'"))
            .map(|(_, m)| m.clone())
            .expect("the ambiguity should reach the scene");
        assert!(msg.contains('2'), "says how many: {msg}");
    }

    #[test]
    fn an_expression_cycle_surfaces_as_a_scene_warning() {
        use crate::expr::{Expr, PropPath};
        // A visible square whose opacity references itself: evaluate must return
        // (not hang) and report the cycle in the scene's warnings.
        let square = Node::shape(
            2,
            "square",
            Shape::Rect { size: Value::constant(Vec2::new(10.0, 10.0)), radius: Value::constant(0.0) },
        )
        .with_transform(Transform {
            opacity: Value::expr(Expr::reference(NodeId(2), PropPath::Opacity)),
            ..Transform::default()
        });
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(square));

        let scene = evaluate(&doc, 0.0);
        assert!(!scene.warnings.is_empty(), "the cycle should reach the scene");
    }

    /// A layer with a time range draws only inside `[in, out)`, and `out` is
    /// exclusive so two abutting clips never both draw on the seam frame.
    #[test]
    fn a_trimmed_layer_only_draws_inside_its_window() {
        use crate::node::LayerTiming;
        let square = Node::shape(
            1,
            "square",
            Shape::Rect { size: Value::constant(Vec2::new(10.0, 10.0)), radius: Value::constant(0.0) },
        )
        .with_timing(LayerTiming::new(10, 20));
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(square));

        let drawn = |f: f64| !evaluate(&doc, f).items.is_empty();
        assert!(!drawn(9.0), "before in");
        assert!(drawn(10.0), "on in");
        assert!(drawn(19.5), "inside, between frames");
        assert!(!drawn(20.0), "out is exclusive");
    }

    /// The payoff of local time: two layers share the *same* keyframes and play
    /// them at different comp times, purely from their `start`.
    #[test]
    fn keyframes_are_sampled_in_layer_local_time() {
        use crate::node::LayerTiming;
        let clip = |id: u64, at: i64| {
            Node::shape(
                id,
                "clip",
                Shape::Rect {
                    size: Value::constant(Vec2::new(10.0, 10.0)),
                    radius: Value::constant(0.0),
                },
            )
            .with_timing(LayerTiming::new(at, at + 24))
            .with_transform(Transform {
                position: Value::Keyframed(Track::new(vec![
                    Keyframe::linear(0, Vec2::new(0.0, 0.0)),
                    Keyframe::linear(24, Vec2::new(240.0, 0.0)),
                ])),
                ..Transform::default()
            })
        };
        let doc = Document::new(
            100.0,
            100.0,
            Node::group(0, "root").with_child(clip(1, 0)).with_child(clip(2, 100)),
        );

        // 12 frames into each clip, both sit at the same *local* midpoint.
        let x_of = |scene: &Scene, id: u64| {
            scene.items.iter().find(|i| i.source == NodeId(id)).unwrap().transform.as_coeffs()[4]
        };
        let early = evaluate(&doc, 12.0);
        let late = evaluate(&doc, 112.0);
        assert_eq!(early.items.len(), 1, "only the first clip is live at 12");
        assert!((x_of(&early, 1) - 120.0).abs() < 1e-6);
        assert!((x_of(&late, 2) - 120.0).abs() < 1e-6, "same animation, retimed");
    }

    /// Slipping moves the content under a fixed window: same `[in, out)`, later
    /// `start`, so the layer shows an earlier part of its animation.
    #[test]
    fn slipping_shifts_content_without_moving_the_window() {
        use crate::node::LayerTiming;
        let square = |start: i64| {
            Node::shape(
                1,
                "square",
                Shape::Rect {
                    size: Value::constant(Vec2::new(10.0, 10.0)),
                    radius: Value::constant(0.0),
                },
            )
            .with_timing(LayerTiming { start, in_: 0, out: 24 })
            .with_transform(Transform {
                position: Value::Keyframed(Track::new(vec![
                    Keyframe::linear(0, Vec2::new(0.0, 0.0)),
                    Keyframe::linear(24, Vec2::new(240.0, 0.0)),
                ])),
                ..Transform::default()
            })
        };
        let x_at = |start: i64| {
            let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(square(start)));
            evaluate(&doc, 12.0).items[0].transform.as_coeffs()[4]
        };
        assert!((x_at(0) - 120.0).abs() < 1e-6, "unslipped: local 12");
        assert!((x_at(6) - 60.0).abs() < 1e-6, "slipped later: local 6");
    }

    /// A timed layer must not leak its local frame onto the sibling that
    /// follows it — the shift is scoped to its own subtree.
    #[test]
    fn a_layers_local_time_does_not_leak_to_its_sibling() {
        use crate::node::LayerTiming;
        let animated = |id: u64| {
            Node::shape(
                id,
                "s",
                Shape::Rect {
                    size: Value::constant(Vec2::new(10.0, 10.0)),
                    radius: Value::constant(0.0),
                },
            )
            .with_transform(Transform {
                position: Value::Keyframed(Track::new(vec![
                    Keyframe::linear(0, Vec2::new(0.0, 0.0)),
                    Keyframe::linear(24, Vec2::new(240.0, 0.0)),
                ])),
                ..Transform::default()
            })
        };
        let shifted = animated(1).with_timing(LayerTiming { start: 6, in_: 0, out: 100 });
        let plain = animated(2);
        let doc =
            Document::new(100.0, 100.0, Node::group(0, "root").with_child(shifted).with_child(plain));

        let scene = evaluate(&doc, 12.0);
        let x = |id: u64| {
            scene.items.iter().find(|i| i.source == NodeId(id)).unwrap().transform.as_coeffs()[4]
        };
        assert!((x(1) - 60.0).abs() < 1e-6, "shifted layer at local 6");
        assert!((x(2) - 120.0).abs() < 1e-6, "sibling still at comp frame 12");
    }

    /// **The Stage 2 payoff.** Two clips of *different lengths* share one
    /// expression — opacity = `t01` — and each fades across its own duration.
    /// No keyframes, and nothing about the expression mentions either clip.
    #[test]
    fn one_expression_fits_itself_to_each_clips_length() {
        use crate::expr::{Expr, TimeSource};
        use crate::node::LayerTiming;
        let clip = |id: u64, in_: i64, out: i64| {
            Node::shape(
                id,
                "clip",
                Shape::Rect {
                    size: Value::constant(Vec2::new(10.0, 10.0)),
                    radius: Value::constant(0.0),
                },
            )
            .with_timing(LayerTiming::new(in_, out))
            .with_transform(Transform {
                opacity: Value::expr(Expr::Time(TimeSource::T01)),
                ..Transform::default()
            })
        };
        // A short clip and a long one, starting at different comp frames.
        let doc = Document::new(
            100.0,
            100.0,
            Node::group(0, "root").with_child(clip(1, 0, 10)).with_child(clip(2, 100, 200)),
        );

        let opacity_of = |frame: f64, id: u64| {
            evaluate(&doc, frame).items.iter().find(|i| i.source == NodeId(id)).unwrap().opacity
        };
        // Halfway through each clip — 5 frames into one, 50 into the other —
        // both are at 0.5, because each measures against its own length.
        assert!((opacity_of(5.0, 1) - 0.5).abs() < 1e-9, "short clip midpoint");
        assert!((opacity_of(150.0, 2) - 0.5).abs() < 1e-9, "long clip midpoint");
        // And each starts at 0.
        assert!(opacity_of(0.0, 1).abs() < 1e-9);
        assert!(opacity_of(100.0, 2).abs() < 1e-9);
    }

    /// The same thing through the Rhai scope rather than the IR — the two
    /// spellings have to agree, since they're one vocabulary.
    #[test]
    fn a_script_reads_the_same_layer_clock() {
        use crate::expr::Expr;
        use crate::node::LayerTiming;
        let square = Node::shape(
            1,
            "s",
            Shape::Rect { size: Value::constant(Vec2::new(10.0, 10.0)), radius: Value::constant(0.0) },
        )
        .with_timing(LayerTiming::new(20, 60))
        .with_transform(Transform {
            // Local in/out are 0/40 here, so this reads (localTime + in + out).
            opacity: Value::expr(Expr::Script("(localTime + inPoint + outPoint) / 100.0".into())),
            ..Transform::default()
        });
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(square));

        // Comp frame 30 → local 10, inPoint 0, outPoint 40 → 50/100.
        let scene = evaluate(&doc, 30.0);
        assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
        assert!((scene.items[0].opacity - 0.5).abs() < 1e-9, "{}", scene.items[0].opacity);
    }

    /// An untimed layer still has a meaningful clock: it reads as one clip
    /// spanning the composition, so `t01` works before any trimming exists.
    #[test]
    fn an_untimed_layer_reads_the_comp_as_its_window() {
        use crate::expr::{Expr, TimeSource};
        let square = Node::shape(
            1,
            "s",
            Shape::Rect { size: Value::constant(Vec2::new(10.0, 10.0)), radius: Value::constant(0.0) },
        )
        .with_transform(Transform {
            opacity: Value::expr(Expr::Time(TimeSource::T01)),
            ..Transform::default()
        });
        let mut doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(square));
        doc.fps = 10.0;
        doc.duration = 10.0; // 100 frames
        assert_eq!(doc.duration_frames(), 100);

        assert!((evaluate(&doc, 50.0).items[0].opacity - 0.5).abs() < 1e-9);
        assert!((evaluate(&doc, 100.0).items[0].opacity - 1.0).abs() < 1e-9);
    }

    /// `replace` must keep the node's place among its siblings — that ordering
    /// is draw order, so pre-composing a layer must not restack it.
    #[test]
    fn replace_keeps_the_layers_place_in_draw_order() {
        let mut root = Node::group(0, "root")
            .with_child(dot(1))
            .with_child(dot(2))
            .with_child(dot(3));
        let old = root.replace(NodeId(2), Node::group(9, "instance")).expect("found");
        assert_eq!(old.id, NodeId(2), "the old node comes back");
        let ids: Vec<u64> = root.children.iter().map(|c| c.id.0).collect();
        assert_eq!(ids, vec![1, 9, 3], "swapped in place, not appended");
    }

    /// A comp always shows *something* in the switcher, including one loaded
    /// from a file written before comps had names.
    #[test]
    fn a_nameless_comp_falls_back_to_a_generated_label() {
        use crate::node::CompId;
        let mut comp = Document::new(10.0, 10.0, Node::group(0, "r"));
        assert_eq!(comp.label(CompId(0)), "Comp 1", "1-based for humans");
        comp.name = "  ".into();
        assert_eq!(comp.label(CompId(3)), "Comp 4", "blank is still nameless");
        comp.name = "Subtitles".into();
        assert_eq!(comp.label(CompId(3)), "Subtitles");
    }

    // --- Multi-comp / pre-comps (stage 3) ---

    /// A 10x10 square at the origin, for building comps to nest.
    fn dot(id: u64) -> Node {
        Node::shape(
            id,
            "dot",
            Shape::Rect {
                size: Value::constant(Vec2::new(10.0, 10.0)),
                radius: Value::constant(0.0),
            },
        )
    }

    /// The reason for the registry: **one comp, placed twice**. Both instances
    /// render, each folded through its own layer's transform — which inline
    /// nesting could never express.
    #[test]
    fn one_comp_instanced_twice_renders_twice() {
        use crate::node::{CompId, Project};
        let inner = Document::new(100.0, 100.0, Node::group(0, "inner-root").with_child(dot(1)));
        let mut project = Project::single(inner);
        let inner_id = project.root;

        let place = |id: u64, x: f64| {
            Node::group(id, "instance")
                .with_precomp(inner_id)
                .with_transform(Transform {
                    position: Value::constant(Vec2::new(x, 0.0)),
                    ..Transform::default()
                })
        };
        let outer = Document::new(
            100.0,
            100.0,
            Node::group(10, "outer-root").with_child(place(11, 50.0)).with_child(place(12, 200.0)),
        );
        let outer_id = project.insert(outer);
        project.root = outer_id;

        let scene = evaluate_project(&project, 0.0);
        assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
        assert_eq!(scene.items.len(), 2, "both placements render");
        let mut xs: Vec<f64> =
            scene.items.iter().map(|i| i.transform.as_coeffs()[4]).collect();
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!((xs[0] - 50.0).abs() < 1e-9, "first instance at its layer's x");
        assert!((xs[1] - 200.0).abs() < 1e-9, "second at its own");
        // Provenance points at the *inner* node — both items came from it.
        assert!(scene.items.iter().all(|i| i.source == NodeId(1)));
    }

    /// A precomp layer's opacity folds into everything the nested comp emits —
    /// the "vector paste-through" compositing of v1.
    #[test]
    fn a_precomp_layer_folds_its_opacity_into_the_nested_comp() {
        use crate::node::Project;
        let inner_child = dot(1).with_transform(Transform {
            opacity: Value::constant(0.5),
            ..Transform::default()
        });
        let mut project =
            Project::single(Document::new(100.0, 100.0, Node::group(0, "i").with_child(inner_child)));
        let inner_id = project.root;
        let outer = Document::new(
            100.0,
            100.0,
            Node::group(10, "o").with_child(Node::group(11, "inst").with_precomp(inner_id).with_transform(
                Transform { opacity: Value::constant(0.5), ..Transform::default() },
            )),
        );
        project.root = project.insert(outer);

        let scene = evaluate_project(&project, 0.0);
        assert!((scene.items[0].opacity - 0.25).abs() < 1e-9, "0.5 × 0.5");
    }

    /// Trimming/slipping a precomp retimes **everything inside it**: the nested
    /// comp's own time is the layer's local frame. This is also where nested
    /// timing becomes properly relative, which stage 1 deliberately left open.
    #[test]
    fn a_precomp_is_retimed_by_its_layers_local_time() {
        use crate::node::{LayerTiming, Project};
        // Inner comp: a dot sliding 0 -> 240 over 24 frames of *its* time.
        let inner_child = dot(1).with_transform(Transform {
            position: Value::Keyframed(Track::new(vec![
                Keyframe::linear(0, Vec2::new(0.0, 0.0)),
                Keyframe::linear(24, Vec2::new(240.0, 0.0)),
            ])),
            ..Transform::default()
        });
        let mut project =
            Project::single(Document::new(100.0, 100.0, Node::group(0, "i").with_child(inner_child)));
        let inner_id = project.root;
        // Placed starting at comp frame 100.
        let outer = Document::new(
            100.0,
            100.0,
            Node::group(10, "o").with_child(
                Node::group(11, "inst").with_precomp(inner_id).with_timing(LayerTiming::new(100, 200)),
            ),
        );
        project.root = project.insert(outer);

        // Comp frame 112 = local 12 = the inner animation's midpoint.
        let scene = evaluate_project(&project, 112.0);
        assert_eq!(scene.items.len(), 1);
        assert!((scene.items[0].transform.as_coeffs()[4] - 120.0).abs() < 1e-9);
        // And outside the instance's window it doesn't draw at all.
        assert!(evaluate_project(&project, 99.0).items.is_empty());
    }

    /// Comp-level cycle guard, mirroring the expression one: a comp that
    /// contains itself must warn and stop, not recurse until the stack dies.
    #[test]
    fn a_self_containing_comp_warns_instead_of_hanging() {
        use crate::node::Project;
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "a")));
        let id = project.root;
        project
            .comp_mut(id)
            .unwrap()
            .root
            .children
            .push(Node::group(1, "self").with_precomp(id));

        let scene = evaluate_project(&project, 0.0);
        let (node, msg) = scene.warnings.first().expect("the cycle should warn");
        assert_eq!(*node, NodeId(1), "attributed to the instancing layer");
        assert!(msg.contains("itself"), "{msg}");
    }

    /// The indirect case: A instances B, B instances A. The guard is a stack of
    /// comps being evaluated, so it catches a cycle of any length.
    #[test]
    fn a_mutual_comp_cycle_warns() {
        use crate::node::{CompId, Project};
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "a")));
        let a = project.root;
        let b = project.insert(Document::new(100.0, 100.0, Node::group(10, "b")));
        // a contains b …
        project.comp_mut(a).unwrap().root.children.push(Node::group(1, "->b").with_precomp(b));
        // … and b contains a.
        project.comp_mut(b).unwrap().root.children.push(Node::group(11, "->a").with_precomp(a));

        let scene = evaluate_project(&project, 0.0);
        assert!(
            scene.warnings.iter().any(|(_, m)| m.contains("itself")),
            "expected a cycle warning, got {:?}",
            scene.warnings
        );
        assert_eq!(project.root, a, "sanity: root untouched");
        let _ = CompId(0);
    }

    /// An instance pointing at a deleted comp warns rather than drawing nothing
    /// silently — a blank frame is indistinguishable from a broken one.
    #[test]
    fn a_dangling_precomp_reference_warns() {
        use crate::node::{CompId, Project};
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "a")));
        let id = project.root;
        project
            .comp_mut(id)
            .unwrap()
            .root
            .children
            .push(Node::group(1, "ghost").with_precomp(CompId(999)));

        let scene = evaluate_project(&project, 0.0);
        assert!(
            scene.warnings.iter().any(|(_, m)| m.contains("no longer exists")),
            "{:?}",
            scene.warnings
        );
    }

    /// **The `.pbc` migration.** A pre-project document is exactly one comp, so
    /// an old file deserializes into `Comp` and `Project::single` wraps it —
    /// and a project round-trips like anything else.
    #[test]
    fn a_single_comp_document_becomes_a_one_comp_project() {
        use crate::node::Project;
        let json = r#"{"width":640.0,"height":480.0,"fps":24.0,"duration":5.0,
            "root":{"id":0,"name":"root","transform":{"anchor":{"Const":[0.0,0.0]},
            "position":{"Const":[0.0,0.0]},"rotation_deg":{"Const":0.0},
            "scale":{"Const":[1.0,1.0]},"opacity":{"Const":1.0}},
            "shape":null,"fill":null,"stroke":null,"children":[]}}"#;
        let legacy: Document = serde_json::from_str(json).unwrap();
        let mut project = Project::single(legacy);
        project.migrate();

        assert_eq!(project.comps.len(), 1);
        assert_eq!(project.root_comp().width, 640.0);

        let back: Project =
            serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
        assert_eq!(back.root, project.root);
        assert_eq!(back.root_comp().fps, 24.0);
    }

    /// A precomp layer evaluated through the single-comp entry point can't
    /// resolve — there's no registry — so it says so rather than rendering an
    /// empty frame that looks correct.
    #[test]
    fn a_precomp_without_a_project_warns() {
        use crate::node::CompId;
        let doc = Document::new(
            100.0,
            100.0,
            Node::group(0, "root").with_child(Node::group(1, "inst").with_precomp(CompId(0))),
        );
        let scene = evaluate(&doc, 0.0);
        assert!(
            scene.warnings.iter().any(|(_, m)| m.contains("needs a project")),
            "{:?}",
            scene.warnings
        );
    }


    // --- Shared animation modules (the document-wide property graph) ---

    /// **The story this feature exists for.** Three "subtitles" of different
    /// lengths and in-points all link *one* module for their fade. Each plays it
    /// fitted to its own clip, because the module's body reads `t01` and `t01`
    /// is whichever layer is resolving.
    #[test]
    fn one_module_drives_many_layers_each_fitted_to_its_own_clip() {
        use crate::expr::{Expr, TimeSource};
        use crate::node::{LayerTiming, Module, Project};

        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "root")));
        let comp_id = project.root;
        // The module: opacity = t01. One definition, mentioning no layer.
        let fade = project.add_module(Module::new("fade", Expr::Time(TimeSource::T01)));

        let subtitle = |id: u64, in_: i64, out: i64| {
            dot(id).with_timing(LayerTiming::new(in_, out)).with_transform(Transform {
                opacity: Value::expr(Expr::Use { module: fade, overrides: Vec::new() }),
                ..Transform::default()
            })
        };
        let root = &mut project.comp_mut(comp_id).unwrap().root;
        root.children.push(subtitle(1, 0, 10));
        root.children.push(subtitle(2, 100, 200));
        root.children.push(subtitle(3, 300, 340));

        let opacity_at = |frame: f64, id: u64| {
            evaluate_comp(&project, comp_id, frame)
                .items
                .iter()
                .find(|i| i.source == NodeId(id))
                .map(|i| i.opacity)
        };
        // Halfway through each clip - 5, 50 and 20 frames in - all read 0.5.
        assert!((opacity_at(5.0, 1).unwrap() - 0.5).abs() < 1e-9);
        assert!((opacity_at(150.0, 2).unwrap() - 0.5).abs() < 1e-9);
        assert!((opacity_at(320.0, 3).unwrap() - 0.5).abs() < 1e-9);
    }

    /// Editing the module in one place changes every link - the point of a
    /// module over copied expressions.
    #[test]
    fn editing_a_module_changes_every_link() {
        use crate::expr::{Expr, ExprValue};
        use crate::node::{Module, Project};
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "root")));
        let comp_id = project.root;
        let m = project.add_module(Module::new("dim", Expr::Lit(ExprValue::Num(0.2))));
        for id in 1..=3 {
            project.comp_mut(comp_id).unwrap().root.children.push(
                dot(id).with_transform(Transform {
                    opacity: Value::expr(Expr::Use { module: m, overrides: Vec::new() }),
                    ..Transform::default()
                }),
            );
        }
        let opacities = |p: &Project| {
            evaluate_comp(p, comp_id, 0.0).items.iter().map(|i| i.opacity).collect::<Vec<_>>()
        };
        assert!(opacities(&project).iter().all(|o| (o - 0.2).abs() < 1e-9));

        // One edit, at the definition site.
        project.module_mut(m).unwrap().body = Expr::Lit(ExprValue::Num(0.9));
        assert!(opacities(&project).iter().all(|o| (o - 0.9).abs() < 1e-9), "all three followed");
    }

    /// **Override is a layering, not a fork**: an overridden knob wins, and every
    /// knob left alone still inherits the module's default, so one diverging
    /// instance does not detach from the shared definition.
    #[test]
    fn an_override_replaces_one_knob_and_inherits_the_rest() {
        use crate::expr::{Expr, ExprValue};
        use crate::node::{Module, ParamValue, Project};
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "root")));
        let comp_id = project.root;
        // opacity = level * scale, both knobs.
        let m = project.add_module(
            Module::new(
                "two-knobs",
                Expr::bin(BinOp::Mul,Expr::Param { node: None, name: "level".into() },Expr::Param { node: None, name: "scale".into() }),
            )
            .with_param("level", ParamValue::Num(Value::constant(0.5)))
            .with_param("scale", ParamValue::Num(Value::constant(1.0))),
        );
        let link = |id: u64, overrides: Vec<(String, Expr)>| {
            dot(id).with_transform(Transform {
                opacity: Value::expr(Expr::Use { module: m, overrides }),
                ..Transform::default()
            })
        };
        let root = &mut project.comp_mut(comp_id).unwrap().root;
        root.children.push(link(1, Vec::new()));
        root.children.push(link(2, vec![("scale".into(), Expr::Lit(ExprValue::Num(0.5)))]));

        let scene = evaluate_comp(&project, comp_id, 0.0);
        assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
        let op = |id: u64| scene.items.iter().find(|i| i.source == NodeId(id)).unwrap().opacity;
        assert!((op(1) - 0.5).abs() < 1e-9, "inherits both defaults");
        // Overrode `scale` only; `level` still comes from the module.
        assert!((op(2) - 0.25).abs() < 1e-9, "0.5 inherited x 0.5 override");

        // And the definition still reaches the overridden instance.
        project.module_mut(m).unwrap().params[0].value = ParamValue::Num(Value::constant(1.0));
        let scene = evaluate_comp(&project, comp_id, 0.0);
        let op = |id: u64| scene.items.iter().find(|i| i.source == NodeId(id)).unwrap().opacity;
        assert!((op(2) - 0.5).abs() < 1e-9, "override layered over the new default");
    }

    /// A module that links itself warns and falls back, exactly as a property
    /// cycle and a comp cycle do.
    #[test]
    fn a_module_that_links_itself_warns() {
        use crate::expr::{Expr, ExprValue};
        use crate::node::{Module, Project};
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "root")));
        let comp_id = project.root;
        let m = project.add_module(Module::new("loop", Expr::Lit(ExprValue::Num(1.0))));
        project.module_mut(m).unwrap().body = Expr::Use { module: m, overrides: Vec::new() };
        project.comp_mut(comp_id).unwrap().root.children.push(dot(1).with_transform(Transform {
            opacity: Value::expr(Expr::Use { module: m, overrides: Vec::new() }),
            ..Transform::default()
        }));

        let scene = evaluate_comp(&project, comp_id, 0.0);
        assert!(
            scene.warnings.iter().any(|(_, msg)| msg.contains("links itself")),
            "{:?}",
            scene.warnings
        );
    }

    /// Typos are worth surfacing: an override naming a knob the module does not
    /// have would otherwise silently do nothing.
    #[test]
    fn overriding_an_unknown_knob_warns() {
        use crate::expr::{Expr, ExprValue};
        use crate::node::{Module, Project};
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "root")));
        let comp_id = project.root;
        let m = project.add_module(Module::new("plain", Expr::Lit(ExprValue::Num(1.0))));
        project.comp_mut(comp_id).unwrap().root.children.push(dot(1).with_transform(Transform {
            opacity: Value::expr(Expr::Use {
                module: m,
                overrides: vec![("typo".into(), Expr::Lit(ExprValue::Num(0.0)))],
            }),
            ..Transform::default()
        }));
        let scene = evaluate_comp(&project, comp_id, 0.0);
        assert!(
            scene.warnings.iter().any(|(_, msg)| msg.contains("no parameter")),
            "{:?}",
            scene.warnings
        );
    }

    /// A module link round-trips through `.pbc`, overrides included.
    #[test]
    fn a_project_with_modules_round_trips() {
        use crate::expr::{Expr, ExprValue, TimeSource};
        use crate::node::{Module, ParamValue, Project};
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "root")));
        let m = project.add_module(
            Module::new("fade", Expr::Time(TimeSource::T01))
                .with_param("amount", ParamValue::Num(Value::constant(0.5))),
        );
        let root_id = project.root;
        project.comp_mut(root_id).unwrap().root.children.push(dot(1).with_transform(Transform {
            opacity: Value::expr(Expr::Use {
                module: m,
                overrides: vec![("amount".into(), Expr::Lit(ExprValue::Num(0.25)))],
            }),
            ..Transform::default()
        }));
        let back: Project =
            serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
        assert_eq!(back.modules.len(), 1);
        assert_eq!(back.module(m).unwrap().name, "fade");
        assert_eq!(back.module(m).unwrap().params.len(), 1);
    }

    /// Serde `default` *is* the migration: a `.pbc` written before layer timing
    /// existed loads with `timing: None` and behaves exactly as it did.
    #[test]
    fn a_document_without_timing_still_loads() {
        use crate::node::LayerTiming;
        let json = r#"{"width":100.0,"height":100.0,"fps":24.0,"duration":5.0,
            "root":{"id":0,"name":"root","transform":{"anchor":{"Const":[0.0,0.0]},
            "position":{"Const":[0.0,0.0]},"rotation_deg":{"Const":0.0},
            "scale":{"Const":[1.0,1.0]},"opacity":{"Const":1.0}},
            "shape":null,"fill":null,"stroke":null,"children":[]}}"#;
        let mut doc: Document = serde_json::from_str(json).unwrap();
        doc.migrate();
        assert_eq!(doc.root.timing, None);

        // …and a timed layer round-trips.
        doc.root.timing = Some(LayerTiming { start: 3, in_: 5, out: 9 });
        let back: Document = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert_eq!(back.root.timing, Some(LayerTiming { start: 3, in_: 5, out: 9 }));
    }

    /// A text layer draws through the ordinary shape pipeline: `evaluate` must
    /// hand back real outline geometry, with the fill and transform every other
    /// shape gets. This is the whole payoff of resolving text to a `BezPath` —
    /// no renderer had to learn about text.
    #[test]
    fn a_text_layer_evaluates_to_outline_geometry() {
        let doc = Document::new(
            640.0,
            480.0,
            Node::group(0, "root").with_child(
                Node::shape(
                    1,
                    "caption",
                    Shape::Text {
                        content: Value::constant("Hi".to_string()),
                        family: String::new(),
                        size: Value::constant(48.0),
                        align: crate::text::TextAlign::Left,
                        max_width: None,
                    },
                )
                .with_fill(Color::rgb(1.0, 0.0, 0.0)),
            ),
        );
        let scene = evaluate(&doc, 0.0);
        assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
        assert_eq!(scene.items.len(), 1);
        assert!(!scene.items[0].path.is_empty(), "glyph outlines reached the draw list");
        assert!(scene.items[0].fill.is_some(), "text fills like any other shape");
    }

    /// The font size is a `Value`, so it animates — the reason it's a `Value`
    /// at all. Bigger size at a later frame ⇒ a bigger outline.
    #[test]
    fn a_text_layers_font_size_animates() {
        use crate::value::{Keyframe, Track};
        let doc = Document::new(
            640.0,
            480.0,
            Node::group(0, "root").with_child(Node::shape(
                1,
                "caption",
                Shape::Text {
                    content: Value::constant("Hi".to_string()),
                    family: String::new(),
                    size: Value::Keyframed(Track::new(vec![
                        Keyframe::linear(0, 20.0),
                        Keyframe::linear(10, 80.0),
                    ])),
                    align: crate::text::TextAlign::Left,
                    max_width: None,
                },
            )),
        );
        let width_at = |f: f64| {
            kurbo::Shape::bounding_box(&evaluate(&doc, f).items[0].path).width()
        };
        assert!(width_at(10.0) > width_at(0.0), "the keyframed size drives the glyphs");
    }

    /// A missing font still draws (parley substitutes), so the *only* signal
    /// that the wrong typeface is on screen is this warning. It rides the same
    /// `scene.warnings` channel as a broken script, which is what puts it behind
    /// the comp bar's yellow indicator for free.
    #[test]
    fn a_missing_font_warns_but_still_draws() {
        let text = |family: &str| Shape::Text {
            content: Value::constant("Hi".to_string()),
            family: family.into(),
            size: Value::constant(32.0),
            align: crate::text::TextAlign::Left,
            max_width: None,
        };
        let doc_with = |family: &str| {
            Document::new(
                640.0,
                480.0,
                Node::group(0, "root").with_child(Node::shape(1, "caption", text(family))),
            )
        };

        let scene = evaluate(&doc_with("NoSuchFontFamily-XYZZY"), 0.0);
        assert_eq!(scene.warnings.len(), 1, "the substitution is reported");
        assert_eq!(scene.warnings[0].0, NodeId(1), "blamed on the text layer");
        assert!(scene.warnings[0].1.contains("isn't installed"), "{}", scene.warnings[0].1);
        assert!(!scene.items[0].path.is_empty(), "and it still drew something");

        // The deliberate default must stay silent, or every new text layer
        // would ship with a warning on it.
        assert!(evaluate(&doc_with(""), 0.0).warnings.is_empty(), "blank family is silent");
    }

    /// Text must survive a save/load like any other shape — including the fields
    /// that aren't `Value`s.
    #[test]
    fn a_text_layer_round_trips_through_json() {
        let doc = Document::new(
            640.0,
            480.0,
            Node::group(0, "root").with_child(Node::shape(
                1,
                "caption",
                Shape::Text {
                    content: Value::constant("two\nlines".to_string()),
                    family: "Georgia".into(),
                    size: Value::constant(31.0),
                    align: crate::text::TextAlign::Center,
                    max_width: Some(250.0),
                },
            )),
        );
        let back: Document =
            serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        match back.root.children[0].shape.as_ref().unwrap() {
            Shape::Text { content, family, align, max_width, .. } => {
                assert_eq!(content.resolve(&mut EvalCtx::new(&back, 0.0)), "two\nlines");
                assert_eq!(family, "Georgia");
                assert_eq!(*align, crate::text::TextAlign::Center);
                assert_eq!(*max_width, Some(250.0));
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// A `.pbc` written while `content` was a plain `String` must still open.
    /// The bare string is read as a `Value::Const`, so the document arrives
    /// fully migrated with no separate `migrate()` pass — and the next save
    /// writes it in the current form.
    #[test]
    fn a_legacy_bare_string_content_still_loads() {
        // Hand-built rather than round-tripped: the point is a shape of JSON
        // this crate can no longer *produce*, only read.
        let json = r#"{"Text":{
            "content": "old caption",
            "family": "",
            "size": {"Const": 42.0},
            "align": "Left",
            "max_width": null
        }}"#;
        let shape: Shape = serde_json::from_str(json).expect("legacy content must parse");
        let Shape::Text { content, .. } = &shape else { panic!("expected text, got {shape:?}") };
        assert_eq!(content.resolve(&mut EvalCtx::at(0.0)), "old caption");
        assert!(!content.is_animated(), "a bare string is a constant, not a track");
    }

    /// The typewriter: the effect the whole string value model was built for.
    /// A script slices the caption by the frame, so the text reveals itself
    /// character by character — with no built-in "typewriter" anywhere in the
    /// engine, just a string property driven by an expression.
    #[test]
    fn a_script_can_type_text_out_over_time() {
        let doc = Document::new(
            640.0,
            480.0,
            Node::group(0, "root").with_child(Node::shape(
                1,
                "caption",
                Shape::Text {
                    content: Value::expr(crate::expr::Expr::Script(
                        // `to_int` matters: `frame` is a float and `len` an
                        // int, and Rhai won't compare the two. `min` guards the
                        // tail so the slice can't run off the end.
                        r#"let s = "HELLO"; s.sub_string(0, min(frame.to_int(), s.len))"#.into(),
                    )),
                    family: String::new(),
                    size: Value::constant(48.0),
                    align: crate::text::TextAlign::Left,
                    max_width: None,
                },
            )),
        );
        let at = |f: f64| {
            let mut ctx = EvalCtx::new(&doc, f);
            match doc.root.children[0].shape.as_ref().unwrap() {
                Shape::Text { content, .. } => content.resolve(&mut ctx),
                other => panic!("expected text, got {other:?}"),
            }
        };
        assert_eq!(at(0.0), "");
        assert_eq!(at(1.0), "H");
        assert_eq!(at(3.0), "HEL");
        assert_eq!(at(5.0), "HELLO");
        // Held, not truncated or panicking, once the string runs out.
        assert_eq!(at(50.0), "HELLO");
    }

    #[test]
    fn document_round_trips_through_json() {
        let doc = Document::new(
            640.0,
            480.0,
            Node::group(0, "root").with_child(Node::shape(
                1,
                "dot",
                Shape::Ellipse { size: Value::constant(Vec2::new(20.0, 20.0)) },
            )),
        );
        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.root.children.len(), 1);
    }
}
