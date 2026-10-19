//! File › Scripts › Run Script…: run a script written for the reference app against the open
//! documents. The script runs on a thread of its own ([`vectorcraft_extendscript::remote`]); each
//! frame the app answers the commands it has made and draws the dialog it is waiting in, so the
//! artwork changes on screen as the script works and the interface stays alive.
//!
//! `ui.runScript {path? | data?, name?}`: the script at `path`, or the text in `data`; without
//! either, pick a `.jsx` file. Only one script runs at a time. The web build has no threads to
//! run it on.

use serde_json::{Value, json};

use crate::VectorcraftApp;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::{collections::HashMap, time::Duration};

    use egui::{Id, Key, RichText, Ui};
    use serde_json::Value;
    use vectorcraft_extendscript::{Dialog, Event, Widget, remote};

    use super::VectorcraftApp;
    use crate::{dialogs::modal, picks};

    /// A script in progress, and what the user did to its dialog since the script last asked.
    pub struct Running {
        job: remote::Job,
        name: String,
        queue: std::collections::VecDeque<Event>,
        edits: HashMap<Vec<usize>, String>,
    }

    pub(super) fn start(app: &mut VectorcraftApp, source: String, name: String, file: Option<std::path::PathBuf>) -> Result<(), String> {
        if app.script_run.is_some() {
            return Err("A script is already running".into());
        }
        let source = source.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(source);
        let job = remote::spawn(source, file, || {}).map_err(|e| e.to_string())?;
        app.script_run = Some(Running { job, name: name.clone(), queue: Default::default(), edits: HashMap::new() });
        app.status(format!("Running {name}…"));
        Ok(())
    }

    /// One frame of a running script: answer it, draw what it shows, notice when it ends.
    pub(crate) fn frame(app: &mut VectorcraftApp, ctx: &egui::Context) {
        let Some(mut run) = app.script_run.take() else { return };
        run.job.pump(&mut app.session, Duration::from_millis(8));
        app.sync_views();
        // What the user did before the script asked again goes in as soon as it does.
        if run.job.dialog().is_some()
            && let Some(event) = run.queue.pop_front()
        {
            run.job.answer(event);
        }
        if let Some(text) = run.job.alert().map(str::to_string) {
            if alert(ctx, &run.name, &text) {
                run.job.dismiss_alert();
            }
        } else if let Some(dialog) = run.job.dialog().cloned() {
            let events = show_dialog(ctx, &dialog, &mut run.edits);
            let had = !events.is_empty();
            run.queue.extend(events);
            if let Some(event) = run.queue.pop_front() {
                run.job.answer(event);
            }
            if had {
                run.edits.clear();
            }
        }
        if let Some(result) = run.job.take_result() {
            match result {
                Ok(_) => app.status(format!("{} finished", run.name)),
                Err(e) => {
                    log::warn!("script {}: {e}", run.name);
                    app.status(format!("{}: {e}", run.name));
                }
            }
            app.sync_views();
        } else {
            ctx.request_repaint();
            app.script_run = Some(run);
        }
    }

    fn alert(ctx: &egui::Context, name: &str, text: &str) -> bool {
        let mut done = false;
        modal::show(ctx, name, Id::new("script-alert"), 0.0, 16, |ui| {
            modal::heading(ui, name);
            ui.add_space(6.0);
            ui.label(text);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button(tl!("OK")).clicked() {
                    done = true;
                }
            });
        });
        done || ctx.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Escape))
    }

    /// Draw the dialog; the result is what the user did, in order.
    fn show_dialog(ctx: &egui::Context, dialog: &Dialog, edits: &mut HashMap<Vec<usize>, String>) -> Vec<Event> {
        let mut events = vec![];
        modal::show(ctx, &dialog.title, Id::new("script-dialog"), 0.0, 16, |ui| {
            if !dialog.title.is_empty() {
                modal::heading(ui, &dialog.title);
                ui.add_space(6.0);
            }
            let mut path = vec![];
            children(ui, &dialog.root, &mut path, &mut events, edits);
        });
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            events.push(Event::Close(2));
        }
        events
    }

    type Edits = HashMap<Vec<usize>, String>;

    fn text_width(ui: &Ui, text: &str) -> f32 {
        let font = egui::TextStyle::Body.resolve(ui.style());
        ui.painter().layout_no_wrap(text.to_string(), font, egui::Color32::WHITE).size().x
    }

    /// How wide a control is without being stretched: the size the script gave it, else its text.
    fn natural_width(ui: &Ui, w: &Widget) -> f32 {
        if w.hidden {
            return 0.0;
        }
        if w.size[0] > 0.0 {
            return w.size[0] as f32;
        }
        let gap = w.spacing.unwrap_or(10.0) as f32;
        let shown: Vec<f32> = w.children.iter().filter(|c| !c.hidden).map(|c| natural_width(ui, c)).collect();
        match w.kind.as_str() {
            "group" | "panel" if w.orientation == "row" => shown.iter().sum::<f32>() + gap * shown.len().saturating_sub(1) as f32,
            "group" | "panel" => shown.iter().copied().fold(0.0, f32::max),
            "button" => text_width(ui, &w.text) + 28.0,
            "checkbox" | "radiobutton" => text_width(ui, &w.text) + 26.0,
            "edittext" => 120.0,
            "dropdownlist" | "listbox" => 140.0,
            _ => text_width(ui, &w.text),
        }
    }

    fn children(ui: &mut Ui, w: &Widget, path: &mut Vec<usize>, events: &mut Vec<Event>, edits: &mut Edits) {
        let gap = w.spacing.unwrap_or(10.0) as f32;
        ui.spacing_mut().item_spacing = egui::vec2(gap.min(8.0).max(2.0), gap.min(8.0).max(2.0));
        let fill = w.align_children.to_lowercase().starts_with("fill");
        let center = w.align_children.to_lowercase().starts_with("center");
        for (i, c) in w.children.iter().enumerate() {
            path.push(i);
            let place = if c.alignment.is_empty() { None } else { Some(c.alignment.to_lowercase()) };
            let place = place.as_deref().unwrap_or(if center { "center" } else { "" });
            // A control placed to the right or in the middle of the width it is given.
            let pad = if c.hidden || place.is_empty() || (fill && c.orientation != "row") {
                0.0
            } else {
                let spare = ui.available_width() - natural_width(ui, c);
                if place.starts_with("right") {
                    spare.max(0.0)
                } else if place.starts_with("center") && !fill_column(c) {
                    (spare / 2.0).max(0.0)
                } else {
                    0.0
                }
            };
            if pad > 0.5 && c.orientation == "row" {
                ui.horizontal(|ui| {
                    ui.add_space(pad);
                    widget(ui, c, path, events, edits, false);
                });
            } else if pad > 0.5 {
                ui.horizontal(|ui| {
                    ui.add_space(pad);
                    ui.vertical(|ui| widget(ui, c, path, events, edits, false));
                });
            } else {
                widget(ui, c, path, events, edits, fill);
            }
            path.pop();
        }
    }

    /// A column that centres its own children takes the whole width instead of being centred.
    fn fill_column(w: &Widget) -> bool {
        w.kind == "group" && w.orientation != "row" && w.align_children.to_lowercase().starts_with("center")
    }

    fn pill_button(ui: &mut Ui, w: &Widget) -> egui::Response {
        let h = if w.size[1] > 0.0 { w.size[1] as f32 } else { 26.0 };
        let min = egui::vec2(if w.size[0] > 0.0 { w.size[0] as f32 } else { 0.0 }, h);
        let line = ui.visuals().widgets.inactive.fg_stroke.color.gamma_multiply(0.55);
        let button = egui::Button::new(&w.text).corner_radius(h / 2.0).stroke(egui::Stroke::new(1.0, line)).min_size(min);
        ui.add_enabled(w.enabled, button)
    }

    fn widget(ui: &mut Ui, w: &Widget, path: &mut Vec<usize>, events: &mut Vec<Event>, edits: &mut Edits, fill: bool) {
        if w.hidden {
            return;
        }
        let id = Id::new(("script-widget", path.clone()));
        match w.kind.as_str() {
            // An empty panel is how scripts draw a dividing line.
            "panel" if w.text.is_empty() && w.children.is_empty() => {
                ui.separator();
            }
            "panel" => {
                let frame = ui.group(|ui| {
                    if w.size[0] > 12.0 {
                        ui.set_min_width(w.size[0] as f32 - 12.0);
                    }
                    if w.size[1] > 12.0 {
                        ui.set_min_height(w.size[1] as f32 - 12.0);
                    }
                    if !w.text.is_empty() {
                        ui.label(RichText::new(&w.text).strong());
                    }
                    ui.vertical(|ui| children(ui, w, path, events, edits));
                });
                let _ = frame;
            }
            "group" if w.orientation == "row" => {
                if w.align_children.to_lowercase().starts_with("top") {
                    ui.horizontal_top(|ui| children(ui, w, path, events, edits));
                } else {
                    ui.horizontal(|ui| children(ui, w, path, events, edits));
                }
            }
            "group" => {
                let width = if fill { ui.available_width() } else { natural_width(ui, w) };
                if width > 0.0 && w.align_children.to_lowercase().starts_with("center") {
                    ui.allocate_ui_with_layout(egui::vec2(width, 0.0), egui::Layout::top_down(egui::Align::Center), |ui| {
                        children(ui, w, path, events, edits)
                    });
                } else {
                    ui.vertical(|ui| children(ui, w, path, events, edits));
                }
            }
            "statictext" if !w.shapes.is_empty() => paint_shapes(ui, w),
            "statictext" => {
                ui.label(&w.text);
            }
            "edittext" => {
                let buf = edits.entry(path.clone()).or_insert_with(|| w.text.clone());
                let width = if w.size[0] > 0.0 { w.size[0] as f32 } else { 160.0 };
                let r = if w.multiline {
                    let height = if w.size[1] > 0.0 { w.size[1] as f32 } else { 120.0 };
                    egui::ScrollArea::vertical()
                        .id_salt(id)
                        .min_scrolled_height(height)
                        .max_height(height)
                        .show(ui, |ui| {
                            let rows = (height / 16.0).max(2.0) as usize;
                            ui.add_enabled(w.enabled, egui::TextEdit::multiline(buf).id(id).desired_width(width).desired_rows(rows))
                        })
                        .inner
                } else {
                    ui.add_enabled(w.enabled, egui::TextEdit::singleline(buf).id(id).desired_width(width))
                };
                if r.lost_focus() && *buf != w.text {
                    events.push(Event::SetText(path.clone(), buf.clone()));
                }
            }
            "checkbox" => {
                let mut v = w.value;
                if ui.add_enabled(w.enabled, egui::Checkbox::new(&mut v, &w.text)).changed() {
                    events.push(Event::SetValue(path.clone(), v));
                }
            }
            "radiobutton" => {
                if ui.add_enabled(w.enabled, egui::RadioButton::new(w.value, &w.text)).clicked() && !w.value {
                    events.push(Event::SetValue(path.clone(), true));
                }
            }
            "button" => {
                if pill_button(ui, w).clicked() {
                    events.push(Event::Click(path.clone()));
                }
            }
            "dropdownlist" => {
                let shown = w.selection.and_then(|i| w.items.get(i)).cloned().unwrap_or_default();
                ui.add_enabled_ui(w.enabled, |ui| {
                    egui::ComboBox::from_id_salt(id).selected_text(shown).show_ui(ui, |ui| {
                        for (i, item) in w.items.iter().enumerate() {
                            if ui.selectable_label(w.selection == Some(i), item).clicked() && w.selection != Some(i) {
                                events.push(Event::Select(path.clone(), Some(i)));
                            }
                        }
                    });
                });
            }
            "listbox" => {
                egui::ScrollArea::vertical().id_salt(id).max_height(160.0).show(ui, |ui| {
                    for (i, item) in w.items.iter().enumerate() {
                        if ui.add_enabled(w.enabled, egui::Button::selectable(w.selection == Some(i), item)).clicked() && w.selection != Some(i) {
                            events.push(Event::Select(path.clone(), Some(i)));
                        }
                    }
                });
            }
            "progressbar" => {
                ui.add(egui::ProgressBar::new((w.number / 100.0).clamp(0.0, 1.0) as f32));
            }
            "slider" => {
                ui.label(format!("{}", w.number));
            }
            _ => {
                if !w.text.is_empty() {
                    ui.label(&w.text);
                }
            }
        }
    }

    fn rgba(c: [f32; 4]) -> egui::Color32 {
        let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        egui::Color32::from_rgba_unmultiplied(b(c[0]), b(c[1]), b(c[2]), b(c[3]))
    }

    /// A widget the script paints itself (`onDraw`): its shapes, in a box of the size it asked for.
    fn paint_shapes(ui: &mut Ui, w: &Widget) {
        let [sw, sh] = w.size;
        let (sw, sh) = (if sw > 0.0 { sw } else { 22.0 } as f32, if sh > 0.0 { sh } else { 22.0 } as f32);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(sw, sh), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        let at = |x: f64, y: f64| rect.min + egui::vec2(x as f32, y as f32);
        for s in &w.shapes {
            let [x, y, width, height] = s.rect;
            let stroke = s.stroke.map(|c| egui::Stroke::new(s.width.max(0.5) as f32, rgba(c))).unwrap_or(egui::Stroke::NONE);
            let fill = s.fill.map(rgba).unwrap_or(egui::Color32::TRANSPARENT);
            match s.kind.as_str() {
                "ellipse" => {
                    let r = egui::Rect::from_min_size(at(x, y), egui::vec2(width as f32, height as f32));
                    painter.circle(r.center(), r.width().min(r.height()) / 2.0, fill, stroke);
                }
                "rect" => {
                    let r = egui::Rect::from_min_size(at(x, y), egui::vec2(width as f32, height as f32));
                    painter.rect(r, 0.0, fill, stroke, egui::StrokeKind::Inside);
                }
                "poly" => {
                    let pts: Vec<egui::Pos2> = s.points.iter().map(|p| at(p[0], p[1])).collect();
                    if pts.len() >= 3 {
                        painter.add(egui::Shape::convex_polygon(pts, fill, stroke));
                    }
                }
                _ => {}
            }
        }
    }

    pub(super) fn read_source(app: &VectorcraftApp, path: &str) -> Result<String, String> {
        let bytes = match app.services.read.as_ref() {
            Some(read) => read(path)?,
            None => std::fs::read(path).map_err(|e| format!("{path}: {e}"))?,
        };
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub(super) fn pick(app: &mut VectorcraftApp) -> Option<String> {
        let pick = crate::FilePick { filters: vec![("Scripts", vectorcraft_engine::cmd::scripts::EXTS)], ..Default::default() };
        picks::open(app, &pick)
    }

    impl Running {
        /// The title of the dialog the script is waiting in.
        pub fn dialog_title(&self) -> Option<String> {
            self.job.dialog().map(|d| d.title.clone())
        }
    }

    pub(super) fn is_running(app: &VectorcraftApp) -> bool {
        app.script_run.is_some()
    }

    pub(super) fn _unused(_: &Value) {}
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::Running;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::frame;

/// The web build has no thread to run a script on.
#[cfg(target_arch = "wasm32")]
pub struct Running;
#[cfg(target_arch = "wasm32")]
pub(crate) fn frame(_app: &mut VectorcraftApp, _ctx: &egui::Context) {}

/// `ui.runScript`: run the script at `path` or given as `data`; without either, pick one.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn command(app: &mut VectorcraftApp, p: &Value) -> Result<Value, String> {
    if native::is_running(app) {
        return Err("A script is already running".into());
    }
    if let Some(data) = p.get("data").and_then(Value::as_str) {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("Script.jsx").to_string();
        native::start(app, data.to_string(), name.clone(), None)?;
        return Ok(json!({ "running": name }));
    }
    let path = match p.get("path").and_then(Value::as_str) {
        Some(path) => path.to_string(),
        None => native::pick(app).ok_or("cancelled")?,
    };
    let name = std::path::Path::new(&path).file_name().and_then(|n| n.to_str()).unwrap_or(&path).to_string();
    let source = native::read_source(app, &path)?;
    native::start(app, source, name.clone(), Some(std::path::PathBuf::from(&path)))?;
    Ok(json!({ "running": name }))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn command(_app: &mut VectorcraftApp, _p: &Value) -> Result<Value, String> {
    Err("Scripts need the desktop app".into())
}
