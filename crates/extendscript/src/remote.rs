//! A script run beside its document: the script runs on a thread of its own and every read,
//! command, alert and dialog goes over a channel to the thread that owns the document (the app's
//! UI thread), which answers them with [`Job::pump`] between frames. The document stays on
//! screen and edits as the script makes them, and a dialog waits for the user without holding the
//! interface up.

use std::{
    sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel},
    time::{Duration, Instant},
};

use serde_json::Value;
use vectorcraft_engine::Session;

use crate::{
    Outcome, ScriptError,
    host::{Backend, document_of, execute_on},
    ui::{Dialog, Event, Presenter},
};

enum ToUi {
    Exec(String, Value, Sender<Result<Value, String>>),
    Document(Sender<Value>),
    Alert(String, Sender<()>),
    Need(Box<Dialog>, Sender<Result<Event, String>>),
    Done(Box<Result<Outcome, ScriptError>>),
}

/// The script thread's end of the channel.
#[derive(Clone)]
struct Link {
    tx: Sender<ToUi>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
}

impl Link {
    fn send(&self, m: ToUi) -> Result<(), String> {
        self.tx.send(m).map_err(|_| "the script was stopped".to_string())?;
        (self.wake)();
        Ok(())
    }

    /// Send a request and wait for its answer.
    fn ask<T>(&self, make: impl FnOnce(Sender<T>) -> ToUi) -> Result<T, String> {
        let (tx, rx) = channel();
        self.send(make(tx))?;
        rx.recv().map_err(|_| "the script was stopped".to_string())
    }
}

impl Backend for Link {
    fn execute(&mut self, command: &str, params: &Value) -> Result<Value, String> {
        self.ask(|tx| ToUi::Exec(command.to_string(), params.clone(), tx))?
    }

    fn document(&mut self) -> Value {
        self.ask(ToUi::Document).unwrap_or(Value::Null)
    }

    fn alert(&mut self, text: &str) {
        // Stopped scripts have nobody to tell.
        let _ = self.ask(|tx| ToUi::Alert(text.to_string(), tx));
    }
}

impl Presenter for Link {
    fn next_event(&mut self, dialog: &Dialog) -> Result<Event, String> {
        self.ask(|tx| ToUi::Need(Box::new(dialog.clone()), tx))?
    }
}

/// A running script, seen from the thread that owns the document.
pub struct Job {
    rx: Receiver<ToUi>,
    dialog: Option<(Dialog, Sender<Result<Event, String>>)>,
    alert: Option<(String, Sender<()>)>,
    done: Option<Result<Outcome, ScriptError>>,
}

/// Start `source` on a thread of its own. `wake` is called whenever the script needs the owner's
/// attention (to repaint the interface).
pub fn spawn(source: String, wake: impl Fn() + Send + Sync + 'static) -> Result<Job, String> {
    let (tx, rx) = channel();
    let link = Link { tx, wake: std::sync::Arc::new(wake) };
    let thread_link = link.clone();
    // The engine recurses deeply on nested scripts: give the thread room.
    std::thread::Builder::new()
        .name("extendscript".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            let result = crate::run_remote(&source, Box::new(thread_link.clone()), Box::new(thread_link.clone()));
            // Whoever stopped listening is gone; the result has nowhere to go.
            let _ = thread_link.send(ToUi::Done(Box::new(result)));
        })
        .map_err(|e| format!("could not start the script: {e}"))?;
    Ok(Job { rx, dialog: None, alert: None, done: None })
}

impl Job {
    /// Answer what the script asks of the document for up to `budget`, stopping at once when it
    /// shows something to the user or ends. Call it every frame while the job is running.
    pub fn pump(&mut self, session: &mut Session, budget: Duration) {
        let start = Instant::now();
        while self.dialog.is_none() && self.alert.is_none() && self.done.is_none() && start.elapsed() < budget {
            match self.rx.recv_timeout(Duration::from_millis(2)) {
                Ok(ToUi::Exec(command, params, reply)) => {
                    let _ = reply.send(execute_on(session, &command, &params));
                }
                Ok(ToUi::Document(reply)) => {
                    let _ = reply.send(document_of(session));
                }
                Ok(ToUi::Alert(text, reply)) => self.alert = Some((text, reply)),
                Ok(ToUi::Need(dialog, reply)) => self.dialog = Some((*dialog, reply)),
                Ok(ToUi::Done(result)) => self.done = Some(*result),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    self.done = Some(Err(ScriptError::Internal("the script stopped unexpectedly".into())));
                }
            }
        }
    }

    /// The dialog waiting for the user.
    pub fn dialog(&self) -> Option<&Dialog> {
        self.dialog.as_ref().map(|(d, _)| d)
    }

    /// Give the script what the user did to the dialog.
    pub fn answer(&mut self, event: Event) {
        if let Some((_, reply)) = self.dialog.take() {
            let _ = reply.send(Ok(event));
        }
    }

    /// The `alert()` waiting to be read.
    pub fn alert(&self) -> Option<&str> {
        self.alert.as_ref().map(|(t, _)| t.as_str())
    }

    pub fn dismiss_alert(&mut self) {
        if let Some((_, reply)) = self.alert.take() {
            let _ = reply.send(());
        }
    }

    /// How the script ended, once it has (once).
    pub fn take_result(&mut self) -> Option<Result<Outcome, ScriptError>> {
        self.done.take()
    }

    pub fn is_finished(&self) -> bool {
        self.done.is_some()
    }

    /// Stop waiting: the script's next request fails and it ends with an error.
    pub fn cancel(&mut self) {
        if let Some((_, reply)) = self.dialog.take() {
            let _ = reply.send(Err("the dialog was cancelled".into()));
        }
        self.dismiss_alert();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn drive(job: &mut Job, session: &mut Session, mut on_dialog: impl FnMut(&Dialog) -> Event) -> Result<Outcome, ScriptError> {
        for _ in 0..2000 {
            job.pump(session, Duration::from_millis(20));
            if let Some(a) = job.alert() {
                let _ = a;
                job.dismiss_alert();
            }
            if let Some(d) = job.dialog().cloned() {
                job.answer(on_dialog(&d));
            }
            if let Some(r) = job.take_result() {
                return r;
            }
        }
        Err(ScriptError::Internal("the script never ended".into()))
    }

    #[test]
    fn a_script_edits_the_owners_document_and_waits_in_a_dialog() {
        let mut session = Session::new();
        session.execute("file.new", &json!({})).unwrap();
        let src = r#"
            var w = new Window("dialog", "Cor");
            var b = w.add("button", undefined, "Pintar");
            b.onClick = function () {
                var r = app.activeDocument.pathItems.rectangle(100, 0, 50, 20);
                r.name = "feito";
            };
            var code = w.show();
            alert("fim");
            code + "|" + app.activeDocument.pageItems.length;
        "#;
        let mut job = spawn(src.into(), || {}).unwrap();
        let mut clicked = false;
        let out = drive(&mut job, &mut session, |_| {
            if clicked {
                Event::Close(1)
            } else {
                clicked = true;
                Event::Click(vec![0])
            }
        })
        .unwrap();
        assert_eq!(out.value, "1|1");
        assert_eq!(out.alerts, vec!["fim".to_string()]);
        assert_eq!(inspect_names(&session), vec!["feito".to_string()]);
    }

    fn inspect_names(s: &Session) -> Vec<String> {
        document_of(s)["layers"].to_string().match_indices("\"name\":\"feito\"").map(|_| "feito".to_string()).collect()
    }

    #[test]
    fn cancelling_a_dialog_ends_the_script_with_an_error() {
        let mut session = Session::new();
        let mut job = spawn("new Window('dialog','x').show();".into(), || {}).unwrap();
        for _ in 0..500 {
            job.pump(&mut session, Duration::from_millis(20));
            if job.dialog().is_some() {
                job.cancel();
            }
            if let Some(r) = job.take_result() {
                assert!(matches!(r, Err(ScriptError::Script { .. })));
                return;
            }
        }
        panic!("the script never ended");
    }
}
