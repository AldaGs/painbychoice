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

use std::collections::{BTreeMap, HashMap};

use kurbo::Vec2;
use serde_json::Value as J;

use crate::composite::{BlendMode, MatteMode};
use crate::path::{Anchor, VectorPath};
use crate::node::{Comp, CompId, LayerTiming, Node, Project, Shape, Stroke, Transform};
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
    let mut built = BTreeMap::new();
    for c in &comps {
        let comp = Importer { items: &items, comp_ids: &comp_ids, report: &mut report, next_id: 1, fps: num(c, "framerate", 30.0) }
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
    report: &'a mut ImportReport,
    next_id: u64,
    fps: f64,
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
                    Some(&id) => n.link = Some(id),
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
        node.transform = self.transform(&props, start, true);

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
        // off; that must not hide what links to them.
        node.hidden = layer_kind != "null" && flags["video"] == J::Bool(false) && flags["audio"] != J::Bool(true);
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
        if let Some(&comp) = self.comp_ids.get(&source) {
            node.precomp = Some(comp);
        } else if let Some(text) = l["text"].as_str() {
            node.shape = Some(Shape::Text {
                content: Value::constant(text.to_string()),
                family: String::new(),
                size: Value::constant(72.0),
                align: TextAlign::Left,
                max_width: None,
            });
            node.fill = Some(Value::constant(Color::rgb(1.0, 1.0, 1.0)));
            if l.get("textProperties").is_some() {
                self.report.drop("text document style (font, size, animators)");
            }
        } else if let Some(contents) = l["contents"].as_array() {
            node.children = self.shape_items(contents, start);
        } else if source != 0 && self.items.get(&source).is_some_and(|i| kind(i) == "Footage") {
            self.report.drop("footage (no file path in export)");
        }

        for e in l["effects"].as_array().into_iter().flatten() {
            self.report.drop(format!("effect: {}", e["name"].as_str().unwrap_or("?")));
        }
        if l.get("masks").is_some() {
            self.report.drop("layer masks (no path in export)");
        }
        self.count_expressions(l);
        Some(node)
    }

    /// Count every expression under `j` into the report. Expressions are not
    /// evaluated yet (step 3 of the plan), so the property keeps its value.
    fn count_expressions(&mut self, j: &J) {
        match j {
            J::Object(o) => {
                if o.get("expression").and_then(J::as_str).is_some_and(|e| !e.is_empty()) {
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
    fn shape_items(&mut self, items: &[J], start: f64) -> Vec<Node> {
        let find = |m: &str| items.iter().find(|i| i["matchName"] == m);
        let fill = find("ADBE Vector Graphic - Fill").map(|f| {
            let props = f["properties"].as_array().cloned().unwrap_or_default();
            self.color(prop(&props, "ADBE Vector Fill Color"), start, Color::rgb(1.0, 0.0, 0.0))
        });
        let stroke = find("ADBE Vector Graphic - Stroke").map(|s| {
            let props = s["properties"].as_array().cloned().unwrap_or_default();
            Stroke {
                color: self.color(prop(&props, "ADBE Vector Stroke Color"), start, Color::rgb(1.0, 1.0, 1.0)),
                width: self.scalar(prop(&props, "ADBE Vector Stroke Width"), start, 1.0, 2.0),
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
                    g.transform = self.transform(&tf, start, false);
                    let contents = prop(&props, "ADBE Vectors Group")
                        .and_then(|c| c["properties"].as_array().cloned())
                        .unwrap_or_default();
                    g.children = self.shape_items(&contents, start);
                    out.push(g);
                    continue;
                }
                "ADBE Vector Shape - Rect" => Some(Shape::Rect {
                    size: self.vec2(prop(&props, "ADBE Vector Rect Size"), start),
                    radius: self.scalar(prop(&props, "ADBE Vector Rect Roundness"), start, 1.0, 0.0),
                }),
                "ADBE Vector Shape - Ellipse" => {
                    Some(Shape::Ellipse { size: self.vec2(prop(&props, "ADBE Vector Ellipse Size"), start) })
                }
                "ADBE Vector Shape - Group" => match prop(&props, "ADBE Vector Shape") {
                    Some(p) if p["path"].is_object() => Some(Shape::Vector { path: self.path(p, start) }),
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
        out
    }

    /// A layer transform or a shape group's. AE scale and opacity are percent.
    fn transform(&mut self, props: &[J], start: f64, layer: bool) -> Transform {
        let (anchor, pos, scale, opacity) = if layer {
            ("ADBE Anchor Point", "ADBE Position", "ADBE Scale", "ADBE Opacity")
        } else {
            ("ADBE Vector Anchor", "ADBE Vector Position", "ADBE Vector Scale", "ADBE Vector Group Opacity")
        };
        let mut t = Transform::default();
        t.anchor = self.vec3(prop(props, anchor), start, 1.0, Vec3::ZERO);
        t.position = self.position(props, pos, start);
        t.scale = self.vec3(prop(props, scale), start, 0.01, Vec3::ONE);
        t.opacity = self.scalar(prop(props, opacity), start, 0.01, 1.0);
        let rz = if layer { "ADBE Rotate Z" } else { "ADBE Vector Rotation" };
        let z = self.scalar(prop(props, rz), start, 1.0, 0.0);
        let x = layer.then(|| self.scalar(prop(props, "ADBE Rotate X"), start, 1.0, 0.0));
        let y = layer.then(|| self.scalar(prop(props, "ADBE Rotate Y"), start, 1.0, 0.0));
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
    fn position(&mut self, props: &[J], joined: &str, start: f64) -> Value<Vec3> {
        let animated = |p: Option<&J>| p.is_some_and(|p| p.get("keyframes").is_some());
        let (px, py) = (prop(props, "ADBE Position_0"), prop(props, "ADBE Position_1"));
        if !animated(prop(props, joined)) && (animated(px) || animated(py)) {
            self.report.drop("separated position (X only kept)");
            let x = self.scalar(px, start, 1.0, 0.0);
            let y = match self.scalar(py, start, 1.0, 0.0) {
                Value::Const(y) => y,
                _ => 0.0,
            };
            return match x {
                Value::Const(x) => Value::constant(Vec3::new(x, y, 0.0)),
                Value::Keyframed(t) => Value::Keyframed(t.map_value(move |x| Vec3::new(x, y, 0.0))),
                Value::Expr(e) => Value::Expr(e),
            };
        }
        self.vec3(prop(props, joined), start, 1.0, Vec3::ZERO)
    }

    /// A bezier path. Animated paths become one track per point and handle,
    /// built by re-reading each key as a two-number property so the easing
    /// conversion is the same one every other property gets. A path whose
    /// point count changes between keys cannot morph point-for-point; it keeps
    /// its first shape.
    fn path(&mut self, p: &J, start: f64) -> VectorPath {
        let shape = |v: &J| -> Vec<[Vec2; 3]> {
            let list = |k: &str| v[k].as_array().cloned().unwrap_or_default();
            let (vs, ins, outs) = (list("vertices"), list("inTangents"), list("outTangents"));
            let pt = |j: Option<&J>| {
                let n = j.map(nums).unwrap_or_default();
                Vec2::new(n.first().copied().unwrap_or(0.0), n.get(1).copied().unwrap_or(0.0))
            };
            (0..vs.len()).map(|i| [pt(vs.get(i)), pt(ins.get(i)), pt(outs.get(i))]).collect()
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
                    self.vec2(Some(&serde_json::json!({ "keyframes": keyed })), start)
                };
                Anchor { point: channel(0), in_tan: channel(1), out_tan: channel(2) }
            })
            .collect();
        VectorPath { anchors, closed }
    }

    fn scalar(&mut self, p: Option<&J>, start: f64, k: f64, or: f64) -> Value<f64> {
        self.value(p, start, or, |v| v.first().map(|x| x * k), |a, b| (b - a).abs())
    }
    fn vec2(&mut self, p: Option<&J>, start: f64) -> Value<Vec2> {
        self.value(
            p,
            start,
            Vec2::new(100.0, 100.0),
            |v| Some(Vec2::new(*v.first()?, *v.get(1)?)),
            |a, b| (*b - *a).hypot(),
        )
    }
    fn vec3(&mut self, p: Option<&J>, start: f64, k: f64, or: Vec3) -> Value<Vec3> {
        self.value(
            p,
            start,
            or,
            |v| Some(Vec3::new(v.first()? * k, v.get(1)? * k, v.get(2).copied().unwrap_or(0.0) * k)),
            |a, b| ((b.x - a.x).powi(2) + (b.y - a.y).powi(2) + (b.z - a.z).powi(2)).sqrt(),
        )
    }
    fn color(&mut self, p: Option<&J>, start: f64, or: Color) -> Value<Color> {
        self.value(
            p,
            start,
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
        start: f64,
        or: T,
        conv: impl Fn(&[f64]) -> Option<T>,
        dist: impl Fn(&T, &T) -> f64,
    ) -> Value<T>
    where
        Value<T>: Sized,
        T: crate::expr::FromExpr + crate::expr::ToExpr,
    {
        let Some(p) = p else { return Value::constant(or) };
        let Some(keys) = p["keyframes"].as_array().filter(|k| !k.is_empty()) else {
            return Value::constant(conv(&nums(&p["value"])).unwrap_or(or));
        };
        let vals: Vec<T> = keys.iter().map(|k| conv(&nums(&k["value"])).unwrap_or_else(|| or.clone())).collect();
        let times: Vec<f64> = keys.iter().map(|k| num(k, "time", 0.0)).collect();
        let mut frames: Vec<i64> = times.iter().map(|t| self.frame(t - start)).collect();
        // Two keys rounding onto one frame would make a zero-length segment;
        // nudge the later one forward, as AE itself cannot put two on a frame.
        for i in 1..frames.len() {
            frames[i] = frames[i].max(frames[i - 1] + 1);
        }
        let mut out = Vec::with_capacity(keys.len());
        for i in 0..keys.len() {
            let mut kf = Keyframe::linear(frames[i], vals[i].clone());
            if let Some(next) = keys.get(i + 1) {
                let (o, n) = ae_handles(&keys[i], next, times[i + 1] - times[i], dist(&vals[i], &vals[i + 1]));
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
             {"index":2,"name":"bottom","layerId":8,"parent":7,"trackMatte":"alpha","sourceId":0,"flags":{},"transform":[
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
        assert_eq!(t.frames(), vec![0, 24]);
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
        let mut im = Importer { items: &items, comp_ids: &ids, report: &mut report, next_id: 1, fps: 10.0 };
        let path = im.path(&shape, 0.0);
        assert!(path.closed);
        assert_eq!(path.anchors.len(), 3);
        assert_eq!(path.anchors[0].out_tan, Value::constant(Vec2::new(2.0, 0.0)));
        let Value::Keyframed(t) = &path.anchors[1].point else { panic!("point 1 should animate") };
        assert_eq!(t.frames(), vec![0, 10]);
        assert_eq!(t.keys()[1].value, Vec2::new(20.0, 0.0));
    }
}
