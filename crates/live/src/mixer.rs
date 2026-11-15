//! The Mixer panel: the project's buses and outputs (ADR 0026).
//!
//! Edits a copy and hands it back whole; the app swaps it in, so undo and the
//! live link see one ordinary document change.

use motion_core::{Bus, Output};

use crate::props::db_label;

/// Draw the mixer. `out` is set to the edited buses and outputs when anything changed.
/// `frame` is where a fader move writes when the gain is keyframed.
/// `meters`: the mix's level, then each bus's (linear peaks).
pub(crate) fn mixer_ui(ui: &mut egui::Ui, buses: &[Bus], outputs: &[Output], frame: i64, meters: &[f32], out: &mut Option<(Vec<Bus>, Vec<Output>)>) {
    let (mut b, mut o) = (buses.to_vec(), outputs.to_vec());
    let mut changed = false;
    let ids: Vec<(String, String)> = b.iter().map(|x| (x.id.clone(), x.name.clone())).collect();
    let name_of = |id: &str| ids.iter().find(|(i, _)| i == id).map_or(id.to_string(), |(_, n)| n.clone());

    let mut remove = None;
    let mut add = false;
    // Channel strips side by side, the Mix strip last, scrolling sideways
    // when there are more than fit (a console layout).
    egui::ScrollArea::horizontal().id_salt("mixer_strips").show(ui, |ui| {
        ui.horizontal_top(|ui| {
            for (i, bus) in b.iter_mut().enumerate() {
                strip_frame(ui, |ui| {
                    changed |= ui.add(egui::TextEdit::singleline(&mut bus.name).desired_width(STRIP_W - 8.0)).changed();
                    let label = bus.duck_by.as_deref().map_or("No duck".to_string(), |k| format!("Duck: {}", name_of(k)));
                    egui::ComboBox::from_id_salt(("duck", i)).width(STRIP_W - 16.0).selected_text(label).show_ui(ui, |ui| {
                        let me = bus.id.clone();
                        for choice in std::iter::once(None).chain(ids.iter().filter(|(k, _)| *k != me).map(|(k, _)| Some(k.clone()))) {
                            let text = choice.as_deref().map_or("No duck".to_string(), |k| name_of(k));
                            if ui.selectable_label(bus.duck_by == choice, text).clicked() && bus.duck_by != choice {
                                bus.duck_by = choice;
                                changed = true;
                            }
                        }
                    });
                    if bus.duck_by.is_some() {
                        changed |= ui
                            .add(egui::Slider::new(&mut bus.duck_level, 0.0..=1.0).show_value(false))
                            .on_hover_text(format!("Level while ducked: {}", db_label(bus.duck_level)))
                            .changed();
                    }
                    ui.horizontal(|ui| {
                        changed |= ui.toggle_value(&mut bus.mute, "M").on_hover_text("Mute").changed();
                        changed |= ui.toggle_value(&mut bus.solo, "S").on_hover_text("Solo").changed();
                        if crate::props::key_button(ui, bus.gain.is_animated()) {
                            bus.gain.insert_key(frame);
                            changed = true;
                        }
                    });
                    let mut g = bus.gain.resolve(&mut motion_core::expr::EvalCtx::at(frame as f64));
                    ui.horizontal(|ui| {
                        if fader(ui, &mut g) {
                            // A keyframed gain gets a key here; a constant just moves.
                            bus.gain.set_at(frame, g);
                            changed = true;
                        }
                        meter(ui, meters.get(1 + i).copied().unwrap_or(0.0));
                    });
                    gain_readout(ui, g);
                    ui.horizontal(|ui| {
                        ui.weak(&bus.id);
                        if ui.small_button("✕").on_hover_text("Remove bus").clicked() {
                            remove = Some(i);
                        }
                    });
                });
            }
            if ui.add_sized([28.0, FADER_H], egui::Button::new("+")).on_hover_text("Add bus").clicked() {
                add = true;
            }
            strip_frame(ui, |ui| {
                ui.vertical_centered(|ui| ui.strong("Mix"));
                if b.is_empty() {
                    ui.weak("No buses: every sound goes straight here.");
                }
                ui.horizontal(|ui| {
                    ui.add_space((STRIP_W - 10.0) / 2.0);
                    meter(ui, meters.first().copied().unwrap_or(0.0));
                });
            });
        });
    });
    if let Some(i) = remove {
        let id = b.remove(i).id;
        for x in &mut b {
            if x.duck_by.as_deref() == Some(&id) {
                x.duck_by = None;
            }
        }
        for x in &mut o {
            x.buses.retain(|k| *k != id);
        }
        changed = true;
    }
    if add {
        b.push(Bus::new(fresh(b.iter().map(|x| x.id.as_str()), "bus"), format!("Bus {}", b.len() + 1)));
        changed = true;
    }

    ui.separator();
    ui.collapsing("Outputs", |ui| {
        ui.weak("What `motion render --output <id>` exports. No buses ticked = the full mix.");
        let mut remove = None;
        for (i, output) in o.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                ui.monospace(&output.id);
                for (id, name) in &ids {
                    let mut on = output.buses.contains(id);
                    if ui.checkbox(&mut on, name).changed() {
                        if on {
                            output.buses.push(id.clone());
                        } else {
                            output.buses.retain(|k| k != id);
                        }
                        changed = true;
                    }
                }
                if ui.small_button("✕").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            o.remove(i);
            changed = true;
        }
        if ui.button("+ Output").clicked() {
            o.push(Output { id: fresh(o.iter().map(|x| x.id.as_str()), "out"), buses: Vec::new() });
            changed = true;
        }
    });

    if changed {
        *out = Some((b, o));
    }
}

const STRIP_W: f32 = 104.0;
const FADER_H: f32 = 220.0;
/// The fader and meter scale: -60 dB at the bottom, +6 at the top.
const DB_MIN: f32 = -60.0;
const DB_MAX: f32 = 6.0;
const TICKS: [i32; 8] = [6, 0, -6, -12, -18, -24, -36, -48];

fn strip_frame(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style()).inner_margin(4.0).show(ui, |ui| {
        ui.set_width(STRIP_W);
        ui.vertical(body);
    });
}

/// dB -> 0 (bottom) .. 1 (top) on the shared scale.
fn db_frac(db: f32) -> f32 {
    ((db - DB_MIN) / (DB_MAX - DB_MIN)).clamp(0.0, 1.0)
}

/// Fader position (0 bottom .. 1 top) -> linear gain. The bottom of travel is
/// silence, and within 0.4 dB of unity it snaps to exactly 0 dB.
fn gain_at(f: f32) -> f64 {
    let db = DB_MIN + f * (DB_MAX - DB_MIN);
    if f <= 0.0 {
        0.0
    } else if db.abs() < 0.4 {
        1.0
    } else {
        10f64.powf(db as f64 / 20.0)
    }
}

/// A vertical gain fader with a dB scale. Drag to move, double-click for 0 dB.
/// Returns whether the gain changed.
fn fader(ui: &mut egui::Ui, gain: &mut f64) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(46.0, FADER_H), egui::Sense::click_and_drag());
    let track = egui::Rect::from_center_size(egui::pos2(rect.right() - 9.0, rect.center().y), egui::vec2(4.0, FADER_H - 16.0));
    let y_of = |db: f32| egui::lerp(track.bottom()..=track.top(), db_frac(db));
    let p = ui.painter();
    let weak = ui.visuals().weak_text_color();
    for t in TICKS {
        let y = y_of(t as f32);
        p.hline(track.left() - 6.0..=track.left() - 2.0, y, egui::Stroke::new(1.0, weak));
        p.text(egui::pos2(track.left() - 8.0, y), egui::Align2::RIGHT_CENTER, t.to_string(), egui::FontId::monospace(9.0), weak);
    }
    p.rect_filled(track, 2.0, ui.visuals().extreme_bg_color);
    let mut changed = false;
    if resp.double_clicked() {
        changed = *gain != 1.0;
        *gain = 1.0;
    } else if let (true, Some(pos)) = (resp.dragged() || resp.clicked(), resp.interact_pointer_pos()) {
        let new = gain_at(((track.bottom() - pos.y) / track.height()).clamp(0.0, 1.0));
        changed = new != *gain;
        *gain = new;
    }
    let y = if *gain <= 0.0 { track.bottom() } else { y_of((20.0 * gain.log10()) as f32) };
    let cap = egui::Rect::from_center_size(egui::pos2(track.center().x, y), egui::vec2(16.0, 24.0));
    let v = ui.visuals();
    let fill = if resp.hovered() || resp.dragged() { v.widgets.hovered.bg_fill } else { v.widgets.inactive.bg_fill };
    let st = egui::Stroke::new(1.5, v.text_color());
    p.rect_filled(cap, 3.0, fill);
    p.rect_stroke(cap, 3.0, st, egui::StrokeKind::Inside);
    p.hline(cap.x_range().shrink(3.0), y, st);
    resp.on_hover_text(format!("{} · double-click for 0 dB", db_label(*gain)));
    changed
}

/// The strip's gain in dB, centred under the fader, accented when off unity.
fn gain_readout(ui: &mut egui::Ui, g: f64) {
    let text = egui::RichText::new(db_label(g)).monospace();
    let text = if (g - 1.0).abs() > 1e-6 { text.color(ui.visuals().selection.stroke.color) } else { text };
    ui.vertical_centered(|ui| ui.label(text));
}

/// A vertical level meter on the fader's scale: green, amber past -12, red past -3.
fn meter(ui: &mut egui::Ui, level: f32) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(10.0, FADER_H), egui::Sense::hover());
    let well = rect.shrink2(egui::vec2(0.0, 8.0));
    let db = 20.0 * level.max(1e-6).log10();
    let p = ui.painter();
    p.rect_filled(well, 1.0, egui::Color32::from_gray(20));
    if level > 1e-6 {
        // Segments by zone, so the colours stack like a hardware meter.
        for (lo, hi, color) in [
            (DB_MIN, -12.0, egui::Color32::from_rgb(90, 200, 110)),
            (-12.0, -3.0, egui::Color32::from_rgb(240, 190, 60)),
            (-3.0, DB_MAX, egui::Color32::from_rgb(235, 80, 70)),
        ] {
            let top = db.min(hi);
            if top <= lo {
                continue;
            }
            let y0 = egui::lerp(well.bottom()..=well.top(), db_frac(lo));
            let y1 = egui::lerp(well.bottom()..=well.top(), db_frac(top));
            p.rect_filled(egui::Rect::from_x_y_ranges(well.x_range(), y1..=y0), 0.0, color);
        }
    }
    resp.on_hover_text(if level > 1e-6 { format!("{db:.1} dBFS") } else { "silent".into() });
}

/// `prefix1`, `prefix2`, … — the first not already taken.
fn fresh<'a>(taken: impl Iterator<Item = &'a str> + Clone, prefix: &str) -> String {
    (1..).map(|n| format!("{prefix}{n}")).find(|c| !taken.clone().any(|t| t == c)).unwrap()
}

#[cfg(test)]
mod tests {
    #[test]
    fn fresh_ids_skip_taken_ones() {
        assert_eq!(super::fresh(["bus1", "bus3"].into_iter(), "bus"), "bus2");
    }

    #[test]
    fn fader_ends_are_silence_and_plus_six() {
        assert_eq!(super::gain_at(0.0), 0.0);
        assert!((super::gain_at(1.0) - 10f64.powf(6.0 / 20.0)).abs() < 1e-6);
        // Near unity snaps to exactly 0 dB.
        assert_eq!(super::gain_at(super::db_frac(0.2)), 1.0);
    }
}
