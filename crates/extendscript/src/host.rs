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
        Self { state: Rc::new(RefCell::new(State { session, backend: None, alerts: vec![], presenter: None, file: None })) }
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
        if let Some(b) = st.backend.as_mut() {
            return b.execute(command, params);
        }
        execute_on(&mut st.session, command, params)
    }

    /// The active document as the engine summarises it (`Null` without one).
    pub fn document(&self) -> Value {
        match self.state.try_borrow_mut() {
            Ok(mut st) => match st.backend.as_mut() {
                Some(b) => b.document(),
                None => inspect::document(&st.session),
            },
            Err(_) => Value::Null,
        }
    }

    /// Give the session back, with the alerts the script made. The handle stays usable but holds
    /// a fresh empty session afterwards.
    pub fn take(&self) -> Result<(Session, Vec<String>), String> {
        let mut st = self.state.try_borrow_mut().map_err(|_| "the document is busy".to_string())?;
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
