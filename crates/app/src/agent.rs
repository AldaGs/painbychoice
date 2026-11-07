//! The agent-facing half of the CLI: `check`, `preview` and `schema`.
//!
//! Built so one write of a spec and one command answer everything an agent
//! needs for its next move — is it valid, what was dropped, what does it look
//! like. So every command takes either a `.pbc` or the authoring JSON directly
//! (no import step), prints one JSON report on stdout, and exits non-zero on
//! any error.
//!
//! The validator is deliberately stricter than the importer. The importer
//! defaults anything it can't read, which is right for an AE export and wrong
//! for a spec someone just wrote: a misspelled `postion` must come back as a
//! finding with its path, not as a layer quietly sitting at the origin.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use motion_core::{CompId, Project};
use serde_json::{json, Value as J};

/// The authoring format reference, printed by `motion schema`.
pub const REFERENCE: &str = include_str!("authoring.md");

/// One finding, addressed by its JSON path (`compositions[0].layers[2].type`).
pub struct Issue {
    pub path: String,
    pub message: String,
}

#[derive(Default)]
pub struct Findings {
    /// Structural problems: the spec doesn't mean what it was written to.
    pub errors: Vec<Issue>,
    /// Accepted but suspicious or partly ignored: unknown keys, dropped types.
    pub warnings: Vec<Issue>,
}

impl Findings {
    fn err(&mut self, path: &str, msg: impl Into<String>) {
        self.errors.push(Issue { path: path.into(), message: msg.into() });
    }
    fn warn(&mut self, path: &str, msg: impl Into<String>) {
        self.warnings.push(Issue { path: path.into(), message: msg.into() });
    }

    pub fn to_json(&self) -> (J, J) {
        let list = |v: &[Issue]| J::Array(v.iter().map(|i| json!({ "path": i.path, "message": i.message })).collect());
        (list(&self.errors), list(&self.warnings))
    }
}

/// A loaded input: the project, what validation found, and what the importer
/// could not carry across.
pub struct Loaded {
    pub project: Project,
    pub findings: Findings,
    pub dropped: BTreeMap<String, usize>,
}

/// Read a `.pbc` or an authoring / AE-export JSON. Authoring input is validated
/// first; a `.pbc` is the app's own format and only has to parse.
///
/// `Err` only when nothing could be loaded at all (missing file, not JSON).
/// A spec with errors still loads, so the report can carry them all at once.
pub fn load(path: &Path) -> Result<Loaded, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let doc: J = serde_json::from_str(&text).map_err(|e| format!("{}: not JSON: {e}", path.display()))?;
    let mut findings = Findings::default();
    if doc.get("compositions").is_some() {
        validate(&doc, &mut findings);
    }
    if doc.get("compositions").is_some() || doc.get("root").is_some() {
        if !findings.errors.is_empty() {
            // Importing a broken spec would only add confusing knock-on errors.
            return Ok(Loaded { project: Project::single(motion_core::demo::demo_document()), findings, dropped: Default::default() });
        }
        let (project, report) = motion_core::ae::import(&text)?;
        return Ok(Loaded { project, findings, dropped: report.dropped });
    }
    let project = Project::from_pbc(&text)?;
    Ok(Loaded { project, findings, dropped: Default::default() })
}

// --- validation ------------------------------------------------------------

const DOC_KEYS: &[&str] = &["$schema", "version", "file", "compositions", "assets"];
const COMP_KEYS: &[&str] = &["name", "width", "height", "fps", "duration", "backgroundColor", "layers"];
const LAYER_KEYS: &[&str] = &[
    "name", "type", "startTime", "in", "out", "source", "parent", "blendMode", "trackMatte", "transform", "text", "shapes",
    "effects", "adjustment", "enabled", "threeD", "stretch", "masks", "jsx",
];
const LAYER_TYPES: &[&str] = &["shape", "text", "null", "precomp", "adjustment", "camera", "light"];
/// Accepted by the format, not drawn by PBC yet.
const DROPPED_TYPES: &[&str] = &["solid", "footage", "audio"];
const TRANSFORM_KEYS: &[&str] = &[
    "anchorPoint", "position", "xPosition", "yPosition", "scale", "rotation", "zRotation", "xRotation", "yRotation",
    "orientation", "opacity",
];
const TEXT_KEYS: &[&str] = &[
    "string", "font", "fontSize", "fillColor", "tracking", "justification", "applyFill", "applyStroke", "leading",
    "strokeColor", "strokeWidth", "boxSize",
];
const GROUP_TRANSFORM_KEYS: &[&str] = &["anchorPoint", "position", "scale", "rotation", "opacity"];
const KEYFRAME_KEYS: &[&str] = &["time", "value", "interp", "ease", "inTangent", "outTangent"];
const EASE_KEYS: &[&str] = &["inSpeed", "inInfluence", "outSpeed", "outInfluence"];
const EFFECTS: &[&str] = &[
    "ADBE Gaussian Blur 2", "ADBE Drop Shadow", "ADBE Fill", "ADBE Brightness & Contrast 2", "ADBE HUE SATURATION",
    "ADBE Exposure2", "ADBE Sharpen", "ADBE Noise", "ADBE Mosaic", "ADBE Fractal Noise", "ADBE Unmult", "APC Colorama",
];

/// The animatable keys of each shape item type.
fn shape_keys(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "group" => &["transform", "items"],
        "rect" => &["size", "position", "roundness"],
        "ellipse" => &["size", "position"],
        "path" => &["path"],
        "fill" => &["color", "opacity"],
        "stroke" => &["color", "strokeWidth", "opacity", "lineCap", "lineJoin", "dashes"],
        "trim" => &["start", "end", "offset"],
        _ => return None,
    })
}

fn validate(doc: &J, f: &mut Findings) {
    keys(doc, "", DOC_KEYS, f);
    let Some(comps) = doc["compositions"].as_array() else {
        return f.err("compositions", "must be an array of compositions");
    };
    if comps.is_empty() {
        f.err("compositions", "needs at least one composition (the first is rendered)");
    }
    let comp_names: Vec<&str> = comps.iter().filter_map(|c| c["name"].as_str()).collect();
    for (ci, c) in comps.iter().enumerate() {
        let p = format!("compositions[{ci}]");
        if !c.is_object() {
            f.err(&p, "must be an object");
            continue;
        }
        keys(c, &p, COMP_KEYS, f);
        if c["name"].as_str().is_none() {
            f.err(&format!("{p}.name"), "required string");
        }
        for k in ["width", "height", "duration"] {
            if !c[k].as_f64().is_some_and(|v| v > 0.0) {
                f.err(&format!("{p}.{k}"), "required positive number");
            }
        }
        if !c["fps"].is_null() && !c["fps"].as_f64().is_some_and(|v| v > 0.0) {
            f.err(&format!("{p}.fps"), "must be a positive number (default 24)");
        }
        if !c["backgroundColor"].is_null() && !is_color(&c["backgroundColor"]) {
            f.err(&format!("{p}.backgroundColor"), "must be \"#rrggbb\" or [r, g, b] in 0..1");
        }
        let duration = c["duration"].as_f64().unwrap_or(f64::INFINITY);
        let Some(layers) = c["layers"].as_array() else {
            f.err(&format!("{p}.layers"), "required array (first layer draws on top)");
            continue;
        };
        let names: Vec<&str> = layers.iter().filter_map(|l| l["name"].as_str()).collect();
        let mut seen = HashMap::new();
        for (li, l) in layers.iter().enumerate() {
            let lp = format!("{p}.layers[{li}]");
            layer(l, &lp, duration, &names, &comp_names, f);
            if let Some(n) = l["name"].as_str() {
                if let Some(prev) = seen.insert(n, li) {
                    f.warn(&format!("{lp}.name"), format!("duplicate name '{n}' (also layers[{prev}]); parent/matte references hit the last one"));
                }
            }
        }
    }
}

fn layer(l: &J, p: &str, duration: f64, names: &[&str], comps: &[&str], f: &mut Findings) {
    if !l.is_object() {
        return f.err(p, "must be an object");
    }
    keys(l, p, LAYER_KEYS, f);
    if l["name"].as_str().is_none() {
        f.err(&format!("{p}.name"), "required string");
    }
    let kind = l["type"].as_str().unwrap_or("");
    if DROPPED_TYPES.contains(&kind) {
        f.warn(&format!("{p}.type"), format!("'{kind}' layers are not drawn yet; use a shape layer with a rect for a solid"));
    } else if !LAYER_TYPES.contains(&kind) {
        f.err(&format!("{p}.type"), format!("unknown layer type '{kind}'{}; expected one of {}", suggest(kind, LAYER_TYPES), LAYER_TYPES.join(", ")));
    }
    for k in ["stretch", "masks", "jsx"] {
        if !l[k].is_null() {
            f.warn(&format!("{p}.{k}"), "not supported; ignored");
        }
    }
    for k in ["startTime", "in", "out"] {
        if !l[k].is_null() && l[k].as_f64().is_none() {
            f.err(&format!("{p}.{k}"), "must be a number of seconds");
        }
    }
    if let (Some(i), Some(o)) = (l["in"].as_f64(), l["out"].as_f64()) {
        if o <= i {
            f.warn(&format!("{p}.out"), "out is not after in; the layer runs to the comp's end");
        }
    }
    reference(l, "parent", p, names, "layer", f);
    if kind == "precomp" {
        match l["source"].as_str() {
            Some(s) if comps.contains(&s) => {}
            Some(s) => f.err(&format!("{p}.source"), format!("no composition named '{s}'{}", suggest(s, comps))),
            None => f.err(&format!("{p}.source"), "a precomp layer needs the name of the composition it shows"),
        }
    }
    if let Some(tm) = l.get("trackMatte").filter(|v| !v.is_null()) {
        let tp = format!("{p}.trackMatte");
        keys(tm, &tp, &["type", "target"], f);
        if let Some(t) = tm["type"].as_str() {
            const MODES: &[&str] = &["Alpha", "Alpha Inverted", "Luma", "Luma Inverted"];
            if !MODES.contains(&t) {
                f.err(&format!("{tp}.type"), format!("expected one of {}", MODES.join(", ")));
            }
        }
        reference(tm, "target", &tp, names, "layer", f);
    }
    if let Some(tf) = l.get("transform").filter(|v| !v.is_null()) {
        let tp = format!("{p}.transform");
        keys(tf, &tp, TRANSFORM_KEYS, f);
        for k in TRANSFORM_KEYS {
            animatable(&tf[*k], &format!("{tp}.{k}"), duration, f);
        }
    }
    if let Some(t) = l.get("text").filter(|v| !v.is_null()) {
        let tp = format!("{p}.text");
        keys(t, &tp, TEXT_KEYS, f);
        if t["string"].as_str().is_none() {
            f.err(&format!("{tp}.string"), "required string");
        }
        for k in ["fillColor", "strokeColor"] {
            if !t[k].is_null() && !is_color(&t[k]) {
                f.err(&format!("{tp}.{k}"), "must be \"#rrggbb\" or [r, g, b] in 0..1");
            }
        }
        if let Some(j) = t["justification"].as_str() {
            if !["left", "center", "right"].contains(&j) {
                f.err(&format!("{tp}.justification"), "expected left, center or right");
            }
        }
    } else if kind == "text" {
        f.err(&format!("{p}.text"), "a text layer needs a text object with a string");
    }
    match l.get("shapes") {
        Some(J::Array(items)) => {
            for (i, s) in items.iter().enumerate() {
                shape(s, &format!("{p}.shapes[{i}]"), duration, f);
            }
        }
        Some(J::Null) | None if kind == "shape" => f.err(&format!("{p}.shapes"), "a shape layer needs a shapes array"),
        Some(J::Null) | None => {}
        Some(_) => f.err(&format!("{p}.shapes"), "must be an array"),
    }
    if let Some(fx) = l.get("effects").filter(|v| !v.is_null()) {
        let Some(fx) = fx.as_array() else { return f.err(&format!("{p}.effects"), "must be an array") };
        for (i, e) in fx.iter().enumerate() {
            let ep = format!("{p}.effects[{i}]");
            keys(e, &ep, &["matchName", "name", "props"], f);
            match e["matchName"].as_str() {
                Some(m) if EFFECTS.contains(&m) => {}
                Some(m) => f.warn(&format!("{ep}.matchName"), format!("effect '{m}' is not supported and is dropped{}", suggest(m, EFFECTS))),
                None => f.err(&format!("{ep}.matchName"), "required string"),
            }
            if let Some(props) = e["props"].as_object() {
                for (k, v) in props {
                    animatable(v, &format!("{ep}.props.{k}"), duration, f);
                }
            }
        }
    }
}

fn shape(s: &J, p: &str, duration: f64, f: &mut Findings) {
    let kind = s["type"].as_str().unwrap_or("");
    let Some(props) = shape_keys(kind) else {
        const KINDS: &[&str] = &["group", "rect", "ellipse", "path", "fill", "stroke", "trim"];
        return f.err(&format!("{p}.type"), format!("unknown shape type '{kind}'{}; expected one of {}", suggest(kind, KINDS), KINDS.join(", ")));
    };
    let mut allowed = vec!["type", "name"];
    allowed.extend_from_slice(props);
    keys(s, p, &allowed, f);
    match kind {
        "group" => {
            if let Some(tf) = s.get("transform").filter(|v| !v.is_null()) {
                keys(tf, &format!("{p}.transform"), GROUP_TRANSFORM_KEYS, f);
                for k in GROUP_TRANSFORM_KEYS {
                    animatable(&tf[*k], &format!("{p}.transform.{k}"), duration, f);
                }
            }
            match s.get("items") {
                Some(J::Array(items)) => {
                    for (i, it) in items.iter().enumerate() {
                        shape(it, &format!("{p}.items[{i}]"), duration, f);
                    }
                }
                _ => f.err(&format!("{p}.items"), "a group needs an items array"),
            }
        }
        "path" => {
            let pp = format!("{p}.path");
            keys(&s["path"], &pp, &["vertices", "inTangents", "outTangents", "closed"], f);
            if !s["path"]["vertices"].as_array().is_some_and(|v| v.len() >= 2) {
                f.err(&format!("{pp}.vertices"), "needs at least two [x, y] points");
            }
        }
        "stroke" => {
            for (k, ok) in [("lineCap", ["butt", "round", "projecting"]), ("lineJoin", ["miter", "round", "bevel"])] {
                if let Some(v) = s[k].as_str() {
                    if !ok.contains(&v) {
                        f.err(&format!("{p}.{k}"), format!("expected one of {}", ok.join(", ")));
                    }
                }
            }
            if let Some(d) = s.get("dashes").filter(|v| !v.is_null()) {
                keys(d, &format!("{p}.dashes"), &["dash", "gap", "offset"], f);
            }
            for k in ["color", "strokeWidth", "opacity"] {
                animatable(&s[k], &format!("{p}.{k}"), duration, f);
            }
        }
        _ => {
            for k in props {
                animatable(&s[*k], &format!("{p}.{k}"), duration, f);
            }
        }
    }
}

/// A value that may be static (`5`, `[x, y]`, `"#ff0000"`) or animated
/// (`{ "keyframes": [...] }`, `{ "expression": "..." }`).
fn animatable(v: &J, p: &str, duration: f64, f: &mut Findings) {
    match v {
        J::Null | J::Number(_) | J::Bool(_) => {}
        J::String(_) if is_color(v) => {}
        J::String(s) => f.err(p, format!("'{s}' is not a value; strings are only \"#rrggbb\" colours")),
        J::Array(a) if a.iter().all(J::is_number) => {}
        J::Array(_) => f.err(p, "arrays must hold only numbers"),
        J::Object(_) => {
            keys(v, p, &["value", "keyframes", "expression"], f);
            if v["value"].is_null() && v["keyframes"].is_null() && v["expression"].is_null() {
                f.err(p, "an object value needs value, keyframes or expression");
            }
            if !v["expression"].is_null() && v["expression"].as_str().is_none() {
                f.err(&format!("{p}.expression"), "must be a string");
            }
            let Some(keys_) = v.get("keyframes").filter(|k| !k.is_null()) else { return };
            let Some(keys_) = keys_.as_array() else { return f.err(&format!("{p}.keyframes"), "must be an array") };
            let mut last = f64::NEG_INFINITY;
            for (i, k) in keys_.iter().enumerate() {
                let kp = format!("{p}.keyframes[{i}]");
                keys(k, &kp, KEYFRAME_KEYS, f);
                match k["time"].as_f64() {
                    Some(t) => {
                        if t < last {
                            f.err(&format!("{kp}.time"), format!("keyframes must be in time order ({t} after {last})"));
                        }
                        if t < 0.0 || t > duration {
                            f.warn(&format!("{kp}.time"), format!("{t}s is outside the comp (0..{duration}s); times are comp seconds"));
                        }
                        last = t;
                    }
                    None => f.err(&format!("{kp}.time"), "required number of seconds (comp time)"),
                }
                if k["value"].is_null() {
                    f.err(&format!("{kp}.value"), "required");
                } else {
                    animatable(&k["value"], &format!("{kp}.value"), duration, f);
                }
                if let Some(i) = k["interp"].as_str() {
                    if !["linear", "bezier", "hold"].contains(&i) {
                        f.err(&format!("{kp}.interp"), "expected linear, bezier or hold");
                    }
                }
                if k["ease"].is_object() {
                    keys(&k["ease"], &format!("{kp}.ease"), EASE_KEYS, f);
                }
            }
        }
    }
}

/// A name that must match a layer (or comp) in scope.
fn reference(o: &J, key: &str, p: &str, names: &[&str], what: &str, f: &mut Findings) {
    match &o[key] {
        J::Null => {}
        J::String(n) if names.contains(&n.as_str()) => {}
        J::String(n) => f.err(&format!("{p}.{key}"), format!("no {what} named '{n}'{}", suggest(n, names))),
        _ => f.err(&format!("{p}.{key}"), format!("must be a {what} name")),
    }
}

/// Unknown keys are warnings, not errors: the format has fields PBC ignores,
/// and refusing them would block a render for nothing. The suggestion is what
/// makes a typo cheap to fix.
fn keys(o: &J, p: &str, allowed: &[&str], f: &mut Findings) {
    let Some(map) = o.as_object() else {
        if !o.is_null() {
            f.err(p, "must be an object");
        }
        return;
    };
    for k in map.keys() {
        if !allowed.contains(&k.as_str()) {
            let at = if p.is_empty() { k.clone() } else { format!("{p}.{k}") };
            f.warn(&at, format!("unknown key '{k}', ignored{}", suggest(k, allowed)));
        }
    }
}

fn is_color(v: &J) -> bool {
    match v {
        J::String(s) => s.strip_prefix('#').is_some_and(|h| h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit())),
        J::Array(a) => (3..=4).contains(&a.len()) && a.iter().all(J::is_number),
        _ => false,
    }
}

/// `" (did you mean 'x'?)"` for the closest candidate within two edits.
fn suggest(word: &str, candidates: &[&str]) -> String {
    fn dist(a: &str, b: &str) -> usize {
        let (a, b): (Vec<char>, Vec<char>) = (a.to_lowercase().chars().collect(), b.to_lowercase().chars().collect());
        let mut row: Vec<usize> = (0..=b.len()).collect();
        for i in 1..=a.len() {
            let mut prev = row[0];
            row[0] = i;
            for j in 1..=b.len() {
                let cur = row[j];
                row[j] = (row[j] + 1).min(row[j - 1] + 1).min(prev + usize::from(a[i - 1] != b[j - 1]));
                prev = cur;
            }
        }
        row[b.len()]
    }
    candidates
        .iter()
        .map(|c| (dist(word, c), c))
        .filter(|(d, _)| *d <= 2)
        .min()
        .map(|(_, c)| format!(" (did you mean '{c}'?)"))
        .unwrap_or_default()
}

// --- rendering ---------------------------------------------------------------

/// One frame as straight RGBA at `scale`, with motion blur, plus its notes.
pub fn frame_pixels(project: &Project, comp_id: CompId, frame: i64, scale: f64) -> Result<(Vec<u8>, Vec<String>), String> {
    let comp = project.comp(comp_id).ok_or("no such composition")?;
    let (mut notes, mut samples) = (Vec::new(), Vec::new());
    for shutter in comp.motion_blur.offsets() {
        let scene = motion_core::evaluate_comp_sample(project, comp_id, frame as f64, shutter, motion_core::mat4::Mat4::IDENTITY);
        notes.extend(scene.warnings.iter().map(|(id, msg)| format!("node {}: {msg}", id.0)));
        let (px, r) = motion_render::rasterize(&scene, comp.width as u32, comp.height as u32, comp.bg, scale).map_err(|e| e.to_string())?;
        samples.push(px);
        notes.extend(r);
    }
    notes.sort_unstable();
    notes.dedup();
    Ok((motion_render::average_frames(&samples), notes))
}

/// `"0,30,60"` or `"auto"` / absent → evenly spaced. Clamped to the comp.
pub fn pick_frames(spec: Option<&str>, last: i64, count: usize) -> Result<Vec<i64>, String> {
    let frames: Vec<i64> = match spec {
        Some(s) if s != "auto" => s
            .split(',')
            .map(|x| x.trim().parse::<i64>().map_err(|_| format!("--frames wants numbers like 0,30,60, not '{x}'")))
            .collect::<Result<_, _>>()?,
        _ => {
            let n = count.max(1) as i64;
            if n == 1 { vec![0] } else { (0..n).map(|i| (i * last + (n - 1) / 2) / (n - 1)).collect() }
        }
    };
    let mut frames: Vec<i64> = frames.into_iter().map(|f| f.clamp(0, last)).collect();
    frames.dedup();
    Ok(frames)
}

/// Tile frames left-to-right, top-to-bottom, with a 4px gutter.
pub fn contact_sheet(cells: &[Vec<u8>], w: u32, h: u32, cols: u32) -> (Vec<u8>, u32, u32) {
    const GAP: u32 = 4;
    let rows = (cells.len() as u32).div_ceil(cols);
    let (sw, sh) = (cols * w + (cols + 1) * GAP, rows * h + (rows + 1) * GAP);
    let mut out = vec![0u8; (sw * sh * 4) as usize];
    for px in out.chunks_exact_mut(4) {
        px.copy_from_slice(&[40, 40, 44, 255]);
    }
    for (i, cell) in cells.iter().enumerate() {
        let (cx, cy) = (i as u32 % cols, i as u32 / cols);
        let (ox, oy) = (GAP + cx * (w + GAP), GAP + cy * (h + GAP));
        for y in 0..h {
            let src = &cell[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
            let at = (((oy + y) * sw + ox) * 4) as usize;
            out[at..at + src.len()].copy_from_slice(src);
        }
    }
    (out, sw, sh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn findings(doc: J) -> Findings {
        let mut f = Findings::default();
        validate(&doc, &mut f);
        f
    }

    fn spec() -> J {
        json!({ "compositions": [{ "name": "Main", "width": 320, "height": 180, "fps": 30, "duration": 2,
            "layers": [{ "name": "Box", "type": "shape",
                "transform": { "position": { "keyframes": [{ "time": 0, "value": [0, 90] }, { "time": 2, "value": [320, 90], "ease": "easeInOut" }] } },
                "shapes": [{ "type": "rect", "size": [40, 40] }, { "type": "fill", "color": "#ff3300" }] }] }] })
    }

    #[test]
    fn a_good_spec_has_no_findings() {
        let f = findings(spec());
        assert!(f.errors.is_empty() && f.warnings.is_empty(), "{:?}", f.errors.iter().chain(&f.warnings).map(|i| &i.message).collect::<Vec<_>>());
    }

    /// The whole point: a typo comes back with its path and the fix.
    #[test]
    fn typos_are_reported_with_path_and_suggestion() {
        let mut d = spec();
        d["compositions"][0]["layers"][0]["transform"]["postion"] = json!([1, 2]);
        d["compositions"][0]["layers"][0]["type"] = json!("shap");
        d["compositions"][0]["layers"][0]["parent"] = json!("Bx");
        let f = findings(d);
        let w = &f.warnings[0];
        assert_eq!(w.path, "compositions[0].layers[0].transform.postion");
        assert!(w.message.contains("did you mean 'position'"), "{}", w.message);
        assert!(f.errors.iter().any(|e| e.path.ends_with(".type") && e.message.contains("'shape'")));
        assert!(f.errors.iter().any(|e| e.path.ends_with(".parent") && e.message.contains("'Box'")));
    }

    #[test]
    fn keyframes_out_of_order_are_an_error() {
        let mut d = spec();
        d["compositions"][0]["layers"][0]["transform"]["position"]["keyframes"][1]["time"] = json!(-1);
        let f = findings(d);
        assert!(f.errors.iter().any(|e| e.path.ends_with("keyframes[1].time")));
    }

    #[test]
    fn auto_frames_span_the_comp() {
        assert_eq!(pick_frames(None, 59, 4).unwrap(), vec![0, 20, 39, 59]);
        assert_eq!(pick_frames(Some("0, 30,999"), 59, 4).unwrap(), vec![0, 30, 59]);
        assert!(pick_frames(Some("a"), 59, 4).is_err());
    }

    #[test]
    fn the_sheet_places_cells_on_a_grid() {
        let red = vec![255, 0, 0, 255].repeat(4); // 2x2
        let (px, w, h) = contact_sheet(&[red.clone(), red.clone(), red], 2, 2, 2);
        assert_eq!((w, h), (2 * 2 + 3 * 4, 2 * 2 + 3 * 4));
        let at = |x: u32, y: u32| &px[((y * w + x) * 4) as usize..][..4];
        assert_eq!(at(4, 4), [255, 0, 0, 255], "first cell");
        assert_eq!(at(10, 4), [255, 0, 0, 255], "second cell");
        assert_eq!(at(4, 10), [255, 0, 0, 255], "third cell wraps to row two");
        assert_eq!(at(10, 10), [40, 40, 44, 255], "fourth slot is empty");
        assert_eq!(at(0, 0), [40, 40, 44, 255], "gutter");
    }
}
