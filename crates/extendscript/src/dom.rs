//! The objects a script reaches: `app`, the document, its layers and page items, `RGBColor` and
//! `alert`. Every property reads the engine's current document and every change is an engine
//! command, so a script's edits go through undo, the journal and the guard like the user's own.
//!
//! The names and shapes follow the public scripting reference of the app this layer stands in
//! for (type names such as `GroupItem`, `PathItem`, `TextFrame`; collections with `length`). What
//! a script observes beyond those documented names is whatever the engine does.

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string,
    object::{
        FunctionObjectBuilder, ObjectInitializer,
        builtins::{JsArray, JsFunction},
    },
    property::Attribute,
};
use boa_gc::{Finalize, Trace};
use serde_json::{Value, json};

use crate::host::Host;

type Getter = fn(&Cap, &mut Context) -> JsResult<JsValue>;
type Setter = fn(&Cap, &JsValue, &mut Context) -> JsResult<()>;

/// What a property's native function needs: the session and the node it belongs to (0 for the
/// document itself).
#[derive(Clone, Trace, Finalize)]
struct Cap {
    host: Host,
    #[unsafe_ignore_trace]
    id: u64,
}

#[derive(Clone, Trace, Finalize)]
struct GetCap {
    cap: Cap,
    #[unsafe_ignore_trace]
    f: Getter,
}

#[derive(Clone, Trace, Finalize)]
struct SetCap {
    cap: Cap,
    #[unsafe_ignore_trace]
    f: Setter,
}

fn error(msg: impl Into<String>) -> JsError {
    JsNativeError::error().with_message(msg.into()).into()
}

fn getter(cap: &Cap, f: Getter, ctx: &Context) -> JsFunction {
    NativeFunction::from_copy_closure_with_captures(|_this, _args, c: &GetCap, ctx| (c.f)(&c.cap, ctx), GetCap { cap: cap.clone(), f })
        .to_js_function(ctx.realm())
}

fn setter(cap: &Cap, f: Setter, ctx: &Context) -> JsFunction {
    NativeFunction::from_copy_closure_with_captures(
        |_this, args, c: &SetCap, ctx| {
            let v = args.first().cloned().unwrap_or_default();
            (c.f)(&c.cap, &v, ctx)?;
            Ok(JsValue::undefined())
        },
        SetCap { cap: cap.clone(), f },
    )
    .to_js_function(ctx.realm())
}

fn text(s: &str) -> JsValue {
    JsValue::from(JsString::from(s))
}

/// Define the globals.
pub(crate) fn install(host: &Host, ctx: &mut Context) -> JsResult<()> {
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

    let rgb = FunctionObjectBuilder::new(ctx.realm(), NativeFunction::from_fn_ptr(|_this, _args, ctx| Ok(color_object(ctx, 0.0, 0.0, 0.0).into())))
        .name(js_string!("RGBColor"))
        .length(0)
        .constructor(true)
        .build();
    ctx.register_global_property(js_string!("RGBColor"), rgb, Attribute::all())?;

    // `app.userInteractionLevel = UserInteractionLevel.DONTDISPLAYALERTS` is how scripts quiet
    // dialogs; a script run from here shows none, so the values only have to exist.
    let levels = ObjectInitializer::new(ctx)
        .property(js_string!("DISPLAYALERTS"), 1, Attribute::all())
        .property(js_string!("DONTDISPLAYALERTS"), 2, Attribute::all())
        .property(js_string!("NEVERINTERACT"), 3, Attribute::all())
        .build();
    ctx.register_global_property(js_string!("UserInteractionLevel"), levels, Attribute::all())?;

    let cap = Cap { host: host.clone(), id: 0 };
    let documents = getter(&cap, app_documents, ctx);
    let active = getter(&cap, app_active_document, ctx);
    let redraw = NativeFunction::from_fn_ptr(|_this, _args, _ctx| Ok(JsValue::undefined()));
    let app = ObjectInitializer::new(ctx)
        .accessor(js_string!("documents"), Some(documents), None, Attribute::all())
        .accessor(js_string!("activeDocument"), Some(active), None, Attribute::all())
        .property(js_string!("userInteractionLevel"), 1, Attribute::all())
        .function(redraw, js_string!("redraw"), 0)
        .build();
    ctx.register_global_property(js_string!("app"), app, Attribute::all())
}

fn color_object(ctx: &mut Context, r: f64, g: f64, b: f64) -> JsObject {
    ObjectInitializer::new(ctx)
        .property(js_string!("typename"), js_string!("RGBColor"), Attribute::all())
        .property(js_string!("red"), r, Attribute::all())
        .property(js_string!("green"), g, Attribute::all())
        .property(js_string!("blue"), b, Attribute::all())
        .build()
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

fn document_object(host: &Host, ctx: &mut Context) -> JsObject {
    let cap = Cap { host: host.clone(), id: 0 };
    let name = getter(&cap, document_name, ctx);
    let layers = getter(&cap, document_layers, ctx);
    let items = getter(&cap, document_page_items, ctx);
    let selection = getter(&cap, document_selection, ctx);
    ObjectInitializer::new(ctx)
        .property(js_string!("typename"), js_string!("Document"), Attribute::all())
        .accessor(js_string!("name"), Some(name), None, Attribute::all())
        .accessor(js_string!("layers"), Some(layers), None, Attribute::all())
        .accessor(js_string!("pageItems"), Some(items), None, Attribute::all())
        .accessor(js_string!("selection"), Some(selection), None, Attribute::all())
        .build()
}

fn document_name(cap: &Cap, _ctx: &mut Context) -> JsResult<JsValue> {
    Ok(text(cap.host.document().get("title").and_then(Value::as_str).unwrap_or_default()))
}

/// The document's top-level layers, topmost first.
fn document_layers(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let doc = cap.host.document();
    let list = doc.get("layers").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    collection(&cap.host, list.iter().collect(), ctx)
}

/// The page items on the document's layers (the layers' own top-level items).
fn document_page_items(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let doc = cap.host.document();
    let layers = doc.get("layers").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let items: Vec<&Value> = layers.iter().flat_map(|l| children(l).into_iter().filter(|c| !is_layer(c))).collect();
    collection(&cap.host, items, ctx)
}

fn document_selection(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let doc = cap.host.document();
    let ids: Vec<u64> = doc.get("selection").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    let found: Vec<&Value> = ids.iter().filter_map(|id| find_node(&doc, *id)).collect();
    collection(&cap.host, found, ctx)
}

fn is_layer(n: &Value) -> bool {
    n.get("kind").and_then(Value::as_str) == Some("Layer")
}

fn children(n: &Value) -> Vec<&Value> {
    n.get("children").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default()
}

/// The node with this id anywhere in the document summary.
fn find_node(doc: &Value, id: u64) -> Option<&Value> {
    fn walk(list: &[Value], id: u64) -> Option<&Value> {
        list.iter().find_map(|n| {
            if n.get("id").and_then(Value::as_u64) == Some(id) {
                return Some(n);
            }
            n.get("children").and_then(Value::as_array).and_then(|c| walk(c, id))
        })
    }
    walk(doc.get("layers").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default(), id)
}

/// A collection: an array of node objects that also answers `getByName`.
fn collection(host: &Host, nodes: Vec<&Value>, ctx: &mut Context) -> JsResult<JsValue> {
    let objects: Vec<JsValue> = nodes.into_iter().map(|n| node_object(host, n, ctx).into()).collect();
    let array = JsArray::from_iter(objects, ctx);
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
    let array: JsObject = array.into();
    array.set(js_string!("getByName"), by_name.to_js_function(ctx.realm()), false, ctx)?;
    Ok(array.into())
}

/// The scripting type name of an engine node kind.
fn typename(kind: &str) -> &'static str {
    match kind {
        "Layer" => "Layer",
        "Group" | "Clip Group" => "GroupItem",
        "Compound Path" => "CompoundPathItem",
        "Type" => "TextFrame",
        "Image" => "RasterItem",
        "Symbol" => "SymbolItem",
        "Placed Document" => "PlacedItem",
        "Mesh" => "MeshItem",
        "Path" | "Guide" | "Rectangle" | "Ellipse" | "Polygon" | "Star" | "Rounded Rectangle" | "Line" => "PathItem",
        _ => "PageItem",
    }
}

fn node_object(host: &Host, node: &Value, ctx: &mut Context) -> JsObject {
    let id = node.get("id").and_then(Value::as_u64).unwrap_or(0);
    let kind = node.get("kind").and_then(Value::as_str).unwrap_or("");
    let cap = Cap { host: host.clone(), id };
    let name = (getter(&cap, node_name, ctx), setter(&cap, set_node_name, ctx));
    let visible = (getter(&cap, node_visible, ctx), setter(&cap, set_node_visible, ctx));
    let hidden = (getter(&cap, node_hidden, ctx), setter(&cap, set_node_hidden, ctx));
    let locked = (getter(&cap, node_locked, ctx), setter(&cap, set_node_locked, ctx));
    let layers = getter(&cap, node_layers, ctx);
    let items = getter(&cap, node_page_items, ctx);
    let fill = (getter(&cap, node_fill, ctx), setter(&cap, set_node_fill, ctx));
    let stroked = (getter(&cap, node_stroked, ctx), setter(&cap, set_node_stroked, ctx));
    ObjectInitializer::new(ctx)
        .property(js_string!("typename"), JsString::from(typename(kind)), Attribute::all())
        .accessor(js_string!("name"), Some(name.0), Some(name.1), Attribute::all())
        .accessor(js_string!("visible"), Some(visible.0), Some(visible.1), Attribute::all())
        .accessor(js_string!("hidden"), Some(hidden.0), Some(hidden.1), Attribute::all())
        .accessor(js_string!("locked"), Some(locked.0), Some(locked.1), Attribute::all())
        .accessor(js_string!("layers"), Some(layers), None, Attribute::all())
        .accessor(js_string!("pageItems"), Some(items), None, Attribute::all())
        .accessor(js_string!("fillColor"), Some(fill.0), Some(fill.1), Attribute::all())
        .accessor(js_string!("stroked"), Some(stroked.0), Some(stroked.1), Attribute::all())
        .build()
}

/// The node's current summary, or an error when it no longer exists.
fn current(cap: &Cap) -> JsResult<Value> {
    let doc = cap.host.document();
    find_node(&doc, cap.id).cloned().ok_or_else(|| error("the object no longer exists"))
}

fn exec(cap: &Cap, command: &str, params: Value) -> JsResult<()> {
    cap.host.exec(command, &params).map(|_| ()).map_err(error)
}

fn node_name(cap: &Cap, _ctx: &mut Context) -> JsResult<JsValue> {
    Ok(text(current(cap)?.get("name").and_then(Value::as_str).unwrap_or_default()))
}

fn set_node_name(cap: &Cap, v: &JsValue, ctx: &mut Context) -> JsResult<()> {
    let name = v.to_string(ctx)?.to_std_string_escaped();
    exec(cap, "object.setProps", json!({"id": cap.id, "name": name}))
}

fn flag(cap: &Cap, key: &str) -> JsResult<bool> {
    Ok(current(cap)?.get(key).and_then(Value::as_bool).unwrap_or(false))
}

fn node_visible(cap: &Cap, _ctx: &mut Context) -> JsResult<JsValue> {
    Ok(flag(cap, "visible")?.into())
}

fn set_node_visible(cap: &Cap, v: &JsValue, _ctx: &mut Context) -> JsResult<()> {
    exec(cap, "object.setProps", json!({"id": cap.id, "visible": v.to_boolean()}))
}

/// `hidden` is the page-item spelling of `visible`, inverted.
fn node_hidden(cap: &Cap, _ctx: &mut Context) -> JsResult<JsValue> {
    Ok((!flag(cap, "visible")?).into())
}

fn set_node_hidden(cap: &Cap, v: &JsValue, _ctx: &mut Context) -> JsResult<()> {
    exec(cap, "object.setProps", json!({"id": cap.id, "visible": !v.to_boolean()}))
}

fn node_locked(cap: &Cap, _ctx: &mut Context) -> JsResult<JsValue> {
    Ok(flag(cap, "locked")?.into())
}

fn set_node_locked(cap: &Cap, v: &JsValue, _ctx: &mut Context) -> JsResult<()> {
    exec(cap, "object.setProps", json!({"id": cap.id, "locked": v.to_boolean()}))
}

/// The sublayers of a layer.
fn node_layers(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let node = current(cap)?;
    collection(&cap.host, children(&node).into_iter().filter(|c| is_layer(c)).collect(), ctx)
}

/// The items directly inside a layer or group.
fn node_page_items(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let node = current(cap)?;
    collection(&cap.host, children(&node).into_iter().filter(|c| !is_layer(c)).collect(), ctx)
}

fn node_fill(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let node = current(cap)?;
    let label = node.get("fill").and_then(Value::as_str).unwrap_or("None");
    let hex = label.strip_prefix('#').filter(|h| h.len() >= 6);
    let channel = |i: usize| hex.and_then(|h| h.get(i..i + 2)).and_then(|p| u8::from_str_radix(p, 16).ok()).map(f64::from);
    match (channel(0), channel(2), channel(4)) {
        (Some(r), Some(g), Some(b)) => Ok(color_object(ctx, r, g, b).into()),
        _ => Ok(ObjectInitializer::new(ctx).property(js_string!("typename"), js_string!("NoColor"), Attribute::all()).build().into()),
    }
}

fn set_node_fill(cap: &Cap, v: &JsValue, ctx: &mut Context) -> JsResult<()> {
    let Some(obj) = v.as_object() else { return Err(error("fillColor takes a color")) };
    let kind = obj.get(js_string!("typename"), ctx)?.to_string(ctx)?.to_std_string_escaped();
    match kind.as_str() {
        "NoColor" => exec(cap, "paint.setFill", json!({"ids": [cap.id], "none": true})),
        "RGBColor" => {
            let mut channel = |key: JsString| -> JsResult<u8> {
                let n = obj.get(key, ctx)?.to_number(ctx)?;
                Ok(if n.is_finite() { n.round().clamp(0.0, 255.0) as u8 } else { 0 })
            };
            let (r, g, b) = (channel(js_string!("red"))?, channel(js_string!("green"))?, channel(js_string!("blue"))?);
            exec(cap, "paint.setFill", json!({"ids": [cap.id], "color": format!("#{r:02x}{g:02x}{b:02x}")}))
        }
        other => Err(error(format!("fillColor: {other} is not supported yet"))),
    }
}

fn node_stroked(cap: &Cap, _ctx: &mut Context) -> JsResult<JsValue> {
    let node = current(cap)?;
    Ok((node.get("stroke").and_then(Value::as_str).unwrap_or("None") != "None").into())
}

/// Only switching a stroke off can be done without knowing which colour a new stroke takes.
fn set_node_stroked(cap: &Cap, v: &JsValue, _ctx: &mut Context) -> JsResult<()> {
    if v.to_boolean() {
        return Ok(());
    }
    exec(cap, "paint.setStroke", json!({"ids": [cap.id], "none": true}))
}
