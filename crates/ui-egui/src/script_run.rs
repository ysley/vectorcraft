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

    pub(super) fn start(app: &mut VectorcraftApp, source: String, name: String) -> Result<(), String> {
        if app.script_run.is_some() {
            return Err("A script is already running".into());
        }
        let source = source.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(source);
        let job = remote::spawn(source, || {}).map_err(|e| e.to_string())?;
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

    fn children(ui: &mut Ui, w: &Widget, path: &mut Vec<usize>, events: &mut Vec<Event>, edits: &mut HashMap<Vec<usize>, String>) {
        for (i, c) in w.children.iter().enumerate() {
            path.push(i);
            widget(ui, c, path, events, edits);
            path.pop();
        }
    }

    fn widget(ui: &mut Ui, w: &Widget, path: &mut Vec<usize>, events: &mut Vec<Event>, edits: &mut HashMap<Vec<usize>, String>) {
        if w.hidden {
            return;
        }
        let id = Id::new(("script-widget", path.clone()));
        match w.kind.as_str() {
            "panel" => {
                ui.group(|ui| {
                    if !w.text.is_empty() {
                        ui.label(RichText::new(&w.text).strong());
                    }
                    ui.vertical(|ui| children(ui, w, path, events, edits));
                });
            }
            "group" => {
                if w.orientation == "row" {
                    ui.horizontal(|ui| children(ui, w, path, events, edits));
                } else {
                    ui.vertical(|ui| children(ui, w, path, events, edits));
                }
            }
            "statictext" => {
                ui.label(&w.text);
            }
            "edittext" => {
                let buf = edits.entry(path.clone()).or_insert_with(|| w.text.clone());
                let r = ui.add_enabled(w.enabled, egui::TextEdit::singleline(buf).id(id));
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
                if ui.add_enabled(w.enabled, egui::Button::new(&w.text)).clicked() {
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
        native::start(app, data.to_string(), name.clone())?;
        return Ok(json!({ "running": name }));
    }
    let path = match p.get("path").and_then(Value::as_str) {
        Some(path) => path.to_string(),
        None => native::pick(app).ok_or("cancelled")?,
    };
    let name = std::path::Path::new(&path).file_name().and_then(|n| n.to_str()).unwrap_or(&path).to_string();
    let source = native::read_source(app, &path)?;
    native::start(app, source, name.clone())?;
    Ok(json!({ "running": name }))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn command(_app: &mut VectorcraftApp, _p: &Value) -> Result<Value, String> {
    Err("Scripts need the desktop app".into())
}
