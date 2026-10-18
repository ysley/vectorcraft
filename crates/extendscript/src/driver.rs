//! Answers dialogs from a prepared list of steps, for runs with nobody at the screen (tests, the
//! command line). A step names a widget by its label or its `name` and says what to do to it.

use crate::ui::{Dialog, Event, Presenter, Widget};

/// One thing the scripted user does.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Click the button, or toggle the check box, with this label or name.
    Click(String),
    /// Type into the edit field with this label or name.
    Type(String, String),
    /// Choose the item with this text in the list with this label or name; the list is found by
    /// holding the item.
    Choose(String),
    /// Close the dialog with OK.
    Ok,
    /// Close the dialog with Cancel.
    Cancel,
}

/// Plays the steps in order; when they run out the dialog is cancelled.
#[derive(Debug, Default)]
pub struct Scripted {
    steps: std::collections::VecDeque<Step>,
    /// Every dialog's title, in the order they were shown.
    pub shown: Vec<String>,
}

impl Scripted {
    pub fn new(steps: impl IntoIterator<Item = Step>) -> Self {
        Self { steps: steps.into_iter().collect(), shown: vec![] }
    }
}

fn find(w: &Widget, path: &mut Vec<usize>, pred: &dyn Fn(&Widget) -> bool) -> bool {
    if !path.is_empty() && pred(w) {
        return true;
    }
    for (i, c) in w.children.iter().enumerate() {
        path.push(i);
        if find(c, path, pred) {
            return true;
        }
        path.pop();
    }
    false
}

impl Presenter for Scripted {
    fn open(&mut self, dialog: &Dialog) {
        self.shown.push(dialog.title.clone());
    }

    fn next_event(&mut self, dialog: &Dialog) -> Result<Event, String> {
        let Some(step) = self.steps.pop_front() else { return Ok(Event::Close(2)) };
        let mut path = vec![];
        let named = |label: &str| {
            let label = label.to_string();
            move |w: &Widget| w.enabled && (w.text == label || (!w.name.is_empty() && w.name == label))
        };
        match step {
            Step::Ok => Ok(Event::Close(1)),
            Step::Cancel => Ok(Event::Close(2)),
            Step::Click(label) => {
                if find(&dialog.root, &mut path, &named(&label)) {
                    Ok(Event::Click(path))
                } else {
                    Err(format!("no enabled control \"{label}\" in the dialog \"{}\"", dialog.title))
                }
            }
            Step::Type(label, text) => {
                if find(&dialog.root, &mut path, &|w| w.kind == "edittext" && named(&label)(w)) {
                    Ok(Event::SetText(path, text))
                } else {
                    Err(format!("no edit field \"{label}\" in the dialog \"{}\"", dialog.title))
                }
            }
            Step::Choose(item) => {
                if find(&dialog.root, &mut path, &|w| w.enabled && w.items.contains(&item)) {
                    let at = dialog_widget(&dialog.root, &path).and_then(|w| w.items.iter().position(|i| *i == item));
                    Ok(Event::Select(path, at))
                } else {
                    Err(format!("no list with the item \"{item}\" in the dialog \"{}\"", dialog.title))
                }
            }
        }
    }
}

fn dialog_widget<'a>(root: &'a Widget, path: &[usize]) -> Option<&'a Widget> {
    path.iter().try_fold(root, |w, &i| w.children.get(i))
}
