//! The `#` lines at the top of a script. `#target`, `#targetengine`, `#strict`, `#script` and
//! `#engine` name the app and engine the script is for, which this runtime is not choosing
//! between, so they are left as comments. `#include "file"` is replaced by the file's text, found
//! next to the script (or where `#includepath` says), and `#includepath` adds a search folder.

use std::path::{Path, PathBuf};

/// How deep `#include`s may nest, which also stops a file that includes itself.
const MAX_DEPTH: usize = 8;

/// The script with its `#` lines dealt with. `dir` is the folder includes are found in.
pub(crate) fn preprocess(source: &str, dir: Option<&Path>) -> Result<String, String> {
    expand(source, dir, &mut vec![], 0)
}

fn expand(source: &str, dir: Option<&Path>, extra: &mut Vec<PathBuf>, depth: usize) -> Result<String, String> {
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix('#') else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        let (word, arg) = match rest.split_once(char::is_whitespace) {
            Some((w, a)) => (w, a.trim()),
            None => (rest.trim_end(), ""),
        };
        match word {
            "include" => {
                if depth >= MAX_DEPTH {
                    return Err("#include nested too deeply".into());
                }
                let name = unquote(arg);
                let path = find(name, dir, extra).ok_or_else(|| format!("#include: cannot find {name}"))?;
                let bytes = std::fs::read(&path).map_err(|e| format!("#include {name}: {e}"))?;
                let text = String::from_utf8_lossy(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes)).into_owned();
                out.push_str(&expand(&text, path.parent(), extra, depth + 1)?);
            }
            "includepath" => {
                for p in unquote(arg).split([';', ':']).filter(|p| !p.is_empty()) {
                    extra.push(resolve(p, dir));
                }
                out.push('\n');
            }
            "target" | "targetengine" | "strict" | "script" | "engine" | "strictmode" | "defines" => {
                out.push_str("//");
                out.push_str(line);
                out.push('\n');
            }
            _ => {
                // Not one of ours (a stray `#` line is a syntax error in the script itself).
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    Ok(out)
}

fn unquote(s: &str) -> &str {
    let s = s.trim().trim_end_matches(';').trim();
    s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).or_else(|| s.strip_prefix('\'').and_then(|s| s.strip_suffix('\''))).unwrap_or(s)
}

fn resolve(p: &str, dir: Option<&Path>) -> PathBuf {
    let p = Path::new(p);
    match dir {
        Some(d) if p.is_relative() => d.join(p),
        _ => p.to_path_buf(),
    }
}

fn find(name: &str, dir: Option<&Path>, extra: &[PathBuf]) -> Option<PathBuf> {
    let direct = resolve(name, dir);
    if direct.is_file() {
        return Some(direct);
    }
    extra.iter().map(|d| d.join(name)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_lines_become_comments_and_includes_are_inlined() {
        let dir = std::env::temp_dir().join(format!("vc-pre-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib.jsxinc"), "var x = 40;\n").unwrap();
        let src = "#target illustrator\n#targetengine \"main\"\n#include \"lib.jsxinc\"\nx + 2;\n";
        let out = preprocess(src, Some(&dir)).unwrap();
        assert!(out.contains("//#target illustrator") && out.contains("var x = 40;") && !out.contains("#include"));
        assert!(preprocess("#include 'nope.jsx'\n", Some(&dir)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_includes_itself_stops() {
        let dir = std::env::temp_dir().join(format!("vc-pre-self-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.jsx"), "#include 'a.jsx'\n").unwrap();
        assert!(preprocess("#include 'a.jsx'\n", Some(&dir)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
