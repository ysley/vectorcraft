//! Layers, groups, paths and text frames as a script sees them, and the colours and text ranges
//! they hand out. See [`crate::dom`] for how a property reaches the engine.

use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};
use serde_json::{Value, json};

use crate::{
    dom::{Cap, Obj, array_of_numbers, children, collection, error, exec, find_node, find_parent, is_layer, number, number_at, text},
    geom::Origin,
    host::Host,
    nodeops,
};

/// Does this node summary describe a path?
pub(crate) fn is_path(n: &Value) -> bool {
    n.get("anchors").is_some()
}

/// The scripting type name of a node summary.
pub(crate) fn typename_of(n: &Value) -> &'static str {
    typename(n)
}

fn typename(n: &Value) -> &'static str {
    if is_path(n) {
        return "PathItem";
    }
    match n.get("kind").and_then(Value::as_str).unwrap_or("") {
        "Layer" => "Layer",
        "Group" | "Clip Group" => "GroupItem",
        "Compound Path" => "CompoundPathItem",
        "Type" => "TextFrame",
        "Image" => "RasterItem",
        "Symbol" => "SymbolItem",
        "Placed Document" => "PlacedItem",
        "Mesh" => "MeshItem",
        _ => "PageItem",
    }
}

/// `new RGBColor()`, and a colour that holds its channels itself.
pub(crate) fn plain_rgb(ctx: &mut Context, r: u8, g: u8, b: u8) -> JsObject {
    Obj::plain(ctx)
        .value("typename", js_string!("RGBColor"))
        .value("red", f64::from(r))
        .value("green", f64::from(g))
        .value("blue", f64::from(b))
        .build()
}

fn no_color(ctx: &mut Context) -> JsObject {
    Obj::plain(ctx).value("typename", js_string!("NoColor")).build()
}

/// The scripting object of an existing node of the active document.
pub(crate) fn node_object(host: &Host, node: &Value, ctx: &mut Context) -> JsObject {
    let id = node.get("id").and_then(Value::as_u64);
    build(Cap::new(host, id), typename(node), ctx)
}

/// The scripting object of the node with this id in the document `uid`, which is made the active
/// one to read it and left active.
pub(crate) fn object_in(host: &Host, uid: u64, id: u64, ctx: &mut Context) -> JsResult<JsValue> {
    let before = host.active_uid();
    host.activate(uid).map_err(error)?;
    let doc = host.document();
    let found = find_node(&doc, id).map(|n| build(Cap::of_doc(host, uid, Some(id)), typename(n), ctx));
    if let Some(b) = before {
        let _ = host.activate(b);
    }
    found.map(Into::into).ok_or_else(|| error("the new object cannot be found"))
}

/// An object that exists in the script before it has content: `pathItems.add()` and
/// `textFrames.add()`. The document gets it when its first content arrives (`setEntirePath`,
/// `contents`).
pub(crate) fn pending(host: &Host, kind: &str, ctx: &mut Context) -> JsObject {
    pending_in(host, kind, None, ctx)
}

/// The same inside a layer or group.
pub(crate) fn pending_in(host: &Host, kind: &str, container: Option<u64>, ctx: &mut Context) -> JsObject {
    let name = if kind == "Type" { "TextFrame" } else { "PathItem" };
    let mut cap = Cap::new(host, None);
    cap.extra = Some(std::rc::Rc::new(Container(container)));
    build(cap, name, ctx)
}

/// Where an object from `add()` goes once it has content.
pub(crate) struct Container(pub Option<u64>);

/// `groupItems.add()`.
pub(crate) fn pending_group(cap: &Cap, container: Option<u64>, ctx: &mut Context) -> JsObject {
    nodeops::pending_group(cap, container, ctx)
}

/// The object for a group with nothing in it yet: the same as any group, whose properties read
/// the document once the first thing is put in it (see [`Pending`](nodeops::Pending)).
pub(crate) fn build_pending_group(cap: Cap, ctx: &mut Context) -> JsObject {
    build(cap, "GroupItem", ctx)
}

fn pending_name(cap: &Cap, _ctx: &mut Context) -> JsResult<JsValue> {
    if cap.id().is_some() {
        return node_name(cap, _ctx);
    }
    let Ok(p) = cap.state::<nodeops::Pending>() else { return node_name(cap, _ctx) };
    Ok(text(p.name.borrow().as_deref().unwrap_or_default()))
}

fn set_pending_name(cap: &Cap, v: &JsValue, ctx: &mut Context) -> JsResult<()> {
    if cap.id().is_some() {
        return set_node_name(cap, v, ctx);
    }
    let Ok(p) = cap.state::<nodeops::Pending>() else { return set_node_name(cap, v, ctx) };
    *p.name.borrow_mut() = Some(v.to_string(ctx)?.to_std_string_escaped());
    Ok(())
}

pub(crate) fn by_id(host: &Host, id: u64, ctx: &mut Context) -> JsResult<JsValue> {
    let doc = host.document();
    let node = find_node(&doc, id).ok_or_else(|| error("the new object cannot be found"))?;
    Ok(node_object(host, node, ctx).into())
}

fn build(cap: Cap, typename: &'static str, ctx: &mut Context) -> JsObject {
    let container = matches!(typename, "Layer" | "GroupItem");
    let mut o = Obj::new(ctx, &cap)
        .value("typename", boa_engine::JsString::from(typename))
        .hidden_method("__id", |cap, _, _| Ok(cap.id().map_or(JsValue::null(), |i| JsValue::from(i as f64))))
        .hidden_method("__doc", |cap, _, _| Ok(cap.doc.map_or(JsValue::null(), |u| JsValue::from(u as f64))))
        .hidden_method("__pending", |cap, _, _| {
            Ok(cap.state::<nodeops::Pending>().map_or(JsValue::null(), |p| JsValue::from(f64::from(p.token.get()))))
        })
        .get_set("name", pending_name, set_pending_name)
        .get_set("visible", |cap, _| Ok(flag(cap, "visible")?.into()), |cap, v, _| set_flag(cap, "visible", v.to_boolean()))
        .get_set("hidden", |cap, _| Ok((!flag(cap, "visible")?).into()), |cap, v, _| set_flag(cap, "visible", !v.to_boolean()))
        .get_set("locked", |cap, _| Ok(flag(cap, "locked")?.into()), |cap, v, _| set_flag(cap, "locked", v.to_boolean()))
        .get_set("selected", |cap, _| Ok(nodeops::is_selected(cap)?.into()), |cap, v, _| nodeops::set_selected(cap, v.to_boolean()))
        .get_set(
            "opacity",
            |cap, _| Ok((current(cap)?.get("opacity").and_then(Value::as_f64).unwrap_or(1.0) * 100.0).into()),
            |cap, v, ctx| {
                let pct = number(v, ctx)?.clamp(0.0, 100.0);
                exec(cap, "object.setProps", json!({"id": cap.need()?, "opacity": pct})).map(|_| ())
            },
        )
        .get("layers", node_layers)
        .get("pageItems", node_page_items)
        .get("parent", node_parent)
        .get("layer", node_layer)
        .get("clipped", |cap, _| Ok((current(cap)?.get("kind").and_then(Value::as_str) == Some("Clip Group")).into()))
        .get("clipping", |cap, _| Ok(nodeops::is_clipping(&cap.host.document(), cap.need()?).into()))
        .get("guides", |cap, _| Ok(current(cap)?.get("guide").and_then(Value::as_bool).unwrap_or(false).into()))
        .get("geometricBounds", node_bounds)
        .get("visibleBounds", node_bounds)
        .get("controlBounds", node_bounds)
        .get("width", |cap, _| {
            let b = bounds_of(cap)?;
            Ok((b[2] - b[0]).into())
        })
        .get("height", |cap, _| {
            let b = bounds_of(cap)?;
            Ok((b[1] - b[3]).into())
        })
        .get_set(
            "position",
            |cap, ctx| {
                let b = bounds_of(cap)?;
                Ok(array_of_numbers(&[b[0], b[1]], ctx))
            },
            |cap, v, ctx| move_to(cap, Some(number_at(v, 0, ctx)?), Some(number_at(v, 1, ctx)?)),
        )
        .get_set("top", |cap, _| Ok(bounds_of(cap)?[1].into()), |cap, v, ctx| move_to(cap, None, Some(number(v, ctx)?)))
        .get_set("left", |cap, _| Ok(bounds_of(cap)?[0].into()), |cap, v, ctx| move_to(cap, Some(number(v, ctx)?), None))
        .method("translate", 2, translate)
        .method("rotate", 1, nodeops::rotate)
        .method("resize", 2, nodeops::resize)
        .method("move", 2, nodeops::move_to_target)
        .method("duplicate", 2, nodeops::duplicate)
        .method("zOrder", 1, z_order)
        .method("remove", 0, remove)
        .get_set("fillColor", |cap, ctx| live_color(cap, Slot::Fill, ctx), |cap, v, ctx| set_color(cap, Slot::Fill, v, ctx))
        .get_set("strokeColor", |cap, ctx| live_color(cap, Slot::Stroke, ctx), |cap, v, ctx| set_color(cap, Slot::Stroke, v, ctx))
        .get_set(
            "strokeWidth",
            |cap, _| Ok(current(cap)?.get("strokeWidth").and_then(Value::as_f64).unwrap_or(0.0).into()),
            |cap, v, ctx| {
                let weight = number(v, ctx)?.max(0.0);
                exec(cap, "stroke.set", json!({"ids": [cap.need()?], "weight": weight})).map(|_| ())
            },
        )
        .get_set("filled", |cap, _| Ok(has_paint(cap, Slot::Fill)?.into()), |cap, v, _| set_painted(cap, Slot::Fill, v.to_boolean()))
        .get_set("stroked", |cap, _| Ok(has_paint(cap, Slot::Stroke)?.into()), |cap, v, _| set_painted(cap, Slot::Stroke, v.to_boolean()))
        // The engine keeps no overprint switches a script could flip here: read as off, accepted.
        .get_set("overprintFill", |_, _| Ok(false.into()), |_, _, _| Ok(()))
        .get_set("overprintStroke", |_, _| Ok(false.into()), |_, _, _| Ok(()))
        .method("setEntirePath", 1, set_entire_path)
        .get_set("contents", contents, set_contents)
        .get("textRange", text_range);
    if container || typename == "CompoundPathItem" {
        o = o.get("pathItems", path_items);
    }
    if container {
        o = o
            .get("groupItems", |cap, ctx| {
                let list = nodeops::kind_items(cap, "GroupItem", ctx)?;
                crate::dom::add_method(&list, "add", cap, |cap, _args, ctx| Ok(pending_group(cap, cap.id(), ctx).into()), ctx)?;
                Ok(list.into())
            })
            .get("compoundPathItems", |cap, ctx| Ok(nodeops::kind_items(cap, "CompoundPathItem", ctx)?.into()))
            .get("rasterItems", |cap, ctx| Ok(nodeops::kind_items(cap, "RasterItem", ctx)?.into()))
            .get("placedItems", |cap, ctx| Ok(nodeops::kind_items(cap, "PlacedItem", ctx)?.into()))
            .get("symbolItems", |cap, ctx| Ok(nodeops::kind_items(cap, "SymbolItem", ctx)?.into()))
            .get("textFrames", |cap, ctx| {
                let list = nodeops::kind_items(cap, "TextFrame", ctx)?;
                crate::dom::add_method(&list, "add", cap, |cap, _args, ctx| Ok(pending_in(&cap.host, "Type", cap.id(), ctx).into()), ctx)?;
                Ok(list.into())
            });
    }
    o.build()
}

/// The paths directly inside a layer, group or compound path, with `add()` and `rectangle()`.
fn path_items(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let list = nodeops::kind_items(cap, "PathItem", ctx)?;
    crate::dom::add_method(&list, "add", cap, |cap, _args, ctx| Ok(pending_in(&cap.host, "Path", cap.id(), ctx).into()), ctx)?;
    crate::dom::add_method(&list, "rectangle", cap, crate::dom::path_rectangle_in, ctx)?;
    Ok(list.into())
}

fn node_layer(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let doc = cap.host.document();
    let lid = nodeops::layer_of(&doc, cap.need()?).ok_or_else(|| error("the object is not in a layer"))?;
    let node = find_node(&doc, lid).ok_or_else(|| error("the layer no longer exists"))?;
    Ok(node_object(&cap.host, node, ctx).into())
}

/// `remove()`: a layer goes with its contents, anything else is deleted.
fn remove(cap: &Cap, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = cap.need()?;
    let is_layer_node = find_node(&cap.host.document(), id).is_some_and(is_layer);
    if is_layer_node {
        exec(cap, "layer.delete", json!({"ids": [id]}))?;
    } else {
        exec(cap, "edit.clear", json!({"ids": [id]}))?;
    }
    Ok(JsValue::undefined())
}

/// The node's current summary, or an error when it does not exist (yet, or any more).
fn current(cap: &Cap) -> JsResult<Value> {
    let id = cap.need()?;
    let doc = cap.host.document();
    // Without its children: a group's whole subtree is not copied to read one property of it.
    let node = find_node(&doc, id).ok_or_else(|| error("the object no longer exists"))?;
    Ok(match node.as_object() {
        Some(m) => Value::Object(m.iter().filter(|(k, _)| k.as_str() != "children").map(|(k, v)| (k.clone(), v.clone())).collect()),
        None => node.clone(),
    })
}

/// A layer always has a name; anything else only when one was given: an object the user did not
/// name reads as "" (the Layers panel shows its kind or its text, which is not its name).
fn node_name(cap: &Cap, _ctx: &mut Context) -> JsResult<JsValue> {
    let node = current(cap)?;
    let named = node.get("named").and_then(Value::as_bool).unwrap_or(false) || node.get("kind").and_then(Value::as_str) == Some("Layer");
    Ok(text(if named { node.get("name").and_then(Value::as_str).unwrap_or_default() } else { "" }))
}

fn set_node_name(cap: &Cap, v: &JsValue, ctx: &mut Context) -> JsResult<()> {
    let name = v.to_string(ctx)?.to_std_string_escaped();
    exec(cap, "object.setProps", json!({"id": cap.need()?, "name": name})).map(|_| ())
}

fn flag(cap: &Cap, key: &str) -> JsResult<bool> {
    Ok(current(cap)?.get(key).and_then(Value::as_bool).unwrap_or(false))
}

fn set_flag(cap: &Cap, key: &str, on: bool) -> JsResult<()> {
    exec(cap, "object.setProps", json!({"id": cap.need()?, key: on})).map(|_| ())
}

/// The group or layer an item sits in; the document for a top-level layer.
fn node_parent(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let id = cap.need()?;
    let doc = cap.host.document();
    match find_parent(&doc, id) {
        Some(Some(parent)) => Ok(node_object(&cap.host, parent, ctx).into()),
        Some(None) => Ok(crate::dom::document_object(&cap.host, ctx).into()),
        None => Err(error("the object no longer exists")),
    }
}

/// The sublayers of a layer.
fn node_layers(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    // `current` leaves the children out; the list itself needs them.
    let (id, doc) = (cap.need()?, cap.host.document());
    let node = find_node(&doc, id).ok_or_else(|| error("the object no longer exists"))?;
    let objects = children(node).into_iter().filter(|c| is_layer(c)).map(|c| node_object(&cap.host, c, ctx)).collect();
    Ok(collection(objects, ctx)?.into())
}

/// The items directly inside a layer or group.
fn node_page_items(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let (id, doc) = (cap.need()?, cap.host.document());
    let node = find_node(&doc, id).ok_or_else(|| error("the object no longer exists"))?;
    let mut items: Vec<&Value> = children(node).into_iter().filter(|c| !is_layer(c)).collect();
    // The clipping path of a clip group is its first item, the way the reference app lists it
    // (the engine keeps it at the bottom).
    if node.get("kind").and_then(Value::as_str) == Some("Clip Group")
        && let Some(mask) = items.pop()
    {
        items.insert(0, mask);
    }
    let objects = items.into_iter().map(|c| node_object(&cap.host, c, ctx)).collect();
    Ok(collection(objects, ctx)?.into())
}

// ----------------------------------------------------------------------------------------------
// Geometry
// ----------------------------------------------------------------------------------------------

/// `[left, top, right, bottom]` in script coordinates (all zero for an empty layer).
fn bounds_of(cap: &Cap) -> JsResult<[f64; 4]> {
    let node = current(cap)?;
    let origin = Origin::of(&cap.host.document());
    Ok(node.get("bounds").and_then(|b| origin.rect_to_script(b)).unwrap_or([0.0; 4]))
}

fn node_bounds(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    Ok(array_of_numbers(&bounds_of(cap)?, ctx))
}

/// Move so the left edge (and/or the top edge) is where the script says.
fn move_to(cap: &Cap, left: Option<f64>, top: Option<f64>) -> JsResult<()> {
    let b = bounds_of(cap)?;
    let dx = left.map_or(0.0, |l| l - b[0]);
    // The page's y points down, the script's up.
    let dy = top.map_or(0.0, |t| -(t - b[1]));
    exec(cap, "object.move", json!({"ids": [cap.need()?], "dx": dx, "dy": dy})).map(|_| ())
}

/// `translate(deltaX, deltaY)`
fn translate(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let arg = |i: usize, ctx: &mut Context| match args.get(i) {
        Some(v) if !v.is_undefined() => number(v, ctx),
        _ => Ok(0.0),
    };
    let (dx, dy) = (arg(0, ctx)?, arg(1, ctx)?);
    exec(cap, "object.move", json!({"ids": [cap.need()?], "dx": dx, "dy": -dy}))?;
    Ok(JsValue::undefined())
}

/// `zOrder(ZOrderMethod.SENDTOBACK)` and its siblings.
fn z_order(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let method = args.first().cloned().unwrap_or_default().to_string(ctx)?.to_std_string_escaped();
    let command = match method.as_str() {
        "sendToBack" => "object.arrange.sendToBack",
        "bringToFront" => "object.arrange.bringToFront",
        "bringForward" => "object.arrange.bringForward",
        "sendBackward" => "object.arrange.sendBackward",
        other => return Err(error(format!("zOrder: unknown method `{other}`"))),
    };
    exec(cap, command, json!({"ids": [cap.need()?]}))?;
    Ok(JsValue::undefined())
}

/// `setEntirePath([[x, y], …])`: gives an object from `pathItems.add()` its anchors.
fn set_entire_path(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    if cap.id().is_some() {
        return Err(error("setEntirePath on a path that already has points is not supported yet"));
    }
    let points = args.first().cloned().unwrap_or_default();
    let Some(list) = points.as_object() else { return Err(error("setEntirePath takes an array of points")) };
    let len = list.get(js_string!("length"), ctx)?.to_length(ctx)?;
    if !(1..=100_000).contains(&len) {
        return Err(error("setEntirePath takes between 1 and 100000 points"));
    }
    let origin = Origin::of(&cap.host.document());
    let mut anchors = Vec::new();
    for i in 0..len {
        let point = list.get(i, ctx)?;
        let (x, y) = origin.to_engine(number_at(&point, 0, ctx)?, number_at(&point, 1, ctx)?);
        anchors.push(json!({"x": x, "y": y}));
    }
    let made = exec(cap, "path.create", json!({"anchors": anchors, "closed": false}))?;
    cap.bind(made.get("id").and_then(Value::as_u64).ok_or_else(|| error("the path was not created"))?);
    finish_pending(cap)?;
    Ok(JsValue::undefined())
}

// ----------------------------------------------------------------------------------------------
// Colour
// ----------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Fill,
    Stroke,
    /// The characters of a text frame.
    Char,
}

impl Slot {
    fn from_tag(tag: u8) -> Self {
        match tag / 3 {
            0 => Slot::Fill,
            1 => Slot::Stroke,
            _ => Slot::Char,
        }
    }

    fn base_tag(self) -> u8 {
        match self {
            Slot::Fill => 0,
            Slot::Stroke => 3,
            Slot::Char => 6,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Slot::Stroke => "stroke",
            Slot::Fill | Slot::Char => "fill",
        }
    }
}

fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

/// The slot's solid colour, or `None` for no paint (or a gradient, pattern…).
fn rgb_of(cap: &Cap, slot: Slot) -> JsResult<Option<[u8; 3]>> {
    let node = current(cap)?;
    let label = node.get(slot.key()).and_then(Value::as_str).unwrap_or("None");
    let digits = label.strip_prefix('#').filter(|h| h.len() >= 6);
    let channel = |i: usize| digits.and_then(|h| h.get(i..i + 2)).and_then(|p| u8::from_str_radix(p, 16).ok());
    Ok(match (channel(0), channel(2), channel(4)) {
        (Some(r), Some(g), Some(b)) => Some([r, g, b]),
        _ => None,
    })
}

fn write_paint(cap: &Cap, slot: Slot, rgb: [u8; 3]) -> JsResult<()> {
    write_color(cap, slot, json!(hex(rgb)))
}

/// Paint with a colour in the engine's own words (`"#rrggbb"`, `{c, m, y, k}`…).
fn write_color(cap: &Cap, slot: Slot, color: Value) -> JsResult<()> {
    let id = cap.need()?;
    match slot {
        Slot::Fill => exec(cap, "paint.setFill", json!({"ids": [id], "color": color})),
        Slot::Stroke => exec(cap, "paint.setStroke", json!({"ids": [id], "color": color})),
        Slot::Char => exec(cap, "text.setStyle", json!({"id": id, "fill": color})),
    }
    .map(|_| ())
}

/// The colour an object has now. Writing a channel of it changes the object, as a script
/// expects of `item.strokeColor.red = 237` right after `item.strokeColor = new RGBColor()`.
fn live_color(cap: &Cap, slot: Slot, ctx: &mut Context) -> JsResult<JsValue> {
    if rgb_of(cap, slot)?.is_none() {
        return Ok(no_color(ctx).into());
    }
    let channel = |tag: u8| slot.base_tag() + tag;
    Ok(Obj::new(ctx, cap)
        .value("typename", js_string!("RGBColor"))
        .get_set_tag("red", channel(0), color_channel, set_color_channel)
        .get_set_tag("green", channel(1), color_channel, set_color_channel)
        .get_set_tag("blue", channel(2), color_channel, set_color_channel)
        .build()
        .into())
}

fn color_channel(cap: &Cap, _ctx: &mut Context) -> JsResult<JsValue> {
    let rgb = rgb_of(cap, Slot::from_tag(cap.tag))?.unwrap_or([0; 3]);
    Ok(f64::from(rgb.get(usize::from(cap.tag % 3)).copied().unwrap_or(0)).into())
}

fn set_color_channel(cap: &Cap, v: &JsValue, ctx: &mut Context) -> JsResult<()> {
    let slot = Slot::from_tag(cap.tag);
    let mut rgb = rgb_of(cap, slot)?.unwrap_or([0; 3]);
    if let Some(c) = rgb.get_mut(usize::from(cap.tag % 3)) {
        *c = byte(number(v, ctx)?);
    }
    write_paint(cap, slot, rgb)
}

fn byte(n: f64) -> u8 {
    n.round().clamp(0.0, 255.0) as u8
}

/// Assign a colour object: an `RGBColor`, or `NoColor` to remove the paint.
fn set_color(cap: &Cap, slot: Slot, v: &JsValue, ctx: &mut Context) -> JsResult<()> {
    let Some(obj) = v.as_object() else { return Err(error("a colour was expected")) };
    let kind = obj.get(js_string!("typename"), ctx)?.to_string(ctx)?.to_std_string_escaped();
    match kind.as_str() {
        "NoColor" => remove_paint(cap, slot),
        "RGBColor" => {
            let mut channel = |key: boa_engine::JsString| -> JsResult<u8> { Ok(byte(number(&obj.get(key, ctx)?, ctx)?)) };
            let rgb = [channel(js_string!("red"))?, channel(js_string!("green"))?, channel(js_string!("blue"))?];
            write_paint(cap, slot, rgb)
        }
        "CMYKColor" => {
            // Percentages, as the reference app's CMYKColor holds them.
            let mut channel = |key: boa_engine::JsString| -> JsResult<f64> { Ok(number(&obj.get(key, ctx)?, ctx)?.clamp(0.0, 100.0)) };
            let (c, m, y, k) =
                (channel(js_string!("cyan"))?, channel(js_string!("magenta"))?, channel(js_string!("yellow"))?, channel(js_string!("black"))?);
            write_color(cap, slot, json!({"c": c, "m": m, "y": y, "k": k}))
        }
        other => Err(error(format!("{other} is not supported yet"))),
    }
}

fn remove_paint(cap: &Cap, slot: Slot) -> JsResult<()> {
    let id = cap.need()?;
    match slot {
        Slot::Fill => exec(cap, "paint.setFill", json!({"ids": [id], "none": true})).map(|_| ()),
        Slot::Stroke => exec(cap, "paint.setStroke", json!({"ids": [id], "none": true})).map(|_| ()),
        Slot::Char => Err(error("characters cannot be left without a fill")),
    }
}

fn has_paint(cap: &Cap, slot: Slot) -> JsResult<bool> {
    Ok(current(cap)?.get(slot.key()).and_then(Value::as_str).unwrap_or("None") != "None")
}

/// `filled` and `stroked`: off removes the paint; on gives black when there is none, since a
/// script cannot know which colour a new paint should take.
fn set_painted(cap: &Cap, slot: Slot, on: bool) -> JsResult<()> {
    if !on {
        return remove_paint(cap, slot);
    }
    if has_paint(cap, slot)? { Ok(()) } else { write_paint(cap, slot, [0, 0, 0]) }
}

// ----------------------------------------------------------------------------------------------
// Text
// ----------------------------------------------------------------------------------------------

fn contents(cap: &Cap, _ctx: &mut Context) -> JsResult<JsValue> {
    Ok(text(current(cap)?.get("text").and_then(Value::as_str).unwrap_or_default()))
}

/// Setting the contents of a frame from `textFrames.add()` creates its text.
fn set_contents(cap: &Cap, v: &JsValue, ctx: &mut Context) -> JsResult<()> {
    let contents = v.to_string(ctx)?.to_std_string_escaped();
    match cap.id() {
        Some(id) => exec(cap, "text.setText", json!({"id": id, "text": contents})).map(|_| ()),
        None => {
            let (x, y) = Origin::of(&cap.host.document()).to_engine(0.0, 0.0);
            let made = exec(cap, "text.create", json!({"x": x, "y": y, "text": contents}))?;
            let id = made.get("id").and_then(Value::as_u64).or_else(|| made.get("ids").and_then(|i| i.get(0)).and_then(Value::as_u64));
            cap.bind(id.ok_or_else(|| error("the text was not created"))?);
            finish_pending(cap)
        }
    }
}

/// An object from `add()` in a layer or group goes there once it exists.
fn finish_pending(cap: &Cap) -> JsResult<()> {
    let Some(Container(Some(c))) = cap.extra.as_ref().and_then(|e| e.downcast_ref::<Container>()).map(|c| Container(c.0)) else { return Ok(()) };
    let target = nodeops::Ref::Node { doc: cap.doc, id: Some(c), pending: None };
    nodeops::adopt(cap, &target, cap.need()?, nodeops::Place::Begin)
}

fn text_range(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let character = Obj::new(ctx, cap)
        .set_only("size", |cap, v, ctx| {
            let size = number(v, ctx)?;
            exec(cap, "text.setStyle", json!({"id": cap.need()?, "size": size})).map(|_| ())
        })
        .set_only("textFont", |cap, v, ctx| {
            // A font object from `app.textFonts.getByName()`, or a name.
            let name = match v.as_object() {
                Some(o) => o.get(js_string!("name"), ctx)?.to_string(ctx)?.to_std_string_escaped(),
                None => v.to_string(ctx)?.to_std_string_escaped(),
            };
            exec(cap, "text.setStyle", json!({"id": cap.need()?, "font": name})).map(|_| ())
        })
        .get_set("fillColor", |cap, ctx| live_color(cap, Slot::Char, ctx), |cap, v, ctx| set_color(cap, Slot::Char, v, ctx))
        .build();
    let paragraph = Obj::new(ctx, cap)
        .set_only("justification", |cap, v, ctx| {
            let how = v.to_string(ctx)?.to_std_string_escaped();
            if !matches!(how.as_str(), "left" | "center" | "right" | "justifyAll") {
                return Err(error(format!("justification `{how}` is not supported yet")));
            }
            exec(cap, "text.setStyle", json!({"id": cap.need()?, "justify": how})).map(|_| ())
        })
        .build();
    Ok(Obj::new(ctx, cap)
        .value("typename", js_string!("TextRange"))
        .get_set("contents", contents, set_contents)
        .value("characterAttributes", character)
        .value("paragraphAttributes", paragraph)
        .build()
        .into())
}
