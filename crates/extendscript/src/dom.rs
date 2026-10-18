//! The objects a script reaches: `app`, the document and its collections, and the shared
//! plumbing the other object modules build on. Every property reads the engine's current document
//! and every change is an engine command, so a script's edits go through undo, the journal and
//! the guard like the user's own.
//!
//! The names and shapes follow the public scripting reference of the app this layer stands in
//! for (type names such as `GroupItem`, `PathItem`, `TextFrame`; collections with `length`). What
//! a script observes beyond those documented names is whatever the engine does.

use std::{cell::Cell, rc::Rc};

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string,
    object::{
        FunctionObjectBuilder, ObjectInitializer,
        builtins::{JsArray, JsFunction},
    },
    property::Attribute,
    realm::Realm,
};
use boa_gc::{Finalize, Trace};
use serde_json::{Value, json};

use crate::{geom::Origin, host::Host, node};

pub(crate) type Getter = fn(&Cap, &mut Context) -> JsResult<JsValue>;
pub(crate) type Setter = fn(&Cap, &JsValue, &mut Context) -> JsResult<()>;
pub(crate) type Method = fn(&Cap, &[JsValue], &mut Context) -> JsResult<JsValue>;

/// What a property's native function needs: the session and the object it belongs to. An object
/// made before it has content (`pathItems.add()`) has no id until its first content arrives.
#[derive(Clone, Trace, Finalize)]
pub(crate) struct Cap {
    pub host: Host,
    #[unsafe_ignore_trace]
    id: Rc<Cell<Option<u64>>>,
    /// Which of several related properties a shared function serves.
    #[unsafe_ignore_trace]
    pub tag: u8,
    /// State of an object that is not a document item (an open file), shared by its functions.
    #[unsafe_ignore_trace]
    pub extra: Option<Rc<dyn std::any::Any>>,
}

impl Cap {
    pub fn new(host: &Host, id: Option<u64>) -> Self {
        Self { host: host.clone(), id: Rc::new(Cell::new(id)), tag: 0, extra: None }
    }

    pub fn with_tag(&self, tag: u8) -> Self {
        Self { host: self.host.clone(), id: Rc::clone(&self.id), tag, extra: self.extra.clone() }
    }

    /// A capture carrying `state` for the object's functions.
    pub fn with_state(host: &Host, state: Rc<dyn std::any::Any>) -> Self {
        Self { host: host.clone(), id: Rc::new(Cell::new(None)), tag: 0, extra: Some(state) }
    }

    pub fn state<T: 'static>(&self) -> JsResult<&T> {
        self.extra.as_deref().and_then(|e| e.downcast_ref::<T>()).ok_or_else(|| error("not that kind of object"))
    }

    pub fn id(&self) -> Option<u64> {
        self.id.get()
    }

    /// The id, for an object that exists in the document.
    pub fn need(&self) -> JsResult<u64> {
        self.id.get().ok_or_else(|| error("the object has no content yet"))
    }

    pub fn bind(&self, id: u64) {
        self.id.set(Some(id));
    }
}

#[derive(Clone, Trace, Finalize)]
struct FnCap<F: 'static + Copy> {
    cap: Cap,
    #[unsafe_ignore_trace]
    f: F,
}

pub(crate) fn error(msg: impl Into<String>) -> JsError {
    JsNativeError::error().with_message(msg.into()).into()
}

pub(crate) fn text(s: &str) -> JsValue {
    JsValue::from(JsString::from(s))
}

/// Run an engine command for a script.
pub(crate) fn exec(cap: &Cap, command: &str, params: Value) -> JsResult<Value> {
    cap.host.exec(command, &params).map_err(error)
}

fn getter_fn(realm: &Realm, cap: &Cap, f: Getter) -> JsFunction {
    NativeFunction::from_copy_closure_with_captures(|_this, _args, c: &FnCap<Getter>, ctx| (c.f)(&c.cap, ctx), FnCap { cap: cap.clone(), f })
        .to_js_function(realm)
}

fn setter_fn(realm: &Realm, cap: &Cap, f: Setter) -> JsFunction {
    NativeFunction::from_copy_closure_with_captures(
        |_this, args, c: &FnCap<Setter>, ctx| {
            let v = args.first().cloned().unwrap_or_default();
            (c.f)(&c.cap, &v, ctx)?;
            Ok(JsValue::undefined())
        },
        FnCap { cap: cap.clone(), f },
    )
    .to_js_function(realm)
}

fn method_fn(realm: &Realm, cap: &Cap, f: Method) -> JsFunction {
    NativeFunction::from_copy_closure_with_captures(|_this, args, c: &FnCap<Method>, ctx| (c.f)(&c.cap, args, ctx), FnCap { cap: cap.clone(), f })
        .to_js_function(realm)
}

/// Builds a script object property by property.
pub(crate) struct Obj<'a> {
    init: ObjectInitializer<'a>,
    realm: Realm,
    /// What the native functions work on; an object built without it holds plain values only.
    cap: Option<Cap>,
}

impl<'a> Obj<'a> {
    pub fn new(ctx: &'a mut Context, cap: &Cap) -> Self {
        let realm = ctx.realm().clone();
        Self { init: ObjectInitializer::new(ctx), realm, cap: Some(cap.clone()) }
    }

    /// An object of plain values (a colour a script made itself).
    pub fn plain(ctx: &'a mut Context) -> Self {
        let realm = ctx.realm().clone();
        Self { init: ObjectInitializer::new(ctx), realm, cap: None }
    }

    pub fn value(mut self, name: &'static str, v: impl Into<JsValue>) -> Self {
        self.init.property(JsString::from(name), v, Attribute::all());
        self
    }

    pub fn get(mut self, name: &'static str, g: Getter) -> Self {
        let Some(cap) = &self.cap else { return self };
        let g = getter_fn(&self.realm, cap, g);
        self.init.accessor(JsString::from(name), Some(g), None, Attribute::all());
        self
    }

    pub fn get_set(self, name: &'static str, g: Getter, s: Setter) -> Self {
        self.get_set_tag(name, 0, g, s)
    }

    /// A read-write property whose functions are told `tag`.
    pub fn get_set_tag(mut self, name: &'static str, tag: u8, g: Getter, s: Setter) -> Self {
        let Some(cap) = &self.cap else { return self };
        let cap = cap.with_tag(tag);
        let g = getter_fn(&self.realm, &cap, g);
        let s = setter_fn(&self.realm, &cap, s);
        self.init.accessor(JsString::from(name), Some(g), Some(s), Attribute::all());
        self
    }

    pub fn set_only(mut self, name: &'static str, s: Setter) -> Self {
        let Some(cap) = &self.cap else { return self };
        let s = setter_fn(&self.realm, cap, s);
        self.init.accessor(JsString::from(name), None, Some(s), Attribute::all());
        self
    }

    pub fn method(mut self, name: &'static str, _length: usize, f: Method) -> Self {
        let Some(cap) = &self.cap else { return self };
        let m = method_fn(&self.realm, cap, f);
        self.init.property(JsString::from(name), m, Attribute::all());
        self
    }

    pub fn build(mut self) -> JsObject {
        self.init.build()
    }
}

/// A number read from a script value; NaN and infinities are errors.
pub(crate) fn number(v: &JsValue, ctx: &mut Context) -> JsResult<f64> {
    let n = v.to_number(ctx)?;
    if n.is_finite() { Ok(n) } else { Err(error("a number was expected")) }
}

/// The `i`th number of an array-like script value.
pub(crate) fn number_at(v: &JsValue, i: u32, ctx: &mut Context) -> JsResult<f64> {
    let Some(o) = v.as_object() else { return Err(error("an array was expected")) };
    let item = o.get(i, ctx)?;
    number(&item, ctx)
}

pub(crate) fn array_of_numbers(values: &[f64], ctx: &mut Context) -> JsValue {
    JsArray::from_iter(values.iter().map(|v| JsValue::from(*v)), ctx).into()
}

// ----------------------------------------------------------------------------------------------
// Globals
// ----------------------------------------------------------------------------------------------

/// Define the globals.
pub(crate) fn install(host: &Host, ctx: &mut Context) -> JsResult<()> {
    install_dollar(host, ctx)?;
    crate::ui::install(host, ctx)?;
    crate::fs::install(host, ctx)?;
    let alert = NativeFunction::from_copy_closure_with_captures(
        |_this, args, host: &Host, ctx| {
            let msg = match args.first() {
                Some(v) => v.to_string(ctx)?.to_std_string_escaped(),
                None => String::new(),
            };
            host.alert(msg);
            Ok(JsValue::undefined())
        },
        host.clone(),
    );
    ctx.register_global_callable(js_string!("alert"), 1, alert)?;

    let rgb = FunctionObjectBuilder::new(ctx.realm(), NativeFunction::from_fn_ptr(|_this, _args, ctx| Ok(node::plain_rgb(ctx, 0, 0, 0).into())))
        .name(js_string!("RGBColor"))
        .length(0)
        .constructor(true)
        .build();
    ctx.register_global_property(js_string!("RGBColor"), rgb, Attribute::all())?;

    // The values only have to exist and differ: a script run from here shows no dialogs, and the
    // enumerations are read by name.
    for (name, members) in [
        ("UserInteractionLevel", &[("DISPLAYALERTS", "1"), ("DONTDISPLAYALERTS", "2"), ("NEVERINTERACT", "3")][..]),
        ("Justification", &[("LEFT", "left"), ("CENTER", "center"), ("RIGHT", "right"), ("FULLJUSTIFY", "justifyAll")][..]),
        (
            "ZOrderMethod",
            &[("BRINGTOFRONT", "bringToFront"), ("BRINGFORWARD", "bringForward"), ("SENDBACKWARD", "sendBackward"), ("SENDTOBACK", "sendToBack")][..],
        ),
    ] {
        let mut init = ObjectInitializer::new(ctx);
        for (key, value) in members {
            init.property(JsString::from(*key), JsString::from(*value), Attribute::all());
        }
        let object = init.build();
        ctx.register_global_property(JsString::from(name), object, Attribute::all())?;
    }

    let cap = Cap::new(host, None);
    let app = Obj::new(ctx, &cap)
        .get("documents", app_documents)
        .get("activeDocument", app_active_document)
        .value("userInteractionLevel", 1)
        .method("redraw", 0, |_cap, _args, _ctx| Ok(JsValue::undefined()))
        .build();
    ctx.register_global_property(js_string!("app"), app, Attribute::all())
}

fn app_documents(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let items: Vec<JsValue> = if cap.host.document().is_null() { vec![] } else { vec![document_object(&cap.host, ctx).into()] };
    Ok(JsArray::from_iter(items, ctx).into())
}

fn app_active_document(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    if cap.host.document().is_null() {
        return Err(error("there is no document open"));
    }
    Ok(document_object(&cap.host, ctx).into())
}

// ----------------------------------------------------------------------------------------------
// The document
// ----------------------------------------------------------------------------------------------

pub(crate) fn document_object(host: &Host, ctx: &mut Context) -> JsObject {
    let cap = Cap::new(host, None);
    Obj::new(ctx, &cap)
        .value("typename", js_string!("Document"))
        .get("name", |cap, _ctx| Ok(text(cap.host.document().get("title").and_then(Value::as_str).unwrap_or_default())))
        .get("layers", document_layers)
        .get("pageItems", document_page_items)
        .get("pathItems", document_path_items)
        .get("textFrames", document_text_frames)
        .get("artboards", document_artboards)
        .get("selection", document_selection)
        .build()
}

pub(crate) fn is_layer(n: &Value) -> bool {
    n.get("kind").and_then(Value::as_str) == Some("Layer")
}

pub(crate) fn children(n: &Value) -> Vec<&Value> {
    n.get("children").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default()
}

fn layer_list(doc: &Value) -> &[Value] {
    doc.get("layers").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default()
}

/// The node with this id anywhere in the document summary.
pub(crate) fn find_node(doc: &Value, id: u64) -> Option<&Value> {
    fn walk(list: &[Value], id: u64) -> Option<&Value> {
        list.iter().find_map(|n| {
            if n.get("id").and_then(Value::as_u64) == Some(id) {
                return Some(n);
            }
            n.get("children").and_then(Value::as_array).and_then(|c| walk(c, id))
        })
    }
    walk(layer_list(doc), id)
}

/// Where the node with this id sits: `Some(None)` for a top-level layer (its parent is the
/// document), `Some(Some(parent))` for anything else, `None` when there is no such node.
pub(crate) fn find_parent(doc: &Value, id: u64) -> Option<Option<&Value>> {
    fn walk<'a>(list: &'a [Value], parent: Option<&'a Value>, id: u64) -> Option<Option<&'a Value>> {
        for n in list {
            if n.get("id").and_then(Value::as_u64) == Some(id) {
                return Some(parent);
            }
            if let Some(found) = n.get("children").and_then(Value::as_array).and_then(|c| walk(c, Some(n), id)) {
                return Some(found);
            }
        }
        None
    }
    walk(layer_list(doc), None, id)
}

/// Every node below the layers, in document order.
fn all_nodes(doc: &Value) -> Vec<&Value> {
    fn walk<'a>(list: &'a [Value], out: &mut Vec<&'a Value>) {
        for n in list {
            out.push(n);
            if let Some(c) = n.get("children").and_then(Value::as_array) {
                walk(c, out);
            }
        }
    }
    let mut out = vec![];
    walk(layer_list(doc), &mut out);
    out
}

/// A collection: an array of objects that also answers `getByName`.
/// A plain array of script objects.
pub(crate) fn collection_plain(objects: Vec<JsObject>, ctx: &mut Context) -> JsResult<JsArray> {
    Ok(JsArray::from_iter(objects.into_iter().map(JsValue::from), ctx))
}

pub(crate) fn collection(objects: Vec<JsObject>, ctx: &mut Context) -> JsResult<JsObject> {
    let array: JsObject = JsArray::from_iter(objects.into_iter().map(JsValue::from), ctx).into();
    let by_name = NativeFunction::from_fn_ptr(|this, args, ctx| {
        let wanted = args.first().cloned().unwrap_or_default().to_string(ctx)?;
        let Some(list) = this.as_object() else { return Ok(JsValue::null()) };
        let len = list.get(js_string!("length"), ctx)?.to_length(ctx)?;
        for i in 0..len {
            let item = list.get(i, ctx)?;
            if let Some(o) = item.as_object()
                && o.get(js_string!("name"), ctx)?.to_string(ctx)? == wanted
            {
                return Ok(item);
            }
        }
        Err(error(format!("no item named \"{}\"", wanted.to_std_string_escaped())))
    });
    array.set(js_string!("getByName"), by_name.to_js_function(ctx.realm()), false, ctx)?;
    Ok(array)
}

fn add_method(array: &JsObject, name: &'static str, cap: &Cap, f: Method, ctx: &mut Context) -> JsResult<()> {
    let m = method_fn(ctx.realm(), cap, f);
    array.set(JsString::from(name), m, false, ctx)?;
    Ok(())
}

/// The scripting objects for these nodes.
fn node_objects(host: &Host, nodes: Vec<&Value>, ctx: &mut Context) -> Vec<JsObject> {
    nodes.into_iter().map(|n| node::node_object(host, n, ctx)).collect()
}

/// The top-level layers, topmost first.
fn document_layers(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let doc = cap.host.document();
    let objects = node_objects(&cap.host, layer_list(&doc).iter().collect(), ctx);
    Ok(collection(objects, ctx)?.into())
}

/// The items directly on the document's layers.
fn document_page_items(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let doc = cap.host.document();
    let items: Vec<&Value> = layer_list(&doc).iter().flat_map(|l| children(l).into_iter().filter(|c| !is_layer(c))).collect();
    let objects = node_objects(&cap.host, items, ctx);
    Ok(collection(objects, ctx)?.into())
}

fn document_selection(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let doc = cap.host.document();
    let ids: Vec<u64> = doc.get("selection").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    let found: Vec<&Value> = ids.iter().filter_map(|id| find_node(&doc, *id)).collect();
    let objects = node_objects(&cap.host, found, ctx);
    Ok(collection(objects, ctx)?.into())
}

/// Every path in the document, with `add()` and `rectangle()`.
fn document_path_items(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let doc = cap.host.document();
    let paths: Vec<&Value> = all_nodes(&doc).into_iter().filter(|n| node::is_path(n)).collect();
    let objects = node_objects(&cap.host, paths, ctx);
    let list = collection(objects, ctx)?;
    add_method(&list, "add", cap, |cap, _args, ctx| Ok(node::pending(&cap.host, "Path", ctx).into()), ctx)?;
    add_method(&list, "rectangle", cap, path_rectangle, ctx)?;
    Ok(list.into())
}

/// `rectangle(top, left, width, height)`
fn path_rectangle(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let arg = |i: usize, ctx: &mut Context| number(args.get(i).unwrap_or(&JsValue::undefined()), ctx);
    let (top, left, width, height) = (arg(0, ctx)?, arg(1, ctx)?, arg(2, ctx)?, arg(3, ctx)?);
    let origin = Origin::of(&cap.host.document());
    let (x, y) = origin.to_engine(left, top);
    let made = exec(cap, "shape.rectangle", json!({"x": x, "y": y, "width": width, "height": height}))?;
    let id = made.get("id").and_then(Value::as_u64).ok_or_else(|| error("the rectangle was not created"))?;
    node::by_id(&cap.host, id, ctx)
}

/// Every text frame in the document, with `add()`.
fn document_text_frames(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let doc = cap.host.document();
    let frames: Vec<&Value> = all_nodes(&doc).into_iter().filter(|n| n.get("kind").and_then(Value::as_str) == Some("Type")).collect();
    let objects = node_objects(&cap.host, frames, ctx);
    let list = collection(objects, ctx)?;
    add_method(&list, "add", cap, |cap, _args, ctx| Ok(node::pending(&cap.host, "Type", ctx).into()), ctx)?;
    Ok(list.into())
}

// ----------------------------------------------------------------------------------------------
// Artboards
// ----------------------------------------------------------------------------------------------

fn document_artboards(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let count = cap.host.document().get("artboards").and_then(Value::as_array).map_or(0, Vec::len);
    let objects = (0..count).map(|i| artboard_object(&cap.host, i as u64, ctx)).collect();
    let list = collection(objects, ctx)?;
    add_method(&list, "add", cap, artboards_add, ctx)?;
    Ok(list.into())
}

/// `add([left, top, right, bottom])`
fn artboards_add(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let rect = args.first().cloned().unwrap_or_default();
    let (left, top, right, bottom) = (number_at(&rect, 0, ctx)?, number_at(&rect, 1, ctx)?, number_at(&rect, 2, ctx)?, number_at(&rect, 3, ctx)?);
    let origin = Origin::of(&cap.host.document());
    let (x, y) = origin.to_engine(left, top);
    exec(cap, "artboard.new", json!({"x": x, "y": y, "width": right - left, "height": top - bottom}))?;
    let count = cap.host.document().get("artboards").and_then(Value::as_array).map_or(0, Vec::len);
    let index = count.saturating_sub(1) as u64;
    Ok(artboard_object(&cap.host, index, ctx).into())
}

fn artboard_object(host: &Host, index: u64, ctx: &mut Context) -> JsObject {
    let cap = Cap::new(host, Some(index));
    Obj::new(ctx, &cap)
        .value("typename", js_string!("Artboard"))
        .get_set("name", artboard_name, set_artboard_name)
        .get_set("artboardRect", artboard_rect, set_artboard_rect)
        .build()
}

fn artboard_json(cap: &Cap) -> JsResult<Value> {
    let index = usize::try_from(cap.need()?).map_err(|_| error("no such artboard"))?;
    cap.host
        .document()
        .get("artboards")
        .and_then(Value::as_array)
        .and_then(|a| a.get(index))
        .cloned()
        .ok_or_else(|| error("the artboard no longer exists"))
}

fn artboard_name(cap: &Cap, _ctx: &mut Context) -> JsResult<JsValue> {
    Ok(text(artboard_json(cap)?.get("name").and_then(Value::as_str).unwrap_or_default()))
}

fn set_artboard_name(cap: &Cap, v: &JsValue, ctx: &mut Context) -> JsResult<()> {
    let name = v.to_string(ctx)?.to_std_string_escaped();
    exec(cap, "artboard.setProps", json!({"index": cap.need()?, "name": name})).map(|_| ())
}

fn artboard_rect(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let origin = Origin::of(&cap.host.document());
    let rect = origin.rect_to_script(&artboard_json(cap)?).unwrap_or([0.0; 4]);
    Ok(array_of_numbers(&rect, ctx))
}

fn set_artboard_rect(cap: &Cap, v: &JsValue, ctx: &mut Context) -> JsResult<()> {
    let (left, top, right, bottom) = (number_at(v, 0, ctx)?, number_at(v, 1, ctx)?, number_at(v, 2, ctx)?, number_at(v, 3, ctx)?);
    let origin = Origin::of(&cap.host.document());
    let (x, y) = origin.to_engine(left, top);
    exec(cap, "artboard.setProps", json!({"index": cap.need()?, "x": x, "y": y, "width": right - left, "height": top - bottom})).map(|_| ())
}

/// `$`, the scripting environment's own object: `writeln` and `write` print to a console that
/// is not shown, `sleep` waits, the rest describe the host.
fn install_dollar(host: &Host, ctx: &mut Context) -> JsResult<()> {
    let ignore = |_: &JsValue, _: &[JsValue], _: &mut Context| Ok(JsValue::undefined());
    let sleep = NativeFunction::from_fn_ptr(|_, args, ctx| {
        // Scripts sleep to let the interface catch up, which nothing here needs; cap what a
        // runaway value could do.
        let ms = args.first().map(|v| v.to_number(ctx)).transpose()?.unwrap_or(0.0);
        if ms.is_finite() && ms > 0.0 {
            std::thread::sleep(std::time::Duration::from_millis(ms.min(50.0) as u64));
        }
        Ok(JsValue::undefined())
    });
    let dollar = ObjectInitializer::new(ctx)
        .function(NativeFunction::from_fn_ptr(ignore), js_string!("writeln"), 1)
        .function(NativeFunction::from_fn_ptr(ignore), js_string!("write"), 1)
        .function(NativeFunction::from_fn_ptr(ignore), js_string!("gc"), 0)
        .function(sleep, js_string!("sleep"), 1)
        .function(NativeFunction::from_fn_ptr(crate::fs::eval_file), js_string!("evalFile"), 1)
        .property(js_string!("os"), JsString::from(std::env::consts::OS), Attribute::all())
        .property(js_string!("version"), js_string!("VectorCraft"), Attribute::all())
        .property(js_string!("locale"), js_string!("pt_BR"), Attribute::all())
        .property(js_string!("level"), 0, Attribute::all())
        .property(js_string!("fileName"), JsString::from(host.file_name().as_str()), Attribute::all())
        .build();
    ctx.register_global_property(js_string!("$"), dollar, Attribute::all())
}
