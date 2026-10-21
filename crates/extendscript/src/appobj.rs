//! The application and its documents as a script reaches them: `app` (open, new, copy, paste,
//! menu commands), `app.documents`, what a `Document` does beyond its art (close, save, select,
//! the active layer and artboard), and the enumerations and option objects scripts build
//! (`DocumentPreset`, `CMYKColor`, `PDFSaveOptions`, `ElementPlacement`…).
//!
//! Documents are held by their `uid`: a script object keeps working for the document it stands
//! for while others open and close, and an object of a closed document says so.

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string,
    object::{FunctionObjectBuilder, ObjectInitializer},
    property::Attribute,
};
use serde_json::{Value, json};

use crate::{
    dom::{Cap, Obj, add_method, collection, document_object_for, error, exec, find_node, number},
    host::Host,
    node,
    nodeops::{self, Ref},
};

// ----------------------------------------------------------------------------------------------
// Enumerations and constructors
// ----------------------------------------------------------------------------------------------

/// The values only have to exist and differ: scripts read the enumerations by name and compare.
const ENUMS: &[(&str, &[(&str, &str)])] = &[
    ("UserInteractionLevel", &[("DISPLAYALERTS", "1"), ("DONTDISPLAYALERTS", "2"), ("NEVERINTERACT", "3")]),
    ("Justification", &[("LEFT", "left"), ("CENTER", "center"), ("RIGHT", "right"), ("FULLJUSTIFY", "justifyAll")]),
    (
        "ZOrderMethod",
        &[("BRINGTOFRONT", "bringToFront"), ("BRINGFORWARD", "bringForward"), ("SENDBACKWARD", "sendBackward"), ("SENDTOBACK", "sendToBack")],
    ),
    ("DocumentColorSpace", &[("RGB", "rgb"), ("CMYK", "cmyk")]),
    (
        "RulerUnits",
        &[
            ("Points", "Points"),
            ("Picas", "Picas"),
            ("Inches", "Inches"),
            ("Millimeters", "Millimeters"),
            ("Centimeters", "Centimeters"),
            ("Pixels", "Pixels"),
            ("Unknown", "Points"),
        ],
    ),
    (
        "ElementPlacement",
        &[("PLACEATBEGINNING", "begin"), ("PLACEATEND", "end"), ("PLACEBEFORE", "before"), ("PLACEAFTER", "after"), ("INSIDE", "inside")],
    ),
    (
        "Transformation",
        &[
            ("DOCUMENTORIGIN", "documentorigin"),
            ("TOPLEFT", "topleft"),
            ("LEFT", "left"),
            ("BOTTOMLEFT", "bottomleft"),
            ("TOP", "top"),
            ("CENTER", "center"),
            ("BOTTOM", "bottom"),
            ("TOPRIGHT", "topright"),
            ("RIGHT", "right"),
            ("BOTTOMRIGHT", "bottomright"),
        ],
    ),
    ("SaveOptions", &[("SAVECHANGES", "yes"), ("DONOTSAVECHANGES", "no"), ("PROMPTTOSAVECHANGES", "ask")]),
    ("PDFCompatibility", &[("ACROBAT4", "1.3"), ("ACROBAT5", "1.4"), ("ACROBAT6", "1.5"), ("ACROBAT7", "1.6"), ("ACROBAT8", "1.7")]),
    (
        "Compatibility",
        &[
            ("ILLUSTRATOR8", "8"),
            ("ILLUSTRATOR10", "10"),
            ("ILLUSTRATOR11", "11"),
            ("ILLUSTRATOR12", "12"),
            ("ILLUSTRATOR13", "13"),
            ("ILLUSTRATOR14", "14"),
            ("ILLUSTRATOR15", "15"),
            ("ILLUSTRATOR16", "16"),
            ("ILLUSTRATOR17", "17"),
        ],
    ),
];

#[derive(Clone, Copy)]
enum Field {
    Num(f64),
    Str(&'static str),
    Bool(bool),
}

fn plain_object(ctx: &mut Context, typename: &str, fields: &[(&str, Field)]) -> JsObject {
    let mut init = ObjectInitializer::new(ctx);
    init.property(js_string!("typename"), JsString::from(typename), Attribute::all());
    for (key, value) in fields {
        let v: JsValue = match value {
            Field::Num(n) => (*n).into(),
            Field::Str(s) => JsString::from(*s).into(),
            Field::Bool(b) => (*b).into(),
        };
        init.property(JsString::from(*key), v, Attribute::all());
    }
    init.build()
}

fn constructor(ctx: &mut Context, name: &'static str, f: fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>) -> JsResult<()> {
    let ctor = FunctionObjectBuilder::new(ctx.realm(), NativeFunction::from_fn_ptr(f)).name(JsString::from(name)).length(0).constructor(true).build();
    ctx.register_global_property(JsString::from(name), ctor, Attribute::all())
}

pub(crate) fn install(host: &Host, ctx: &mut Context) -> JsResult<()> {
    for (name, members) in ENUMS {
        let mut init = ObjectInitializer::new(ctx);
        for (key, value) in *members {
            init.property(JsString::from(*key), JsString::from(*value), Attribute::all());
        }
        let object = init.build();
        ctx.register_global_property(JsString::from(*name), object, Attribute::all())?;
    }
    constructor(ctx, "CMYKColor", |_, _, ctx| {
        Ok(plain_object(
            ctx,
            "CMYKColor",
            &[("cyan", Field::Num(0.0)), ("magenta", Field::Num(0.0)), ("yellow", Field::Num(0.0)), ("black", Field::Num(0.0))],
        )
        .into())
    })?;
    constructor(ctx, "GrayColor", |_, _, ctx| Ok(plain_object(ctx, "GrayColor", &[("gray", Field::Num(0.0))]).into()))?;
    constructor(ctx, "NoColor", |_, _, ctx| Ok(plain_object(ctx, "NoColor", &[]).into()))?;
    constructor(ctx, "DocumentPreset", |_, _, ctx| {
        Ok(plain_object(
            ctx,
            "DocumentPreset",
            &[
                ("width", Field::Num(612.0)),
                ("height", Field::Num(792.0)),
                ("colorMode", Field::Str("cmyk")),
                ("units", Field::Str("Points")),
                ("title", Field::Str("")),
                ("numArtboards", Field::Num(1.0)),
                ("artboardSpacing", Field::Num(20.0)),
                ("artboardRowsOrCols", Field::Num(1.0)),
                ("previewMode", Field::Str("default")),
                ("rasterResolution", Field::Str("")),
                ("transparencyGrid", Field::Str("")),
            ],
        )
        .into())
    })?;
    constructor(ctx, "PDFSaveOptions", |_, _, ctx| {
        Ok(plain_object(
            ctx,
            "PDFSaveOptions",
            &[
                ("pdfPreset", Field::Str("")),
                ("compatibility", Field::Str("1.7")),
                ("preserveEditability", Field::Bool(true)),
                ("generateThumbnails", Field::Bool(false)),
                ("viewAfterSaving", Field::Bool(false)),
                ("artboardRange", Field::Str("")),
                ("optimization", Field::Bool(false)),
            ],
        )
        .into())
    })?;
    constructor(ctx, "IllustratorSaveOptions", |_, _, ctx| {
        Ok(plain_object(
            ctx,
            "IllustratorSaveOptions",
            &[
                ("compatibility", Field::Str("17")),
                ("pdfCompatible", Field::Bool(true)),
                ("compressed", Field::Bool(true)),
                ("embedLinkedFiles", Field::Bool(false)),
            ],
        )
        .into())
    })?;

    let cap = Cap::app(host);
    let app = Obj::new(ctx, &cap)
        .value("typename", js_string!("Application"))
        .value("name", js_string!("VectorCraft"))
        .get("documents", app_documents)
        .get_set("activeDocument", app_active_document, set_app_active_document)
        .get("textFonts", app_text_fonts)
        .value("userInteractionLevel", 1)
        .method("open", 1, app_open)
        .method("copy", 0, |cap, _, _| edit(cap, "edit.copy"))
        .method("cut", 0, |cap, _, _| edit(cap, "edit.cut"))
        .method("paste", 0, |cap, _, _| edit(cap, "edit.paste"))
        .method("executeMenuCommand", 1, execute_menu_command)
        .method("redraw", 0, |_cap, _args, _ctx| Ok(JsValue::undefined()))
        .build();
    ctx.register_global_property(js_string!("app"), app, Attribute::all())
}

// ----------------------------------------------------------------------------------------------
// The application
// ----------------------------------------------------------------------------------------------

fn doc_list(host: &Host) -> Vec<Value> {
    host.document().get("documents").and_then(Value::as_array).cloned().unwrap_or_default()
}

fn uid_of(d: &Value) -> Option<u64> {
    d.get("uid").and_then(Value::as_u64)
}

fn app_documents(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let objects = doc_list(&cap.host).iter().filter_map(uid_of).map(|u| document_object_for(&cap.host, Some(u), ctx)).collect();
    let list = collection(objects, ctx)?;
    add_method(&list, "add", cap, documents_add, ctx)?;
    add_method(&list, "addDocument", cap, documents_add_document, ctx)?;
    Ok(list.into())
}

fn app_active_document(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    match cap.host.active_uid() {
        Some(uid) => Ok(document_object_for(&cap.host, Some(uid), ctx).into()),
        None => Err(error("there is no document open")),
    }
}

fn set_app_active_document(cap: &Cap, v: &JsValue, ctx: &mut Context) -> JsResult<()> {
    match nodeops::reference(&cap.host, Some(v), ctx)? {
        Some(Ref::Doc(uid)) => cap.host.activate(uid).map_err(error),
        _ => Err(error("activeDocument: a document was expected")),
    }
}

/// `app.documents.add(colorSpace, width, height, numArtboards, …)`: points, a new document in front.
fn documents_add(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let mode = match args.first().filter(|v| !v.is_undefined() && !v.is_null()) {
        Some(v) => v.to_string(ctx)?.to_std_string_escaped(),
        None => "cmyk".to_string(),
    };
    let dim = |i: usize, default: f64, ctx: &mut Context| -> JsResult<f64> {
        match args.get(i).filter(|v| !v.is_undefined() && !v.is_null()) {
            Some(v) => number(v, ctx),
            None => Ok(default),
        }
    };
    let (w, h, n) = (dim(1, 612.0, ctx)?, dim(2, 792.0, ctx)?, dim(3, 1.0, ctx)?);
    new_document(cap, json!({"width": w, "height": h, "units": "Points", "colorMode": mode, "artboards": n.clamp(1.0, 1000.0) as u64}), ctx)
}

/// `app.documents.addDocument(startupPreset, presetOptions)`
fn documents_add_document(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let mut params = serde_json::Map::new();
    if let Some(p) = args.get(1).and_then(JsValue::as_object) {
        let get = |k: &str, ctx: &mut Context| -> JsResult<JsValue> { p.get(JsString::from(k), ctx) };
        for key in ["width", "height"] {
            let v = get(key, ctx)?;
            if !v.is_undefined() && !v.is_null() {
                params.insert(key.into(), json!(number(&v, ctx)?));
            }
        }
        for (script_key, key) in [("colorMode", "colorMode"), ("units", "units"), ("title", "name")] {
            let v = get(script_key, ctx)?;
            if !v.is_undefined() && !v.is_null() {
                let text = v.to_string(ctx)?.to_std_string_escaped();
                if !text.is_empty() {
                    params.insert(key.into(), json!(text));
                }
            }
        }
        let n = get("numArtboards", ctx)?;
        if !n.is_undefined() && !n.is_null() {
            params.insert("artboards".into(), json!(number(&n, ctx)?.clamp(1.0, 1000.0) as u64));
        }
    }
    new_document(cap, Value::Object(params), ctx)
}

fn new_document(cap: &Cap, params: Value, ctx: &mut Context) -> JsResult<JsValue> {
    exec(cap, "file.new", params)?;
    let uid = cap.host.active_uid().ok_or_else(|| error("the document was not created"))?;
    Ok(document_object_for(&cap.host, Some(uid), ctx).into())
}

fn same_file(a: &std::path::Path, b: &str) -> bool {
    let b = std::path::Path::new(b);
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// `app.open(file)`: the file as a document in front; one that is already open is not opened again.
fn app_open(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let path = crate::fs::path_arg(args.first(), ctx)?;
    if !path.exists() {
        return Err(error(format!("cannot open {}: no such file", path.display())));
    }
    for d in doc_list(&cap.host) {
        if let (Some(uid), Some(p)) = (uid_of(&d), d.get("path").and_then(Value::as_str))
            && same_file(&path, p)
        {
            cap.host.activate(uid).map_err(error)?;
            return Ok(document_object_for(&cap.host, Some(uid), ctx).into());
        }
    }
    exec(cap, "document.open", json!({"path": path.to_string_lossy()}))?;
    let uid = cap.host.active_uid().ok_or_else(|| error("the file was not opened"))?;
    Ok(document_object_for(&cap.host, Some(uid), ctx).into())
}

fn edit(cap: &Cap, command: &str) -> JsResult<JsValue> {
    exec(cap, command, json!({}))?;
    Ok(JsValue::undefined())
}

/// `app.executeMenuCommand(name)`: the commands scripts use, by their menu names.
fn execute_menu_command(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = args.first().cloned().unwrap_or_default().to_string(ctx)?.to_std_string_escaped();
    let uid = cap.host.active_uid().ok_or_else(|| error("there is no document open"))?;
    let (command, params) = match name.as_str() {
        "copy" => ("edit.copy", json!({})),
        "cut" => ("edit.cut", json!({})),
        "paste" => ("edit.paste", json!({})),
        "pasteFront" => ("edit.pasteInFront", json!({"artboard": cap.host.active_artboard(uid)})),
        "pasteBack" => ("edit.pasteInBack", json!({"artboard": cap.host.active_artboard(uid)})),
        "pasteInPlace" => ("edit.pasteInPlace", json!({})),
        "selectall" => ("select.all", json!({})),
        "deselectall" => ("select.none", json!({})),
        "group" => ("object.group", json!({})),
        "ungroup" => ("object.ungroup", json!({})),
        "clear" => ("edit.clear", json!({})),
        "lock" => ("object.lock", json!({})),
        // The window's view: nothing here to fit.
        "fitall" | "fitin" | "actualsize" | "zoomin" | "zoomout" | "fitall2" => return Ok(JsValue::undefined()),
        other => return Err(error(format!("executeMenuCommand: `{other}` is not supported yet"))),
    };
    exec(cap, command, params)?;
    Ok(JsValue::undefined())
}

/// `app.textFonts`: a font is found by name; whether it is installed is the text engine's to say.
fn app_text_fonts(_cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let list = collection(vec![], ctx)?;
    let by_name = NativeFunction::from_fn_ptr(|_, args, ctx| {
        let name = args.first().cloned().unwrap_or_default().to_string(ctx)?;
        let font = plain_object(ctx, "TextFont", &[]);
        font.set(js_string!("name"), name.clone(), false, ctx)?;
        font.set(js_string!("family"), name, false, ctx)?;
        Ok(font.into())
    });
    list.set(js_string!("getByName"), by_name.to_js_function(ctx.realm()), false, ctx)?;
    Ok(list.into())
}

// ----------------------------------------------------------------------------------------------
// A document
// ----------------------------------------------------------------------------------------------

pub(crate) fn document_full_name(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let doc = cap.host.document();
    let path = doc.get("path").and_then(Value::as_str).filter(|p| !p.is_empty());
    let name = doc.get("title").and_then(Value::as_str).unwrap_or_default();
    Ok(crate::fs::file_object(&cap.host, std::path::PathBuf::from(path.unwrap_or(name)), ctx).into())
}

pub(crate) fn document_folder(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let doc = cap.host.document();
    let path = doc.get("path").and_then(Value::as_str).filter(|p| !p.is_empty());
    let folder = path.and_then(|p| std::path::Path::new(p).parent()).map(std::path::Path::to_path_buf).unwrap_or_default();
    Ok(crate::fs::folder_object(&cap.host, folder, ctx).into())
}

pub(crate) fn document_color_space(cap: &Cap, _ctx: &mut Context) -> JsResult<JsValue> {
    let mode = cap.host.document().get("colorMode").and_then(Value::as_str).unwrap_or("Rgb").to_ascii_lowercase();
    Ok(JsString::from(if mode.starts_with("cmyk") { "cmyk" } else { "rgb" }).into())
}

/// `activeView`: there is no window to zoom, the zoom is a number that stays as it is set.
pub(crate) fn document_view(_cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    Ok(plain_object(ctx, "View", &[("zoom", Field::Num(1.0))]).into())
}

pub(crate) fn set_selection(cap: &Cap, v: &JsValue, ctx: &mut Context) -> JsResult<()> {
    let mut ids = vec![];
    if let Some(o) = v.as_object() {
        let items: Vec<JsValue> = if o.get(js_string!("length"), ctx)?.is_undefined() {
            vec![v.clone()]
        } else {
            let len = o.get(js_string!("length"), ctx)?.to_length(ctx)?;
            if len > 1_000_000 {
                return Err(error("selection: too many items"));
            }
            (0..len).map(|i| o.get(i, ctx)).collect::<JsResult<_>>()?
        };
        for item in items {
            match nodeops::reference(&cap.host, Some(&item), ctx)? {
                Some(Ref::Node { id: Some(id), .. }) => ids.push(id),
                _ => return Err(error("selection: an item was expected")),
            }
        }
    }
    nodeops::set_selection_ids(cap, &ids)
}

pub(crate) fn active_layer(cap: &Cap, ctx: &mut Context) -> JsResult<JsValue> {
    let doc = cap.host.document();
    let id = doc.get("currentLayer").and_then(Value::as_u64).ok_or_else(|| error("the document has no layer"))?;
    let n = find_node(&doc, id).ok_or_else(|| error("the layer no longer exists"))?;
    Ok(node::node_object(&cap.host, n, ctx).into())
}

pub(crate) fn set_active_layer(cap: &Cap, v: &JsValue, ctx: &mut Context) -> JsResult<()> {
    match nodeops::reference(&cap.host, Some(v), ctx)? {
        Some(Ref::Node { id: Some(id), .. }) => exec(cap, "layer.setCurrent", json!({"id": id})).map(|_| ()),
        _ => Err(error("activeLayer: a layer was expected")),
    }
}

/// `layers.add()`: a layer at the top, which becomes the active one.
pub(crate) fn layers_add(cap: &Cap, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let made = exec(cap, "layer.new", json!({"top": true}))?;
    let id = made.get("id").and_then(Value::as_u64).ok_or_else(|| error("the layer was not made"))?;
    node::by_id(&cap.host, id, ctx)
}

pub(crate) fn document_activate(cap: &Cap, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let uid = cap.doc.ok_or_else(|| error("not a document"))?;
    cap.host.activate(uid).map_err(error)?;
    Ok(JsValue::undefined())
}

/// `close(saveOptions)`: without saving unless asked to; the document in front afterwards is the
/// engine's choice, a script sets the one it wants.
pub(crate) fn document_close(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let uid = cap.doc.ok_or_else(|| error("not a document"))?;
    let save = match args.first().filter(|v| !v.is_undefined() && !v.is_null()) {
        Some(v) => v.to_string(ctx)?.to_std_string_escaped(),
        None => "no".to_string(),
    };
    if save == "yes" {
        document_save(cap, &[], ctx)?;
    }
    let index = doc_list(&cap.host).iter().position(|d| uid_of(d) == Some(uid)).ok_or_else(|| error("the document was closed"))?;
    let before = cap.host.active_uid().filter(|u| *u != uid);
    exec(cap, "file.close", json!({"index": index, "discard": true, "force": true}))?;
    if let Some(b) = before {
        let _ = cap.host.activate(b);
    }
    Ok(JsValue::undefined())
}

pub(crate) fn document_save(cap: &Cap, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    exec(cap, "file.save", json!({}))?;
    Ok(JsValue::undefined())
}

/// `saveAs(file, options)`: the document takes the new name; options pick the format.
pub(crate) fn document_save_as(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let path = crate::fs::path_arg(args.first(), ctx)?;
    let mut params = serde_json::Map::new();
    params.insert("path".into(), json!(path.to_string_lossy()));
    if let Some(o) = args.get(1).and_then(JsValue::as_object) {
        let kind = o.get(js_string!("typename"), ctx)?.to_string(ctx)?.to_std_string_escaped();
        match kind.as_str() {
            "PDFSaveOptions" => {
                params.insert("format".into(), json!("pdf"));
                let preset = o.get(js_string!("pdfPreset"), ctx)?;
                let range = o.get(js_string!("artboardRange"), ctx)?;
                let mut options = serde_json::Map::new();
                // A preset the user made in the other application is not here: the default is used.
                let _ = preset;
                if !range.is_undefined() && !range.is_null() {
                    let r = range.to_string(ctx)?.to_std_string_escaped();
                    if !r.trim().is_empty() {
                        options.insert("range".into(), json!(r));
                    }
                }
                if !options.is_empty() {
                    params.insert("options".into(), Value::Object(options));
                }
            }
            "IllustratorSaveOptions" => {
                params.insert("format".into(), json!("ai"));
            }
            _ => {}
        }
    }
    exec(cap, "file.saveAs", Value::Object(params))?;
    Ok(JsValue::undefined())
}

/// `selectObjectsOnActiveArtboard()`
pub(crate) fn select_on_active_artboard(cap: &Cap, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let uid = cap.doc.ok_or_else(|| error("not a document"))?;
    exec(cap, "select.allOnArtboard", json!({"artboard": cap.host.active_artboard(uid)}))?;
    Ok(JsValue::undefined())
}

/// `artboards.setActiveArtboardIndex(i)`
pub(crate) fn set_active_artboard(cap: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let uid = cap.doc.ok_or_else(|| error("not a document"))?;
    let i = number(args.first().unwrap_or(&JsValue::undefined()), ctx)?;
    let count = cap.host.document().get("artboards").and_then(Value::as_array).map_or(0, Vec::len);
    if i < 0.0 || i as usize >= count {
        return Err(error("the artboard index is out of range"));
    }
    cap.host.set_active_artboard(uid, i as usize);
    Ok(JsValue::undefined())
}

pub(crate) fn get_active_artboard(cap: &Cap, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let uid = cap.doc.ok_or_else(|| error("not a document"))?;
    Ok((cap.host.active_artboard(uid) as f64).into())
}
