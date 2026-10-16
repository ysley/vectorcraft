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
//! Not supported yet: dialogs (`Window`), `File` and `Folder`, `$.evalFile`, text, artboards,
//! geometry and the E4X XML literals; a script that uses them fails with the engine's error
//! naming the missing object.
#![forbid(unsafe_code)]

mod dom;
mod host;

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
    let host = Host::new(std::mem::replace(session, Session::new()));
    let result = evaluate(&host, source);
    // The script is over: take the session back whatever happened.
    let (back, alerts) = host.take().map_err(ScriptError::Internal)?;
    *session = back;
    match result {
        Ok(value) => Ok(Outcome { alerts, value }),
        Err(message) => Err(ScriptError::Script { message, alerts }),
    }
}

fn evaluate(host: &Host, source: &str) -> Result<String, String> {
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
}
