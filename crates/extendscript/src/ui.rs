//! Dialogs: the `Window` a script builds out of panels, groups, labels, lists, check boxes and
//! buttons, and what happens while it is shown.
//!
//! A widget is a plain script object that keeps its state in ordinary properties (`text`,
//! `value`, `items`, `children`, and `selection` as an accessor over the list), so scripts read
//! and write it as they would any object. `show()` turns the tree into a [`Dialog`] snapshot,
//! hands it to the [`Presenter`] and applies what comes back (a click, a choice, typed text, a
//! close) to the script's objects, calling `onClick` and `onChange` as the user would trigger
//! them. After each event the tree is snapshotted again, since a handler may have changed it. The
//! presenter is the interface: the desktop app draws the dialog; headless runs answer from a
//! script of steps ([`crate::driver`]).
//!
//! Layout properties (`orientation`, `alignChildren`, `spacing`, `margins`, sizes) are stored but
//! only the presenter reads them.

use boa_engine::{
    Context, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string,
    object::{
        FunctionObjectBuilder, ObjectInitializer,
        builtins::{JsArray, JsFunction},
    },
    property::Attribute,
};

use crate::{dom::error, host::Host};

/// What a presenter shows: the dialog's title and its widget tree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dialog {
    pub title: String,
    pub root: Widget,
}

/// One widget as it is when the dialog is snapshotted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Widget {
    /// `window`, `panel`, `group`, `statictext`, `edittext`, `checkbox`, `radiobutton`, `button`,
    /// `dropdownlist`, `listbox`, `slider` or `progressbar`.
    pub kind: String,
    /// The label, or an edit field's content.
    pub text: String,
    /// A check box's or radio button's state.
    pub value: bool,
    /// A slider's or progress bar's value.
    pub number: f64,
    /// The chosen item of a list.
    pub selection: Option<usize>,
    /// The texts of a list's items.
    pub items: Vec<String>,
    pub enabled: bool,
    /// `ok` or `cancel` on a button that closes the dialog by itself.
    pub name: String,
    pub orientation: String,
    pub children: Vec<Widget>,
}

/// What the user did. A path is the child indices from the dialog's root.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Click(Vec<usize>),
    SetText(Vec<usize>, String),
    SetValue(Vec<usize>, bool),
    Select(Vec<usize>, Option<usize>),
    /// Close the dialog; `show()` returns the code (1 for OK, 2 for Cancel).
    Close(i32),
}

/// Whoever answers a dialog.
pub trait Presenter {
    /// A dialog is about to be shown.
    fn open(&mut self, _dialog: &Dialog) {}
    /// The next thing the user does, given the dialog as it is now. An error stops the script.
    fn next_event(&mut self, dialog: &Dialog) -> Result<Event, String>;
}

// ----------------------------------------------------------------------------------------------
// Reading script objects
// ----------------------------------------------------------------------------------------------

fn string_prop(o: &JsObject, key: JsString, ctx: &mut Context) -> JsResult<String> {
    let v = o.get(key, ctx)?;
    if v.is_undefined() || v.is_null() { Ok(String::new()) } else { Ok(v.to_string(ctx)?.to_std_string_escaped()) }
}

fn bool_prop(o: &JsObject, key: JsString, default: bool, ctx: &mut Context) -> JsResult<bool> {
    let v = o.get(key, ctx)?;
    Ok(if v.is_undefined() { default } else { v.to_boolean() })
}

fn number_prop(o: &JsObject, key: JsString, ctx: &mut Context) -> JsResult<f64> {
    let v = o.get(key, ctx)?;
    if v.is_undefined() || v.is_null() {
        return Ok(0.0);
    }
    let n = v.to_number(ctx)?;
    Ok(if n.is_finite() { n } else { 0.0 })
}

fn object_prop(o: &JsObject, key: JsString, ctx: &mut Context) -> JsResult<Option<JsObject>> {
    Ok(o.get(key, ctx)?.as_object())
}

/// The elements of an array-like property.
fn elements(o: &JsObject, key: JsString, ctx: &mut Context) -> JsResult<Vec<JsValue>> {
    let Some(list) = object_prop(o, key, ctx)? else { return Ok(vec![]) };
    let len = list.get(js_string!("length"), ctx)?.to_length(ctx)?;
    // A dialog with more widgets than this is a runaway loop in the script.
    if len > 100_000 {
        return Err(error("too many items in a dialog"));
    }
    (0..len).map(|i| list.get(i, ctx)).collect()
}

fn push(list: &JsObject, v: JsValue, ctx: &mut Context) -> JsResult<()> {
    JsArray::from_object(list.clone())?.push(v, ctx)?;
    Ok(())
}

fn snapshot_widget(o: &JsObject, depth: usize, ctx: &mut Context) -> JsResult<Widget> {
    if depth > 64 {
        return Err(error("the dialog is nested too deeply"));
    }
    let kind = string_prop(o, js_string!("type"), ctx)?;
    let numeric = matches!(kind.as_str(), "slider" | "progressbar");
    let mut items = vec![];
    for item in elements(o, js_string!("items"), ctx)? {
        items.push(match item.as_object() {
            Some(i) => string_prop(&i, js_string!("text"), ctx)?,
            None => item.to_string(ctx)?.to_std_string_escaped(),
        });
    }
    let selection = {
        let n = number_prop(o, js_string!("__sel"), ctx)?;
        let has = !o.get(js_string!("__sel"), ctx)?.is_undefined();
        if has && n >= 0.0 && (n as usize) < items.len() { Some(n as usize) } else { None }
    };
    let mut children = vec![];
    for c in elements(o, js_string!("children"), ctx)? {
        if let Some(c) = c.as_object() {
            children.push(snapshot_widget(&c, depth + 1, ctx)?);
        }
    }
    Ok(Widget {
        kind,
        text: string_prop(o, js_string!("text"), ctx)?,
        value: !numeric && bool_prop(o, js_string!("value"), false, ctx)?,
        number: if numeric { number_prop(o, js_string!("value"), ctx)? } else { 0.0 },
        selection,
        items,
        enabled: bool_prop(o, js_string!("enabled"), true, ctx)?,
        name: string_prop(o, js_string!("__name"), ctx)?,
        orientation: string_prop(o, js_string!("orientation"), ctx)?,
        children,
    })
}

fn widget_at(root: &JsObject, path: &[usize], ctx: &mut Context) -> JsResult<JsObject> {
    let mut cur = root.clone();
    for &i in path {
        let kids = elements(&cur, js_string!("children"), ctx)?;
        cur = kids.get(i).and_then(JsValue::as_object).ok_or_else(|| error("the dialog changed under the answer"))?;
    }
    Ok(cur)
}

/// Call `onClick`, `onChange`… of a widget, if the script gave it one.
fn fire(widget: &JsObject, key: JsString, ctx: &mut Context) -> JsResult<()> {
    let handler = widget.get(key, ctx)?;
    if let Some(f) = handler.as_callable() {
        f.call(&JsValue::from(widget.clone()), &[], ctx)?;
    }
    Ok(())
}

// ----------------------------------------------------------------------------------------------
// Making widgets
// ----------------------------------------------------------------------------------------------

type Native = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

fn function(ctx: &Context, f: Native) -> JsFunction {
    NativeFunction::from_fn_ptr(f).to_js_function(ctx.realm())
}

const CONTAINERS: &[&str] = &["window", "panel", "group"];
const LISTS: &[&str] = &["dropdownlist", "listbox"];
const LEAVES: &[&str] = &["statictext", "edittext", "checkbox", "radiobutton", "button", "slider", "progressbar"];

fn this_object(this: &JsValue) -> JsResult<JsObject> {
    this.as_object().ok_or_else(|| error("not a dialog object"))
}

fn make_widget(kind: &str, text: Option<String>, ctx: &mut Context) -> JsResult<JsObject> {
    let add = function(ctx, container_add);
    let remove = function(ctx, container_remove);
    let remove_all = function(ctx, container_remove_all);
    let list_add = function(ctx, list_add_item);
    let list_remove_all = function(ctx, list_remove_all_items);
    let list_find = function(ctx, list_find_item);
    let selection_get = function(ctx, selection_get);
    let selection_set = function(ctx, selection_set);
    let children = JsArray::new(ctx)?;
    let items = JsArray::new(ctx)?;
    let mut init = ObjectInitializer::new(ctx);
    init.property(js_string!("type"), JsString::from(kind), Attribute::all());
    init.property(js_string!("text"), JsString::from(text.unwrap_or_default().as_str()), Attribute::all());
    init.property(js_string!("enabled"), true, Attribute::all());
    init.property(js_string!("visible"), true, Attribute::all());
    if CONTAINERS.contains(&kind) {
        init.property(js_string!("children"), children, Attribute::all());
        init.property(js_string!("orientation"), js_string!("column"), Attribute::all());
        init.property(js_string!("add"), add, Attribute::all());
        init.property(js_string!("remove"), remove, Attribute::all());
        init.property(js_string!("removeAll"), remove_all, Attribute::all());
    } else if LISTS.contains(&kind) {
        init.property(js_string!("items"), items, Attribute::all());
        init.property(js_string!("add"), list_add, Attribute::all());
        init.property(js_string!("removeAll"), list_remove_all, Attribute::all());
        init.property(js_string!("find"), list_find, Attribute::all());
        init.accessor(js_string!("selection"), Some(selection_get), Some(selection_set), Attribute::all());
    } else if matches!(kind, "checkbox" | "radiobutton") {
        init.property(js_string!("value"), false, Attribute::all());
    } else if matches!(kind, "slider" | "progressbar") {
        init.property(js_string!("value"), 0, Attribute::all());
        init.property(js_string!("minvalue"), 0, Attribute::all());
        init.property(js_string!("maxvalue"), 100, Attribute::all());
    }
    Ok(init.build())
}

/// `add(type, bounds, text, properties)` on a window, panel or group.
fn container_add(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let me = this_object(this)?;
    let kind = args.first().cloned().unwrap_or_default().to_string(ctx)?.to_std_string_escaped().to_lowercase();
    if !CONTAINERS.contains(&kind.as_str()) && !LISTS.contains(&kind.as_str()) && !LEAVES.contains(&kind.as_str()) {
        return Err(error(format!("add(\"{kind}\"): this kind of control is not supported yet")));
    }
    if kind == "window" {
        return Err(error("a window cannot be added to another"));
    }
    // The third argument is the label, or the items of a list.
    let third = args.get(2).cloned().unwrap_or_default();
    let is_list = LISTS.contains(&kind.as_str());
    let label = if third.is_undefined() || third.is_null() || (is_list && third.is_object()) {
        None
    } else {
        Some(third.to_string(ctx)?.to_std_string_escaped())
    };
    let widget = make_widget(&kind, label, ctx)?;
    if is_list && let Some(list) = third.as_object() {
        let items = object_prop(&widget, js_string!("items"), ctx)?.ok_or_else(|| error("a list has no items"))?;
        let len = list.get(js_string!("length"), ctx)?.to_length(ctx)?.min(100_000);
        for i in 0..len {
            let text = list.get(i, ctx)?;
            push(&items, JsValue::from(new_item(&text.to_string(ctx)?.to_std_string_escaped(), i, ctx)), ctx)?;
        }
    }
    if let Some(props) = args.get(3).and_then(JsValue::as_object) {
        let name = props.get(js_string!("name"), ctx)?;
        if !name.is_undefined() {
            widget.set(js_string!("__name"), JsString::from(name.to_string(ctx)?.to_std_string_escaped().to_lowercase().as_str()), false, ctx)?;
        }
        widget.set(js_string!("properties"), props, false, ctx)?;
    }
    widget.set(js_string!("parent"), me.clone(), false, ctx)?;
    let children = object_prop(&me, js_string!("children"), ctx)?.ok_or_else(|| error("this control cannot hold others"))?;
    push(&children, widget.clone().into(), ctx)?;
    Ok(widget.into())
}

fn container_remove(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let me = this_object(this)?;
    let Some(target) = args.first().and_then(JsValue::as_object) else { return Ok(JsValue::undefined()) };
    let kept: Vec<JsValue> =
        elements(&me, js_string!("children"), ctx)?.into_iter().filter(|c| c.as_object().is_none_or(|o| !JsObject::equals(&o, &target))).collect();
    me.set(js_string!("children"), JsArray::from_iter(kept, ctx), false, ctx)?;
    Ok(JsValue::undefined())
}

fn container_remove_all(this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    this_object(this)?.set(js_string!("children"), JsArray::new(ctx)?, false, ctx)?;
    Ok(JsValue::undefined())
}

fn new_item(text: &str, index: u64, ctx: &mut Context) -> JsObject {
    ObjectInitializer::new(ctx)
        .property(js_string!("type"), js_string!("item"), Attribute::all())
        .property(js_string!("text"), JsString::from(text), Attribute::all())
        .property(js_string!("index"), index as f64, Attribute::all())
        .build()
}

/// `add("item", text)` on a list.
fn list_add_item(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let me = this_object(this)?;
    let kind = args.first().cloned().unwrap_or_default().to_string(ctx)?.to_std_string_escaped();
    if kind != "item" {
        return Err(error("a list takes items: add(\"item\", text)"));
    }
    let text = args.get(1).cloned().unwrap_or_default().to_string(ctx)?.to_std_string_escaped();
    let items = object_prop(&me, js_string!("items"), ctx)?.ok_or_else(|| error("a list has no items"))?;
    let index = items.get(js_string!("length"), ctx)?.to_length(ctx)?;
    let item = new_item(&text, index, ctx);
    push(&items, item.clone().into(), ctx)?;
    Ok(item.into())
}

fn list_remove_all_items(this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let me = this_object(this)?;
    me.set(js_string!("items"), JsArray::new(ctx)?, false, ctx)?;
    me.delete_property_or_throw(js_string!("__sel"), ctx)?;
    Ok(JsValue::undefined())
}

fn list_find_item(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let me = this_object(this)?;
    let wanted = args.first().cloned().unwrap_or_default().to_string(ctx)?;
    for item in elements(&me, js_string!("items"), ctx)? {
        if let Some(o) = item.as_object()
            && o.get(js_string!("text"), ctx)?.to_string(ctx)? == wanted
        {
            return Ok(item);
        }
    }
    Ok(JsValue::null())
}

/// `selection`: the chosen item (`null` for none).
fn selection_get(this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let me = this_object(this)?;
    if me.get(js_string!("__sel"), ctx)?.is_undefined() {
        return Ok(JsValue::null());
    }
    let n = number_prop(&me, js_string!("__sel"), ctx)?;
    let items = elements(&me, js_string!("items"), ctx)?;
    Ok(if n >= 0.0 { items.get(n as usize).cloned().unwrap_or_else(JsValue::null) } else { JsValue::null() })
}

/// Choose by index, by item, or clear with `null`.
fn selection_set(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let me = this_object(this)?;
    let v = args.first().cloned().unwrap_or_default();
    let count = elements(&me, js_string!("items"), ctx)?.len();
    let index = if v.is_null() || v.is_undefined() {
        None
    } else if let Some(item) = v.as_object() {
        Some(number_prop(&item, js_string!("index"), ctx)?)
    } else {
        Some(v.to_number(ctx)?)
    };
    match index {
        Some(n) if n.is_finite() && n >= 0.0 && (n as usize) < count => me.set(js_string!("__sel"), n.trunc(), false, ctx)?,
        Some(_) => return Err(error("selection: no such item")),
        None => {
            me.delete_property_or_throw(js_string!("__sel"), ctx)?;
            true
        }
    };
    Ok(JsValue::undefined())
}

// ----------------------------------------------------------------------------------------------
// The window
// ----------------------------------------------------------------------------------------------

pub(crate) fn install(host: &Host, ctx: &mut Context) -> JsResult<()> {
    let make = NativeFunction::from_copy_closure_with_captures(
        |_this, args, host: &Host, ctx| {
            let kind = match args.first() {
                Some(v) if !v.is_undefined() => v.to_string(ctx)?.to_std_string_escaped().to_lowercase(),
                _ => "dialog".to_string(),
            };
            if !matches!(kind.as_str(), "dialog" | "palette" | "window") {
                return Err(error(format!("Window(\"{kind}\"): only dialog, palette and window are supported")));
            }
            let title = match args.get(1) {
                Some(v) if !v.is_undefined() => Some(v.to_string(ctx)?.to_std_string_escaped()),
                _ => None,
            };
            let window = make_widget("window", title, ctx)?;
            window.set(js_string!("windowType"), JsString::from(kind.as_str()), false, ctx)?;
            let show = NativeFunction::from_copy_closure_with_captures(|this, _args, host: &Host, ctx| show_window(this, host, ctx), host.clone())
                .to_js_function(ctx.realm());
            window.set(js_string!("show"), show, false, ctx)?;
            let close = function(ctx, close_window);
            window.set(js_string!("close"), close, false, ctx)?;
            for name in ["hide", "center"] {
                let nop = function(ctx, |_this, _args, _ctx| Ok(JsValue::undefined()));
                window.set(JsString::from(name), nop, false, ctx)?;
            }
            // `window.layout.layout(true)` and `.resize()` are how scripts ask for a layout pass;
            // the presenter lays the dialog out when it draws it.
            let layout = {
                let l = function(ctx, |_this, _args, _ctx| Ok(JsValue::undefined()));
                let r = function(ctx, |_this, _args, _ctx| Ok(JsValue::undefined()));
                ObjectInitializer::new(ctx)
                    .property(js_string!("layout"), l, Attribute::all())
                    .property(js_string!("resize"), r, Attribute::all())
                    .build()
            };
            window.set(js_string!("layout"), layout, false, ctx)?;
            Ok(window.into())
        },
        host.clone(),
    );
    let window = FunctionObjectBuilder::new(ctx.realm(), make).name(js_string!("Window")).length(2).constructor(true).build();
    ctx.register_global_property(js_string!("Window"), window, Attribute::all())
}

/// `close(code)`: ends `show()`, which returns the code.
fn close_window(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let me = this_object(this)?;
    let code = match args.first() {
        Some(v) if !v.is_undefined() => v.to_number(ctx)?,
        _ => 0.0,
    };
    me.set(js_string!("__closed"), if code.is_finite() { code.trunc() } else { 0.0 }, false, ctx)?;
    Ok(JsValue::undefined())
}

fn closed_code(window: &JsObject, ctx: &mut Context) -> JsResult<Option<i32>> {
    let v = window.get(js_string!("__closed"), ctx)?;
    if v.is_undefined() {
        return Ok(None);
    }
    Ok(Some(v.to_number(ctx)? as i32))
}

/// Show the dialog and answer it until it closes; the result is what `show()` returns.
fn show_window(this: &JsValue, host: &Host, ctx: &mut Context) -> JsResult<JsValue> {
    let window = this_object(this)?;
    // A palette does not hold the script up.
    if string_prop(&window, js_string!("windowType"), ctx)? == "palette" {
        return Ok(0.into());
    }
    window.delete_property_or_throw(js_string!("__closed"), ctx)?;
    host.open_dialog(&snapshot(&window, ctx)?);
    // Each event is one step of an interaction; a script that never closes its dialog is stopped.
    for _ in 0..10_000 {
        if let Some(code) = closed_code(&window, ctx)? {
            return Ok(code.into());
        }
        let dialog = snapshot(&window, ctx)?;
        let event = host.next_dialog_event(&dialog).map_err(error)?;
        apply(&window, event, ctx)?;
    }
    Err(error("the dialog was never closed"))
}

fn snapshot(window: &JsObject, ctx: &mut Context) -> JsResult<Dialog> {
    let root = snapshot_widget(window, 0, ctx)?;
    Ok(Dialog { title: root.text.clone(), root })
}

/// Do what the user did to the script's widgets, calling the handlers a user's action calls.
fn apply(window: &JsObject, event: Event, ctx: &mut Context) -> JsResult<()> {
    match event {
        Event::Close(code) => {
            window.set(js_string!("__closed"), f64::from(code), false, ctx)?;
        }
        Event::Click(path) => {
            let w = widget_at(window, &path, ctx)?;
            if !bool_prop(&w, js_string!("enabled"), true, ctx)? {
                return Ok(());
            }
            let kind = string_prop(&w, js_string!("type"), ctx)?;
            if kind == "checkbox" {
                let now = !bool_prop(&w, js_string!("value"), false, ctx)?;
                w.set(js_string!("value"), now, false, ctx)?;
            } else if kind == "radiobutton" {
                choose_radio(&w, ctx)?;
            }
            fire(&w, js_string!("onClick"), ctx)?;
            if kind == "button" {
                // A button named ok or cancel closes the dialog after its handler.
                match string_prop(&w, js_string!("__name"), ctx)?.as_str() {
                    "ok" => {
                        window.set(js_string!("__closed"), 1, false, ctx)?;
                    }
                    "cancel" => {
                        window.set(js_string!("__closed"), 2, false, ctx)?;
                    }
                    _ => {}
                }
            }
        }
        Event::SetValue(path, on) => {
            let w = widget_at(window, &path, ctx)?;
            if string_prop(&w, js_string!("type"), ctx)? == "radiobutton" && on {
                choose_radio(&w, ctx)?;
            } else {
                w.set(js_string!("value"), on, false, ctx)?;
            }
            fire(&w, js_string!("onClick"), ctx)?;
        }
        Event::Select(path, index) => {
            let w = widget_at(window, &path, ctx)?;
            match index {
                Some(i) => w.set(js_string!("__sel"), i as f64, false, ctx)?,
                None => {
                    w.delete_property_or_throw(js_string!("__sel"), ctx)?;
                    true
                }
            };
            fire(&w, js_string!("onChange"), ctx)?;
        }
        Event::SetText(path, text) => {
            let w = widget_at(window, &path, ctx)?;
            w.set(js_string!("text"), JsString::from(text.as_str()), false, ctx)?;
            fire(&w, js_string!("onChanging"), ctx)?;
            fire(&w, js_string!("onChange"), ctx)?;
        }
    }
    Ok(())
}

/// Turn a radio button on and the others of its group off.
fn choose_radio(w: &JsObject, ctx: &mut Context) -> JsResult<()> {
    if let Some(parent) = object_prop(w, js_string!("parent"), ctx)? {
        for sibling in elements(&parent, js_string!("children"), ctx)? {
            if let Some(s) = sibling.as_object()
                && string_prop(&s, js_string!("type"), ctx)? == "radiobutton"
            {
                s.set(js_string!("value"), false, false, ctx)?;
            }
        }
    }
    w.set(js_string!("value"), true, false, ctx)?;
    let _ = JsNativeError::error();
    Ok(())
}

#[cfg(test)]
mod tests {
    use vectorcraft_engine::Session;

    use crate::{
        driver::{Scripted, Step},
        run_with,
    };

    fn go(src: &str, steps: Vec<Step>) -> Result<String, String> {
        let mut s = Session::new();
        run_with(&mut s, src, Some(Box::new(Scripted::new(steps)))).map(|o| o.value).map_err(|e| e.to_string())
    }

    const DIALOG: &str = r#"
        var w = new Window("dialog", "Cor");
        var p = w.add("panel", undefined, "Modelo");
        var lista = p.add("dropdownlist", undefined, ["A", "B"]);
        var sub = p.add("dropdownlist");
        var chk = w.add("checkbox", undefined, "Invertido");
        var ok = w.add("button", undefined, "Aplicar", {name: "ok"});
        var log = [];
        lista.onChange = function () {
            sub.removeAll();
            var n = lista.selection.text;
            sub.add("item", n + "1");
            sub.add("item", n + "2");
            sub.selection = 0;
            log.push("lista=" + n);
        };
        sub.onChange = function () { log.push("sub=" + sub.selection.text); };
        chk.onClick = function () { log.push("chk=" + chk.value); };
        var r = w.show();
        r + "|" + log.join(",") + "|" + (lista.selection === null ? "-" : lista.selection.index) + "|" + sub.items.length;
    "#;

    #[test]
    fn a_dialog_runs_its_handlers_as_the_user_acts() {
        let out = go(DIALOG, vec![Step::Choose("B".into()), Step::Click("Invertido".into()), Step::Click("Aplicar".into())]).unwrap();
        assert_eq!(out, "1|lista=B,chk=true|1|2");
    }

    #[test]
    fn cancel_returns_2_and_an_unchosen_list_has_a_null_selection() {
        assert_eq!(go(DIALOG, vec![Step::Cancel]).unwrap(), "2||-|0");
    }

    #[test]
    fn close_from_a_handler_returns_its_code() {
        let src = r#"var w = new Window("dialog", "x"); var b = w.add("button", undefined, "Sair");
            b.onClick = function () { w.close(7); }; w.show();"#;
        assert_eq!(go(src, vec![Step::Click("Sair".into())]).unwrap(), "7");
    }

    #[test]
    fn edit_fields_and_radio_buttons() {
        let src = r#"var w = new Window("dialog", "x"); var e = w.add("edittext", undefined, "", {name: "campo"});
            var g = w.add("group"); var a = g.add("radiobutton", undefined, "um"); var b = g.add("radiobutton", undefined, "dois");
            a.value = true; w.add("button", undefined, "OK", {name: "ok"}); w.show();
            e.text + "|" + a.value + "|" + b.value;"#;
        let out = go(src, vec![Step::Type("campo".into(), "oi".into()), Step::Click("dois".into()), Step::Ok]);
        assert_eq!(out.unwrap(), "oi|false|true");
    }

    #[test]
    fn a_missing_control_or_presenter_is_an_error_not_a_hang() {
        assert!(go(DIALOG, vec![Step::Click("nada".into())]).is_err());
        let mut s = Session::new();
        let err = crate::run(&mut s, "new Window('dialog','x').show();").unwrap_err().to_string();
        assert!(err.contains("cannot show dialogs"), "{err}");
    }
}
