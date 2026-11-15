//! Editor preferences: the Settings dialog and the values it edits.
//!
//! User data, not document data, so it lives in `~/.pbc/settings.json` rather
//! than the `.pbc`. Read anywhere through [`get`]; the dialog writes back and
//! saves on every change.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, RwLock};

use serde::{Deserialize, Serialize};

/// A named colour label, as layers and asset folders pick from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Label {
    pub(crate) name: String,
    pub(crate) rgb: [u8; 3],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    /// Seconds between autosaves; 0 turns autosave off.
    pub(crate) autosave_secs: u64,
    /// egui zoom factor for the whole editor.
    pub(crate) ui_scale: f32,
    /// The chrome base every panel shade derives from.
    pub(crate) ui_base: [u8; 3],
    /// Selection and highlight colour.
    pub(crate) accent: [u8; 3],
    /// Layer / folder colour labels. A label is stored by index.
    pub(crate) labels: Vec<Label>,
}

impl Default for Settings {
    fn default() -> Self {
        let l = |name: &str, rgb| Label { name: name.into(), rgb };
        Settings {
            autosave_secs: 60,
            ui_scale: 1.0,
            ui_base: [0x2d, 0x2d, 0x2d],
            accent: [0x45, 0x80, 0xc8],
            labels: vec![
                l("Red", [0xc8, 0x46, 0x46]),
                l("Orange", [0xd2, 0x7a, 0x32]),
                l("Yellow", [0xd8, 0xc0, 0x4a]),
                l("Green", [0x5c, 0xb0, 0x5a]),
                l("Aqua", [0x48, 0xb4, 0xb4]),
                l("Blue", [0x4e, 0x82, 0xd6]),
                l("Violet", [0x9a, 0x6a, 0xd6]),
                l("Pink", [0xd6, 0x6a, 0xb0]),
            ],
        }
    }
}

static SETTINGS: LazyLock<RwLock<Settings>> = LazyLock::new(|| RwLock::new(load()));
static OPEN: AtomicBool = AtomicBool::new(false);

/// A copy of the current settings.
pub(crate) fn get() -> Settings {
    SETTINGS.read().unwrap().clone()
}

/// Label `i`'s colour, if the palette has it.
pub(crate) fn label_color(i: Option<u8>) -> Option<egui::Color32> {
    let i = i? as usize;
    SETTINGS.read().unwrap().labels.get(i).map(|l| rgb(l.rgb))
}

pub(crate) fn rgb(c: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(c[0], c[1], c[2])
}

/// Ask for the dialog to show on the next frame.
pub(crate) fn open() {
    OPEN.store(true, Ordering::Relaxed);
}

fn path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(std::path::PathBuf::from(home).join(".pbc").join("settings.json"))
}

fn load() -> Settings {
    // A missing or unreadable file is just the defaults; serde(default) fills
    // anything an older file lacks.
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(s: &Settings) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(p, text);
    }
}

/// A "set label" submenu: one swatch per palette entry, plus None.
/// Returns the pick, `Some(None)` meaning cleared.
pub(crate) fn label_menu(ui: &mut egui::Ui, current: Option<u8>) -> Option<Option<u8>> {
    let mut pick = None;
    ui.menu_button("Label", |ui| {
        if ui.selectable_label(current.is_none(), "None").clicked() {
            pick = Some(None);
            ui.close();
        }
        for (i, l) in get().labels.iter().enumerate() {
            let text = egui::RichText::new(format!("■  {}", l.name)).color(rgb(l.rgb));
            if ui.selectable_label(current == Some(i as u8), text).clicked() {
                pick = Some(Some(i as u8));
                ui.close();
            }
        }
    });
    pick
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Page {
    #[default]
    General,
    Interface,
}

/// Draw the dialog if it's open. Called once per frame from the main window.
pub(crate) fn window(ctx: &egui::Context) {
    let mut open = OPEN.load(Ordering::Relaxed);
    if !open {
        return;
    }
    let page_id = egui::Id::new("settings_page");
    let mut page: Page = ctx.data(|d| d.get_temp(page_id)).unwrap_or_default();
    let mut s = get();
    let before = s.clone();
    egui::Window::new("Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([520.0, 420.0])
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(110.0);
                    for (p, name) in [(Page::General, "General"), (Page::Interface, "Interface")] {
                        ui.selectable_value(&mut page, p, name);
                    }
                });
                ui.separator();
                ui.vertical(|ui| match page {
                    Page::General => general(ui, &mut s),
                    Page::Interface => interface(ui, &mut s),
                });
            });
            ui.separator();
            if ui.button("Restore defaults").clicked() {
                s = Settings::default();
            }
        });
    OPEN.store(open, Ordering::Relaxed);
    ctx.data_mut(|d| d.insert_temp(page_id, page));
    if s != before {
        let retheme = s.ui_base != before.ui_base || s.accent != before.accent || s.ui_scale != before.ui_scale;
        *SETTINGS.write().unwrap() = s.clone();
        save(&s);
        if retheme {
            crate::theme::install(ctx);
        }
    }
}

fn general(ui: &mut egui::Ui, s: &mut Settings) {
    ui.heading("General");
    egui::Grid::new("settings_general").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
        ui.label("Autosave every");
        ui.add(egui::DragValue::new(&mut s.autosave_secs).range(0..=3600).suffix(" s"))
            .on_hover_text("0 turns autosave off");
        ui.end_row();
    });
}

fn color(ui: &mut egui::Ui, c: &mut [u8; 3]) -> bool {
    ui.color_edit_button_srgb(c).changed()
}

fn interface(ui: &mut egui::Ui, s: &mut Settings) {
    ui.heading("Interface");
    egui::Grid::new("settings_ui").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
        ui.label("UI scale");
        ui.add(egui::Slider::new(&mut s.ui_scale, 0.75..=2.0).step_by(0.05));
        ui.end_row();
        ui.label("Panel colour");
        color(ui, &mut s.ui_base);
        ui.end_row();
        ui.label("Accent colour");
        color(ui, &mut s.accent);
        ui.end_row();
    });
    ui.add_space(8.0);
    ui.strong("Colour labels");
    ui.weak("Layers and asset folders pick these from their right-click menu.");
    let mut remove = None;
    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
        for (i, l) in s.labels.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                color(ui, &mut l.rgb);
                ui.add(egui::TextEdit::singleline(&mut l.name).desired_width(140.0));
                if ui.small_button("✕").on_hover_text("Remove label").clicked() {
                    remove = Some(i);
                }
            });
        }
    });
    if let Some(i) = remove {
        s.labels.remove(i);
    }
    if s.labels.len() < 16 && ui.button("+ Label").clicked() {
        s.labels.push(Label { name: format!("Label {}", s.labels.len() + 1), rgb: [0x80, 0x80, 0x80] });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn older_settings_files_fill_in_defaults() {
        let s: super::Settings = serde_json::from_str(r#"{"autosave_secs": 5}"#).unwrap();
        assert_eq!(s.autosave_secs, 5);
        assert_eq!(s.labels.len(), 8);
    }
}
