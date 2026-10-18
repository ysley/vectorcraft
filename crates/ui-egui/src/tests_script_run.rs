//! File › Scripts › Run Script… (`ui.runScript`): a script runs beside the open document, its
//! commands change the artwork frame by frame, and the dialog it shows is drawn and answered here.

use std::time::{Duration, Instant};

use serde_json::json;
use vectorcraft_engine::Session;

use crate::{VectorcraftApp, theme};

fn app() -> VectorcraftApp {
    let mut session = Session::new();
    session.execute("file.new", &json!({})).unwrap();
    VectorcraftApp::new(session, crate::Services::default())
}

fn frame(app: &mut VectorcraftApp, ctx: &egui::Context, escape: bool) {
    let mut input = egui::RawInput::default();
    if escape {
        input.events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        });
    }
    let mut out = ctx.run_ui(input, |ui| crate::script_run::frame(app, ui.ctx()));
    out.textures_delta.clear();
}

fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install_fonts(&ctx);
    theme::apply(&ctx, Default::default());
    ctx
}

fn until(app: &mut VectorcraftApp, ctx: &egui::Context, mut done: impl FnMut(&VectorcraftApp) -> bool) {
    let start = Instant::now();
    while !done(app) {
        assert!(start.elapsed() < Duration::from_secs(20), "timed out: {}", app.ui.status);
        frame(app, ctx, false);
    }
}

fn items(app: &VectorcraftApp) -> usize {
    vectorcraft_extendscript::document_of(&app.session)["layers"].to_string().matches("\"kind\"").count()
}

#[test]
fn a_script_draws_in_the_open_document_and_finishes() {
    let mut app = app();
    let ctx = context();
    let src = "var r = app.activeDocument.pathItems.rectangle(50, 10, 40, 20); r.name = 'feito';";
    app.run("ui.runScript", json!({"data": src, "name": "Teste.jsx"})).unwrap();
    assert!(app.script_run.is_some());
    until(&mut app, &ctx, |a| a.script_run.is_none());
    assert_eq!(app.ui.status, "Teste.jsx finished");
    assert!(items(&app) >= 1);
}

#[test]
fn a_script_dialog_waits_for_the_user_and_escape_cancels_it() {
    let mut app = app();
    let ctx = context();
    let src = "var w = new Window('dialog', 'Cor da gola'); w.add('statictext', undefined, 'Escolha'); \
               var code = w.show(); app.activeDocument.pathItems.rectangle(10, 0, 5, 5).name = 'cod' + code;";
    app.run("ui.runScript", json!({"data": src})).unwrap();
    until(&mut app, &ctx, |a| a.script_run.as_ref().is_some_and(|r| r.dialog_title().is_some()));
    assert_eq!(app.script_run.as_ref().and_then(|r| r.dialog_title()).as_deref(), Some("Cor da gola"));
    // The interface is not held up: another run is refused while it waits.
    assert!(app.run("ui.runScript", json!({"data": "1"})).is_err());
    frame(&mut app, &ctx, true);
    until(&mut app, &ctx, |a| a.script_run.is_none());
    assert!(vectorcraft_extendscript::document_of(&app.session).to_string().contains("cod2"));
}

#[test]
fn a_script_error_reaches_the_status_line() {
    let mut app = app();
    let ctx = context();
    app.run("ui.runScript", json!({"data": "nope.nothing();", "name": "Ruim.jsx"})).unwrap();
    until(&mut app, &ctx, |a| a.script_run.is_none());
    assert!(app.ui.status.starts_with("Ruim.jsx: "), "{}", app.ui.status);
}
