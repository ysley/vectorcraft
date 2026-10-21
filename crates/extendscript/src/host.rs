//! The session a script runs against, shared by the native functions the script calls.

use std::{cell::RefCell, rc::Rc};

use boa_gc::{Finalize, Trace};
use serde_json::Value;
use vectorcraft_engine::{Session, inspect};

use crate::ui::{Dialog, Event, Presenter};

/// What the script and its host exchange while it runs.
/// Where a script's commands and reads go when the document is not in this thread: a link to the
/// thread that owns it ([`crate::remote`]).
pub(crate) trait Backend {
    fn execute(&mut self, command: &str, params: &Value) -> Result<Value, String>;
    fn document(&mut self) -> Value;
    /// Show the script's `alert()` and wait for it to be dismissed.
    fn alert(&mut self, _text: &str) {}
}

pub(crate) struct State {
    pub session: Session,
    pub backend: Option<Box<dyn Backend>>,
    /// The text of every `alert()` the script made, in order.
    pub alerts: Vec<String>,
    /// Whoever answers the dialogs the script shows (none: showing one is an error).
    pub presenter: Option<Box<dyn Presenter>>,
    /// The script file being run, when it came from one.
    pub file: Option<std::path::PathBuf>,
    /// The document's summary, kept until a command may have changed it: reading one property
    /// of an item would otherwise summarise (or fetch) the whole document again.
    pub doc_cache: Option<Rc<Value>>,
    /// Where each document's script coordinates start (its first artboard's top-left corner when
    /// the script first met it), by `uid`: moving the artboard later does not move the origin, as
    /// the reference app's document coordinates do not move.
    pub origins: std::collections::HashMap<u64, [f64; 2]>,
    /// The groups made with `groupItems.add()` that have no content yet, by token.
    pub pending: Vec<Rc<crate::nodeops::Pending>>,
    /// The artboard a script made active in each document (`setActiveArtboardIndex`), by `uid`.
    pub active_artboard: std::collections::HashMap<u64, usize>,
}

/// A handle the script's native functions share. The script runs on one thread and every
/// borrow is released before control returns to the script, so borrows never nest.
#[derive(Clone, Trace, Finalize)]
pub(crate) struct Host {
    #[unsafe_ignore_trace]
    state: Rc<RefCell<State>>,
}

impl Host {
    pub fn new(session: Session) -> Self {
        Self {
            state: Rc::new(RefCell::new(State {
                session,
                backend: None,
                alerts: vec![],
                presenter: None,
                file: None,
                doc_cache: None,
                origins: Default::default(),
                pending: vec![],
                active_artboard: Default::default(),
            })),
        }
    }

    pub fn set_file(&self, file: Option<std::path::PathBuf>) {
        if let Ok(mut st) = self.state.try_borrow_mut() {
            st.file = file;
        }
    }

    /// The script file's path as scripts see it (`$.fileName`): empty without one.
    pub fn file_name(&self) -> String {
        self.state.try_borrow().ok().and_then(|st| st.file.as_ref().map(|f| f.to_string_lossy().replace('\\', "/"))).unwrap_or_default()
    }

    pub fn script_dir(&self) -> Option<std::path::PathBuf> {
        self.state.try_borrow().ok().and_then(|st| st.file.as_ref().and_then(|f| f.parent().map(std::path::Path::to_path_buf)))
    }

    pub fn set_presenter(&self, presenter: Option<Box<dyn Presenter>>) {
        if let Ok(mut st) = self.state.try_borrow_mut() {
            st.presenter = presenter;
        }
    }

    pub fn open_dialog(&self, dialog: &Dialog) {
        let taken = self.state.try_borrow_mut().ok().and_then(|mut st| st.presenter.take());
        if let Some(mut p) = taken {
            p.open(dialog);
            if let Ok(mut st) = self.state.try_borrow_mut() {
                st.presenter = Some(p);
            }
        }
    }

    /// The user's next action on `dialog`; the presenter is not borrowed while it waits.
    pub fn next_dialog_event(&self, dialog: &Dialog) -> Result<Event, String> {
        let taken = self.state.try_borrow_mut().map_err(|_| "the document is busy".to_string())?.presenter.take();
        let mut p = taken.ok_or_else(|| "this run cannot show dialogs".to_string())?;
        let event = p.next_event(dialog);
        if let Ok(mut st) = self.state.try_borrow_mut() {
            st.doc_cache = None; // the user may have worked on the document meanwhile
            st.presenter = Some(p);
        }
        event
    }

    /// A host whose document lives elsewhere.
    pub fn remote(backend: Box<dyn Backend>) -> Self {
        let host = Self::new(Session::new());
        if let Ok(mut st) = host.state.try_borrow_mut() {
            st.backend = Some(backend);
        }
        host
    }

    pub fn alert(&self, text: String) {
        // The backend may wait for the user: hold no borrow while it does.
        let taken = self.state.try_borrow_mut().ok().and_then(|mut st| st.backend.take());
        if let Some(mut b) = taken {
            b.alert(&text);
            if let Ok(mut st) = self.state.try_borrow_mut() {
                st.backend = Some(b);
            }
        }
        if let Ok(mut st) = self.state.try_borrow_mut() {
            st.alerts.push(text);
        }
    }

    /// Run an engine command; the error is the engine's message.
    ///
    /// Painting an object from a script does not change the fill and stroke new art takes (the
    /// toolbar's defaults), as it does when the user paints the selection: they are put back.
    pub fn exec(&self, command: &str, params: &Value) -> Result<Value, String> {
        let mut st = self.state.try_borrow_mut().map_err(|_| "the document is busy".to_string())?;
        st.doc_cache = None;
        if let Some(b) = st.backend.as_mut() {
            return b.execute(command, params);
        }
        execute_on(&mut st.session, command, params)
    }

    /// The active document as the engine summarises it (`Null` without one).
    pub fn document(&self) -> Rc<Value> {
        match self.state.try_borrow_mut() {
            Ok(mut st) => {
                if let Some(doc) = &st.doc_cache {
                    return doc.clone();
                }
                let mut summary = match st.backend.as_mut() {
                    Some(b) => b.document(),
                    None => inspect::document(&st.session),
                };
                // The document's script origin: fixed when first met.
                if let Some(uid) = summary.get("uid").and_then(Value::as_u64) {
                    let first = summary.get("artboards").and_then(Value::as_array).and_then(|a| a.first());
                    let num = |k: &str| first.and_then(|a| a.get(k)).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0);
                    let found = [num("x"), num("y")];
                    let origin = *st.origins.entry(uid).or_insert(found);
                    summary["scriptOrigin"] = serde_json::json!({"x": origin[0], "y": origin[1]});
                }
                let doc = Rc::new(summary);
                st.doc_cache = Some(doc.clone());
                doc
            }
            Err(_) => Rc::new(Value::Null),
        }
    }

    pub fn register_pending(&self, p: Rc<crate::nodeops::Pending>) -> u32 {
        match self.state.try_borrow_mut() {
            Ok(mut st) => {
                st.pending.push(p);
                (st.pending.len() - 1) as u32
            }
            Err(_) => 0,
        }
    }

    pub fn pending_of(&self, token: u32) -> Option<Rc<crate::nodeops::Pending>> {
        self.state.try_borrow().ok().and_then(|st| st.pending.get(token as usize).cloned())
    }

    /// The artboard a script made active in the document `uid` (the first by default).
    pub fn active_artboard(&self, uid: u64) -> usize {
        self.state.try_borrow().ok().and_then(|st| st.active_artboard.get(&uid).copied()).unwrap_or(0)
    }

    pub fn set_active_artboard(&self, uid: u64, index: usize) {
        if let Ok(mut st) = self.state.try_borrow_mut() {
            st.active_artboard.insert(uid, index);
        }
    }

    /// The `uid` of the active document.
    pub fn active_uid(&self) -> Option<u64> {
        self.document().get("uid").and_then(Value::as_u64)
    }

    /// Make the document with this `uid` the active one. An error when it is closed.
    pub fn activate(&self, uid: u64) -> Result<(), String> {
        let doc = self.document();
        if doc.get("uid").and_then(Value::as_u64) == Some(uid) {
            return Ok(());
        }
        let index = doc
            .get("documents")
            .and_then(Value::as_array)
            .and_then(|list| list.iter().position(|d| d.get("uid").and_then(Value::as_u64) == Some(uid)))
            .ok_or_else(|| "the document was closed".to_string())?;
        self.exec("document.activate", &serde_json::json!({"index": index})).map(|_| ())
    }

    /// Give the session back, with the alerts the script made. The handle stays usable but holds
    /// a fresh empty session afterwards.
    pub fn take(&self) -> Result<(Session, Vec<String>), String> {
        let mut st = self.state.try_borrow_mut().map_err(|_| "the document is busy".to_string())?;
        st.doc_cache = None;
        let session = std::mem::replace(&mut st.session, Session::new());
        Ok((session, std::mem::take(&mut st.alerts)))
    }
}

/// Run an engine command for a script; the error is the engine's message.
///
/// Painting an object from a script does not change the fill and stroke new art takes (the
/// toolbar's defaults), as it does when the user paints the selection: they are put back.
pub fn execute_on(session: &mut Session, command: &str, params: &Value) -> Result<Value, String> {
    let (defaults, fill_active) = (session.paint.clone(), session.fill_active);
    let result = session.execute(command, params).map_err(|e| e.to_string());
    session.paint = defaults;
    session.fill_active = fill_active;
    result
}

/// The document a script sees: the engine's summary of the active one.
pub fn document_of(session: &Session) -> Value {
    inspect::document(session)
}
