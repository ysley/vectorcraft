//! `File` and `Folder`: the paths a script builds, and reading, writing and listing through
//! them; also `$.evalFile`. Paths follow the scripting environment's rules: `/` separates
//! folders in any system, `~` is the user's home, `%20` stands for a space, and `fsName` gives
//! the system's own form.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
};

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, NativeFunction, Source, js_string,
    object::FunctionObjectBuilder,
    property::Attribute,
};

use crate::{
    dom::{Cap, Obj, collection_plain, error, text},
    host::Host,
    preprocess::preprocess,
};

/// Files bigger than this are not read into a script.
const MAX_READ: u64 = 64 * 1024 * 1024;

/// One `File` or `Folder` object's state.
struct Entry {
    path: PathBuf,
    folder: bool,
    io: RefCell<Io>,
}

#[derive(Default)]
struct Io {
    /// `r`, `w` or `a` while open.
    mode: Option<char>,
    /// What an open-for-reading file holds, and where reading has got to.
    data: String,
    pos: usize,
    /// What is waiting to be written.
    out: String,
    error: String,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(h) = s.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(h, 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The path a script's string names.
fn to_path(s: &str) -> PathBuf {
    let s = percent_decode(s);
    if s == "~" {
        return home();
    }
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        return home().join(rest);
    }
    // `/c/Users/x`: a drive as the first folder, the form scripts use for Windows paths.
    let b = s.as_bytes();
    if cfg!(windows) && b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b'/' {
        return PathBuf::from(format!("{}:{}", b[1] as char, &s[2..]));
    }
    PathBuf::from(s)
}

fn slashed(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn make(host: &Host, path: PathBuf, folder: bool, ctx: &mut Context) -> JsObject {
    let state = Rc::new(Entry { path, folder, io: RefCell::new(Io::default()) });
    let cap = Cap::with_state(host, state);
    let mut o = Obj::new(ctx, &cap)
        .value("typename", JsString::from(if folder { "Folder" } else { "File" }))
        .get("fsName", |c, _| Ok(text(&c.state::<Entry>()?.path.to_string_lossy())))
        .get("absoluteURI", |c, _| Ok(text(&slashed(&c.state::<Entry>()?.path))))
        .get("fullName", |c, _| Ok(text(&slashed(&c.state::<Entry>()?.path))))
        .get("name", |c, _| Ok(text(&c.state::<Entry>()?.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())))
        .get("displayName", |c, _| Ok(text(&c.state::<Entry>()?.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())))
        .get("path", |c, _| Ok(text(&c.state::<Entry>()?.path.parent().map(slashed).unwrap_or_default())))
        .get("exists", |c, _| {
            let e = c.state::<Entry>()?;
            Ok((if e.folder { e.path.is_dir() } else { e.path.is_file() }).into())
        })
        .get("parent", |c, ctx| {
            let e = c.state::<Entry>()?;
            Ok(match e.path.parent() {
                Some(p) => make(&c.host, p.to_path_buf(), true, ctx).into(),
                None => JsValue::null(),
            })
        })
        .method("toString", 0, |c, _, _| Ok(text(&slashed(&c.state::<Entry>()?.path))));
    o = if folder {
        o.method("create", 0, |c, _, _| Ok(std::fs::create_dir_all(&c.state::<Entry>()?.path).is_ok().into()))
            .method("remove", 0, |c, _, _| Ok(std::fs::remove_dir(&c.state::<Entry>()?.path).is_ok().into()))
            .method("getFiles", 1, get_files)
    } else {
        o.get("length", |c, _| Ok(std::fs::metadata(&c.state::<Entry>()?.path).map(|m| m.len() as f64).unwrap_or(0.0).into()))
            .get("eof", |c, _| {
                let io = c.state::<Entry>()?.io.borrow();
                Ok((io.pos >= io.data.len()).into())
            })
            .get("error", |c, _| Ok(text(&c.state::<Entry>()?.io.borrow().error)))
            .method("open", 1, open)
            .method("close", 0, close)
            .method("read", 1, read)
            .method("readln", 0, readln)
            .method("write", 1, |c, a, ctx| write(c, a, false, ctx))
            .method("writeln", 1, |c, a, ctx| write(c, a, true, ctx))
            .method("remove", 0, |c, _, _| Ok(std::fs::remove_file(&c.state::<Entry>()?.path).is_ok().into()))
            .method("copy", 1, |c, a, ctx| {
                let to = path_arg(a.first(), ctx)?;
                Ok(std::fs::copy(&c.state::<Entry>()?.path, to).is_ok().into())
            })
    };
    let obj = o.build();
    // Properties scripts set without effect here.
    for (k, v) in [("encoding", "UTF-8"), ("lineFeed", "unix")] {
        let _ = obj.set(JsString::from(k), JsString::from(v), false, ctx);
    }
    obj
}

/// A path given as a `File`, a `Folder` or a string.
fn path_arg(v: Option<&JsValue>, ctx: &mut Context) -> JsResult<PathBuf> {
    let Some(v) = v else { return Err(error("a file or a path is needed")) };
    if let Some(o) = v.as_object() {
        let name = o.get(js_string!("fsName"), ctx)?;
        if !name.is_undefined() {
            return Ok(PathBuf::from(name.to_string(ctx)?.to_std_string_escaped()));
        }
    }
    Ok(to_path(&v.to_string(ctx)?.to_std_string_escaped()))
}

fn open(c: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let e = c.state::<Entry>()?;
    let mode = match args.first() {
        Some(v) => v.to_string(ctx)?.to_std_string_escaped().chars().next().unwrap_or('r').to_ascii_lowercase(),
        None => 'r',
    };
    let mut io = e.io.borrow_mut();
    *io = Io::default();
    match mode {
        'r' => {
            let size = std::fs::metadata(&e.path).map(|m| m.len()).unwrap_or(0);
            if size > MAX_READ {
                io.error = "the file is too big".into();
                return Ok(false.into());
            }
            match std::fs::read(&e.path) {
                Ok(b) => {
                    io.data = String::from_utf8_lossy(b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&b)).into_owned();
                }
                Err(err) => {
                    io.error = err.to_string();
                    return Ok(false.into());
                }
            }
        }
        'w' | 'a' => {
            // Making the file now tells the script at once when it cannot be written.
            let made = if mode == 'w' { std::fs::write(&e.path, b"") } else { std::fs::OpenOptions::new().create(true).append(true).open(&e.path).map(|_| ()) };
            if let Err(err) = made {
                io.error = err.to_string();
                return Ok(false.into());
            }
        }
        _ => return Ok(false.into()),
    }
    io.mode = Some(mode);
    Ok(true.into())
}

fn close(c: &Cap, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let e = c.state::<Entry>()?;
    let mut io = e.io.borrow_mut();
    let mode = io.mode.take();
    let out = std::mem::take(&mut io.out);
    if matches!(mode, Some('w' | 'a')) && !out.is_empty() {
        use std::io::Write;
        let r = std::fs::OpenOptions::new().create(true).append(true).open(&e.path).and_then(|mut f| f.write_all(out.as_bytes()));
        if let Err(err) = r {
            io.error = err.to_string();
            return Ok(false.into());
        }
    }
    Ok(true.into())
}

fn read(c: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let e = c.state::<Entry>()?;
    let mut io = e.io.borrow_mut();
    if io.mode != Some('r') {
        return Ok(text(""));
    }
    let n = match args.first() {
        Some(v) if !v.is_undefined() => v.to_number(ctx)?,
        _ => f64::INFINITY,
    };
    let rest = io.data.get(io.pos..).unwrap_or_default();
    let taken: String = if n.is_finite() && n >= 0.0 { rest.chars().take(n as usize).collect() } else { rest.to_string() };
    io.pos += taken.len();
    Ok(text(&taken))
}

fn readln(c: &Cap, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let e = c.state::<Entry>()?;
    let mut io = e.io.borrow_mut();
    if io.mode != Some('r') || io.pos >= io.data.len() {
        return Ok(JsValue::null());
    }
    let rest = io.data.get(io.pos..).unwrap_or_default();
    let (line, used) = match rest.find('\n') {
        Some(i) => (rest[..i].trim_end_matches('\r').to_string(), i + 1),
        None => (rest.to_string(), rest.len()),
    };
    io.pos += used;
    Ok(text(&line))
}

fn write(c: &Cap, args: &[JsValue], newline: bool, ctx: &mut Context) -> JsResult<JsValue> {
    let e = c.state::<Entry>()?;
    let mut io = e.io.borrow_mut();
    if !matches!(io.mode, Some('w' | 'a')) {
        return Ok(false.into());
    }
    for a in args {
        let s = a.to_string(ctx)?.to_std_string_escaped();
        io.out.push_str(&s);
    }
    if newline {
        io.out.push('\n');
    }
    Ok(true.into())
}

/// `getFiles(mask)`: the files and folders inside; a mask is `*.ext`-style text or a function.
fn get_files(c: &Cap, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let e = c.state::<Entry>()?;
    let mask = args.first().cloned().unwrap_or_default();
    let pattern = if mask.is_string() { Some(mask.to_string(ctx)?.to_std_string_escaped().to_lowercase()) } else { None };
    let mut made = vec![];
    let mut names: Vec<_> = std::fs::read_dir(&e.path).map(|d| d.flatten().map(|x| x.path()).collect()).unwrap_or_default();
    names.sort();
    for p in names {
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if pattern.as_deref().is_some_and(|pat| !glob(pat, &name)) {
            continue;
        }
        let obj = make(&c.host, p.clone(), p.is_dir(), ctx);
        if let Some(f) = mask.as_callable()
            && !f.call(&JsValue::undefined(), &[obj.clone().into()], ctx)?.to_boolean()
        {
            continue;
        }
        made.push(obj);
    }
    Ok(collection_plain(made, ctx)?.into())
}

/// `*` and `?` wildcards, the way file masks use them.
fn glob(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], n) || (!n.is_empty() && go(p, &n[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &n[1..]),
            _ => false,
        }
    }
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    // A runaway pattern is not worth the time.
    p.len() <= 256 && go(&p, &n)
}

pub(crate) fn install(host: &Host, ctx: &mut Context) -> JsResult<()> {
    for folder in [false, true] {
        let make_fn = NativeFunction::from_copy_closure_with_captures(
            |_this, args, c: &(Host, bool), ctx| {
                let raw = match args.first() {
                    Some(v) if !v.is_undefined() && !v.is_null() => path_arg(Some(v), ctx)?,
                    _ => PathBuf::new(),
                };
                Ok(make(&c.0, raw, c.1, ctx).into())
            },
            (host.clone(), folder),
        );
        let name = if folder { "Folder" } else { "File" };
        let ctor = FunctionObjectBuilder::new(ctx.realm(), make_fn).name(JsString::from(name)).length(1).constructor(true).build();
        if folder {
            for (k, p) in [
                ("current", std::env::current_dir().unwrap_or_default()),
                ("userData", home()),
                ("appData", home()),
                ("desktop", home().join("Desktop")),
                ("myDocuments", home().join("Documents")),
                ("temp", std::env::temp_dir()),
            ] {
                let o = make(host, p, true, ctx);
                ctor.set(JsString::from(k), o, false, ctx)?;
            }
            ctor.set(js_string!("fs"), JsString::from(if cfg!(windows) { "Windows" } else if cfg!(target_os = "macos") { "Macintosh" } else { "Unix" }), false, ctx)?;
        } else {
            for k in ["openDialog", "saveDialog"] {
                let nothing = NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::null())).to_js_function(ctx.realm());
                ctor.set(JsString::from(k), nothing, false, ctx)?;
            }
        }
        ctx.register_global_property(JsString::from(name), ctor, Attribute::all())?;
    }
    Ok(())
}

/// `$.evalFile(file)`: run another script file in this one's context.
pub(crate) fn eval_file(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args.first(), ctx)?;
    let bytes = std::fs::read(&path).map_err(|e| error(format!("cannot read {}: {e}", path.display())))?;
    let source = String::from_utf8_lossy(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes)).into_owned();
    let source = preprocess(&source, path.parent()).map_err(error)?;
    ctx.eval(Source::from_bytes(&source))
}

#[cfg(test)]
mod tests {
    use vectorcraft_engine::Session;

    use crate::run;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("vc-fs-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn files_are_written_read_listed_and_evaluated() {
        let d = tmp("a");
        let dir = d.to_string_lossy().replace('\\', "/");
        let src = format!(
            r#"var f = new File("{dir}/a.txt"); f.open("w"); f.writeln("um"); f.write("dois"); f.close();
               var g = new File("{dir}/a.txt"); var ok = g.exists; g.open("r"); var l1 = g.readln(); var l2 = g.readln(); var e = g.eof; g.close();
               var s = new File("{dir}/s.jsx"); s.open("w"); s.write("var v = 40 + 2;"); s.close(); $.evalFile(s);
               var n = new Folder("{dir}").getFiles("*.txt").length;
               [ok, l1, l2, e, v, n, g.name, g.parent.exists, Folder("~").exists].join("|");"#
        );
        let mut s = Session::new();
        assert_eq!(run(&mut s, &src).unwrap().value, "true|um|dois|true|42|1|a.txt|true|true");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_file_does_not_exist_and_cannot_be_opened() {
        let mut s = Session::new();
        let out = run(&mut s, "var f = new File('/no/such/file.txt'); [f.exists, f.open('r'), f.fsName.length > 0].join('|');").unwrap();
        assert_eq!(out.value, "false|false|true");
    }
}
