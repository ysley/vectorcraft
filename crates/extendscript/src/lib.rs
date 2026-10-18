//! Runs scripts written for the reference app's scripting language (`.jsx`) against a VectorCraft
//! [`Session`].
//!
//! The language is JavaScript with the reference app's object model on top. A JavaScript engine
//! (`boa_engine`) runs the language; [`dom`] defines the objects a script reaches (`app`, the
//! document, layers, page items, colours) and turns each read and change into the engine's
//! documents and commands. Nothing here reads, runs or copies the reference app: the names come
//! from its public scripting reference, and what a script sees beyond them is what VectorCraft
//! does. `vectorcraft-scriptcheck` reports, before a run, what a script uses.
//!
//! Not supported yet: `File` and `Folder`, `$.evalFile`, text, artboards,
//! geometry and the E4X XML literals; a script that uses them fails with the engine's error
//! naming the missing object.
#![forbid(unsafe_code)]

mod dom;
pub mod driver;
mod geom;
mod host;
mod node;
pub mod remote;
pub mod ui;

pub use host::{document_of, execute_on};
pub use ui::{Dialog, Event, Presenter, Widget};

use boa_engine::{Context, Source};
use vectorcraft_engine::Session;

use host::Host;

/// What a finished script left behind.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outcome {
    /// The text of every `alert()`, in order. A script run from here shows no dialogs.
    pub alerts: Vec<String>,
    /// The value of the script's last expression, as text.
    pub value: String,
}

/// Why a script did not finish.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScriptError {
    /// The script threw, or did not parse. Its changes up to that point stay in the document,
    /// each one its own undo step.
    #[error("{message}")]
    Script { message: String, alerts: Vec<String> },
    /// The host could not run the script at all.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Run `source` against `session`: its changes land in the session's active document.
pub fn run(session: &mut Session, source: &str) -> Result<Outcome, ScriptError> {
    run_with(session, source, None)
}

/// Like [`run`], with someone to answer the dialogs the script shows.
pub fn run_with(session: &mut Session, source: &str, presenter: Option<Box<dyn Presenter>>) -> Result<Outcome, ScriptError> {
    let host = Host::new(std::mem::replace(session, Session::new()));
    host.set_presenter(presenter);
    let result = evaluate(&host, source);
    // The script is over: take the session back whatever happened.
    let (back, alerts) = host.take().map_err(ScriptError::Internal)?;
    *session = back;
    match result {
        Ok(value) => Ok(Outcome { alerts, value }),
        Err(message) => Err(ScriptError::Script { message, alerts }),
    }
}

/// Run `source` against a document that lives elsewhere ([`remote`]); the script's alerts go to
/// the backend as they happen.
pub(crate) fn run_remote(source: &str, backend: Box<dyn host::Backend>, presenter: Box<dyn Presenter>) -> Result<Outcome, ScriptError> {
    let host = Host::remote(backend);
    host.set_presenter(Some(presenter));
    let result = evaluate(&host, source);
    let (_, alerts) = host.take().map_err(ScriptError::Internal)?;
    match result {
        Ok(value) => Ok(Outcome { alerts, value }),
        Err(message) => Err(ScriptError::Script { message, alerts }),
    }
}

fn evaluate(host: &Host, source: &str) -> Result<String, String> {
    // A bug in the host must not take the document down with it.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| evaluate_inner(host, source)))
        .unwrap_or_else(|_| Err("internal error: the script host failed".to_string()))
}

fn evaluate_inner(host: &Host, source: &str) -> Result<String, String> {
    let mut ctx = Context::default();
    dom::install(host, &mut ctx).map_err(|e| e.to_string())?;
    let value = ctx.eval(Source::from_bytes(source)).map_err(|e| e.to_string())?;
    let text = value.to_string(&mut ctx).map_err(|e| e.to_string())?;
    Ok(text.to_std_string_escaped())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn session_with_document() -> Session {
        let mut s = Session::new();
        s.execute("file.new", &json!({})).unwrap();
        s
    }

    #[test]
    fn alert_collects_text_and_the_value_is_the_last_expression() {
        let mut s = Session::new();
        let out = run(&mut s, "var t = 0; for (var i = 1; i <= 3; i++) { t += i; } alert('soma ' + t); t * 2").unwrap();
        assert_eq!(out.alerts, vec!["soma 6".to_string()]);
        assert_eq!(out.value, "12");
    }

    #[test]
    fn a_script_error_names_the_problem_and_gives_the_session_back() {
        let mut s = session_with_document();
        let err = run(&mut s, "alert('antes'); naoExiste.coisa();").unwrap_err();
        match err {
            ScriptError::Script { message, alerts } => {
                assert!(message.contains("naoExiste"), "{message}");
                assert_eq!(alerts, vec!["antes".to_string()]);
            }
            other => panic!("{other:?}"),
        }
        // The document survived: a second script still sees it.
        assert_eq!(run(&mut s, "app.documents.length").unwrap().value, "1");
    }

    #[test]
    fn documents_are_empty_without_one_and_active_document_throws() {
        let mut s = Session::new();
        assert_eq!(run(&mut s, "app.documents.length").unwrap().value, "0");
        assert!(run(&mut s, "app.activeDocument").is_err());
    }

    #[test]
    fn layers_are_found_renamed_and_hidden_through_commands() {
        let mut s = session_with_document();
        let out = run(
            &mut s,
            "var doc = app.activeDocument;
             var lay = doc.layers[0];
             lay.name = 'COR_DA_GOLA';
             lay.visible = false;
             doc.layers.getByName('COR_DA_GOLA').typename + ':' + lay.visible",
        )
        .unwrap();
        assert_eq!(out.value, "Layer:false");
        let doc = vectorcraft_engine::inspect::document(&s);
        assert_eq!(doc["layers"][0]["name"], "COR_DA_GOLA");
        assert_eq!(doc["layers"][0]["visible"], false);
    }

    #[test]
    fn page_items_take_a_colour_and_lose_their_stroke() {
        let mut s = session_with_document();
        s.execute("shape.rectangle", &json!({"x": 0, "y": 0, "width": 40, "height": 20})).unwrap();
        s.execute("shape.rectangle", &json!({"x": 50, "y": 0, "width": 40, "height": 20})).unwrap();
        let out = run(
            &mut s,
            "var cor = new RGBColor(); cor.red = 205; cor.green = 95; cor.blue = 48;
             var itens = app.activeDocument.layers[0].pageItems;
             for (var i = 0; i < itens.length; i++) {
                 itens[i].fillColor = cor;
                 itens[i].stroked = false;
             }
             itens.length + ' ' + itens[0].typename + ' ' + itens[0].fillColor.red + ' ' + itens[0].stroked",
        )
        .unwrap();
        assert_eq!(out.value, "2 PathItem 205 false");
        let doc = vectorcraft_engine::inspect::document(&s);
        let items = doc["layers"][0]["children"].as_array().unwrap();
        assert!(items.iter().all(|i| i["fill"] == "#cd5f30" && i["stroke"] == "None"), "{items:?}");
    }

    #[test]
    fn a_bad_value_is_a_script_error_not_a_crash() {
        let mut s = session_with_document();
        let err = run(&mut s, "app.activeDocument.layers[0].fillColor = 5;").unwrap_err();
        assert!(matches!(err, ScriptError::Script { .. }), "{err:?}");
    }

    fn rect(s: &mut Session, x: f64, y: f64, w: f64, h: f64) -> u64 {
        s.execute("shape.rectangle", &json!({"x": x, "y": y, "width": w, "height": h})).unwrap()["id"].as_u64().unwrap()
    }

    #[test]
    fn bounds_are_left_top_right_bottom_with_y_up_and_position_follows_them() {
        let mut s = session_with_document();
        rect(&mut s, 10.0, 20.0, 40.0, 30.0);
        let out = run(
            &mut s,
            "var it = app.activeDocument.layers[0].pageItems[0];
             var b = it.geometricBounds;
             var first = [b[2] - b[0], b[1] - b[3], it.width, it.height].join(',');
             it.left = 5; it.top = 100;
             var c = it.geometricBounds;
             first + '|' + c.join(',')",
        )
        .unwrap();
        assert_eq!(out.value, "40,30,40,30|5,100,45,70");
    }

    #[test]
    fn translate_moves_up_for_a_positive_y() {
        let mut s = session_with_document();
        rect(&mut s, 0.0, 0.0, 10.0, 10.0);
        let out = run(
            &mut s,
            "var it = app.activeDocument.layers[0].pageItems[0];
             var t0 = it.top; it.translate(3, 7);
             (it.top - t0) + ',' + it.left",
        )
        .unwrap();
        let (dy, left) = out.value.split_once(',').unwrap();
        assert_eq!(dy, "7");
        assert!(left.parse::<f64>().is_ok(), "{}", out.value);
    }

    #[test]
    fn new_paths_take_points_and_a_stroke_set_through_a_live_colour() {
        let mut s = session_with_document();
        let out = run(
            &mut s,
            "var doc = app.activeDocument;
             var line = doc.pathItems.add();
             line.setEntirePath([[10, 10], [60, 10]]);
             line.strokeColor = new RGBColor();
             line.strokeColor.red = 237; line.strokeColor.green = 30; line.strokeColor.blue = 121;
             line.strokeWidth = 2;
             line.filled = false;
             [line.typename, line.width, line.strokeColor.red, line.strokeWidth, line.filled, line.stroked].join(',')",
        )
        .unwrap();
        assert_eq!(out.value, "PathItem,50,237,2,false,true");
        let doc = vectorcraft_engine::inspect::document(&s);
        let item = &doc["layers"][0]["children"][0];
        assert_eq!(item["stroke"], "#ed1e79");
        assert_eq!(item["fill"], "None");
    }

    #[test]
    fn text_frames_get_contents_style_and_position() {
        let mut s = session_with_document();
        let out = run(
            &mut s,
            "var doc = app.activeDocument;
             var t = doc.textFrames.add();
             t.contents = 'abc';
             t.textRange.characterAttributes.size = 18;
             t.textRange.characterAttributes.fillColor = new RGBColor();
             t.textRange.characterAttributes.fillColor.red = 255;
             t.textRange.paragraphAttributes.justification = Justification.CENTER;
             t.left = 40; t.top = 90;
             t.contents + ',' + doc.textFrames.length + ',' + t.left + ',' + t.top",
        )
        .unwrap();
        assert_eq!(out.value, "abc,1,40,90");
        let doc = vectorcraft_engine::inspect::document(&s);
        assert_eq!(doc["layers"][0]["children"][0]["fill"], "#ff0000");
    }

    #[test]
    fn rotating_text_keeps_it_in_the_document() {
        let mut s = session_with_document();
        let out = run(
            &mut s,
            "var t = app.activeDocument.textFrames.add(); t.contents = 'xyz';
             var w = t.width, h = t.height; t.rotate(90);
             (Math.abs(t.width - h) < 0.01 && Math.abs(t.height - w) < 0.01)",
        )
        .unwrap();
        assert_eq!(out.value, "true");
    }

    #[test]
    fn artboards_are_added_read_and_renamed_in_script_coordinates() {
        let mut s = session_with_document();
        let out = run(
            &mut s,
            "var doc = app.activeDocument;
             var n = doc.artboards.length;
             var ab = doc.artboards.add([100, 300, 400, 100]);
             ab.name = 'novo';
             var r = doc.artboards[doc.artboards.length - 1].artboardRect;
             (doc.artboards.length - n) + '|' + r.join(',') + '|' + doc.artboards[doc.artboards.length - 1].name",
        )
        .unwrap();
        assert_eq!(out.value, "1|100,300,400,100|novo");
    }

    #[test]
    fn a_rectangle_can_be_sent_to_the_back() {
        let mut s = session_with_document();
        rect(&mut s, 0.0, 0.0, 10.0, 10.0);
        run(
            &mut s,
            "var doc = app.activeDocument;
             var r = doc.pathItems.rectangle(200, 0, 50, 50);
             r.fillColor = new RGBColor();
             r.filled = true;
             r.zOrder(ZOrderMethod.SENDTOBACK);",
        )
        .unwrap();
        let doc = vectorcraft_engine::inspect::document(&s);
        // The engine lists the top of the stack first: the new rectangle is last.
        let kids = doc["layers"][0]["children"].as_array().unwrap();
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[1]["fill"], "#000000", "{kids:?}");
    }

    #[test]
    fn dimension_guides_script_end_to_end() {
        // A script of the shape the production tools have: measure the selection, draw a guide
        // line and a label beside it in centimetres, then add an artboard around the result.
        let mut s = session_with_document();
        let id = rect(&mut s, 100.0, 100.0, 85.04, 56.69);
        s.execute("select.set", &json!({"ids": [id]})).unwrap();
        let out = run(
            &mut s,
            "var doc = app.activeDocument;
             var sel = doc.selection;
             var cm = 28.3464567;
             for (var i = 0; i < sel.length; i++) {
                 var b = sel[i].geometricBounds;
                 var width = b[2] - b[0];
                 var line = doc.pathItems.add();
                 line.setEntirePath([[b[0], b[1] - cm], [b[0] + width, b[1] - cm]]);
                 line.strokeColor = new RGBColor();
                 line.strokeColor.red = 237;
                 line.filled = false;
                 var label = doc.textFrames.add();
                 label.contents = (width / cm).toFixed(1) + ' cm';
                 label.top = b[1] - cm - 5;
                 label.left = b[0];
                 var lb = label.geometricBounds;
                 doc.artboards.add([Math.min(b[0], lb[0]), Math.max(b[1], lb[1]), Math.max(b[2], lb[2]), Math.min(b[3] - cm, lb[3])]);
             }
             doc.textFrames[0].contents + ',' + doc.pathItems.length + ',' + doc.artboards.length",
        )
        .unwrap();
        assert_eq!(out.value, "3.0 cm,2,2");
    }

    #[test]
    fn painting_from_a_script_leaves_the_new_art_defaults_alone() {
        let mut s = session_with_document();
        let before = rect(&mut s, 0.0, 0.0, 10.0, 10.0);
        run(
            &mut s,
            "var doc = app.activeDocument;
             var line = doc.pathItems.add();
             line.setEntirePath([[0, 0], [30, 0]]);
             line.strokeColor = new RGBColor();
             line.strokeColor.red = 237;
             line.strokeWidth = 5;
             var r = doc.pathItems.rectangle(100, 0, 20, 20);
             r.fillColor = new RGBColor();",
        )
        .unwrap();
        let after = rect(&mut s, 50.0, 0.0, 10.0, 10.0);
        let doc = vectorcraft_engine::inspect::document(&s);
        let node = |id: u64| doc["layers"][0]["children"].as_array().unwrap().iter().find(|n| n["id"] == id).unwrap().clone();
        // A rectangle the script made, and one made after it, take what a rectangle made before took.
        let (b, a) = (node(before), node(after));
        assert_eq!((&a["stroke"], &a["strokeWidth"]), (&b["stroke"], &b["strokeWidth"]), "{a} {b}");
        let made = doc["layers"][0]["children"].as_array().unwrap().iter().find(|n| n["fill"] == "#000000" && n["id"] != before).unwrap();
        assert_eq!((&made["stroke"], &made["strokeWidth"]), (&b["stroke"], &b["strokeWidth"]), "{made} {b}");
    }
}
