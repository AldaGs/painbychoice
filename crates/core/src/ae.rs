//! After Effects import: reads the JSON an AE project is exported to (see
//! `docs/ae-parity-plan.md`) and builds a [`Project`] from it.
//!
//! **Lossy by design, and loud about it.** Whatever has no PBC equivalent yet
//! — expressions, most effects, text animators — is counted into an
//! [`ImportReport`] rather than silently dropped, so the report is the to-do
//! list for AE parity and shrinks as features land.
//!
//! AE draws layer 1 on top; PBC draws children in order, last on top, so a
//! comp's layers are reversed on the way in. Both put the origin at the comp's
//! top-left with y down, so positions carry over unchanged.

use std::collections::{BTreeMap, HashMap, HashSet};

use kurbo::Vec2;
use serde_json::Value as J;

use crate::asset::{Asset, AssetId, AssetKind};
use crate::composite::{BlendMode, Mask, MatteMode};
use crate::effect::{Effect, EffectKind};
use crate::path::{Anchor, VectorPath};
use crate::pathfinder::BoolOp;
use crate::node::{Comp, CompId, LayerTiming, LineCap, LineJoin, Node, NodeId, Project, StrokeStyle, TextAnimator, TextSelector, TextUnit, Shape, Stroke, Transform};
use crate::text::TextAlign;
use crate::value::{Animatable, Color, Handle, Interp, Keyframe, Track, Value};
use crate::vec3::Vec3;

/// What an import could not carry across, by category, with a count each.
#[derive(Debug, Default)]
pub struct ImportReport {
    pub dropped: BTreeMap<String, usize>,
}

impl ImportReport {
    fn drop(&mut self, what: impl Into<String>) {
        *self.dropped.entry(what.into()).or_default() += 1;
    }
}

/// Parse an exported AE project.
pub fn import(text: &str) -> Result<(Project, ImportReport), String> {
    let doc: J = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let root = doc.get("root").ok_or("no `root` item")?;

    // Every project item by id, flattened out of the folder tree.
    let mut items = HashMap::new();
    collect(root, &mut items);
    let mut comps: Vec<&J> = items.values().copied().filter(|i| kind(i) == "Composition").collect();
    comps.sort_by_key(|c| id(c));
    if comps.is_empty() {
        return Err("no compositions in the project".into());
    }

    // Comp ids are assigned up front so a precomp layer can point at a comp
    // that has not been built yet.
    let comp_ids: HashMap<u64, CompId> =
        comps.iter().enumerate().map(|(i, c)| (id(c), CompId(i as u64))).collect();

    let mut report = ImportReport::default();
    // File footage becomes project assets, keyed by AE item id. Only the
    // reference is stored: the file is read when it is drawn, so a path that
    // does not exist on this machine warns at render time and can be relinked.
    let mut assets = BTreeMap::new();
    let mut footage = HashMap::new();
    let mut files: Vec<&J> = items.values().copied().filter(|i| i["footageType"] == "file").collect();
    files.sort_by_key(|i| id(i));
    for item in files {
        let path = item["file"].as_str().unwrap_or("");
        let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let kind = match ext.as_str() {
            "mov" | "mp4" | "m4v" | "avi" | "mxf" | "webm" | "mkv" | "gif" => AssetKind::Video,
            "png" | "jpg" | "jpeg" | "tif" | "tiff" | "psd" | "ai" | "pdf" | "exr" | "bmp" | "tga" | "webp" | "heic"
            | "svg" => AssetKind::Image,
            _ => continue,
        };
        let aid = AssetId(assets.len() as u64);
        let fps = num(item, "framerate", 0.0);
        let mut asset = Asset::image(aid, path, num(item, "width", 0.0), num(item, "height", 0.0));
        asset.kind = kind;
        // One layer of a layered PSD / AI file, as AE's comp import makes.
        let sl = &item["sourceLayer"];
        if sl.is_object() {
            let b = nums(&sl["bounds"]);
            asset.layer = Some(crate::asset::SourceLayer {
                index: sl["index"].as_i64().filter(|i| *i >= 0).map(|i| i as u32),
                name: sl["name"].as_str().unwrap_or("").to_string(),
                bounds: [0, 1, 2, 3].map(|i| b.get(i).copied().unwrap_or(0.0)),
                full_frame: sl["fullFrame"] == true,
            });
        }
        if kind == AssetKind::Video {
            asset.fps = fps;
            asset.frames = (num(item, "seconds", 0.0) * fps).round().max(1.0) as i64;
        }
        assets.insert(aid, asset);
        footage.insert(id(item), aid);
    }

    let mut built = BTreeMap::new();
    for c in &comps {
        let comp = Importer {
            items: &items,
            comp_ids: &comp_ids,
            footage: &footage,
            centred: HashMap::new(),
            comp_size: Vec2::new(num(c, "width", 1920.0), num(c, "height", 1080.0)),
            report: &mut report,
            next_id: 1,
            fps: num(c, "framerate", 30.0),
            expr_layers: Vec::new(),
            expr_layer: J::Null,
            translated: HashSet::new(),
        }
        .comp(c);
        built.insert(comp_ids[&id(c)], comp);
    }

    // The root is the comp nothing else instances — the one AE would render.
    // Several candidates: the longest, then the first.
    let used: std::collections::HashSet<u64> = comps
        .iter()
        .flat_map(|c| c["layers"].as_array().into_iter().flatten())
        .map(|l| l["sourceId"].as_u64().unwrap_or(0))
        .collect();
    let root_comp = comps
        .iter()
        .filter(|c| !used.contains(&id(c)))
        .max_by(|a, b| num(a, "seconds", 0.0).total_cmp(&num(b, "seconds", 0.0)).then(id(b).cmp(&id(a))))
        .unwrap_or(&comps[0]);

    let mut project = Project::single(built.remove(&CompId(0)).unwrap());
    project.comps.extend(built);
    project.root = comp_ids[&id(root_comp)];
    project.assets = assets;
    project.migrate();
    Ok((project, report))
}

fn collect<'a>(item: &'a J, out: &mut HashMap<u64, &'a J>) {
    out.insert(id(item), item);
    for c in item["children"].as_array().into_iter().flatten() {
        collect(c, out);
    }
}

fn id(j: &J) -> u64 {
    j["id"].as_u64().unwrap_or(0)
}
fn kind(j: &J) -> &str {
    j["type"].as_str().unwrap_or("")
}
fn num(j: &J, key: &str, or: f64) -> f64 {
    j[key].as_f64().unwrap_or(or)
}
fn name(j: &J) -> String {
    j["label"].as_str().or(j["name"].as_str()).unwrap_or("").to_string()
}

/// A property's static value as numbers: AE writes scalars as one-element
/// arrays and vectors as arrays, but tolerate a bare number too.
fn nums(v: &J) -> Vec<f64> {
    match v {
        J::Number(n) => vec![n.as_f64().unwrap_or(0.0)],
        J::Array(a) => a.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect(),
        _ => Vec::new(),
    }
}

fn hex_color(s: &str) -> Option<Color> {
    let s = s.strip_prefix('#')?;
    let c = |i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok().map(|v| v as f64 / 255.0);
    Some(Color::rgb(c(0)?, c(2)?, c(4)?))
}

/// The AE speed/influence ease of one segment as PBC's normalized cubic
/// handles: x is the influence, y the speed scaled into the segment's value
/// change. `dist` is the segment's value change (for a spatial property, the
/// distance travelled); a segment that goes nowhere eases flat.
fn ae_handles(out_k: &J, in_k: &J, dt: f64, dist: f64) -> (Handle, Handle) {
    let first = |k: &J, f: &str| nums(&k[f]).first().copied().unwrap_or(0.0);
    let linear = |k: &J, f: &str| k[f].as_str() != Some("Bezier");
    let out = if linear(out_k, "outInterpolation") {
        Handle::LINEAR_OUT
    } else {
        let x = first(out_k, "outInfluence").clamp(0.001, 1.0);
        let y = if dist.abs() > 1e-9 { first(out_k, "outSpeed") * x * dt / dist } else { 0.0 };
        Handle::new(x, y)
    };
    let into = if linear(in_k, "inInterpolation") {
        Handle::LINEAR_IN
    } else {
        let x = first(in_k, "inInfluence").clamp(0.001, 1.0);
        let y = if dist.abs() > 1e-9 { first(in_k, "inSpeed") * x * dt / dist } else { 0.0 };
        Handle::new(1.0 - x, 1.0 - y)
    };
    (out, into)
}

struct Importer<'a> {
    items: &'a HashMap<u64, &'a J>,
    comp_ids: &'a HashMap<u64, CompId>,
    footage: &'a HashMap<u64, AssetId>,
    /// Layers whose content PBC draws centred on the origin (footage), with
    /// the offset that puts AE's top-left layer space back: anything placed
    /// in their layer space — linked children, masks — shifts by it.
    centred: HashMap<NodeId, Vec2>,
    /// The size of the comp being built, for AE's default layer position.
    comp_size: Vec2,
    report: &'a mut ImportReport,
    next_id: u64,
    fps: f64,
    /// What an expression can name: the comp's layers (`thisComp.layer`) and
    /// the layer it sits on (`effect`).
    expr_layers: Vec<J>,
    expr_layer: J,
    /// Expressions on the current layer that became PBC values, so the report
    /// counts only the rest.
    translated: HashSet<String>,
}

impl Importer<'_> {
    fn fresh(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn frame(&self, seconds: f64) -> i64 {
        (seconds * self.fps).round() as i64
    }

    fn comp(&mut self, c: &J) -> Comp {
        let mut root = Node::group(1, "root");
        let mut layers = c["layers"].as_array().cloned().unwrap_or_default();
        self.expr_layers = layers.clone();
        // AE >= 23 can take a matte from the layer *below*; PBC takes it from
        // the one above. The matte layer is consumed rather than drawn, so
        // swapping the pair changes nothing visible.
        let mut k = 0;
        while k + 1 < layers.len() {
            let below = layers[k + 1]["layerId"].as_u64();
            if layers[k]["trackMatte"].is_string() && below.is_some() && layers[k]["matteLayer"].as_u64() == below {
                layers.swap(k, k + 1);
                k += 1;
            }
            k += 1;
        }
        // AE parents and mattes name layers by their ldta id.
        let mut by_ae_id = HashMap::new();
        for (i, l) in layers.iter().enumerate().rev() {
            let above = i.checked_sub(1).and_then(|a| layers[a]["layerId"].as_u64());
            if let Some(n) = self.layer(l, above) {
                by_ae_id.insert(l["layerId"].as_u64().unwrap_or(0), n.id);
                root.children.push(n);
            }
        }
        for (l, n) in layers.iter().rev().zip(root.children.iter_mut()) {
            if let Some(parent) = l["parent"].as_u64() {
                match by_ae_id.get(&parent) {
                    Some(&id) => {
                        n.link = Some(id);
                        if let Some(&c) = self.centred.get(&id) {
                            n.transform.position = shift(n.transform.position.clone(), -c);
                        }
                    }
                    None => self.report.drop("parent that was not imported (camera/light)"),
                }
            }
        }
        let mut comp = Comp::new(num(c, "width", 1920.0), num(c, "height", 1080.0), root);
        comp.name = name(c);
        comp.fps = self.fps;
        comp.duration_frames = self.frame(num(c, "seconds", 0.0)).max(1);
        if let Some(bg) = c["backgroundColor"].as_str().and_then(hex_color) {
            comp.bg = bg;
        }
        comp
    }

    /// One AE layer. Its kind is inferred, since the export does not name it:
    /// a source comp makes a precomp, text makes a text layer, shape contents a
    /// shape layer, and no source a null.
    fn layer(&mut self, l: &J, above: Option<u64>) -> Option<Node> {
        self.expr_layer = l.clone();
        self.translated.clear();
        let layer_kind = l["kind"].as_str().unwrap_or("");
        if matches!(layer_kind, "camera" | "light" | "model" | "mesh") {
            self.report.drop(format!("{layer_kind} layer"));
            return None;
        }
        let mut node = Node::group(self.fresh(), name(l));
        let start = num(l, "startTime", 0.0);
        // Keyframe times are comp times; PBC keys are on the layer's local
        // clock, which starts at `startTime`.
        let props = l["transform"].as_array().cloned().unwrap_or_default();
        node.transform = self.transform(&props, true);
        // The exporter leaves out properties at their default, and AE's
        // defaults are not zero: a layer sits at the comp centre, and a layer
        // with a sized source (footage, solid, precomp) turns about its centre.
        let has = |m: &str| prop(&props, m).is_some();
        // The separated X/Y are exported on every layer, so they only count
        // when they are what is animated (see `position`).
        let keyed = |m: &str| prop(&props, m).is_some_and(|p| p.get("keyframes").is_some());
        if !has("ADBE Position") && !keyed("ADBE Position_0") && !keyed("ADBE Position_1") {
            let c = self.comp_size / 2.0;
            node.transform.position = Value::constant(Vec3::new(c.x, c.y, 0.0));
        }
        // A layer with a sized source (footage, solid, precomp) stores its
        // anchor as a fraction of the source — z of its height — and a
        // left-out one is the source's centre. Source-less layers store pixels.
        let src = l["sourceId"].as_u64().and_then(|s| self.items.get(&s)).filter(|i| i.get("width").is_some());
        if let Some(i) = src {
            let (w, h) = (num(i, "width", 0.0), num(i, "height", 0.0));
            node.transform.anchor = if has("ADBE Anchor Point") {
                match std::mem::replace(&mut node.transform.anchor, Value::constant(Vec3::ZERO)) {
                    Value::Const(a) => Value::Const(Vec3::new(a.x * w, a.y * h, a.z * h)),
                    Value::Keyframed(t) => Value::Keyframed(t.map_value(|a| Vec3::new(a.x * w, a.y * h, a.z * h))),
                    e => e,
                }
            } else if layer_kind == "null" {
                // A null's default anchor is its top-left corner, not its centre.
                Value::constant(Vec3::ZERO)
            } else {
                Value::constant(Vec3::new(w / 2.0, h / 2.0, 0.0))
            };
        }

        let in_ = l["inPoint"].as_f64().unwrap_or(start);
        if l.get("duration").is_some() || l.get("startTime").is_some() {
            node.timing = Some(LayerTiming {
                start: self.frame(start),
                in_: self.frame(in_),
                out: self.frame(in_ + num(l, "duration", f64::MAX / 1e6)),
            });
        }
        let flags = &l["flags"];
        // A null draws nothing either way, and AE nulls are usually switched
        // off; that must not hide what links to them. The audio switch is on
        // for every layer by default, so it says nothing about sound.
        node.hidden = layer_kind != "null" && flags["video"] == J::Bool(false);
        if let Some(mode) = l["blendMode"].as_str() {
            match blend_mode(mode) {
                Some(b) => node.blend = b,
                None => self.report.drop(format!("blend mode: {mode}")),
            }
        }
        if let Some(matte) = l["trackMatte"].as_str() {
            // PBC takes a matte from the layer directly above. AE >= 23 can
            // name any layer; that only carries over when it is the one above.
            let from = l["matteLayer"].as_u64();
            let adjacent = from.is_none() || from == above;
            match (matte, adjacent) {
                ("alpha", true) => node.matte = Some(MatteMode::Alpha),
                ("alphaInverted", true) => node.matte = Some(MatteMode::AlphaInverted),
                (_, false) => self.report.drop("track matte from a non-adjacent layer"),
                (m, _) => self.report.drop(format!("track matte: {m}")),
            }
        }
        node.locked = flags["locked"] == J::Bool(true);
        if flags["adjustment"] == J::Bool(true) {
            self.report.drop("adjustment layer");
        }
        if flags["3D"] == J::Bool(true) {
            self.report.drop("3D layer (imported flat)");
        }

        let source = l["sourceId"].as_u64().unwrap_or(0);
        // How AE's normalized mask points map into this layer's space: scaled
        // by the source size, then offset. Shape and text layers use 1x1.
        let mut mask_space = (Vec2::new(1.0, 1.0), Vec2::ZERO);
        if let Some(&comp) = self.comp_ids.get(&source) {
            node.precomp = Some(comp);
        } else if let Some(text) = l["text"].as_str() {
            self.text(&mut node, text, &l["textStyle"]);
            let groups = l["textProperties"]["properties"].as_array().cloned().unwrap_or_default();
            let animators = prop(&groups, "ADBE Text Animators").and_then(|a| a["properties"].as_array()).cloned().unwrap_or_default();
            for a in &animators {
                if let Some(t) = self.text_animator(a, l) {
                    node.text_animators.push(t);
                }
            }
        } else if let Some(contents) = l["contents"].as_array() {
            node.children = self.shape_items(contents);
        } else if let Some(item) = self.items.get(&source).filter(|i| kind(i) == "Footage") {
            let size = Vec2::new(num(item, "width", 0.0), num(item, "height", 0.0));
            mask_space = (size, Vec2::ZERO);
            match item["footageType"].as_str() {
                Some("file") => match self.footage.get(&source) {
                    Some(&asset) => {
                        node.shape = Some(Shape::Image { asset, size: Value::constant(size), time_remap: None });
                        // PBC draws footage centred on the origin; AE's layer
                        // space starts at its top-left corner.
                        let c = size / 2.0;
                        node.transform.anchor = shift(node.transform.anchor, -c);
                        self.centred.insert(node.id, c);
                        mask_space = (size, -c);
                    }
                    None => self.report.drop("footage of an unsupported type (audio, data)"),
                },
                Some("solid") if layer_kind != "null" && flags["adjustment"] != J::Bool(true) => {
                    let rgb = nums(&item["solidColor"]);
                    // A few plugin solids store 0-255; everything else is 0-1.
                    let k = if rgb.iter().any(|c| *c > 1.0) { 1.0 / 255.0 } else { 1.0 };
                    let ch = |i: usize| rgb.get(i).copied().unwrap_or(1.0) * k;
                    let corners = [Vec2::ZERO, Vec2::new(size.x, 0.0), size, Vec2::new(0.0, size.y)];
                    node.shape = Some(Shape::Vector { path: VectorPath::polyline(corners, true) });
                    node.fill = Some(Value::constant(Color::rgb(ch(0), ch(1), ch(2))));
                }
                Some("solid") => {}
                _ => self.report.drop("missing footage (placeholder)"),
            }
        }

        for e in l["effects"].as_array().into_iter().flatten() {
            match self.effect(e) {
                Some(fx) => node.effects.push(fx),
                // Expression controls draw nothing; expressions read them inline.
                // Type Array's controls are read by its text animator.
                None if e["matchName"].as_str().is_some_and(|m| m.ends_with(" Control") || m.starts_with("Pseudo/Type Array")) => {}
                // Offset at its defaults (no stored values) moves nothing.
                None if e["matchName"] == "ADBE Offset" && e["properties"].as_array().is_none_or(|p| p.is_empty()) => {}
                None => self.report.drop(format!("effect: {}", e["name"].as_str().unwrap_or("?"))),
            }
        }
        // A mask in mode None does nothing to the layer's alpha in AE either;
        // it is there for effects (Stroke, Trace Path) to read.
        let masks: Vec<&J> =
            l["masks"].as_array().into_iter().flatten().filter(|m| m["maskMode"] != "none").collect();
        if let Some(m) = masks.first() {
            let props = m["properties"].as_array().cloned().unwrap_or_default();
            // Subtract is an inverted Add for a single mask.
            let inverted = (m["inverted"] == true) != (m["maskMode"] == "subtract");
            match (m["maskMode"].as_str(), prop(&props, "ADBE Mask Shape").filter(|p| p["path"].is_object())) {
                (Some("add" | "subtract") | None, Some(p)) => {
                    let path = self.path(p, mask_space.0, mask_space.1);
                    node.mask = Some(Mask { inverted, ..Mask::new(Shape::Vector { path }) });
                }
                (_, None) => self.report.drop("mask without a path"),
                (Some(mode), _) => self.report.drop(format!("mask mode: {mode}")),
            }
        }
        for _ in masks.iter().skip(1) {
            self.report.drop("additional mask (only the first is kept)");
        }
        self.count_expressions(l);
        Some(node)
    }

    /// A text layer from its document-wide style. PBC picks fonts by family
    /// and centres text on its origin; AE names a PostScript font and puts a
    /// text layer's origin on the first baseline at the alignment edge, so the
    /// anchor (and anything placed in the layer's space) moves by the
    /// difference.
    fn text(&mut self, node: &mut Node, text: &str, style: &J) {
        let size = num(style, "fontSize", 72.0).max(1.0);
        let align = match style["justification"].as_str().unwrap_or("left") {
            "center" | "justifyCenter" => TextAlign::Center,
            "right" | "justifyRight" => TextAlign::Right,
            _ => TextAlign::Left,
        };
        let ps = style["font"].as_str().unwrap_or("");
        let family = self.family(ps);
        let rgb = |k: &str| {
            let c = nums(&style[k]);
            (c.len() >= 3).then(|| Color::rgb(c[0], c[1], c[2]))
        };
        if style["applyFill"] != false {
            node.fill = Some(Value::constant(rgb("fillColor").unwrap_or(Color::rgb(1.0, 1.0, 1.0))));
        }
        if style["applyStroke"] == true {
            node.stroke = Some(Stroke {
                color: Value::constant(rgb("strokeColor").unwrap_or(Color::rgb(0.0, 0.0, 0.0))),
                width: Value::constant(num(style, "strokeWidth", 1.0)),
                style: Default::default(),
            });
        }
        if num(style, "tracking", 0.0) != 0.0 {
            self.report.drop("text tracking");
        }
        if num(style, "leading", 0.0) != 0.0 {
            self.report.drop("text leading");
        }
        let content = text.to_string();
        // Paragraph text wraps inside a box given in layer space; PBC centres
        // the wrapped block, so line its aligned edge and top up with the box.
        let (bp, bs) = (nums(&style["boxPos"]), nums(&style["boxSize"]));
        let boxed = (bp.len() == 2 && bs.len() == 2 && bs[0] > 0.0).then(|| (Vec2::new(bp[0], bp[1]), Vec2::new(bs[0], bs[1])));
        let max_width = boxed.map(|(_, s)| s.x);
        let origin = match boxed {
            Some((p, s)) => crate::text::text_size(&content, &family, size, align, max_width).map(|b| {
                let x = match align {
                    TextAlign::Left => -b.x / 2.0 - p.x,
                    TextAlign::Center => -(p.x + s.x / 2.0),
                    TextAlign::Right => b.x / 2.0 - (p.x + s.x),
                };
                Vec2::new(x, -b.y / 2.0 - p.y)
            }),
            None => crate::text::text_origin(&content, &family, size, align, None),
        };
        if let Some(o) = origin {
            node.transform.anchor = shift(node.transform.anchor.clone(), o);
            self.centred.insert(node.id, -o);
        }
        node.shape = Some(Shape::Text { content: Value::constant(content), family, size: Value::constant(size), align, max_width });
    }

    /// The installed family a PostScript font name belongs to: the longest
    /// family whose name, letters and digits only, starts the PostScript name
    /// ("RocGrotesk-CompressedBold" is in "Roc Grotesk Compressed"). PBC text
    /// has no weight or style, so a remainder like "Bold" is reported.
    fn family(&mut self, ps: &str) -> String {
        if ps.is_empty() {
            return String::new();
        }
        let key = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
        let want = key(ps);
        let best = crate::text::system_families()
            .into_iter()
            .filter(|f| !key(f).is_empty() && want.starts_with(&key(f)))
            .max_by_key(|f| key(f).len());
        match best {
            Some(f) => {
                let rest = &want[key(&f).len()..];
                if !rest.is_empty() && !matches!(rest, "regular" | "roman" | "book") {
                    self.report.drop("font weight/style (family only)");
                }
                f
            }
            None => {
                self.report.drop(format!("font not installed: {ps}"));
                String::new()
            }
        }
    }

    /// An AE expression on `p` rewritten as a PBC (Rhai) script, for the
    /// patterns rigs are built from: controller and effect references
    /// (inlined, recursively), `thisComp` size, `time`, `value`, `Math.*`, and
    /// a ternary. `None` for anything else; the rewrite is only kept if it
    /// runs.
    /// `on` is the layer `p` belongs to, which a bare `effect(...)` names.
    fn script(&self, p: &J, on: &J, depth: u32) -> Option<String> {
        let src = p["expression"].as_str().map(str::trim).filter(|e| !e.is_empty())?;
        if depth > 8 || src.contains('\n') && src.lines().filter(|l| !l.trim().is_empty()).count() > 1 {
            return None;
        }
        // The constant-stroke idiom divides by the layer's scale in comp
        // space; PBC strokes scale with their layer, so it folds to a number.
        if src.replace(' ', "").contains("length(toComp([0,0]),toComp([0.7071,0.7071]))") {
            let (sx, sy) = self.comp_scale(on);
            let k = ((sx * sx + sy * sy) / 2.0).sqrt().max(0.001);
            return literal(&nums(&p["value"]).iter().map(|v| v / k).collect::<Vec<_>>());
        }
        let mut s = src.trim_end_matches(';').to_string();
        // References first, while their quoted names are intact.
        let mut out = String::new();
        let mut rest = s.as_str();
        while let Some(i) = [rest.find("thisComp.layer(\""), rest.find("effect(\"")].into_iter().flatten().min() {
            out.push_str(&rest[..i]);
            let mut r = &rest[i..];
            let layer = match r.strip_prefix("thisComp.layer(\"") {
                Some(t) => {
                    let (name, t) = quoted(t)?;
                    r = t.strip_prefix('.')?;
                    self.expr_layers.iter().find(|l| l["name"] == name)?
                }
                None => on,
            };
            let (fx, t) = quoted(r.strip_prefix("effect(\"")?)?;
            let (param, t) = quoted(t.strip_prefix("(\"")?)?;
            let e = layer["effects"].as_array()?.iter().find(|e| e["label"] == fx || e["name"] == fx)?;
            let q = e["properties"].as_array()?.iter().find(|q| q["name"] == param)?;
            let v = match self.script(q, layer, depth + 1) {
                Some(inner) => inner,
                None if q["matchName"].as_str().is_some_and(|m| m.contains("Checkbox")) => (nums(&q["value"]).first().is_some_and(|v| *v != 0.0)).to_string(),
                None => literal(&nums(&q["value"]))?,
            };
            out.push_str(&format!("({v})"));
            rest = t;
        }
        out.push_str(rest);
        s = out;
        for (from, to) in [
            ("thisComp.width", format!("{:?}", self.comp_size.x)),
            ("thisComp.height", format!("{:?}", self.comp_size.y)),
            ("thisComp.frameDuration", format!("{:?}", 1.0 / self.fps)),
            ("Math.PI", "PI()".into()),
            ("Math.ceil", "ceiling".into()),
            ("Math.", "".into()),
        ] {
            s = s.replace(from, &to);
        }
        s = floats(&s);
        s = word(&s, "time", &format!("(frame / {:?})", self.fps));
        s = word(&s, "value", &literal(&nums(&p["value"]))?);
        // `c ? a : b` → `if c { a } else { b }`, one level deep.
        if let (Some(q), Some(c)) = (s.find('?'), s.rfind(':')) {
            if q < c {
                s = format!("if {} {{ {} }} else {{ {} }}", &s[..q].trim(), s[q + 1..c].trim(), s[c + 1..].trim());
            }
        }
        crate::expr::eval_script(&s, 0.0).ok()?;
        Some(s)
    }

    /// One AE text animator: its Position and Opacity, and its first selector
    /// — a Range selector (Square shape), or the Type Array expression
    /// selector, which becomes a stagger read from the layer's Type Array
    /// effect. Anything else is reported.
    fn text_animator(&mut self, a: &J, l: &J) -> Option<TextAnimator> {
        let groups = a["properties"].as_array().cloned().unwrap_or_default();
        let props = prop(&groups, "ADBE Text Animator Properties").and_then(|g| g["properties"].as_array()).cloned().unwrap_or_default();
        let selectors = prop(&groups, "ADBE Text Selectors").and_then(|g| g["properties"].as_array()).cloned().unwrap_or_default();
        let mut position = Vec2::ZERO;
        let mut opacity = None;
        for p in &props {
            let v = nums(&p["value"]);
            if p["keyframes"].as_array().is_some_and(|k| !k.is_empty()) || p["expression"].as_str().is_some_and(|e| !e.is_empty()) {
                self.report.drop("text animator property: animated (static value kept)");
            }
            match p["matchName"].as_str().unwrap_or("") {
                "ADBE Text Position 3D" if v.len() >= 2 => position = Vec2::new(v[0], v[1]),
                "ADBE Text Opacity" if !v.is_empty() => opacity = Some(v[0] / 100.0),
                m => self.report.drop(format!("text animator property: {m}")),
            }
        }
        let Some(sel) = selectors.first() else { return None };
        if selectors.len() > 1 {
            self.report.drop("text animator: additional selector");
        }
        let sp = sel["properties"].as_array().cloned().unwrap_or_default();
        let unit = |code: Option<f64>| match code.unwrap_or(1.0) as i64 {
            2 => TextUnit::CharsExcludingSpaces,
            3 => TextUnit::Words,
            4 => TextUnit::Lines,
            _ => TextUnit::Chars,
        };
        let code = |props: &[J], m: &str| prop(props, m).and_then(|p| nums(&p["value"]).first().copied());
        let pct = |me: &mut Self, m: &str, or: f64| me.value(prop(&sp, m), or, |v| v.first().copied(), |a, b| (a - b).abs());
        let (based_on, selector) = match sel["matchName"].as_str().unwrap_or("") {
            "ADBE Text Selector" => {
                let adv = prop(&sp, "ADBE Text Range Advanced").and_then(|g| g["properties"].as_array()).cloned().unwrap_or_default();
                if code(&adv, "ADBE Text Range Shape").is_some_and(|s| s != 1.0) {
                    self.report.drop("text range selector shape (Square used)");
                }
                if code(&adv, "ADBE Text Range Units").is_some_and(|u| u == 2.0) {
                    self.report.drop("text range selector by index");
                }
                let sel = TextSelector::Range {
                    start: pct(self, "ADBE Text Percent Start", 0.0),
                    end: pct(self, "ADBE Text Percent End", 100.0),
                    offset: pct(self, "ADBE Text Percent Offset", 0.0),
                };
                (unit(code(&adv, "ADBE Text Range Type2")), sel)
            }
            "ADBE Text Expressible Selector" => {
                let amount = prop(&sp, "ADBE Text Expressible Amount")?;
                let expr = amount["expression"].as_str().unwrap_or("");
                if !expr.contains("Type Array") {
                    return None;
                }
                self.translated.insert(expr.to_string());
                // The selector's name picks the effect: "<Name> - Selector 1".
                let label = sel["label"].as_str().unwrap_or("");
                let name = label.split('<').nth(1).and_then(|r| r.split('>').next()).unwrap_or("Type Array");
                let fx = l["effects"].as_array()?.iter().find(|e| {
                    e["matchName"].as_str().is_some_and(|m| m.starts_with("Pseudo/Type Array")) && (e["label"] == name || e["name"] == name)
                })?;
                let fp = fx["properties"].as_array().cloned().unwrap_or_default();
                let mn = fx["matchName"].as_str().unwrap_or("");
                let ctl = |i: u32| prop(&fp, &format!("{mn}-{i:04}")).and_then(|p| nums(&p["value"]).first().copied()).unwrap_or(0.0);
                let influence = prop(&fp, &format!("{mn}-0001")).cloned();
                // With Use Markers on, a layer marker sets each unit's delay
                // from the curve's first key.
                let first_key = influence.as_ref().and_then(|p| p["keyframes"][0]["time"].as_f64()).unwrap_or(0.0);
                let delays = match ctl(4) != 0.0 {
                    true => nums(&l["markers"]).iter().map(|m| (m - first_key) * self.fps).collect(),
                    false => Vec::new(),
                };
                let sel = TextSelector::Stagger {
                    amount: self.value(influence.as_ref(), 100.0, |v| v.first().copied(), |a, b| (a - b).abs()),
                    delays,
                    step: ctl(2),
                    shift: ctl(3),
                    reverse: ctl(5) != 0.0,
                };
                (unit(code(&sp, "ADBE Text Range Type2")), sel)
            }
            m => {
                self.report.drop(format!("text selector: {m}"));
                return None;
            }
        };
        Some(TextAnimator { based_on, selector, position, opacity })
    }

    /// The PBC effect for an AE one, where PBC has it. Parameters are read by
    /// their AE match name, `<effect>-<4-digit index>`.
    fn effect(&mut self, e: &J) -> Option<Effect> {
        let mn = e["matchName"].as_str()?;
        let props = e["properties"].as_array().cloned().unwrap_or_default();
        let p = |i: u32| prop(&props, &format!("{mn}-{i:04}")).cloned();
        // A parameter scaled by `k` from AE's units to PBC's.
        let mut f = |i: u32, or: f64| self.scalar(p(i).as_ref(), 1.0, or);
        let fk = |me: &mut Self, i: u32, k: f64, or: f64| me.scalar(p(i).as_ref(), k, or);
        // A static number, for parameters PBC folds into others.
        let n = |i: u32, or: f64| p(i).and_then(|q| nums(&q["value"]).first().copied()).unwrap_or(or);
        let kind = match mn {
            "ADBE Mosaic" => EffectKind::Mosaic { horizontal: f(1, 10.0), vertical: f(2, 10.0) },
            "ADBE Fractal Noise" => EffectKind::FractalNoise {
                contrast: f(4, 100.0),
                brightness: f(5, 0.0),
                scale: f(10, 100.0),
                complexity: f(15, 6.0),
                evolution: f(23, 0.0),
                seed: f(27, 0.0),
            },
            "APC Colorama" => {
                let (phase, cycles) = (f(4, 0.0), f(38, 1.0));
                let arb = prop(&props, "APC Colorama-0012").and_then(|p| p["arb"].as_str()).unwrap_or("");
                let palette = colorama_palette(arb);
                if palette.is_empty() {
                    return None;
                }
                EffectKind::Colorama { phase, cycles, palette }
            }
            "ADBE Unmult" => EffectKind::Unmult,
            "ADBE Gaussian Blur 2" => {
                // AE's Blurriness is about twice the Gaussian's sigma.
                EffectKind::GaussianBlur { radius: fk(self, 1, 0.5, 0.0) }
            }
            "ADBE Sharpen" => EffectKind::Sharpen { amount: fk(self, 1, 0.01, 0.0) },
            "ADBE Brightness & Contrast 2" => EffectKind::BrightnessContrast {
                brightness: fk(self, 1, 1.0 / 255.0, 0.0),
                contrast: fk(self, 2, 0.01, 0.0),
            },
            "ADBE HUE SATURATION" => {
                if n(7, 0.0) != 0.0 {
                    return None; // Colorize is a different effect
                }
                EffectKind::HueSaturation {
                    hue: fk(self, 4, 1.0, 0.0),
                    saturation: fk(self, 5, 0.01, 0.0),
                    lightness: fk(self, 6, 0.01, 0.0),
                }
            }
            "ADBE Exposure2" => {
                if n(1, 1.0) != 1.0 {
                    return None; // per-channel mode
                }
                EffectKind::Exposure { exposure: fk(self, 3, 1.0, 0.0), offset: fk(self, 4, 1.0, 0.0), gamma: fk(self, 5, 1.0, 1.0) }
            }
            "ADBE Fill" => {
                if n(6, 0.0) != 0.0 || p(1).is_some_and(|m| nums(&m["value"]).first().is_some_and(|v| *v != 0.0)) {
                    return None; // inverted, or limited to a mask
                }
                let color = self.color(p(2).as_ref(), Color::rgb(1.0, 0.0, 0.0));
                EffectKind::Tint { color, amount: fk(self, 5, 0.01, 1.0) }
            }
            "ADBE Drop Shadow" => {
                if n(6, 0.0) != 0.0 {
                    return None; // Shadow Only
                }
                // Direction is clockwise from up; opacity is stored 0-255.
                let (dir, dist) = (n(3, 135.0).to_radians(), n(4, 5.0));
                EffectKind::DropShadow {
                    color: self.color(p(1).as_ref(), Color::rgb(0.0, 0.0, 0.0)),
                    offset_x: Value::constant(dist * dir.sin()),
                    offset_y: Value::constant(-dist * dir.cos()),
                    radius: fk(self, 5, 0.5, 0.0),
                    opacity: fk(self, 2, 1.0 / 255.0, 127.5 / 255.0),
                }
            }
            _ => return None,
        };
        Some(Effect::new(kind))
    }

    /// A layer's static scale times its parents', per axis, as fractions.
    /// Keyed scales count by their first key.
    fn comp_scale(&self, l: &J) -> (f64, f64) {
        let (mut sx, mut sy) = (1.0, 1.0);
        let mut at = Some(l);
        for _ in 0..32 {
            let Some(l) = at else { break };
            let props = l["transform"].as_array().cloned().unwrap_or_default();
            if let Some(p) = prop(&props, "ADBE Scale") {
                // ponytail: an animated scale folds at its settled (last) key, since a
                // pop-in starting at 0 would divide by zero; per-frame needs a script.
                let v = match p["keyframes"].as_array().and_then(|k| k.last()) {
                    Some(k) => nums(&k["value"]),
                    None => nums(&p["value"]),
                };
                sx *= v.first().copied().unwrap_or(100.0) / 100.0;
                sy *= v.get(1).copied().unwrap_or(100.0) / 100.0;
            }
            at = l["parent"].as_u64().and_then(|id| self.expr_layers.iter().find(|x| x["layerId"].as_u64() == Some(id)));
        }
        (sx.abs(), sy.abs())
    }

    /// Count every expression under `j` that was not translated into the
    /// report; those properties keep their static value.
    fn count_expressions(&mut self, j: &J) {
        match j {
            // An expression control's own expression is inlined where it is read.
            J::Object(o) if o.get("matchName").and_then(J::as_str).is_some_and(|m| m.ends_with(" Control")) => {}
            J::Object(o) => {
                let e = o.get("expression").and_then(J::as_str).unwrap_or("");
                if !e.is_empty() && !self.translated.contains(e) {
                    self.report.drop("expression");
                }
                for v in o.values() {
                    self.count_expressions(v);
                }
            }
            J::Array(a) => a.iter().for_each(|v| self.count_expressions(v)),
            _ => {}
        }
    }

    /// A shape layer's contents, or a shape group's. Each AE group becomes a
    /// group node; its paths become leaves carrying the group's fill and
    /// stroke, which in AE paint every path above them in the same group.
    fn shape_items(&mut self, items: &[J]) -> Vec<Node> {
        let find = |m: &str| items.iter().find(|i| i["matchName"] == m);
        let fill = find("ADBE Vector Graphic - Fill").map(|f| {
            let props = f["properties"].as_array().cloned().unwrap_or_default();
            self.color(prop(&props, "ADBE Vector Fill Color"), Color::rgb(1.0, 0.0, 0.0))
        });
        let stroke = find("ADBE Vector Graphic - Stroke").map(|s| {
            let props = s["properties"].as_array().cloned().unwrap_or_default();
            Stroke {
                color: self.color(prop(&props, "ADBE Vector Stroke Color"), Color::rgb(1.0, 1.0, 1.0)),
                width: self.scalar(prop(&props, "ADBE Vector Stroke Width"), 1.0, 2.0),
                style: stroke_style(&props),
            }
        });

        let mut out = Vec::new();
        // AE lists contents top-first; PBC draws last on top.
        for item in items.iter().rev() {
            let props = item["properties"].as_array().cloned().unwrap_or_default();
            let mn = item["matchName"].as_str().unwrap_or("");
            let shape = match mn {
                "ADBE Vector Group" => {
                    let mut g = Node::group(self.fresh(), name(item));
                    let tf = prop(&props, "ADBE Vector Transform Group")
                        .and_then(|t| t["properties"].as_array().cloned())
                        .unwrap_or_default();
                    g.transform = self.transform(&tf, false);
                    let contents = prop(&props, "ADBE Vectors Group")
                        .and_then(|c| c["properties"].as_array().cloned())
                        .unwrap_or_default();
                    g.children = self.shape_items(&contents);
                    out.push(g);
                    continue;
                }
                "ADBE Vector Shape - Rect" => Some(Shape::Rect {
                    size: self.vec2(prop(&props, "ADBE Vector Rect Size")),
                    radius: self.scalar(prop(&props, "ADBE Vector Rect Roundness"), 1.0, 0.0),
                }),
                "ADBE Vector Shape - Ellipse" => {
                    Some(Shape::Ellipse { size: self.vec2(prop(&props, "ADBE Vector Ellipse Size")) })
                }
                "ADBE Vector Shape - Group" => match prop(&props, "ADBE Vector Shape") {
                    Some(p) if p["path"].is_object() => Some(Shape::Vector { path: self.path(p, Vec2::new(1.0, 1.0), Vec2::ZERO) }),
                    _ => {
                        self.report.drop("bezier path (no vertices in export)");
                        None
                    }
                },
                "ADBE Vector Graphic - Fill" | "ADBE Vector Graphic - Stroke" | "ADBE Vector Materials Group" => None,
                "" => None,
                other => {
                    self.report.drop(format!("shape item: {other}"));
                    None
                }
            };
            if let Some(shape) = shape {
                let mut n = Node::shape(self.fresh(), name(item), shape);
                n.fill = fill.clone();
                n.stroke = stroke.clone();
                out.push(n);
            }
        }
        // AE paints all of a group's paths as one shape: a letter's counters
        // are holes in it, by the nonzero rule, not shapes filled on top. With
        // more than one path, gather them into a Merge compound carrying the
        // paint, which is the same thing in PBC.
        let paths: Vec<usize> = (0..out.len()).filter(|&i| out[i].shape.is_some()).collect();
        if paths.len() > 1 {
            let mut compound = Node::group(self.fresh(), "Paths");
            compound.compound = Some(BoolOp::Merge);
            compound.fill = fill;
            compound.stroke = stroke;
            for &i in paths.iter().rev() {
                let mut leaf = out.remove(i);
                (leaf.fill, leaf.stroke) = (None, None);
                compound.children.insert(0, leaf);
            }
            out.insert(paths[0], compound);
        }
        out
    }

    /// A layer transform or a shape group's. AE scale and opacity are percent.
    fn transform(&mut self, props: &[J], layer: bool) -> Transform {
        let (anchor, pos, scale, opacity) = if layer {
            ("ADBE Anchor Point", "ADBE Position", "ADBE Scale", "ADBE Opacity")
        } else {
            ("ADBE Vector Anchor", "ADBE Vector Position", "ADBE Vector Scale", "ADBE Vector Group Opacity")
        };
        let mut t = Transform::default();
        t.anchor = self.vec3(prop(props, anchor), 1.0, Vec3::ZERO);
        t.position = self.position(props, pos);
        t.scale = self.vec3(prop(props, scale), 0.01, Vec3::ONE);
        t.opacity = self.scalar(prop(props, opacity), 0.01, 1.0);
        let rz = if layer { "ADBE Rotate Z" } else { "ADBE Vector Rotation" };
        let z = self.scalar(prop(props, rz), 1.0, 0.0);
        let x = layer.then(|| self.scalar(prop(props, "ADBE Rotate X"), 1.0, 0.0));
        let y = layer.then(|| self.scalar(prop(props, "ADBE Rotate Y"), 1.0, 0.0));
        let c = |v: &Option<Value<f64>>| match v {
            Some(Value::Const(v)) => *v,
            _ => 0.0,
        };
        let (cx, cy) = (c(&x), c(&y));
        t.rotation = match (x, y, z) {
            (None | Some(Value::Const(_)), None | Some(Value::Const(_)), Value::Const(z)) => {
                Value::constant(Vec3::new(cx, cy, z))
            }
            (_, _, Value::Keyframed(track)) => Value::Keyframed(track.map_value(|z| Vec3::new(0.0, 0.0, z))),
            _ => {
                self.report.drop("animated X/Y rotation");
                Value::constant(Vec3::ZERO)
            }
        };
        if prop(props, "ADBE Orientation").is_some_and(|o| nums(&o["value"]).iter().any(|v| *v != 0.0)) {
            self.report.drop("3D orientation");
        }
        t
    }

    /// Position, joined or separated. The export carries both forms; the
    /// separated X/Y win only when they are the ones animated.
    fn position(&mut self, props: &[J], joined: &str) -> Value<Vec3> {
        let animated = |p: Option<&J>| p.is_some_and(|p| p.get("keyframes").is_some());
        let (px, py) = (prop(props, "ADBE Position_0"), prop(props, "ADBE Position_1"));
        // Separated X/Y animate is what's live then; the joined value beside
        // it can be a stale copy, even keyed (in comp space, not the parent's).
        if animated(px) || animated(py) {
            let (x, y) = (self.scalar(px, 1.0, 0.0), self.scalar(py, 1.0, 0.0));
            return match (x, y) {
                (Value::Const(x), Value::Const(y)) => Value::constant(Vec3::new(x, y, 0.0)),
                (Value::Keyframed(t), Value::Const(y)) => Value::Keyframed(t.map_value(move |x| Vec3::new(x, y, 0.0))),
                (Value::Const(x), Value::Keyframed(t)) => Value::Keyframed(t.map_value(move |y| Vec3::new(x, y, 0.0))),
                // ponytail: both axes keyed keeps X's keys at Y's first value;
                // merge the two tracks when a project needs it.
                (Value::Keyframed(t), y) => {
                    self.report.drop("separated position with both axes keyed (X kept)");
                    let y = match y { Value::Keyframed(u) => u.keys().first().map_or(0.0, |k| k.value), _ => 0.0 };
                    Value::Keyframed(t.map_value(move |x| Vec3::new(x, y, 0.0)))
                }
                // A scripted axis has no Vec3 form; the joined value stands in.
                _ => self.vec3(prop(props, joined), 1.0, Vec3::ZERO),
            };
        }
        self.vec3(prop(props, joined), 1.0, Vec3::ZERO)
    }

    /// A bezier path. Animated paths become one track per point and handle,
    /// built by re-reading each key as a two-number property so the easing
    /// conversion is the same one every other property gets. A path whose
    /// point count changes between keys cannot morph point-for-point; it keeps
    /// its first shape.
    fn path(&mut self, p: &J, scale: Vec2, offset: Vec2) -> VectorPath {
        let shape = |v: &J| -> Vec<[Vec2; 3]> {
            let list = |k: &str| v[k].as_array().cloned().unwrap_or_default();
            let (vs, ins, outs) = (list("vertices"), list("inTangents"), list("outTangents"));
            let pt = |j: Option<&J>| {
                let n = j.map(nums).unwrap_or_default();
                Vec2::new(n.first().copied().unwrap_or(0.0), n.get(1).copied().unwrap_or(0.0))
            };
            let sc = |v: Vec2| Vec2::new(v.x * scale.x, v.y * scale.y);
            (0..vs.len()).map(|i| [sc(pt(vs.get(i))) + offset, sc(pt(ins.get(i))), sc(pt(outs.get(i)))]).collect()
        };
        let base = shape(&p["path"]);
        let closed = p["path"]["closed"].as_bool().unwrap_or(true);
        let keys = p["keyframes"].as_array().cloned().unwrap_or_default();
        let shapes: Vec<_> = keys.iter().map(|k| shape(&k["path"])).collect();
        let animated = !keys.is_empty() && shapes.iter().all(|s| s.len() == base.len());
        if !keys.is_empty() && !animated {
            self.report.drop("path keyframes with a changing point count (first shape kept)");
        }
        let anchors = (0..base.len())
            .map(|i| {
                let mut channel = |part: usize| -> Value<Vec2> {
                    // Most points of a morphing path never move: keep those constant.
                    if !animated || shapes.iter().all(|s| s[i][part] == base[i][part]) {
                        return Value::constant(base[i][part]);
                    }
                    let mut keyed = keys.clone();
                    for (k, s) in keyed.iter_mut().zip(&shapes) {
                        k["value"] = serde_json::json!([s[i][part].x, s[i][part].y]);
                    }
                    self.vec2(Some(&serde_json::json!({ "keyframes": keyed })))
                };
                Anchor { point: channel(0), in_tan: channel(1), out_tan: channel(2) }
            })
            .collect();
        VectorPath { anchors, closed }
    }

    fn scalar(&mut self, p: Option<&J>, k: f64, or: f64) -> Value<f64> {
        self.value(p, or, |v| v.first().map(|x| x * k), |a, b| (b - a).abs())
    }
    fn vec2(&mut self, p: Option<&J>) -> Value<Vec2> {
        self.value(
            p,
            Vec2::new(100.0, 100.0),
            |v| Some(Vec2::new(*v.first()?, *v.get(1)?)),
            |a, b| (*b - *a).hypot(),
        )
    }
    fn vec3(&mut self, p: Option<&J>, k: f64, or: Vec3) -> Value<Vec3> {
        self.value(
            p,
            or,
            |v| Some(Vec3::new(v.first()? * k, v.get(1)? * k, v.get(2).copied().unwrap_or(0.0) * k)),
            |a, b| ((b.x - a.x).powi(2) + (b.y - a.y).powi(2) + (b.z - a.z).powi(2)).sqrt(),
        )
    }
    fn color(&mut self, p: Option<&J>, or: Color) -> Value<Color> {
        self.value(
            p,
            or,
            |v| Some(Color::rgba(*v.first()?, *v.get(1)?, *v.get(2)?, v.get(3).copied().unwrap_or(1.0))),
            // Colour speeds are rare and their units unclear; ease by the
            // largest channel change so the handles are at least in range.
            |a, b| [b.r - a.r, b.g - a.g, b.b - a.b].iter().fold(0.0f64, |m, d| m.max(d.abs())),
        )
    }

    /// One property: constant, or keyframed with AE's easing converted.
    fn value<T: Animatable + Clone>(
        &mut self,
        p: Option<&J>,
        or: T,
        conv: impl Fn(&[f64]) -> Option<T>,
        dist: impl Fn(&T, &T) -> f64,
    ) -> Value<T>
    where
        Value<T>: Sized,
        T: crate::expr::FromExpr + crate::expr::ToExpr,
    {
        let Some(p) = p else { return Value::constant(or) };
        if let Some(src) = self.script(p, &self.expr_layer, 0) {
            // A script that ignores the clock folds to a constant through the
            // usual unit conversion. A live one is kept only where PBC's units
            // are AE's, since nothing converts its output each frame.
            let probe = crate::expr::eval_script(&src, 0.0).ok();
            let via = probe.as_ref().map(expr_nums).and_then(|v| conv(&v));
            let direct = probe.and_then(T::from_expr);
            let kept = match (src.contains("frame"), via, direct) {
                (false, Some(v), _) => Some(Value::constant(v)),
                (true, Some(v), Some(d)) if dist(&v, &d) < 1e-9 => Some(Value::expr(crate::expr::Expr::Script(src))),
                _ => None,
            };
            if let Some(v) = kept {
                self.translated.insert(p["expression"].as_str().unwrap_or("").to_string());
                return v;
            }
        }
        let Some(keys) = p["keyframes"].as_array().filter(|k| !k.is_empty()) else {
            return Value::constant(conv(&nums(&p["value"])).unwrap_or(or));
        };
        let vals: Vec<T> = keys.iter().map(|k| conv(&nums(&k["value"])).unwrap_or_else(|| or.clone())).collect();
        let times: Vec<f64> = keys.iter().map(|k| num(k, "time", 0.0)).collect();
        let mut frames: Vec<i64> = times.iter().map(|t| self.frame(*t)).collect();
        // Two keys rounding onto one frame would make a zero-length segment;
        // nudge the later one forward, as AE itself cannot put two on a frame.
        for i in 1..frames.len() {
            frames[i] = frames[i].max(frames[i - 1] + 1);
        }
        let mut out = Vec::with_capacity(keys.len());
        for i in 0..keys.len() {
            let mut kf = Keyframe::linear(frames[i], vals[i].clone());
            if let Some(next) = keys.get(i + 1) {
                // AE's speeds are in its own units (percent, not PBC's 0-1),
                // so the change they scale against is too: the path length
                // for a spatial property, the first dimension's change for
                // anything else, as that's the dimension the speed is read from.
                let (a, b) = (nums(&keys[i]["value"]), nums(&next["value"]));
                let raw = if p["valueType"] == "Spatial" {
                    a.iter().zip(&b).map(|(a, b)| (b - a) * (b - a)).sum::<f64>().sqrt()
                } else {
                    a.first().zip(b.first()).map_or(0.0, |(a, b)| b - a)
                };
                let (o, n) = ae_handles(&keys[i], next, times[i + 1] - times[i], raw);
                kf.out_handle = o;
                if keys[i]["outInterpolation"] == "Hold" {
                    kf.interp = Interp::Hold;
                }
                // The in-handle lives on the next key; stash it there below.
                out.push((kf, Some(n)));
            } else {
                out.push((kf, None));
            }
        }
        let mut keys_out = Vec::with_capacity(out.len());
        let mut pending_in = None;
        for (mut kf, next_in) in out {
            if let Some(h) = pending_in.take() {
                kf.in_handle = h;
            }
            pending_in = next_in;
            keys_out.push(kf);
        }
        Value::Keyframed(Track::new(keys_out))
    }
}

/// Colorama's Output Cycle palette from its raw parameter bytes (hex): 8-byte
/// stops, a big-endian u32 position out of 65535 then ARGB, ended by an empty
/// stop. Positions become 0..1.
fn colorama_palette(hex: &str) -> Vec<(f64, Color)> {
    let bytes: Vec<u8> = (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect();
    let mut out = Vec::new();
    for (i, st) in bytes.chunks_exact(8).enumerate() {
        if i > 0 && st.iter().all(|&b| b == 0) || out.len() == crate::effect::COLORAMA_STOPS {
            break;
        }
        let pos = u32::from_be_bytes([st[0], st[1], st[2], st[3]]) as f64 / 65535.0;
        let c = |k: usize| st[k] as f64 / 255.0;
        out.push((pos, Color { r: c(5), g: c(6), b: c(7), a: c(4) }));
    }
    out
}

/// A shape stroke's caps, joins and miter limit. AE stores 1/2/3 for butt,
/// round, projecting caps and miter, round, bevel joins, and leaves the
/// defaults (butt, miter, 4) out.
fn stroke_style(props: &[J]) -> StrokeStyle {
    let code = |m: &str| prop(props, m).and_then(|p| nums(&p["value"]).first().copied()).unwrap_or(1.0) as i64;
    StrokeStyle {
        cap: match code("ADBE Vector Stroke Line Cap") {
            2 => LineCap::Round,
            3 => LineCap::Square,
            _ => LineCap::Butt,
        },
        join: match code("ADBE Vector Stroke Line Join") {
            2 => LineJoin::Round,
            3 => LineJoin::Bevel,
            _ => LineJoin::Miter,
        },
        miter_limit: prop(props, "ADBE Vector Stroke Miter Limit").and_then(|p| nums(&p["value"]).first().copied()).unwrap_or(4.0),
    }
}

/// A script result's components, the way the exporter writes values.
fn expr_nums(v: &crate::expr::ExprValue) -> Vec<f64> {
    use crate::expr::ExprValue as E;
    match v {
        E::Num(n) => vec![*n],
        E::Vec3(p) => vec![p.x, p.y, p.z],
        E::Color(c) => vec![c.r, c.g, c.b, c.a],
        _ => Vec::new(),
    }
}

/// The name in `name")rest` and what follows its closing `")`.
fn quoted(s: &str) -> Option<(&str, &str)> {
    let end = s.find("\")")?;
    Some((&s[..end], &s[end + 2..]))
}

/// A property value as a script literal: a number, or an array.
fn literal(v: &[f64]) -> Option<String> {
    match v {
        [] => None,
        [x] => Some(format!("{x:?}")),
        v => Some(format!("[{}]", v.iter().map(|x| format!("{x:?}")).collect::<Vec<_>>().join(", "))),
    }
}

/// Integer literals as floats: AE's numbers are all doubles, and Rhai would
/// otherwise do integer division on `1/2`.
fn floats(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len() + 8);
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        let prev = if i == 0 { ' ' } else { b[i - 1] as char };
        if c.is_ascii_digit() && !(prev.is_alphanumeric() || prev == '_' || prev == '.') {
            let start = i;
            while i < b.len() && (b[i] as char).is_ascii_digit() {
                i += 1;
            }
            out.push_str(&s[start..i]);
            let next = b.get(i).map(|&c| c as char);
            if next != Some('.') && !next.is_some_and(|c| c.is_alphabetic()) {
                out.push_str(".0");
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// `s` with every whole-word `w` (not a property access like `x.w`) replaced.
fn word(s: &str, w: &str, with: &str) -> String {
    let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.');
    let mut out = String::new();
    let mut i = 0;
    while let Some(j) = s[i..].find(w).map(|j| j + i) {
        let before = s[..j].chars().next_back();
        let after = s[j + w.len()..].chars().next();
        out.push_str(&s[i..j]);
        let after_ident = after.is_some_and(|c| c.is_alphanumeric() || c == '_');
        out.push_str(if ident(before) || after_ident { w } else { with });
        i = j + w.len();
    }
    out.push_str(&s[i..]);
    out
}

/// Move a position-like value by `d`, keyframes and all. An expression is
/// left alone: it computes its own value, and nothing evaluates AE expressions
/// yet anyway.
fn shift(v: Value<Vec3>, d: Vec2) -> Value<Vec3> {
    let by = move |p: Vec3| Vec3::new(p.x + d.x, p.y + d.y, p.z);
    match v {
        Value::Const(p) => Value::Const(by(p)),
        Value::Keyframed(t) => Value::Keyframed(t.map_value(by)),
        e => e,
    }
}

/// The exporter's blend-mode names, for the modes PBC has.
fn blend_mode(name: &str) -> Option<BlendMode> {
    Some(match name {
        "multiply" => BlendMode::Multiply,
        "screen" => BlendMode::Screen,
        "overlay" => BlendMode::Overlay,
        "darken" => BlendMode::Darken,
        "lighten" => BlendMode::Lighten,
        "colorDodge" | "classicColorDodge" => BlendMode::ColorDodge,
        "colorBurn" | "classicColorBurn" => BlendMode::ColorBurn,
        "hardLight" => BlendMode::HardLight,
        "softLight" => BlendMode::SoftLight,
        "difference" | "classicDifference" => BlendMode::Difference,
        "exclusion" => BlendMode::Exclusion,
        "hue" => BlendMode::Hue,
        "saturation" => BlendMode::Saturation,
        "color" => BlendMode::Color,
        "luminosity" => BlendMode::Luminosity,
        _ => return None,
    })
}

/// A child property by match name.
fn prop<'a>(props: &'a [J], match_name: &str) -> Option<&'a J> {
    props.iter().find(|p| p["matchName"] == match_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_effects_and_the_constant_stroke_idiom_import() {
        let json = r##"{"root":{"id":0,"type":"Folder","children":[
          {"id":1,"name":"Main","type":"Composition","width":100,"height":100,"framerate":24,"seconds":1,"layers":[
            {"index":1,"name":"S","sourceId":0,"layerId":2,"parent":3,"flags":{},
             "transform":[{"matchName":"ADBE Scale","value":[200,200,100]}],
             "effects":[
               {"matchName":"ADBE Drop Shadow","properties":[{"matchName":"ADBE Drop Shadow-0003","value":[90]},{"matchName":"ADBE Drop Shadow-0004","value":[10]}]},
               {"matchName":"ADBE HUE SATURATION","properties":[{"matchName":"ADBE HUE SATURATION-0005","value":[-50]}]}],
             "contents":[{"matchName":"ADBE Vector Graphic - Stroke","properties":[{"matchName":"ADBE Vector Stroke Width","value":[8],
               "expression":"value / Math.max(length(toComp([0,0]), toComp([0.7071,0.7071])), 0.001);"}]},
               {"matchName":"ADBE Vector Shape - Rect","properties":[]}]},
            {"index":2,"name":"P","kind":"null","sourceId":0,"layerId":3,"flags":{},"transform":[{"matchName":"ADBE Scale","value":[100,100,100]}]}]}]}}"##;
        let (p, report) = import(json).unwrap();
        assert!(!report.dropped.contains_key("expression"), "{:?}", report.dropped);
        let s = p.root_comp().root.children.iter().find(|n| n.name == "S").unwrap();
        let EffectKind::DropShadow { offset_x, offset_y, opacity, .. } = &s.effects[0].kind else { panic!() };
        // 90 degrees is straight right; the default opacity is half.
        assert!(matches!(offset_x, Value::Const(x) if (x - 10.0).abs() < 1e-9));
        assert!(matches!(offset_y, Value::Const(y) if y.abs() < 1e-9));
        assert_eq!(opacity, &Value::constant(0.5));
        let EffectKind::HueSaturation { saturation, .. } = &s.effects[1].kind else { panic!() };
        assert_eq!(saturation, &Value::constant(-0.5));
        // 200% under a 100% parent: the 8 px stroke is authored as 4.
        let stroke = s.children.iter().find_map(|n| n.stroke.as_ref()).expect("stroke");
        assert_eq!(stroke.width, Value::constant(4.0));
    }

    #[test]
    fn rig_expressions_and_grid_effects_import() {
        // A controller layer drives a Mosaic through a nested, checkbox-gated
        // expression, and a Colorama carries its palette as raw bytes.
        let json = r##"{"root":{"id":0,"type":"Folder","children":[
          {"id":1,"name":"Main","type":"Composition","width":900,"height":300,"framerate":24,"seconds":2,"layers":[
            {"index":1,"name":"CTRL","kind":"null","sourceId":0,"flags":{"video":false},"effects":[
              {"matchName":"ADBE Checkbox Control","label":"Link","properties":[{"name":"Checkbox","matchName":"ADBE Checkbox Control-0001","value":[1]}]},
              {"matchName":"ADBE Slider Control","label":"W","properties":[{"name":"Slider","matchName":"ADBE Slider Control-0001","value":[150]}]},
              {"matchName":"ADBE Slider Control","label":"H","properties":[{"name":"Slider","matchName":"ADBE Slider Control-0001","value":[7],
                "expression":"(effect(\"Link\")(\"Checkbox\") == true) ? effect(\"W\")(\"Slider\") : value;"}]}]},
            {"index":2,"name":"GRID","sourceId":0,"flags":{"video":false},"effects":[
              {"matchName":"ADBE Mosaic","properties":[
                {"matchName":"ADBE Mosaic-0002","value":[2],"expression":"Math.round(thisComp.height/thisComp.layer(\"CTRL\").effect(\"H\")(\"Slider\"))"}]},
              {"matchName":"ADBE Fractal Noise","properties":[{"matchName":"ADBE Fractal Noise-0023","value":[0],"expression":"time * 10"}]},
              {"matchName":"APC Colorama","properties":[{"matchName":"APC Colorama-0012","arb":"00000000ffff000000008000ff0000ff000000000000000000000000"}]}]}]}]}}"##;
        let (p, report) = import(json).unwrap();
        let grid = &p.root_comp().root.children[0];
        assert!(grid.hidden, "video off hides a layer whatever its audio switch says");
        let EffectKind::Mosaic { vertical, .. } = &grid.effects[0].kind else { panic!("{:?}", grid.effects) };
        assert_eq!(vertical, &Value::constant(2.0), "300 / 150, through the gated H slider");
        let EffectKind::FractalNoise { evolution, .. } = &grid.effects[1].kind else { panic!() };
        assert!(evolution.is_expr(), "time-driven stays live");
        let EffectKind::Colorama { palette, .. } = &grid.effects[2].kind else { panic!() };
        assert_eq!(palette.len(), 2);
        assert_eq!(palette[1].0, 32768.0 / 65535.0);
        assert_eq!(palette[1].1, Color::rgb(0.0, 0.0, 1.0));
        assert!(report.dropped.is_empty(), "{:?}", report.dropped);
    }

    #[test]
    fn ae_ease_becomes_matching_handles() {
        // A 0→100 move over 1s, eased out at 33% influence with zero speed:
        // the handle sits at (0.33, 0) — PBC's flat ease-out.
        let a = serde_json::json!({"outInterpolation": "Bezier", "outSpeed": [0], "outInfluence": [0.33]});
        let b = serde_json::json!({"inInterpolation": "Bezier", "inSpeed": [0], "inInfluence": [0.33]});
        let (o, i) = ae_handles(&a, &b, 1.0, 100.0);
        assert_eq!((o.x, o.y), (0.33, 0.0));
        assert!((i.x - 0.67).abs() < 1e-9 && (i.y - 1.0).abs() < 1e-9);
        // Linear speed (100/s across 100 units in 1s) lands on the diagonal.
        let a = serde_json::json!({"outInterpolation": "Bezier", "outSpeed": [100], "outInfluence": [0.5]});
        let (o, _) = ae_handles(&a, &b, 1.0, 100.0);
        assert_eq!((o.x, o.y), (0.5, 0.5));
    }

    #[test]
    fn imports_a_minimal_project() {
        let json = r##"{"root":{"id":0,"type":"Folder","children":[
          {"id":1,"name":"Main","type":"Composition","width":100,"height":50,"framerate":24,"seconds":2,
           "backgroundColor":"#ff0000","layers":[
             {"index":1,"name":"top","layerId":7,"blendMode":"screen","sourceId":0,"startTime":0,"duration":2,"flags":{},
              "contents":[{"matchName":"ADBE Vector Group","label":"G","properties":[
                {"matchName":"ADBE Vectors Group","properties":[
                  {"matchName":"ADBE Vector Shape - Rect","properties":[{"matchName":"ADBE Vector Rect Size","value":[10,20]}]},
                  {"matchName":"ADBE Vector Graphic - Fill","properties":[{"matchName":"ADBE Vector Fill Color","value":[0,1,0,1]}]}]}]}],
              "transform":[{"matchName":"ADBE Opacity","value":[50]}]},
             {"index":2,"name":"bottom","layerId":8,"parent":7,"trackMatte":"alpha","sourceId":0,"startTime":1,"duration":1,"flags":{},"transform":[
               {"matchName":"ADBE Position","keyframes":[
                 {"time":0,"value":[0,0,0],"outInterpolation":"Linear"},
                 {"time":1,"value":[10,0,0],"inInterpolation":"Linear"}]}]}]}]}}"##;
        let (p, _) = import(json).unwrap();
        let c = p.root_comp();
        assert_eq!((c.width, c.height, c.fps, c.duration_frames), (100.0, 50.0, 24.0, 48));
        assert_eq!(c.bg, Color::rgb(1.0, 0.0, 0.0));
        // Reversed: AE's layer 1 is drawn last.
        assert_eq!(c.root.children[1].name, "top");
        assert_eq!(c.root.children[1].transform.opacity, Value::constant(0.5));
        let rect = &c.root.children[1].children[0].children[0];
        assert!(matches!(rect.shape, Some(Shape::Rect { .. })));
        assert_eq!(rect.fill, Some(Value::constant(Color::rgb(0.0, 1.0, 0.0))));
        let Value::Keyframed(t) = &c.root.children[0].transform.position else { panic!("not keyed") };
        // Key times are layer time: a late start moves the layer, not its keys.
        assert_eq!(t.frames(), vec![0, 24]);
        assert_eq!(c.root.children[0].timing.map(|t| t.start), Some(24));
        assert_eq!(c.root.children[1].blend, BlendMode::Screen);
        assert_eq!(c.root.children[0].link, Some(c.root.children[1].id));
        assert_eq!(c.root.children[0].matte, Some(MatteMode::Alpha));
    }

    #[test]
    fn paths_import_with_relative_tangents_and_animate_per_point() {
        let tri = |x: f64| serde_json::json!({"closed": true,
            "vertices": [[0, 0], [x, 0], [0, 10]], "inTangents": [[0, 0], [-1, 0], [0, 0]], "outTangents": [[2, 0], [0, 0], [0, 0]]});
        let shape = serde_json::json!({"matchName": "ADBE Vector Shape", "path": tri(10.0), "keyframes": [
            {"time": 0, "path": tri(10.0), "outInterpolation": "Linear"},
            {"time": 1, "path": tri(20.0), "inInterpolation": "Linear"}]});
        let mut report = ImportReport::default();
        let (items, ids) = (HashMap::new(), HashMap::new());
        let footage = HashMap::new();
        let mut im = Importer {
            items: &items,
            comp_ids: &ids,
            footage: &footage,
            centred: HashMap::new(),
            comp_size: Vec2::new(100.0, 100.0),
            report: &mut report,
            next_id: 1,
            fps: 10.0,
            expr_layers: Vec::new(),
            expr_layer: J::Null,
            translated: HashSet::new(),
        };
        let path = im.path(&shape, Vec2::new(1.0, 1.0), Vec2::ZERO);
        assert!(path.closed);
        assert_eq!(path.anchors.len(), 3);
        assert_eq!(path.anchors[0].out_tan, Value::constant(Vec2::new(2.0, 0.0)));
        let Value::Keyframed(t) = &path.anchors[1].point else { panic!("point 1 should animate") };
        assert_eq!(t.frames(), vec![0, 10]);
        assert_eq!(t.keys()[1].value, Vec2::new(20.0, 0.0));
    }

    #[test]
    fn footage_solids_and_masks_land_in_ae_layer_space() {
        let json = r##"{"root":{"id":0,"type":"Folder","children":[
          {"id":5,"name":"clip.mov","type":"Footage","footageType":"file","file":"D:/a/clip.mov","width":200,"height":100,"framerate":25,"seconds":2},
          {"id":6,"name":"Red","type":"Footage","footageType":"solid","solidColor":[1,0,0],"width":40,"height":30},
          {"id":1,"name":"Main","type":"Composition","width":400,"height":400,"framerate":25,"seconds":2,"layers":[
            {"index":1,"name":"child","layerId":3,"parent":2,"sourceId":6,"flags":{},
             "transform":[{"matchName":"ADBE Position","value":[100,50,0]},{"matchName":"ADBE Anchor Point","value":[0.25,0.5,0]}]},
            {"index":2,"name":"clip","layerId":2,"sourceId":5,"flags":{},
             "transform":[{"matchName":"ADBE Position_0","value":[0]},{"matchName":"ADBE Position_1","value":[0]}],
             "masks":[{"matchName":"ADBE Mask Atom","properties":[{"matchName":"ADBE Mask Shape","path":
               {"closed":true,"vertices":[[0,0],[1,1]],"inTangents":[[0,0],[0,0]],"outTangents":[[0,0],[0,0]]}}]}]}]}]}}"##;
        let (p, report) = import(json).unwrap();
        assert!(report.dropped.is_empty(), "{:?}", report.dropped);
        let a = &p.assets[&AssetId(0)];
        assert_eq!((a.kind, a.width, a.frames, a.fps), (AssetKind::Video, 200.0, 50, 25.0));
        let c = p.root_comp();
        let (clip, child) = (&c.root.children[0], &c.root.children[1]);
        assert!(matches!(clip.shape, Some(Shape::Image { .. })));
        // AE's default anchor is the source centre, which is PBC's origin for
        // centred footage; the default position is the comp centre.
        assert_eq!(clip.transform.anchor, Value::constant(Vec3::new(0.0, 0.0, 0.0)));
        assert_eq!(clip.transform.position, Value::constant(Vec3::new(200.0, 200.0, 0.0)));
        // Everything else in the footage's layer space moves by half its size.
        assert_eq!(child.transform.position, Value::constant(Vec3::new(0.0, 0.0, 0.0)));
        assert_eq!(child.link, Some(clip.id));
        let Some(Mask { shape: Shape::Vector { path }, .. }) = &clip.mask else { panic!("no mask") };
        assert_eq!(path.anchors[1].point, Value::constant(Vec2::new(100.0, 50.0)));
        // A solid is an exact rect from its top-left corner, in its colour.
        assert_eq!(child.fill, Some(Value::constant(Color::rgb(1.0, 0.0, 0.0))));
        let Some(Shape::Vector { path }) = &child.shape else { panic!("no solid") };
        assert_eq!(path.anchors[2].point, Value::constant(Vec2::new(40.0, 30.0)));
        // A sourced layer's anchor is stored as a fraction of its source.
        assert_eq!(child.transform.anchor, Value::constant(Vec3::new(10.0, 15.0, 0.0)));
    }

    #[test]
    fn a_groups_paths_share_one_fill_as_a_merge_compound() {
        let path = |x: f64| serde_json::json!({"matchName": "ADBE Vector Shape - Group", "properties": [
            {"matchName": "ADBE Vector Shape", "path": {"closed": true,
              "vertices": [[0, 0], [x, 0], [x, x]], "inTangents": [[0,0],[0,0],[0,0]], "outTangents": [[0,0],[0,0],[0,0]]}}]});
        let fill = serde_json::json!({"matchName": "ADBE Vector Graphic - Fill",
            "properties": [{"matchName": "ADBE Vector Fill Color", "value": [0, 0, 1, 1]}]});
        let items = vec![path(10.0), path(5.0), fill];
        let (all, ids, footage) = (HashMap::new(), HashMap::new(), HashMap::new());
        let mut report = ImportReport::default();
        let mut im = Importer {
            items: &all,
            comp_ids: &ids,
            footage: &footage,
            centred: HashMap::new(),
            comp_size: Vec2::new(100.0, 100.0),
            report: &mut report,
            next_id: 1,
            fps: 10.0,
            expr_layers: Vec::new(),
            expr_layer: J::Null,
            translated: HashSet::new(),
        };
        let out = im.shape_items(&items);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].compound, Some(BoolOp::Merge));
        assert_eq!(out[0].fill, Some(Value::constant(Color::rgb(0.0, 0.0, 1.0))));
        assert_eq!(out[0].children.len(), 2);
        assert!(out[0].children.iter().all(|c| c.fill.is_none()));
    }

    #[test]
    fn text_takes_its_style_and_moves_its_origin_to_the_baseline() {
        let json = r##"{"root":{"id":0,"type":"Folder","children":[
          {"id":1,"name":"Main","type":"Composition","width":400,"height":400,"framerate":25,"seconds":2,"layers":[
            {"index":1,"name":"title","layerId":1,"sourceId":0,"kind":"text","text":"Hola","flags":{},
             "transform":[{"matchName":"ADBE Anchor Point","value":[0,0,0]}],
             "textStyle":{"fontSize":50,"justification":"right","fillColor":[1,0,0],"applyFill":true,
                          "strokeColor":[0,0,1],"applyStroke":true,"strokeWidth":3}}]}]}}"##;
        let (p, _) = import(json).unwrap();
        let n = &p.root_comp().root.children[0];
        let Some(Shape::Text { size, align, .. }) = &n.shape else { panic!("not text") };
        assert_eq!((size, *align), (&Value::constant(50.0), TextAlign::Right));
        assert_eq!(n.fill, Some(Value::constant(Color::rgb(1.0, 0.0, 0.0))));
        assert_eq!(n.stroke.as_ref().map(|s| s.width.clone()), Some(Value::constant(3.0)));
        // AE's origin (right edge, first baseline) becomes the anchor: right of
        // and below PBC's centred origin.
        let o = crate::text::text_origin("Hola", "", 50.0, TextAlign::Right, None).unwrap();
        assert!(o.x > 0.0 && o.y > 0.0, "{o:?}");
        assert_eq!(n.transform.anchor, Value::constant(Vec3::new(o.x, o.y, 0.0)));
    }
}
