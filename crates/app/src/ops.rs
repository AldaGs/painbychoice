//! The agent surface over a `.pbc` (ADR 0028): `query` reads a compact
//! summary, `apply` runs a batch of edit ops atomically, `describe` lists what
//! the engine can do. All three print one JSON value.
//!
//! `apply` edits the file in place and bumps `rev`; an open editor takes the
//! write as one undo step (ADR 0024). Ops are validated together: if any
//! fails, or the result is invalid, nothing is written.

use std::path::Path;

use motion_core::node::{LayerTiming, Node, NodeId, Shape};
use motion_core::{CompId, Project};
use serde_json::{json, Value as J};

/// The file as JSON (so `apply` keeps the editor's layout and any other keys)
/// and its project, migrated.
fn open(path: &Path) -> Result<(J, Project), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let file: J = serde_json::from_str(&text).map_err(|e| format!("{}: not JSON: {e}", path.display()))?;
    let project = Project::from_pbc(&text)?;
    Ok((file, project))
}

fn kind(n: &Node) -> &'static str {
    match (&n.shape, n.precomp, &n.audio) {
        (_, Some(_), _) => "precomp",
        (Some(Shape::Text { .. }), ..) => "text",
        (Some(Shape::Image { .. }), ..) => "footage",
        (Some(_), ..) => "shape",
        (None, _, Some(_)) => "audio",
        _ => "group",
    }
}

/// `motion query`: comps, top-level layers with timing, buses, outputs, assets,
/// and each caption layer's subtitles (`words` adds per-word frames).
pub fn query(path: &Path, words: bool) -> Result<J, String> {
    let (file, p) = open(path)?;
    let comps: Vec<J> = p
        .comps
        .iter()
        .map(|(id, c)| {
            let fps = c.fps;
            let tb = c.timebase();
            let layers: Vec<J> = c
                .root
                .children
                .iter()
                .map(|n| {
                    let t = n.timing.unwrap_or(LayerTiming::new(0, c.duration_frames));
                    let mut l = json!({
                        "id": n.id.0, "name": n.name, "kind": kind(n),
                        "in": t.in_, "out": t.out, "start": t.start,
                        "seconds": [t.in_ as f64 / fps, t.out as f64 / fps],
                    });
                    let o = l.as_object_mut().unwrap();
                    if n.hidden {
                        o.insert("hidden".into(), true.into());
                    }
                    if let Some(s) = n.sync {
                        o.insert("sync".into(), s.into());
                    }
                    if !n.tags.is_empty() {
                        o.insert("tags".into(), json!(n.tags));
                    }
                    if !n.children.is_empty() {
                        o.insert("children".into(), n.children.len().into());
                    }
                    if let Some(a) = &n.audio {
                        o.insert("bus".into(), json!(a.bus));
                    }
                    if let Some(Shape::Text { content, .. }) = &n.shape {
                        if n.subtitles.is_none() {
                            let s = content.resolve(&mut motion_core::expr::EvalCtx::at(0.0));
                            o.insert("text".into(), s.chars().take(80).collect::<String>().into());
                        }
                    }
                    if let Some(s) = &n.subtitles {
                        let w = &s.transcript.words;
                        let subs: Vec<J> = motion_core::subs::layout(w, &s.rules, &tb)
                            .iter()
                            .map(|x| json!({ "from": t.start + x.start, "to": t.start + x.end, "text": x.text(w) }))
                            .collect();
                        o.insert("subtitles".into(), subs.into());
                        if words {
                            let f = |s: f64| t.start + tb.seconds_to_frames(s);
                            let ws: Vec<J> = w.iter().map(|x| json!([x.text, f(x.start), f(x.end)])).collect();
                            o.insert("words".into(), ws.into());
                        }
                    }
                    l
                })
                .collect();
            let cuts: Vec<J> = c.cuts.iter().enumerate().map(|(i, x)| json!({ "cut": i, "at": x.at, "frames": x.len, "said": x.text })).collect();
            json!({
                "id": id.0, "name": c.label(*id), "width": c.width, "height": c.height,
                "fps": fps, "frames": c.duration_frames, "layers": layers, "cuts": cuts,
            })
        })
        .collect();
    let assets: Vec<J> = p
        .assets
        .values()
        .map(|a| {
            let proxy = motion_render::proxy::existing(&a.path).map(|p| p.display().to_string());
            json!({ "id": a.id.0, "name": a.name, "path": a.path.display().to_string(), "proxy": proxy })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "rev": file["rev"].as_u64().unwrap_or(0),
        "root": p.root.0,
        "comps": comps,
        "buses": p.buses,
        "outputs": p.outputs,
        "assets": assets,
    }))
}

/// `motion proxy`: make the preview proxies the editor would make for this
/// project's video (ADR 0029). `all` includes footage too small to want one.
/// `motion proxy --prune [GB]`: shrink the proxy folder to `gb` (default:
/// the usual budget), least recently used first.
pub fn prune_proxies(gb: Option<f64>) -> Result<J, String> {
    let max = gb.map_or(motion_render::proxy::budget(), |g| (g.max(0.0) * 1e9) as u64);
    let (files, bytes) = motion_render::proxy::prune(max);
    Ok(json!({ "ok": true, "removed": files, "freedBytes": bytes, "dir": motion_render::proxy::dir().map(|d| d.display().to_string()) }))
}

pub fn proxy(path: &Path, all: bool) -> Result<J, String> {
    let (_, p) = open(path)?;
    let mut made = Vec::new();
    for a in p.assets.values().filter(|a| a.kind == motion_core::AssetKind::Video) {
        if !all && !motion_render::proxy::wanted(a.width, a.height) {
            continue;
        }
        let had = motion_render::proxy::existing(&a.path).is_some();
        let r = motion_render::proxy::make(&a.path);
        made.push(match r {
            Ok(proxy) => json!({ "asset": a.id.0, "source": a.path.display().to_string(), "proxy": proxy.display().to_string(), "new": !had }),
            Err(e) => json!({ "asset": a.id.0, "source": a.path.display().to_string(), "error": e }),
        });
    }
    let ok = made.iter().all(|m| m.get("error").is_none());
    Ok(json!({ "ok": ok, "proxies": made }))
}

/// `motion apply`: run `ops` against the file, then write it with `rev + 1`.
/// Returns the per-op results. Nothing is written on any error.
pub fn apply(path: &Path, ops: &J) -> Result<J, String> {
    let (mut file, mut p) = open(path)?;
    let list = ops.get("ops").unwrap_or(ops).as_array().ok_or("ops must be a JSON array (or {\"ops\": [...]})")?;
    let mut results = Vec::new();
    for (i, op) in list.iter().enumerate() {
        let r = run(&mut p, op).map_err(|e| format!("op {i} ({}): {e}", op["op"].as_str().unwrap_or("?")))?;
        results.push(r);
    }
    validate(&p)?;
    let rev = file["rev"].as_u64().unwrap_or(0) + 1;
    let obj = file.as_object_mut().ok_or("the file is not a JSON object")?;
    obj.remove("document");
    obj.insert("project".into(), serde_json::to_value(&p).map_err(|e| e.to_string())?);
    obj.insert("rev".into(), rev.into());
    let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    // Write beside and rename, so an editor polling the file never reads half of it.
    let tmp = path.with_extension("pbc.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(json!({ "ok": true, "rev": rev, "results": results }))
}

/// One id per layer in each comp, and a root that exists — what the editor's
/// live link refuses otherwise.
fn validate(p: &Project) -> Result<(), String> {
    fn walk(n: &Node, seen: &mut std::collections::HashSet<u64>) -> Option<u64> {
        (!seen.insert(n.id.0)).then_some(n.id.0).or_else(|| n.children.iter().find_map(|c| walk(c, seen)))
    }
    for (id, c) in &p.comps {
        if let Some(dup) = walk(&c.root, &mut Default::default()) {
            return Err(format!("comp {} would have two layers with id {dup}", id.0));
        }
    }
    p.comp(p.root).map(|_| ()).ok_or_else(|| "root names a comp that does not exist".into())
}

fn max_id(n: &Node) -> u64 {
    n.children.iter().map(max_id).fold(n.id.0, u64::max)
}

fn num(op: &J, key: &str) -> Result<i64, String> {
    op[key].as_i64().ok_or_else(|| format!("`{key}` must be a whole number of frames"))
}

/// Set `value` at JSON pointer `path` inside `target` (via serde), creating
/// the last key if it is missing.
fn set_at<T: serde::Serialize + serde::de::DeserializeOwned>(target: &mut T, path: &str, value: J) -> Result<(), String> {
    let mut v = serde_json::to_value(&*target).map_err(|e| e.to_string())?;
    let (parent, key) = path.rsplit_once('/').ok_or("`path` is a JSON pointer, like /audio/bus")?;
    let slot = v.pointer_mut(parent).ok_or_else(|| format!("nothing at {parent}"))?;
    match slot {
        J::Object(m) => {
            m.insert(key.replace("~1", "/").replace("~0", "~"), value);
        }
        J::Array(a) => {
            let i: usize = key.parse().map_err(|_| format!("{key} is not an index"))?;
            *a.get_mut(i).ok_or_else(|| format!("index {i} is out of range"))? = value;
        }
        _ => return Err(format!("{parent} is not an object or array")),
    }
    *target = serde_json::from_value(v).map_err(|e| format!("{path}: {e}"))?;
    Ok(())
}

fn run(p: &mut Project, op: &J) -> Result<J, String> {
    let comp = op["comp"].as_u64().map_or(p.root, CompId);
    let name = op["op"].as_str().ok_or("missing `op`")?;
    if name == "set" && op.get("layer").is_none() {
        set_at(p, op["path"].as_str().ok_or("missing `path`")?, op["value"].clone())?;
        return Ok(json!({}));
    }
    let c = p.comps.get_mut(&comp).ok_or_else(|| format!("no comp {}", comp.0))?;
    let frames = c.duration_frames;
    let mut next = max_id(&c.root).max(c.removed_id_floor.saturating_sub(1)) + 1;
    let layer = op["layer"].as_u64().map(NodeId);
    let need = || layer.ok_or("missing `layer` (an id)");
    let out = match name {
        "set" => {
            let n = c.root.find_mut(need()?).ok_or("no such layer")?;
            set_at(n, op["path"].as_str().ok_or("missing `path`")?, op["value"].clone())?;
            json!({})
        }
        "trim" | "move" => {
            let id = need()?;
            let n = c.root.find_mut(id).ok_or("no such layer")?;
            let mut t = n.timing.unwrap_or(LayerTiming::new(0, frames));
            if name == "move" {
                let d = num(op, "to")? - t.in_;
                t = LayerTiming { start: t.start + d, in_: t.in_ + d, out: t.out + d };
            } else {
                if op.get("in").is_some() {
                    t.in_ = num(op, "in")?;
                }
                if op.get("out").is_some() {
                    t.out = num(op, "out")?;
                }
                if t.out <= t.in_ {
                    return Err(format!("out ({}) must be after in ({})", t.out, t.in_));
                }
            }
            let patch = motion_core::edit::with_sync(&c.root, frames, [(id, t)].into());
            motion_core::edit::apply(&mut c.root, &patch);
            json!({ "changed": patch.keys().map(|k| k.0).collect::<Vec<_>>() })
        }
        "insert" => {
            let mut base = serde_json::to_value(Node::group(next, "Layer")).map_err(|e| e.to_string())?;
            let given = op["layer"].as_object().ok_or("`layer` must be the new layer's JSON")?;
            base.as_object_mut().unwrap().extend(given.clone());
            let n: Node = serde_json::from_value(base).map_err(|e| format!("layer: {e}"))?;
            let id = n.id.0;
            let parent = match op["parent"].as_u64() {
                Some(pid) => c.root.find_mut(NodeId(pid)).ok_or("no such parent")?,
                None => &mut c.root,
            };
            let at = op["index"].as_u64().map_or(parent.children.len(), |i| (i as usize).min(parent.children.len()));
            parent.children.insert(at, n);
            json!({ "id": id })
        }
        "delete" => {
            let id = need()?;
            p.delete_layer(comp, id).ok_or("no such layer")?;
            json!({})
        }
        "razor" => {
            let ids = motion_core::edit::synced(&c.root, need()?);
            let made = motion_core::edit::razor(&mut c.root, frames, &ids, num(op, "at")?, &mut next);
            if made.is_empty() {
                return Err("`at` is not inside the layer".into());
            }
            json!({ "made": made.iter().map(|k| k.0).collect::<Vec<_>>() })
        }
        "cut" => {
            let (a, b) = (num(op, "from")?, num(op, "to")?);
            if b <= a {
                return Err("`to` must be after `from`".into());
            }
            let _ = c;
            motion_core::edit::cut_range(p, comp, a, b, &mut next);
            let c = p.comp(comp).unwrap();
            let cut = c.cuts.last().unwrap();
            json!({
                "removed": cut.removed.iter().map(|(_, n)| n.id.0).collect::<Vec<_>>(),
                "said": cut.text, "cut": c.cuts.len() - 1, "frames": c.duration_frames,
            })
        }
        "restore" => {
            let i = op["cut"].as_u64().ok_or("`cut` must be the index `query` lists")? as usize;
            let _ = c;
            if !motion_core::edit::restore_cut(p, comp, i) {
                return Err(format!("no cut {i}"));
            }
            json!({ "frames": p.comp(comp).unwrap().duration_frames })
        }
        "preset" => {
            let want = op["name"].as_str().ok_or("missing `name`")?;
            let style = motion_core::captions::builtin()
                .iter()
                .chain(&p.caption_presets)
                .find(|s| s.name.eq_ignore_ascii_case(want))
                .cloned()
                .ok_or_else(|| format!("no caption preset '{want}' (see `motion describe`)"))?;
            let c = p.comps.get_mut(&comp).unwrap();
            let (w, h) = (c.width, c.height);
            let n = c.root.find_mut(need()?).ok_or("no such layer")?;
            if !matches!(n.shape, Some(Shape::Text { .. })) {
                return Err("presets apply to text layers".into());
            }
            motion_core::captions::apply(n, &style, w, h);
            json!({})
        }
        other => return Err(format!("unknown op '{other}' (see `motion describe op`)")),
    };
    Ok(out)
}

/// What `apply` understands, with an example each — `describe`'s op entries.
const OPS: &[(&str, &str, &str)] = &[
    ("op.set", "Set any field of a layer (or, without `layer`, of the project) by JSON pointer.", r#"{"op":"set","layer":4,"path":"/audio/bus","value":"dlg"}"#),
    ("op.trim", "Set a layer's in and/or out (comp frames). Sync-locked layers follow.", r#"{"op":"trim","layer":4,"in":12,"out":96}"#),
    ("op.move", "Slide a layer so its in-point lands on `to`, content and keys with it.", r#"{"op":"move","layer":4,"to":48}"#),
    ("op.insert", "Add a layer from JSON (fields default as a group; `id` defaults to a fresh one). Optional `parent`, `index`.", r#"{"op":"insert","layer":{"name":"Title","timing":{"start":0,"in_":0,"out":48}}}"#),
    ("op.delete", "Remove a layer and everything inside it.", r#"{"op":"delete","layer":4}"#),
    ("op.razor", "Split a layer (and its sync group) at comp frame `at`.", r#"{"op":"razor","layer":4,"at":60}"#),
    ("op.cut", "Remove comp frames from..to from every layer and close the gap; captions lose those words.", r#"{"op":"cut","from":120,"to":150}"#),
    ("op.restore", "Put back a cut (index from `query`'s `cuts`): time, layers and words return.", r#"{"op":"restore","cut":0}"#),
    ("op.preset", "Bake a caption preset into a text layer.", r#"{"op":"preset","layer":7,"name":"Viral pop"}"#),
];

/// `motion describe [id]`: the index of capabilities, or one in full.
pub fn describe(id: Option<&str>, project: Option<&Project>) -> Result<J, String> {
    let mut all: Vec<J> = Vec::new();
    for (id, summary, example) in OPS {
        all.push(json!({ "id": id, "kind": "op", "summary": summary, "example": serde_json::from_str::<J>(example).unwrap() }));
    }
    for t in motion_core::effect::EffectType::ALL {
        all.push(json!({
            "id": t.id(), "kind": "effect", "summary": format!("{} ({})", t.label(), t.category()),
            "params": t.params().iter().map(|p| p.id).collect::<Vec<_>>(),
            "writes": "an entry in layer.effects",
            "example": motion_core::effect::Effect::seed(t),
        }));
    }
    for d in motion_core::registry::NodeRegistry::with_builtins().iter() {
        all.push(json!({
            "id": format!("node.{}", d.id), "kind": "node", "summary": format!("{} ({})", d.label, d.category.label()),
            "inputs": d.inputs.iter().map(|s| json!({ "id": s.id, "type": s.ty })).collect::<Vec<_>>(),
            "outputs": d.outputs.iter().map(|s| json!({ "id": s.id, "type": s.ty })).collect::<Vec<_>>(),
        }));
    }
    let presets = motion_core::captions::builtin().iter().chain(project.map_or(&[][..], |p| &p.caption_presets));
    for s in presets {
        all.push(json!({ "id": format!("caption.{}", s.name), "kind": "caption-preset", "summary": format!("{} · {}", s.family, s.animation), "example": s }));
    }
    let Some(want) = id else {
        let index: Vec<J> = all.iter().map(|d| json!({ "id": d["id"], "summary": d["summary"] })).collect();
        return Ok(json!({ "ok": true, "count": index.len(), "capabilities": index, "more": "motion describe <id> for one in full; motion schema for the authoring format" }));
    };
    let hits: Vec<&J> = all.iter().filter(|d| d["id"].as_str().is_some_and(|i| i == want || i.starts_with(&format!("{want}.")))).collect();
    match hits.as_slice() {
        [] => Err(format!("nothing named '{want}'")),
        [one] => Ok(json!({ "ok": true, "capability": one })),
        many => Ok(json!({ "ok": true, "capabilities": many })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> std::path::PathBuf {
        let mut a = Node::group(1, "a");
        a.timing = Some(LayerTiming::new(0, 100));
        let mut c = motion_core::node::Comp::new(64.0, 64.0, Node::group(0, "root").with_child(a));
        c.duration_frames = 100;
        let p = Project::single(c);
        let path = std::env::temp_dir().join(format!("pbc_ops_{name}_{}.pbc", std::process::id()));
        std::fs::write(&path, json!({ "project": p, "rev": 3 }).to_string()).unwrap();
        path
    }

    /// A batch runs in order and writes once with rev + 1; a failing batch
    /// writes nothing.
    #[test]
    fn apply_is_atomic_and_bumps_rev() {
        let path = file("atomic");
        let ops = json!([
            { "op": "razor", "layer": 1, "at": 40 },
            { "op": "set", "layer": 1, "path": "/name", "value": "left" },
            { "op": "cut", "from": 40, "to": 50 },
        ]);
        let r = apply(&path, &ops).unwrap();
        assert_eq!(r["rev"], 4);
        let q = query(&path, false).unwrap();
        let layers = q["comps"][0]["layers"].as_array().unwrap();
        assert_eq!(layers[0]["name"], "left");
        assert_eq!((layers[1]["in"].as_i64(), layers[1]["out"].as_i64()), (Some(40), Some(90)));
        assert_eq!(q["comps"][0]["frames"], 90);

        let before = std::fs::read_to_string(&path).unwrap();
        assert!(apply(&path, &json!([{ "op": "set", "layer": 1, "path": "/name", "value": "x" }, { "op": "delete", "layer": 99 }])).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "nothing written");
        let _ = std::fs::remove_file(&path);
    }

    /// `apply delete` leaves no graph node naming the deleted layer, as the
    /// editor's delete doesn't.
    #[test]
    fn delete_untargets_the_graph() {
        let path = file("delete");
        let (_, mut p) = open(&path).unwrap();
        let osc = p.graph.add_node("osc", kurbo::Vec2::ZERO);
        let sink = p.graph.bind_output(motion_core::graph::Endpoint::new(osc, "value"), p.root, NodeId(1), motion_core::expr::PropPath::Rotation);
        std::fs::write(&path, json!({ "project": p, "rev": 0 }).to_string()).unwrap();
        apply(&path, &json!([{ "op": "delete", "layer": 1 }])).unwrap();
        let (_, p) = open(&path).unwrap();
        let cfg = &p.graph.node(sink).unwrap().config;
        assert!(cfg.out_target.is_none() && cfg.out_comp.is_none(), "the sink no longer names layer 1");
        assert!(p.graph.bindings().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    /// A cut is listed by `query` and `restore` puts the timeline back.
    #[test]
    fn a_cut_can_be_restored() {
        let path = file("restore");
        let before = query(&path, false).unwrap()["comps"][0]["layers"].clone();
        apply(&path, &json!([{ "op": "cut", "from": 40, "to": 50 }])).unwrap();
        let q = query(&path, false).unwrap();
        assert_eq!(q["comps"][0]["frames"], 90);
        assert_eq!(q["comps"][0]["cuts"][0]["frames"], 10);
        apply(&path, &json!([{ "op": "restore", "cut": 0 }])).unwrap();
        let q = query(&path, false).unwrap();
        assert_eq!(q["comps"][0]["frames"], 100);
        assert_eq!(q["comps"][0]["layers"], before, "the same layer, the same timing");
        assert!(apply(&path, &json!([{ "op": "restore", "cut": 0 }])).is_err(), "nothing left to restore");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn describe_lists_ops_effects_and_presets() {
        let d = describe(None, None).unwrap();
        let ids: Vec<&str> = d["capabilities"].as_array().unwrap().iter().filter_map(|c| c["id"].as_str()).collect();
        assert!(ids.contains(&"op.cut") && ids.contains(&"fx.gaussian_blur") && ids.contains(&"caption.Viral pop"));
        assert!(describe(Some("op.cut"), None).unwrap()["capability"]["example"].is_object());
    }
}
