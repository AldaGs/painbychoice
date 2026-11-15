//! The Mixer panel: the project's buses and outputs (ADR 0026).
//!
//! Edits a copy and hands it back whole; the app swaps it in, so undo and the
//! live link see one ordinary document change.

use motion_core::{Bus, Output};

use crate::props::db_label;

/// Draw the mixer. `out` is set to the edited buses and outputs when anything changed.
pub(crate) fn mixer_ui(ui: &mut egui::Ui, buses: &[Bus], outputs: &[Output], out: &mut Option<(Vec<Bus>, Vec<Output>)>) {
    let (mut b, mut o) = (buses.to_vec(), outputs.to_vec());
    let mut changed = false;
    let ids: Vec<(String, String)> = b.iter().map(|x| (x.id.clone(), x.name.clone())).collect();
    let name_of = |id: &str| ids.iter().find(|(i, _)| i == id).map_or(id.to_string(), |(_, n)| n.clone());

    ui.heading("Buses");
    if b.is_empty() {
        ui.weak("No buses: every sound goes straight to the mix.");
    }
    let mut remove = None;
    egui::Grid::new("mixer_buses").num_columns(6).striped(true).show(ui, |ui| {
        for (i, bus) in b.iter_mut().enumerate() {
            changed |= ui.add(egui::TextEdit::singleline(&mut bus.name).desired_width(90.0)).changed();
            ui.horizontal(|ui| {
                changed |= ui.add(egui::Slider::new(&mut bus.gain, 0.0..=2.0).show_value(false)).changed();
                ui.weak(db_label(bus.gain));
            });
            changed |= ui.toggle_value(&mut bus.mute, "M").on_hover_text("Mute").changed();
            changed |= ui.toggle_value(&mut bus.solo, "S").on_hover_text("Solo").changed();
            ui.horizontal(|ui| {
                let label = bus.duck_by.as_deref().map_or("No duck".to_string(), |k| format!("Duck under {}", name_of(k)));
                egui::ComboBox::from_id_salt(("duck", i)).selected_text(label).show_ui(ui, |ui| {
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
                    changed |= ui.add(egui::Slider::new(&mut bus.duck_level, 0.0..=1.0).show_value(false)).on_hover_text("Level while ducked").changed();
                    ui.weak(db_label(bus.duck_level));
                }
            });
            if ui.small_button("✕").on_hover_text(format!("Remove (id \"{}\")", bus.id)).clicked() {
                remove = Some(i);
            }
            ui.end_row();
        }
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
    if ui.button("+ Bus").clicked() {
        b.push(Bus::new(fresh(b.iter().map(|x| x.id.as_str()), "bus"), format!("Bus {}", b.len() + 1)));
        changed = true;
    }

    ui.separator();
    ui.heading("Outputs");
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

    if changed {
        *out = Some((b, o));
    }
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
}
