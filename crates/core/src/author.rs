//! The authoring JSON (`motion.schema.json` from aftereffects-aep-parser, what
//! `json2jsx` turns into an After Effects project) read without After Effects:
//! rewritten into the shape an exported `.aep` has, so [`crate::ae::import`]
//! builds the project and both routes share one importer.
//!
//! The rewrite follows `json2jsx`'s reading of the format: keyframe times are
//! comp seconds, an ease preset is speed 0 at influence 75, a bare key is
//! linear, colours may be hex, parents and mattes name layers.

use std::collections::HashMap;

use serde_json::{json, Value as J};

use crate::ae::ImportReport;

/// The export-shaped project for an authoring document.
pub(crate) fn to_export(doc: &J, report: &mut ImportReport) -> J {
    let comps = doc["compositions"].as_array().cloned().unwrap_or_default();
    // Comp ids start at 1; 0 is the root folder.
    let ids: HashMap<&str, u64> = comps.iter().enumerate().map(|(i, c)| (c["name"].as_str().unwrap_or(""), i as u64 + 1)).collect();
    let children: Vec<J> = comps.iter().enumerate().map(|(i, c)| comp(c, i as u64 + 1, &ids, report)).collect();
    if doc["assets"].as_array().is_some_and(|a| !a.is_empty()) {
        report.drop("authoring assets (footage)");
    }
    json!({ "file": doc["file"], "root": { "id": 0, "name": "root", "type": "Folder", "children": children } })
}

fn comp(c: &J, id: u64, ids: &HashMap<&str, u64>, report: &mut ImportReport) -> J {
    let layers = c["layers"].as_array().cloned().unwrap_or_default();
    let duration = c["duration"].as_f64().unwrap_or(0.0);
    // AE layers are numbered from the top; parents and mattes name them.
    let by_name: HashMap<&str, u64> = layers.iter().enumerate().map(|(i, l)| (l["name"].as_str().unwrap_or(""), i as u64 + 1)).collect();
    let out: Vec<J> = layers.iter().enumerate().map(|(i, l)| layer(l, i as u64 + 1, duration, ids, &by_name, report)).collect();
    json!({
        "id": id,
        "name": c["name"],
        "type": "Composition",
        "width": c["width"],
        "height": c["height"],
        "framerate": c["fps"].as_f64().unwrap_or(24.0),
        "seconds": duration,
        "backgroundColor": c["backgroundColor"].as_str().unwrap_or("#000000"),
        "layers": out,
    })
}

fn layer(l: &J, id: u64, comp_duration: f64, ids: &HashMap<&str, u64>, by_name: &HashMap<&str, u64>, report: &mut ImportReport) -> J {
    let kind = l["type"].as_str().unwrap_or("null");
    let start = l["startTime"].as_f64().unwrap_or(0.0);
    let in_ = l["in"].as_f64().unwrap_or(start);
    let out = l["out"].as_f64().filter(|o| *o > in_).unwrap_or(comp_duration);
    if l.get("stretch").is_some() {
        report.drop("layer time stretch");
    }
    if l["masks"].as_array().is_some_and(|m| !m.is_empty()) {
        report.drop("authoring masks");
    }
    if l["jsx"].as_str().is_some_and(|s| !s.is_empty()) {
        report.drop("layer jsx escape hatch");
    }
    if matches!(kind, "footage" | "audio" | "solid") {
        report.drop(format!("{kind} layer"));
    }
    // Keyframe times are comp seconds here and layer seconds in an export.
    let t = Times { shift: start };
    let mut o = json!({
        "name": l["name"],
        "layerId": id,
        "kind": if matches!(kind, "null" | "camera" | "light") { kind } else { "av" },
        "startTime": start,
        "inPoint": in_,
        // Measured from startTime, as the export does.
        "duration": out - start,
        "flags": {
            "adjustment": kind == "adjustment" || l["adjustment"] == true,
            "video": l["enabled"] != false,
            "3D": l["threeD"] == true,
        },
    });
    if let Some(&s) = l["source"].as_str().and_then(|s| ids.get(s)).filter(|_| kind == "precomp") {
        o["sourceId"] = json!(s);
    }
    if let Some(&p) = l["parent"].as_str().and_then(|p| by_name.get(p)) {
        o["parent"] = json!(p);
    }
    if let Some(b) = l["blendMode"].as_str() {
        // "Color Dodge" → "colorDodge", the export's spelling.
        let mut words = b.split(' ');
        let first = words.next().unwrap_or("").to_lowercase();
        o["blendMode"] = json!(words.fold(first, |s, w| s + w));
    }
    if let Some(tm) = l["trackMatte"].as_object() {
        let mode = match tm.get("type").and_then(J::as_str).unwrap_or("Alpha") {
            "Alpha" => "alpha",
            "Alpha Inverted" => "alphaInverted",
            "Luma" => "luma",
            _ => "lumaInverted",
        };
        o["trackMatte"] = json!(mode);
        if let Some(&m) = tm.get("target").and_then(J::as_str).and_then(|n| by_name.get(n)) {
            o["matteLayer"] = json!(m);
        }
    }
    let tf = &l["transform"];
    let mut transform = Vec::new();
    for (key, mn) in [
        ("anchorPoint", "ADBE Anchor Point"),
        ("position", "ADBE Position"),
        ("xPosition", "ADBE Position_0"),
        ("yPosition", "ADBE Position_1"),
        ("scale", "ADBE Scale"),
        ("rotation", "ADBE Rotate Z"),
        ("zRotation", "ADBE Rotate Z"),
        ("xRotation", "ADBE Rotate X"),
        ("yRotation", "ADBE Rotate Y"),
        ("orientation", "ADBE Orientation"),
        ("opacity", "ADBE Opacity"),
    ] {
        if !tf[key].is_null() {
            transform.push(t.prop(mn, &tf[key], key == "position", report));
        }
    }
    o["transform"] = json!(transform);
    if let Some(text) = l["text"].as_object() {
        let mut style = json!({
            "font": text.get("font").and_then(J::as_str).unwrap_or("ArialMT"),
            "fontSize": text.get("fontSize").and_then(J::as_f64).unwrap_or(48.0),
            "fillColor": color(text.get("fillColor").unwrap_or(&J::Null)).unwrap_or(json!([1.0, 1.0, 1.0])),
            "tracking": text.get("tracking").and_then(J::as_f64).unwrap_or(0.0),
            "justification": text.get("justification").and_then(J::as_str).unwrap_or("left"),
            "applyFill": text.get("applyFill").and_then(J::as_bool).unwrap_or(true),
            "applyStroke": text.get("applyStroke").and_then(J::as_bool).unwrap_or(false),
        });
        if let Some(lead) = text.get("leading").and_then(J::as_f64).filter(|v| *v > 0.0) {
            style["leading"] = json!(lead);
        } else {
            style["autoLeading"] = json!(true);
        }
        if let Some(c) = text.get("strokeColor").and_then(color) {
            style["strokeColor"] = c;
            style["strokeWidth"] = text.get("strokeWidth").cloned().unwrap_or(json!(0.0));
        }
        if let Some(b) = text.get("boxSize") {
            style["boxSize"] = b.clone();
            style["boxPos"] = json!([-b[0].as_f64().unwrap_or(0.0) / 2.0, -b[1].as_f64().unwrap_or(0.0) / 2.0]);
        }
        o["text"] = text.get("string").cloned().unwrap_or(json!(""));
        o["textStyle"] = style;
    }
    if let Some(shapes) = l["shapes"].as_array() {
        o["contents"] = json!(shapes.iter().filter_map(|s| t.shape(s, report)).collect::<Vec<_>>());
    }
    if let Some(fx) = l["effects"].as_array() {
        let effects: Vec<J> = fx
            .iter()
            .map(|e| {
                let mn = e["matchName"].as_str().unwrap_or("");
                let props: Vec<J> = e["props"].as_object().into_iter().flatten().map(|(k, v)| t.prop(k, v, false, report)).collect();
                json!({ "matchName": mn, "name": e["name"].as_str().unwrap_or(mn), "properties": props })
            })
            .collect();
        o["effects"] = json!(effects);
    }
    o
}

/// How to place keyframe times on the layer's clock.
struct Times {
    shift: f64,
}

impl Times {
    /// One shape item as the export writes it.
    fn shape(&self, s: &J, report: &mut ImportReport) -> Option<J> {
        let p = |mn: &str, key: &str, spatial: bool, report: &mut ImportReport| (!s[key].is_null()).then(|| self.prop(mn, &s[key], spatial, report));
        let item = |mn: &str, props: Vec<Option<J>>| {
            json!({ "matchName": mn, "label": s["name"].as_str().unwrap_or(""), "properties": props.into_iter().flatten().collect::<Vec<_>>() })
        };
        Some(match s["type"].as_str()? {
            "group" => {
                let tf = &s["transform"];
                let tprops: Vec<J> = [
                    ("anchorPoint", "ADBE Vector Anchor"),
                    ("position", "ADBE Vector Position"),
                    ("scale", "ADBE Vector Scale"),
                    ("rotation", "ADBE Vector Rotation"),
                    ("opacity", "ADBE Vector Group Opacity"),
                ]
                .into_iter()
                .filter(|(k, _)| !tf[*k].is_null())
                .map(|(k, mn)| self.prop(mn, &tf[k], k == "position", report))
                .collect();
                let items: Vec<J> = s["items"].as_array().into_iter().flatten().filter_map(|i| self.shape(i, report)).collect();
                item(
                    "ADBE Vector Group",
                    vec![
                        Some(json!({ "matchName": "ADBE Vectors Group", "properties": items })),
                        Some(json!({ "matchName": "ADBE Vector Transform Group", "properties": tprops })),
                    ],
                )
            }
            "rect" => item(
                "ADBE Vector Shape - Rect",
                vec![
                    p("ADBE Vector Rect Size", "size", false, report),
                    p("ADBE Vector Rect Position", "position", true, report),
                    p("ADBE Vector Rect Roundness", "roundness", false, report),
                ],
            ),
            "ellipse" => item(
                "ADBE Vector Shape - Ellipse",
                vec![p("ADBE Vector Ellipse Size", "size", false, report), p("ADBE Vector Ellipse Position", "position", true, report)],
            ),
            "path" => {
                let path = s["path"].as_object()?;
                let n = path.get("vertices").and_then(J::as_array).map_or(0, Vec::len);
                let tangents = |k: &str| path.get(k).filter(|t| t.as_array().is_some_and(|a| a.len() == n)).cloned().unwrap_or(json!(vec![[0.0, 0.0]; n]));
                let shape = json!({
                    "vertices": path.get("vertices"),
                    "inTangents": tangents("inTangents"),
                    "outTangents": tangents("outTangents"),
                    "closed": path.get("closed").and_then(J::as_bool).unwrap_or(true),
                });
                item("ADBE Vector Shape - Group", vec![Some(json!({ "matchName": "ADBE Vector Shape", "path": shape }))])
            }
            "fill" => item(
                "ADBE Vector Graphic - Fill",
                vec![p("ADBE Vector Fill Color", "color", false, report), p("ADBE Vector Fill Opacity", "opacity", false, report)],
            ),
            "stroke" => {
                let code = |k: &str, names: [&str; 3]| s[k].as_str().and_then(|v| names.iter().position(|n| *n == v)).map(|i| json!({ "value": [i + 1] }));
                let mut props = vec![
                    p("ADBE Vector Stroke Color", "color", false, report),
                    p("ADBE Vector Stroke Width", "strokeWidth", false, report),
                    p("ADBE Vector Stroke Opacity", "opacity", false, report),
                ];
                for (mn, v) in [
                    ("ADBE Vector Stroke Line Cap", code("lineCap", ["butt", "round", "projecting"])),
                    ("ADBE Vector Stroke Line Join", code("lineJoin", ["miter", "round", "bevel"])),
                ] {
                    props.push(v.map(|mut v| {
                        v["matchName"] = json!(mn);
                        v
                    }));
                }
                if let Some(d) = s["dashes"].as_object() {
                    let dp: Vec<J> = [("dash", "ADBE Vector Stroke Dash 1"), ("gap", "ADBE Vector Stroke Gap 1"), ("offset", "ADBE Vector Stroke Offset")]
                        .into_iter()
                        .filter_map(|(k, mn)| d.get(k).map(|v| self.prop(mn, v, false, report)))
                        .collect();
                    props.push(Some(json!({ "matchName": "ADBE Vector Stroke Dashes", "properties": dp })));
                }
                item("ADBE Vector Graphic - Stroke", props)
            }
            "trim" => item(
                "ADBE Vector Filter - Trim",
                vec![
                    p("ADBE Vector Trim Start", "start", false, report),
                    p("ADBE Vector Trim End", "end", false, report),
                    p("ADBE Vector Trim Offset", "offset", false, report),
                ],
            ),
            other => {
                report.drop(format!("authoring shape item: {other}"));
                return None;
            }
        })
    }

    /// An AnimatableValue as an export property: `value`, `keyframes` with
    /// AE's interpolation fields, and `expression`.
    fn prop(&self, mn: &str, v: &J, spatial: bool, _report: &mut ImportReport) -> J {
        let mut o = json!({ "matchName": mn });
        if spatial {
            o["valueType"] = json!("Spatial");
        }
        let obj = match v {
            J::Object(m) => m.clone(),
            v => {
                o["value"] = numbers(v);
                return o;
            }
        };
        if let Some(v) = obj.get("value") {
            o["value"] = numbers(v);
        }
        if let Some(e) = obj.get("expression").and_then(J::as_str) {
            o["expression"] = json!(e);
        }
        if let Some(keys) = obj.get("keyframes").and_then(J::as_array).filter(|k| !k.is_empty()) {
            let out: Vec<J> = keys
                .iter()
                .map(|k| {
                    let mut kf = json!({ "time": k["time"].as_f64().unwrap_or(0.0) - self.shift, "value": numbers(&k["value"]) });
                    for t in ["inTangent", "outTangent"] {
                        if let Some(v) = k.get(t) {
                            kf[t] = v.clone();
                        }
                    }
                    let (interp, ease) = match (k["interp"].as_str(), &k["ease"]) {
                        (Some("hold"), _) => ("Hold", None),
                        (Some("linear"), _) | (_, J::Null) => ("Linear", None),
                        (_, J::String(p)) if p == "linear" => ("Linear", None),
                        (_, J::String(_)) => ("Bezier", Some((0.0, 75.0, 0.0, 75.0))),
                        (_, e) => {
                            let f = |key: &str, or: f64| e[key].as_f64().unwrap_or(or);
                            ("Bezier", Some((f("inSpeed", 0.0), f("inInfluence", 33.33), f("outSpeed", 0.0), f("outInfluence", 33.33))))
                        }
                    };
                    kf["inInterpolation"] = json!(interp);
                    kf["outInterpolation"] = json!(interp);
                    if let Some((is, ii, os, oi)) = ease {
                        // Influence is a percentage here and a fraction in an export.
                        kf["inSpeed"] = json!([is]);
                        kf["inInfluence"] = json!([ii.clamp(0.1, 100.0) / 100.0]);
                        kf["outSpeed"] = json!([os]);
                        kf["outInfluence"] = json!([oi.clamp(0.1, 100.0) / 100.0]);
                    }
                    kf
                })
                .collect();
            o["keyframes"] = json!(out);
        }
        o
    }
}

/// A value as the export's number array: hex colours as RGBA 0..1, booleans
/// as 0/1, a bare number as one element.
fn numbers(v: &J) -> J {
    match v {
        J::Number(n) => json!([n]),
        J::Bool(b) => json!([*b as u8]),
        J::String(_) => color(v).map(|mut c| {
            c.as_array_mut().unwrap().push(json!(1.0));
            c
        }).unwrap_or(J::Null),
        v => v.clone(),
    }
}

/// `#rrggbb` or an `[r, g, b]` array as `[r, g, b]` in 0..1.
fn color(v: &J) -> Option<J> {
    match v {
        J::String(s) => {
            let h = s.trim_start_matches('#');
            let c = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok().map(|x| x as f64 / 255.0);
            Some(json!([c(0)?, c(2)?, c(4)?]))
        }
        J::Array(a) if a.len() >= 3 => Some(json!(a[..3])),
        _ => None,
    }
}
