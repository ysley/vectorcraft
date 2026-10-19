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
    /// Hidden by the script (`visible = false`).
    pub hidden: bool,
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
    /// `alignChildren` of a container (`center`, `fill`, `left`…), `alignment` of a control in its parent.
    pub align_children: String,
    pub alignment: String,
    /// The gap between a container's children the script asked for, in points.
    pub spacing: Option<f64>,
    /// An edit field with `properties.multiline`: several lines, scrolling.
    pub multiline: bool,
    /// The size the script asked for (`preferredSize`, else `size`), in points; zero is not set.
    pub size: [f64; 2],
    /// What the widget's `onDraw` painted, in its own coordinates.
    pub shapes: Vec<Shape>,
    pub children: Vec<Widget>,
}

/// One painted shape of an `onDraw`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shape {
    /// `ellipse`, `rect`, `poly` or `text`.
    pub kind: String,
    /// `[x, y, width, height]` of an ellipse or rectangle; `[x, y, 0, 0]` where text starts.
    pub rect: [f64; 4],
    pub points: Vec<[f64; 2]>,
    pub text: String,
    /// Red, green, blue and alpha, each 0 to 1.
    pub fill: Option<[f32; 4]>,
    pub stroke: Option<[f32; 4]>,
    pub width: f64,
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
    let size = size_of(o, ctx)?;
    let shapes = draw(o, size, ctx);
    Ok(Widget {
        size,
        shapes,
        hidden: !bool_prop(o, js_string!("visible"), true, ctx)?,
        kind,
        text: string_prop(o, js_string!("text"), ctx)?,
        value: !numeric && bool_prop(o, js_string!("value"), false, ctx)?,
        number: if numeric { number_prop(o, js_string!("value"), ctx)? } else { 0.0 },
        selection,
        items,
        enabled: bool_prop(o, js_string!("enabled"), true, ctx)?,
        name: string_prop(o, js_string!("__name"), ctx)?,
        orientation: string_prop(o, js_string!("orientation"), ctx)?,
        align_children: string_prop(o, js_string!("alignChildren"), ctx)?,
        alignment: string_prop(o, js_string!("alignment"), ctx)?,
        spacing: {
            let v = o.get(js_string!("spacing"), ctx)?;
            if v.is_undefined() || v.is_null() { None } else { v.to_number(ctx).ok().filter(|n| n.is_finite() && *n >= 0.0) }
        },
        multiline: match object_prop(o, js_string!("properties"), ctx)? {
            Some(p) => p.get(js_string!("multiline"), ctx)?.to_boolean(),
            None => false,
        },
        children,
    })
}

/// `preferredSize`, else `size`, as `[width, height]` (zero where not set).
fn size_of(o: &JsObject, ctx: &mut Context) -> JsResult<[f64; 2]> {
    for key in ["preferredSize", "size"] {
        if let Some(a) = object_prop(o, JsString::from(key), ctx)? {
            let w = a.get(0, ctx)?;
            let h = a.get(1, ctx)?;
            let (w, h) = (if w.is_undefined() { 0.0 } else { w.to_number(ctx)? }, if h.is_undefined() { 0.0 } else { h.to_number(ctx)? });
            if w.is_finite() && h.is_finite() && (w > 0.0 || h > 0.0) {
                return Ok([w, h]);
            }
        }
    }
    Ok([0.0, 0.0])
}

fn color_of(v: &JsValue, ctx: &mut Context) -> Option<[f32; 4]> {
    let a = v.as_object()?;
    let n = |i: u32, ctx: &mut Context, d: f64| -> f64 {
        a.get(i, ctx).ok().filter(|v| !v.is_undefined()).and_then(|v| v.to_number(ctx).ok()).filter(|v| v.is_finite()).unwrap_or(d)
    };
    Some([n(0, ctx, 0.0) as f32, n(1, ctx, 0.0) as f32, n(2, ctx, 0.0) as f32, n(3, ctx, 1.0) as f32])
}

/// Run the widget's `onDraw` against its recording `graphics` and read what it painted. A script
/// error in it leaves the widget undrawn, as a failing paint handler would.
fn draw(o: &JsObject, size: [f64; 2], ctx: &mut Context) -> Vec<Shape> {
    let Ok(v) = o.get(js_string!("onDraw"), ctx) else { return vec![] };
    let Some(f) = v.as_callable() else { return vec![] };
    let Ok(Some(g)) = object_prop(o, js_string!("graphics"), ctx) else { return vec![] };
    let Ok(fresh) = JsArray::new(ctx) else { return vec![] };
    let _ = g.set(js_string!("__shapes"), fresh, false, ctx);
    // A widget the script gave no size to is drawn at the size it asked to be.
    if let Some(sz) = object_prop(o, js_string!("size"), ctx).ok().flatten()
        && size[0] > 0.0
    {
        let _ = sz.set(0, size[0], false, ctx);
        let _ = sz.set(1, size[1], false, ctx);
    }
    if f.call(&JsValue::from(o.clone()), &[], ctx).is_err() {
        return vec![];
    }
    let Ok(list) = elements(&g, js_string!("__shapes"), ctx) else { return vec![] };
    let mut out = vec![];
    for item in list.into_iter().filter_map(|v| v.as_object()).take(2000) {
        let num = |k: &str, ctx: &mut Context| number_prop(&item, JsString::from(k), ctx).unwrap_or(0.0);
        let mut shape = Shape {
            kind: string_prop(&item, js_string!("kind"), ctx).unwrap_or_default(),
            rect: [num("x", ctx), num("y", ctx), num("w", ctx), num("h", ctx)],
            text: string_prop(&item, js_string!("text"), ctx).unwrap_or_default(),
            width: num("width", ctx),
            ..Shape::default()
        };
        if let Ok(v) = item.get(js_string!("fill"), ctx) {
            shape.fill = color_of(&v, ctx);
        }
        if let Ok(v) = item.get(js_string!("stroke"), ctx) {
            shape.stroke = color_of(&v, ctx);
        }
        if let Ok(pts) = elements(&item, js_string!("points"), ctx) {
            for p in pts.chunks(2).take(2000) {
                if let [x, y] = p {
                    shape.points.push([x.to_number(ctx).unwrap_or(0.0), y.to_number(ctx).unwrap_or(0.0)]);
                }
            }
        }
        out.push(shape);
    }
    out
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

/// A `[width, height]` that also answers to `.width` and `.height`, as sizes do in scripts.
fn dimension(ctx: &mut Context) -> JsResult<JsObject> {
    let a = JsArray::from_iter([JsValue::from(0), JsValue::from(0)], ctx);
    let o: JsObject = a.into();
    o.set(js_string!("width"), 0, false, ctx)?;
    o.set(js_string!("height"), 0, false, ctx)?;
    Ok(o)
}

fn font(name: &str, style: &str, size: f64, ctx: &mut Context) -> JsObject {
    ObjectInitializer::new(ctx)
        .property(js_string!("name"), JsString::from(name), Attribute::all())
        .property(js_string!("family"), JsString::from(name), Attribute::all())
        .property(js_string!("style"), JsString::from(style), Attribute::all())
        .property(js_string!("size"), size, Attribute::all())
        .build()
}

fn pair_of_consts(ctx: &mut Context, a: &str, b: &str) -> JsObject {
    ObjectInitializer::new(ctx).property(JsString::from(a), 0, Attribute::all()).property(JsString::from(b), 1, Attribute::all()).build()
}

fn num_arg(args: &[JsValue], i: usize, ctx: &mut Context) -> JsResult<f64> {
    let v = args.get(i).cloned().unwrap_or_default();
    let n = if v.is_undefined() { 0.0 } else { v.to_number(ctx)? };
    Ok(if n.is_finite() { n } else { 0.0 })
}

fn set_path(g: &JsValue, kind: &str, vals: [f64; 4], ctx: &mut Context) -> JsResult<JsValue> {
    let g = this_object(g)?;
    let points = JsArray::new(ctx)?;
    let path = ObjectInitializer::new(ctx)
        .property(js_string!("kind"), JsString::from(kind), Attribute::all())
        .property(js_string!("x"), vals[0], Attribute::all())
        .property(js_string!("y"), vals[1], Attribute::all())
        .property(js_string!("w"), vals[2], Attribute::all())
        .property(js_string!("h"), vals[3], Attribute::all())
        .property(js_string!("points"), points, Attribute::all())
        .build();
    g.set(js_string!("__path"), path, false, ctx)?;
    Ok(JsValue::undefined())
}

/// `fillPath(brush)` and `strokePath(pen)`: record the current path as painted.
fn paint_path(this: &JsValue, args: &[JsValue], stroke: bool, ctx: &mut Context) -> JsResult<JsValue> {
    let g = this_object(this)?;
    let Some(path) = object_prop(&g, js_string!("__path"), ctx)? else { return Ok(JsValue::undefined()) };
    let Some(tool) = args.first().and_then(JsValue::as_object) else { return Ok(JsValue::undefined()) };
    let shape = ObjectInitializer::new(ctx).build();
    for k in ["kind", "x", "y", "w", "h", "points"] {
        let v = path.get(JsString::from(k), ctx)?;
        shape.set(JsString::from(k), v, false, ctx)?;
    }
    let color = tool.get(js_string!("color"), ctx)?;
    shape.set(if stroke { js_string!("stroke") } else { js_string!("fill") }, color, false, ctx)?;
    if stroke {
        let w = tool.get(js_string!("lineWidth"), ctx)?;
        shape.set(js_string!("width"), w, false, ctx)?;
    }
    let list = object_prop(&g, js_string!("__shapes"), ctx)?;
    if let Some(list) = list {
        push(&list, shape.into(), ctx)?;
    }
    Ok(JsValue::undefined())
}

/// What a widget's `graphics` is while its `onDraw` runs: it records what is painted, for the
/// presenter to paint, and holds the widget's `font`.
fn make_graphics(ctx: &mut Context) -> JsResult<JsObject> {
    let brush_types = pair_of_consts(ctx, "SOLID_COLOR", "THEME_COLOR");
    let pen_types = pair_of_consts(ctx, "SOLID_COLOR", "THEME_COLOR");
    let default_font = font("dialog", "REGULAR", 12.0, ctx);
    let shapes = JsArray::new(ctx)?;
    let mut init = ObjectInitializer::new(ctx);
    init.property(js_string!("BrushType"), brush_types, Attribute::all());
    init.property(js_string!("PenType"), pen_types, Attribute::all());
    init.property(js_string!("font"), default_font, Attribute::all());
    init.property(js_string!("__shapes"), shapes, Attribute::all());
    let g = init.build();
    let methods: [(&str, usize, Native); 11] = [
        ("newBrush", 2, |_, a, ctx| {
            let color = a.get(1).cloned().unwrap_or_default();
            Ok(ObjectInitializer::new(ctx).property(js_string!("color"), color, Attribute::all()).build().into())
        }),
        ("newPen", 3, |_, a, ctx| {
            let color = a.get(1).cloned().unwrap_or_default();
            let width = num_arg(a, 2, ctx)?;
            Ok(ObjectInitializer::new(ctx)
                .property(js_string!("color"), color, Attribute::all())
                .property(js_string!("lineWidth"), width, Attribute::all())
                .build()
                .into())
        }),
        ("ellipsePath", 4, |t, a, ctx| {
            let v = [num_arg(a, 0, ctx)?, num_arg(a, 1, ctx)?, num_arg(a, 2, ctx)?, num_arg(a, 3, ctx)?];
            set_path(t, "ellipse", v, ctx)
        }),
        ("rectPath", 4, |t, a, ctx| {
            let v = [num_arg(a, 0, ctx)?, num_arg(a, 1, ctx)?, num_arg(a, 2, ctx)?, num_arg(a, 3, ctx)?];
            set_path(t, "rect", v, ctx)
        }),
        ("newPath", 0, |t, _, ctx| set_path(t, "poly", [0.0; 4], ctx)),
        ("moveTo", 2, |t, a, ctx| {
            let (x, y) = (num_arg(a, 0, ctx)?, num_arg(a, 1, ctx)?);
            point(t, x, y, ctx)
        }),
        ("lineTo", 2, |t, a, ctx| {
            let (x, y) = (num_arg(a, 0, ctx)?, num_arg(a, 1, ctx)?);
            point(t, x, y, ctx)
        }),
        ("closePath", 0, |_, _, _| Ok(JsValue::undefined())),
        ("fillPath", 1, |t, a, ctx| paint_path(t, a, false, ctx)),
        ("strokePath", 1, |t, a, ctx| paint_path(t, a, true, ctx)),
        ("measureString", 1, |_, a, ctx| {
            let n = a.first().cloned().unwrap_or_default().to_string(ctx)?.to_std_string_escaped().chars().count() as f64;
            Ok(ObjectInitializer::new(ctx)
                .property(js_string!("width"), n * 7.0, Attribute::all())
                .property(js_string!("height"), 14.0, Attribute::all())
                .build()
                .into())
        }),
    ];
    for (name, len, f) in methods {
        let func = NativeFunction::from_fn_ptr(f).to_js_function(ctx.realm());
        let _ = len;
        g.set(JsString::from(name), func, false, ctx)?;
    }
    for name in ["drawString", "drawOSControl", "drawImage"] {
        let nop = function(ctx, |_, _, _| Ok(JsValue::undefined()));
        g.set(JsString::from(name), nop, false, ctx)?;
    }
    Ok(g)
}

fn point(t: &JsValue, x: f64, y: f64, ctx: &mut Context) -> JsResult<JsValue> {
    let g = this_object(t)?;
    if let Some(path) = object_prop(&g, js_string!("__path"), ctx)?
        && let Some(points) = object_prop(&path, js_string!("points"), ctx)?
    {
        push(&points, x.into(), ctx)?;
        push(&points, y.into(), ctx)?;
    }
    Ok(JsValue::undefined())
}

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
    let (preferred, size) = (dimension(ctx)?, dimension(ctx)?);
    let graphics = make_graphics(ctx)?;
    let mut init = ObjectInitializer::new(ctx);
    init.property(js_string!("graphics"), graphics, Attribute::all());
    init.property(js_string!("preferredSize"), preferred, Attribute::all());
    init.property(js_string!("size"), size, Attribute::all());
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
    // The second argument is the bounds `[left, top, right, bottom]` or the size `[width, height]`.
    if let Some(b) = args.get(1).and_then(JsValue::as_object).filter(|b| b.is_array()) {
        let mut n = [0.0f64; 4];
        let len = b.get(js_string!("length"), ctx)?.to_length(ctx)?.min(4) as usize;
        for (i, slot) in n.iter_mut().enumerate().take(len) {
            let v = b.get(i as u32, ctx)?;
            *slot = if v.is_undefined() { 0.0 } else { v.to_number(ctx)? };
        }
        let size = match len {
            4 => Some([n[2] - n[0], n[3] - n[1]]),
            2 => Some([n[0], n[1]]),
            _ => None,
        };
        if let Some([w, h]) = size.filter(|[w, h]| w.is_finite() && h.is_finite() && (*w > 0.0 || *h > 0.0)) {
            widget.set(js_string!("size"), JsArray::from_iter([JsValue::from(w), JsValue::from(h)], ctx), false, ctx)?;
        }
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
    let new_font = NativeFunction::from_fn_ptr(|_, a, ctx| {
        let name = a.first().cloned().unwrap_or_default().to_string(ctx)?.to_std_string_escaped();
        let style = a.get(1).filter(|v| !v.is_undefined() && !v.is_null()).cloned().unwrap_or_default().to_string(ctx)?.to_std_string_escaped();
        let size = num_arg(a, 2, ctx)?;
        Ok(font(&name, &style, size, ctx).into())
    })
    .to_js_function(ctx.realm());
    let styles = ObjectInitializer::new(ctx)
        .property(js_string!("REGULAR"), js_string!("REGULAR"), Attribute::all())
        .property(js_string!("BOLD"), js_string!("BOLD"), Attribute::all())
        .property(js_string!("ITALIC"), js_string!("ITALIC"), Attribute::all())
        .property(js_string!("BOLDITALIC"), js_string!("BOLDITALIC"), Attribute::all())
        .build();
    let keyboard = ObjectInitializer::new(ctx)
        .property(js_string!("ctrlKey"), false, Attribute::all())
        .property(js_string!("shiftKey"), false, Attribute::all())
        .property(js_string!("altKey"), false, Attribute::all())
        .property(js_string!("metaKey"), false, Attribute::all())
        .build();
    let environment = ObjectInitializer::new(ctx).property(js_string!("keyboardState"), keyboard, Attribute::all()).build();
    let script_ui = ObjectInitializer::new(ctx)
        .property(js_string!("newFont"), new_font, Attribute::all())
        .property(js_string!("FontStyle"), styles, Attribute::all())
        .property(js_string!("environment"), environment, Attribute::all())
        .build();
    ctx.register_global_property(js_string!("ScriptUI"), script_ui, Attribute::all())?;
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

    #[test]
    fn on_draw_swatches_are_recorded_for_the_presenter() {
        struct Spy(std::rc::Rc<std::cell::RefCell<Vec<crate::Widget>>>);
        impl crate::Presenter for Spy {
            fn next_event(&mut self, d: &crate::Dialog) -> Result<crate::Event, String> {
                self.0.borrow_mut().extend(d.root.children.clone());
                Ok(crate::Event::Close(1))
            }
        }
        let src = r#"var w = new Window("dialog", "x");
            ScriptUI.newFont("dialog", "BOLD", 14);
            var sw = w.add("statictext", undefined, ""); sw.preferredSize = [22, 22];
            sw.graphics.font = ScriptUI.newFont(sw.graphics.font.name, "BOLD", 16);
            sw.onDraw = function () {
                var g = this.graphics; var W = this.size[0], r = W / 2 - 1;
                g.ellipsePath(W / 2 - r, W / 2 - r, r * 2, r * 2);
                g.fillPath(g.newBrush(g.BrushType.SOLID_COLOR, [1, 0.5, 0, 1]));
                g.strokePath(g.newPen(g.PenType.SOLID_COLOR, [0, 0, 0, 1], 2));
            };
            w.show();"#;
        let seen = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
        let mut s = Session::new();
        run_with(&mut s, src, Some(Box::new(Spy(seen.clone())))).unwrap();
        let widgets = seen.borrow();
        let shapes = &widgets[0].shapes;
        assert_eq!(shapes.len(), 2, "{shapes:?}");
        assert_eq!(shapes[0].kind, "ellipse");
        assert_eq!(shapes[0].fill, Some([1.0, 0.5, 0.0, 1.0]));
        assert_eq!(shapes[1].stroke, Some([0.0, 0.0, 0.0, 1.0]));
        assert_eq!(widgets[0].size, [22.0, 22.0]);
    }
}
