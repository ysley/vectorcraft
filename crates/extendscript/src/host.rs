//! The session a script runs against, shared by the native functions the script calls.

use std::{cell::RefCell, rc::Rc};

use boa_gc::{Finalize, Trace};
use serde_json::Value;
use vectorcraft_engine::{Session, inspect};

/// What the script and its host exchange while it runs.
pub(crate) struct State {
    pub session: Session,
    /// The text of every `alert()` the script made, in order.
    pub alerts: Vec<String>,
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
        Self { state: Rc::new(RefCell::new(State { session, alerts: vec![] })) }
    }

    pub fn alert(&self, text: String) {
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
        let (defaults, fill_active) = (st.session.paint.clone(), st.session.fill_active);
        let result = st.session.execute(command, params).map_err(|e| e.to_string());
        st.session.paint = defaults;
        st.session.fill_active = fill_active;
        result
    }

    /// The active document as the engine summarises it (`Null` without one).
    pub fn document(&self) -> Value {
        match self.state.try_borrow() {
            Ok(st) => inspect::document(&st.session),
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
