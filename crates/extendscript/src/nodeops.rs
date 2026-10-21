//! What a script does with items beyond reading them: putting them somewhere (`move`,
//! `duplicate`), resizing and turning them about a corner, selecting them, and the empty groups,
//! paths and text frames a script makes before they have content (`groupItems.add()`).
//!
//! Every operation is an engine command (`layer.move`, `edit.duplicate`, `object.scale`…), so it
//! goes through undo, the journal and the guard like the user's own. Positions inside a layer or
//! group follow the reference app's words: `PLACEATBEGINNING` is the top of the stack,
//! `PLACEATEND` the bottom, `PLACEBEFORE` in front of the target, `PLACEAFTER` behind it.

use std::{cell::RefCell, rc::Rc};

use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};
use serde_json::{Value, json};

use crate::{
    dom::{Cap, collection, error, exec, find_node, find_parent, is_layer, number},
    geom::Origin,
    host::Host,
    node,
};

/// Where an item goes relative to its target.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Place {
    /// Top of the target's stack (`PLACEATBEGINNING`; also what a script means by a document).
    Begin,
    /// Bottom of the target's stack (`PLACEATEND`).
    End,
    /// In front of the target item (`PLACEBEFORE`).
    Before,
    /// Behind the target item (`PLACEAFTER`).
    After,
    /// Inside the target (`INSIDE`), on top.
    Inside,
}

pub(crate) fn place_of(v: Option<&JsValue>, ctx: &mut Context) -> JsResult<Place> {
    let Some(v) = v.filter(|v| !v.is_undefined() && !v.is_null()) else { return Ok(Place::Begin) };
    Ok(match v.to_string(ctx)?.to_std_string_escaped().as_str() {
        "begin" => Place::Begin,
        "end" => Place::End,
        "before" => Place::Before,
        "after" => Place::After,
        "inside" => Place::Inside,
        other => return Err(error(format!("unknown ElementPlacement `{other}`"))),
    })
}

/// A group a script made with `groupItems.add()` that holds nothing yet: the engine has no empty
/// groups, so the group is made around the first thing that is put in it.
pub(crate) struct Pending {
    /// The layer or group it was made in.
    pub container: Option<u64>,
    /// A name set before it had content.
    pub name: RefCell<Option<String>>,
    /// The id, once it has content (shared with the script object).
    pub id: Rc<std::cell::Cell<Option<u64>>>,
    /// Its place in the host's list of such groups.
    pub token: std::cell::Cell<u32>,
}

/// What a script object stands for.
pub(crate) enum Ref {
    /// The document with this `uid`.
    Doc(u64),
    /// A layer, group or item of a document; the id is unset for a group with no content yet.
    Node { doc: Option<u64>, id: Option<u64>, pending: Option<Rc<Pending>> },
}

/// Read what a script object stands for (`None`: not a document, layer or item).
pub(crate) fn reference(host: &Host, v: Option<&JsValue>, ctx: &mut Context) -> JsResult<Option<Ref>> {
    let Some(o) = v.and_then(JsValue::as_object) else { return Ok(None) };
    if o.get(js_string!("typename"), ctx)?.to_string(ctx)?.to_std_string_escaped() == "Document" {
        let uid = call_hidden(&o, "__uid", ctx)?;
        return Ok(uid.as_number().map(|u| Ref::Doc(u as u64)));
    }
    let id = call_hidden(&o, "__id", ctx)?;
    let known = !o.get(js_string!("__id"), ctx)?.is_undefined();
    if !known {
        return Ok(None);
    }
    let doc = call_hidden(&o, "__doc", ctx)?.as_number().map(|u| u as u64);
    let pending = call_hidden(&o, "__pending", ctx)?.as_number().and_then(|t| host.pending_of(t as u32));
    Ok(Some(Ref::Node { doc, id: id.as_number().map(|n| n as u64), pending }))
}

fn call_hidden(o: &JsObject, name: &str, ctx: &mut Context) -> JsResult<JsValue> {
    let f = o.get(boa_engine::JsString::from(name), ctx)?;
    match f.as_callable() {
        Some(c) => c.call(&JsValue::from(o.clone()), &[], ctx),
        None => Ok(JsValue::undefined()),
    }
}

// ----------------------------------------------------------------------------------------------
// Selection
// ----------------------------------------------------------------------------------------------

fn selection_ids(cap: &Cap) -> Vec<u64> {
    cap.host.document().get("selection").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default()
}

pub(crate) fn set_selection_ids(cap: &Cap, ids: &[u64]) -> JsResult<()> {
    if ids.is_empty() { exec(cap, "select.none", json!({})).map(|_| ()) } else { exec(cap, "select.set", json!({"ids": ids})).map(|_| ()) }
}

pub(crate) fn is_selected(cap: &Cap) -> JsResult<bool> {
    Ok(selection_ids(cap).contains(&cap.need()?))
}

pub(crate) fn set_selected(cap: &Cap, on: bool) -> JsResult<()> {
    let id = cap.need()?;
    let mut ids = selection_ids(cap);
    if on {
        if !ids.contains(&id) {
            ids.push(id);
        }
    } else {
        ids.retain(|i| *i != id);
    }
    set_selection_ids(cap, &ids)
}

// ----------------------------------------------------------------------------------------------
// Putting an item somewhere
// ----------------------------------------------------------------------------------------------

/// The id of the layer new art goes into.
fn current_layer(cap: &Cap) -> JsResult<u64> {
    cap.host.document().get("currentLayer").and_then(Value::as_u64).ok_or_else(|| error("the document has no layer"))
}

/// Put the object with this id in `target` (in the active document).
pub(crate) fn adopt(cap: &Cap, target: &Ref, item: u64, place: Place) -> JsResult<()> {
    let (target_id, pending) = match target {
        Ref::Doc(_) => (Some(current_layer(cap)?), None),
        Ref::Node { id, pending, .. } => (*id, pending.clone()),
    };
    let Some(target_id) = target_id else {
        // A group with no content yet: it is made around this item.
        let Some(p) = pending else { return Err(error("the object has no content yet")) };
        if let Some(container) = p.container {
            exec(cap, "layer.move", json!({"ids": [item], "target": container, "place": "inside"}))?;
        }
        let made = exec(cap, "object.group", json!({"ids": [item]}))?;
        let gid = made.get("id").and_then(Value::as_u64).ok_or_else(|| error("the group was not made"))?;
        p.id.set(Some(gid));
        if let Some(name) = p.name.borrow().as_ref() {
            exec(cap, "object.setProps", json!({"id": gid, "name": name}))?;
        }
        return Ok(());
    };
    match place {
        Place::Before => exec(cap, "layer.move", json!({"ids": [item], "target": target_id, "place": "above"})).map(|_| ()),
        Place::After => exec(cap, "layer.move", json!({"ids": [item], "target": target_id, "place": "below"})).map(|_| ()),
        Place::Begin | Place::Inside => exec(cap, "layer.move", json!({"ids": [item], "target": target_id, "place": "inside"})).map(|_| ()),
        Place::End => {
            exec(cap, "layer.move", json!({"ids": [item], "target": target_id, "place": "inside"}))?;
            exec(cap, "object.arrange.sendToBack", json!({"ids": [item]})).map(|_| ())
        }
    }
}

/// `move(target, placement)`: the item itself goes.
pub(crate) fn move_to_target(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = cap.need()?;
    let target = reference(&cap.host, args.first(), ctx)?.ok_or_else(|| error("move: a layer, group or item was expected"))?;
    let place = place_of(args.get(1), ctx)?;
    let target_doc = match &target {
        Ref::Doc(u) => Some(*u),
        Ref::Node { doc, .. } => *doc,
    };
    if target_doc.is_some() && target_doc != cap.doc {
        return Err(error("move: the target is in another document (use duplicate, or copy and paste)"));
    }
    let before = selection_ids(cap);
    adopt(cap, &target, id, place)?;
    // Moving does not change what is selected, as for the reference app.
    set_selection_ids(cap, &before)?;
    Ok(JsValue::undefined())
}

/// `duplicate(target, placement)`: a copy goes there (in place when there is no target); the
/// copy is the result. Into another document it goes through the clipboard.
pub(crate) fn duplicate(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = cap.need()?;
    let target = reference(&cap.host, args.first(), ctx)?;
    let place = place_of(args.get(1), ctx)?;
    let before = selection_ids(cap);
    let here = cap.doc;
    let target_doc = match &target {
        None => here,
        Some(Ref::Doc(u)) => Some(*u),
        Some(Ref::Node { doc, .. }) => doc.or(here),
    };
    let new_id;
    let new_doc;
    if target_doc != here {
        let dest = target_doc.ok_or_else(|| error("duplicate: no such document"))?;
        exec(cap, "select.set", json!({"ids": [id]}))?;
        exec(cap, "edit.copy", json!({}))?;
        cap.host.activate(dest).map_err(error)?;
        let pasted = exec(cap, "edit.pasteInPlace", json!({}));
        let pasted = match pasted {
            Ok(p) => p,
            Err(e) => {
                if let Some(h) = here {
                    let _ = cap.host.activate(h);
                }
                return Err(e);
            }
        };
        let copy = pasted.get("ids").and_then(|a| a.get(0)).and_then(Value::as_u64).ok_or_else(|| error("duplicate: nothing was pasted"))?;
        // The copy is the only object that was pasted: in the target document it goes to place.
        if let Some(t) = &target {
            let dest_cap = Cap::of_doc(&cap.host, dest, Some(copy));
            adopt(&dest_cap, t, copy, place)?;
        }
        let done = set_selection_ids(&Cap::of_doc(&cap.host, dest, None), &[]);
        if let Some(h) = here {
            cap.host.activate(h).map_err(error)?;
        }
        done?;
        new_id = copy;
        new_doc = Some(dest);
    } else {
        let made = exec(cap, "edit.duplicate", json!({"ids": [id]}))?;
        let copy = made.get("ids").and_then(|a| a.get(0)).and_then(Value::as_u64).ok_or_else(|| error("duplicate: nothing was copied"))?;
        if let Some(t) = &target {
            adopt(cap, t, copy, place)?;
        }
        new_id = copy;
        new_doc = here;
    }
    set_selection_ids(cap, &before)?;
    let dest = new_doc.ok_or_else(|| error("duplicate: no such document"))?;
    node::object_in(&cap.host, dest, new_id, ctx)
}

// ----------------------------------------------------------------------------------------------
// Resizing and turning
// ----------------------------------------------------------------------------------------------

/// A point of the item's bounds a script names with `Transformation` (`[x, y]` in the engine's
/// page: y down), or the script origin for `documentorigin`.
fn anchor_point(cap: &Cap, how: &str) -> JsResult<[f64; 2]> {
    let doc = cap.host.document();
    let node = find_node(&doc, cap.need()?).ok_or_else(|| error("the object no longer exists"))?;
    let b = node.get("bounds").ok_or_else(|| error("the object has no bounds"))?;
    let num = |k: &str| b.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let (x, y, w, h) = (num("x"), num("y"), num("width"), num("height"));
    let (l, c, r) = (x, x + w / 2.0, x + w);
    let (t, m, bo) = (y, y + h / 2.0, y + h);
    Ok(match how {
        "topleft" => [l, t],
        "top" => [c, t],
        "topright" => [r, t],
        "left" => [l, m],
        "right" => [r, m],
        "bottomleft" => [l, bo],
        "bottom" => [c, bo],
        "bottomright" => [r, bo],
        "documentorigin" => {
            let (ox, oy) = Origin::of(&doc).to_engine(0.0, 0.0);
            [ox, oy]
        }
        _ => [c, m],
    })
}

fn how_of(v: Option<&JsValue>, ctx: &mut Context) -> JsResult<String> {
    Ok(match v.filter(|v| !v.is_undefined() && !v.is_null()) {
        Some(v) => v.to_string(ctx)?.to_std_string_escaped(),
        None => "center".to_string(),
    })
}

/// `resize(scaleX, scaleY, changePositions, changeFillPatterns, changeFillGradients,
/// changeStrokePattern, changeLineWidths, scaleAbout)`: percentages, about a point of the item.
pub(crate) fn resize(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let pct = |i: usize, ctx: &mut Context| -> JsResult<Option<f64>> {
        match args.get(i) {
            Some(v) if !v.is_undefined() && !v.is_null() => number(v, ctx).map(Some),
            _ => Ok(None),
        }
    };
    let sx = pct(0, ctx)?.ok_or_else(|| error("resize: the horizontal scale is missing"))?;
    let sy = pct(1, ctx)?.unwrap_or(sx);
    if sx <= 0.0 || sy <= 0.0 {
        return Err(error("resize: a scale must be above zero"));
    }
    // The line-width argument is a percentage too: the same as the scale (or left out) scales
    // strokes with the art, 100 keeps them.
    let strokes = match pct(6, ctx)? {
        Some(w) => (w - 100.0).abs() > 1e-9 || (sx - 100.0).abs() < 1e-9 && (sy - 100.0).abs() < 1e-9,
        None => true,
    };
    let origin = anchor_point(cap, &how_of(args.get(7), ctx)?)?;
    exec(cap, "object.scale", json!({"ids": [cap.need()?], "sx": sx, "sy": sy, "origin": origin, "strokes": strokes}))?;
    Ok(JsValue::undefined())
}

/// `rotate(angle, changePositions, changeFillPatterns, changeFillGradients, changeStrokePattern,
/// rotateAbout)`: counter-clockwise degrees about a point of the item (the centre by default).
pub(crate) fn rotate(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let angle = number(args.first().unwrap_or(&JsValue::undefined()), ctx)?;
    let origin = anchor_point(cap, &how_of(args.get(5), ctx)?)?;
    exec(cap, "object.rotate", json!({"ids": [cap.need()?], "angle": angle, "origin": origin}))?;
    Ok(JsValue::undefined())
}

// ----------------------------------------------------------------------------------------------
// Items a script makes before they have content
// ----------------------------------------------------------------------------------------------

/// `groupItems.add()`: a group in `container` (the current layer without one).
pub(crate) fn pending_group(cap: &Cap, container: Option<u64>, ctx: &mut Context) -> JsObject {
    let pending = Rc::new(Pending { container, name: RefCell::new(None), id: Rc::new(std::cell::Cell::new(None)), token: std::cell::Cell::new(0) });
    pending.token.set(cap.host.register_pending(Rc::clone(&pending)));
    let group_cap = Cap::pending(&cap.host, cap.doc, Rc::clone(&pending.id), pending);
    node::build_pending_group(group_cap, ctx)
}

/// The objects of one kind directly inside a layer or group (or the paths of a compound path).
pub(crate) fn kind_items(cap: &Cap, typename: &str, ctx: &mut Context) -> JsResult<JsObject> {
    let doc = cap.host.document();
    let Some(id) = cap.id() else { return collection(vec![], ctx) };
    let node = find_node(&doc, id).ok_or_else(|| error("the object no longer exists"))?;
    let items: Vec<&Value> = crate::dom::children(node).into_iter().filter(|c| node::typename_of(c) == typename).collect();
    let objects = crate::dom::node_objects(&cap.host, items, ctx);
    collection(objects, ctx)
}

/// Is `id` the clipping path of the clip group it is in (or a path of the compound path that is)?
pub(crate) fn is_clipping(doc: &Value, id: u64) -> bool {
    let is_mask = |doc: &Value, id: u64| -> bool {
        match find_parent(doc, id) {
            Some(Some(p)) if p.get("kind").and_then(Value::as_str) == Some("Clip Group") => {
                crate::dom::children(p).last().and_then(|c| c.get("id")).and_then(Value::as_u64) == Some(id)
            }
            _ => false,
        }
    };
    if is_mask(doc, id) {
        return true;
    }
    match find_parent(doc, id) {
        Some(Some(p)) if p.get("kind").and_then(Value::as_str) == Some("Compound Path") => {
            p.get("id").and_then(Value::as_u64).is_some_and(|pid| is_mask(doc, pid))
        }
        _ => false,
    }
}

/// The layer an item is in: the nearest layer above it.
pub(crate) fn layer_of(doc: &Value, id: u64) -> Option<u64> {
    let mut cur = id;
    loop {
        let p = find_parent(doc, cur)??;
        let pid = p.get("id").and_then(Value::as_u64)?;
        if is_layer(p) {
            return Some(pid);
        }
        cur = pid;
    }
}
