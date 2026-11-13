// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! What a `cfg` predicate is on the one target the wasm renderer modules build for.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// A `cfg` predicate's value on wasm32-unknown-unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Truth {
    True,
    False,
    /// The target does not settle it.
    Unknown,
}

impl Truth {
    const fn not(self) -> Self {
        match self {
            Truth::True => Truth::False,
            Truth::False => Truth::True,
            Truth::Unknown => Truth::Unknown,
        }
    }
}

/// The target triple the wasm modules build for.
const TRIPLE: &str = "wasm32-unknown-unknown";

/// The target's `name = "value"` cfgs.
const KEYED: &[(&str, &str)] = &[
    ("target_arch", "wasm32"),
    ("target_os", "unknown"),
    ("target_family", "wasm"),
    ("target_vendor", "unknown"),
    ("target_env", ""),
    ("target_pointer_width", "32"),
    ("target_endian", "little"),
];

/// Evaluates `pred` on `wasm32-unknown-unknown` in a shipped build.
pub(crate) fn on_wasm32(pred: &str) -> Truth {
    evaluate(pred, &|pred| {
        if matches!(pred, "test" | "unix" | "windows") {
            return Truth::False;
        }
        if let Some((name, value)) = pred.split_once('=') {
            let value = value
                .trim()
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .filter(|v| !v.contains(['"', '\\']));
            return match (KEYED.iter().find(|(key, _)| *key == name.trim()), value) {
                (Some((_, ours)), Some(value)) if *ours == value => Truth::True,
                (Some(_), Some(_)) => Truth::False,
                _ => Truth::Unknown,
            };
        }
        Truth::Unknown
    })
}

/// With tests disabled, every other cfg atom remains unsettled.
pub(crate) fn when_test_disabled(pred: &str) -> Truth {
    evaluate(pred, &|atom| {
        if atom == "test" {
            Truth::False
        } else {
            Truth::Unknown
        }
    })
}

/// Parse the complete predicate before accepting even an absorbing boolean result.
fn evaluate(pred: &str, atom: &dyn Fn(&str) -> Truth) -> Truth {
    fn parse(input: &str, at: &mut usize, atom: &dyn Fn(&str) -> Truth) -> Option<Truth> {
        let bytes = input.as_bytes();
        let space = |at: &mut usize| {
            while bytes.get(*at).is_some_and(u8::is_ascii_whitespace) {
                *at += 1;
            }
        };
        space(at);
        let start = *at;
        if !bytes
            .get(*at)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            return None;
        }
        *at += 1;
        while bytes
            .get(*at)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            *at += 1;
        }
        let name = &input[start..*at];
        space(at);
        if bytes.get(*at) == Some(&b'(') {
            *at += 1;
            space(at);
            let mut values = Vec::new();
            if bytes.get(*at) != Some(&b')') {
                loop {
                    values.push(parse(input, at, atom)?);
                    space(at);
                    if bytes.get(*at) != Some(&b',') {
                        break;
                    }
                    *at += 1;
                    space(at);
                    if bytes.get(*at) == Some(&b')') {
                        break;
                    }
                }
            }
            if bytes.get(*at) != Some(&b')') {
                return None;
            }
            *at += 1;
            return match name {
                "not" if values.len() == 1 => Some(values[0].not()),
                "not" => None,
                "all" if values.contains(&Truth::False) => Some(Truth::False),
                "any" if values.contains(&Truth::True) => Some(Truth::True),
                "all" | "any" if values.contains(&Truth::Unknown) => Some(Truth::Unknown),
                "all" => Some(Truth::True),
                "any" => Some(Truth::False),
                _ => None,
            };
        }
        if bytes.get(*at) == Some(&b'=') {
            *at += 1;
            space(at);
            if bytes.get(*at) != Some(&b'"') {
                return None;
            }
            *at += 1;
            while let Some(&byte) = bytes.get(*at) {
                if byte == b'"' {
                    break;
                }
                if byte == b'\\' {
                    return None;
                }
                *at += 1;
            }
            if bytes.get(*at) != Some(&b'"') {
                return None;
            }
            *at += 1;
        }
        Some(atom(input[start..*at].trim()))
    }
    let mut at = 0;
    let Some(value) = parse(pred, &mut at, atom) else {
        return Truth::Unknown;
    };
    if pred[at..].trim().is_empty() {
        value
    } else {
        Truth::Unknown
    }
}

/// The arguments of `head(..)` when `pred` is exactly that call.
fn call<'a>(pred: &'a str, head: &str) -> Option<&'a str> {
    pred.strip_prefix(head)?
        .trim_start()
        .strip_prefix('(')?
        .strip_suffix(')')
}

/// Whether a `[target.<key>.dependencies]` table can link into a wasm32 build.
pub(crate) fn target_links_on_wasm32(key: &str) -> bool {
    match call(key.trim(), "cfg") {
        Some(pred) => on_wasm32(pred) != Truth::False,
        None => key.trim() == TRIPLE,
    }
}

/// Whether an attribute line (trimmed) proves its item absent from every shipped wasm32 build.
pub(crate) fn attr_never_on_wasm32(attr: &str) -> bool {
    if attr == "#[test]" {
        return true;
    }
    attr.strip_prefix("#[cfg(")
        .and_then(|rest| rest.strip_suffix(")]"))
        .is_some_and(|pred| on_wasm32(pred) == Truth::False)
}

/// One crate's `src` files no shipped wasm32 build loads ([`unloaded_on_wasm32`]).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Unloaded {
    /// The files every loader of which is false on wasm32 or sits in such a file, sorted.
    pub(crate) files: BTreeSet<PathBuf>,
    /// The false loaders that cut them off, in file order.
    pub(crate) cuts: Vec<Cut>,
    /// Why no file is left out though a declaration is false on wasm32.
    pub(crate) unplaced: Option<String>,
}

/// A false loader whose file is out ([`Unloaded::cuts`]).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Cut {
    /// The file it is in, and its 1-based line.
    pub(crate) file: PathBuf,
    pub(crate) line: usize,
    /// The loader as written (`mod metal;`).
    pub(crate) loader: String,
    /// The attribute that masks it (`#[cfg(target_os = "macos")]`).
    pub(crate) gate: String,
}

/// One loader of a file.
struct ModDecl<'a> {
    from: &'a Path,
    /// 0-based.
    line: usize,
    /// The module's name (empty for an `include!`).
    name: &'a str,
    /// Its `#[path = ".."]`, or the `include!`'s path.
    path: Option<&'a str>,
    /// An `include!`: its file is text spliced into `from`, no module.
    include: bool,
    /// Kept by [`attr_never_on_wasm32`]'s masking.
    live: bool,
}

/// What an `include!(..)` on a line names.
enum IncludeArg<'a> {
    /// A plain string path, relative to the including file's directory.
    Plain(&'a str),
    /// A file under `OUT_DIR`: built, never one of `src`'s.
    OutDir,
    /// Anything else.
    Unknown,
}

/// Files in a crate's `src` tree that no shipped `wasm32-unknown-unknown` build compiles.
pub(crate) fn unloaded_on_wasm32(
    files: &BTreeMap<PathBuf, String>,
    roots: &BTreeSet<PathBuf>,
) -> Unloaded {
    let mut decls: Vec<ModDecl<'_>> = Vec::new();
    let mut unplaced: Option<String> = None;
    let mut cannot = |file: &Path, line: usize, why: &str| {
        unplaced.get_or_insert_with(|| format!("{}:{} {why}", file.display(), line + 1));
    };
    // The lines and masking of each file the loop lexes, which the cuts below name their gates by.
    let mut views: BTreeMap<&Path, (Vec<&str>, Vec<bool>)> = BTreeMap::new();
    for (file, text) in files {
        // The loop acts only on a line that may load a file: a file with none is not lexed.
        if !text.lines().any(may_load) {
            continue;
        }
        let lines: Vec<&str> = text.lines().collect();
        let keep = crate::lock_order::unmasked_lines(text, &attr_never_on_wasm32);
        let scope_code = crate::lex::blank_non_code(text);
        let mut depth = 0isize;
        for (i, (raw, scope_line)) in lines.iter().zip(scope_code.lines()).enumerate() {
            let nested = depth != 0;
            for b in scope_line.bytes() {
                match b {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
            }
            let code = crate::lock_order::mask_literals(crate::strip_line_comment(raw));
            let live = keep.get(i).copied().unwrap_or(true);
            if code.contains("include!(") {
                match include_arg(raw) {
                    IncludeArg::Plain(p) => decls.push(ModDecl {
                        from: file,
                        line: i,
                        name: "",
                        path: Some(p),
                        include: true,
                        live,
                    }),
                    IncludeArg::OutDir => {}
                    IncludeArg::Unknown if live => {
                        cannot(
                            file,
                            i,
                            "includes a file this reader cannot place (`include!`)",
                        );
                    }
                    IncludeArg::Unknown => {}
                }
            }
            let name = match mod_line(raw, &code) {
                ModLine::File(name) => name,
                ModLine::None | ModLine::InlineOpen => continue,
                ModLine::Unplaced => {
                    // A compact inline module (`mod outer { mod live; }`) or
                    // another declaration on this line can still load a file.
                    if live {
                        cannot(file, i, "declares a module this line-reader cannot place");
                    }
                    continue;
                }
            };
            let mut path = None;
            for attr in lines[..i]
                .iter()
                .rev()
                .map(|l| l.trim())
                .take_while(|t| t.starts_with("#[") || t.starts_with("//"))
            {
                if attr.starts_with("#[path") {
                    match path_attr(attr) {
                        Some(p) => path = Some(p),
                        None => cannot(
                            file,
                            i,
                            "names its file in a `path` this reader cannot read",
                        ),
                    }
                } else if attr.starts_with("#[cfg_attr(") && attr.contains("path") {
                    cannot(file, i, "has its file chosen by a `cfg_attr` path");
                }
            }
            if nested || raw.starts_with(char::is_whitespace) {
                if live {
                    cannot(file, i, "declares a module inside another item");
                }
                continue;
            }
            decls.push(ModDecl {
                from: file,
                line: i,
                name,
                path,
                include: false,
                live,
            });
        }
        views.insert(file, (lines, keep));
    }
    let dir = |f: &Path| f.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    let included: BTreeSet<PathBuf> = decls
        .iter()
        .filter(|d| d.include)
        .filter_map(|d| d.path.map(|p| normalize(&dir(d.from).join(p))))
        .collect();
    if let Some(d) = decls
        .iter()
        .find(|d| !d.include && included.contains(d.from))
    {
        cannot(d.from, d.line, "declares a module in an included file");
    }
    if decls.iter().all(|d| d.live) {
        return Unloaded::default();
    }
    if let Some(why) = unplaced {
        return Unloaded {
            unplaced: Some(why),
            ..Unloaded::default()
        };
    }
    let path_loaded: BTreeSet<PathBuf> = decls
        .iter()
        .filter(|d| !d.include)
        .filter_map(|d| d.path.map(|p| normalize(&dir(d.from).join(p))))
        .collect();
    let targets = |d: &ModDecl<'_>| -> Vec<PathBuf> {
        if let Some(p) = d.path {
            return vec![normalize(&dir(d.from).join(p))];
        }
        let mod_rs = matches!(
            d.from.file_name().and_then(|n| n.to_str()),
            Some("lib.rs" | "main.rs" | "mod.rs")
        );
        let mut owners = Vec::new();
        if mod_rs || path_loaded.contains(d.from) || roots.contains(d.from) {
            owners.push(dir(d.from));
        }
        if !mod_rs && let Some(stem) = d.from.file_stem() {
            owners.push(dir(d.from).join(stem));
        }
        owners
            .iter()
            .flat_map(|o| {
                [
                    o.join(format!("{}.rs", d.name)),
                    o.join(d.name).join("mod.rs"),
                ]
            })
            .filter(|t| files.contains_key(t))
            .collect()
    };
    let mut loaders: BTreeMap<PathBuf, Vec<(&Path, bool)>> = BTreeMap::new();
    let mut decl_targets = Vec::with_capacity(decls.len());
    for d in &decls {
        let ts = targets(d);
        for t in &ts {
            loaders.entry(t.clone()).or_default().push((d.from, d.live));
        }
        decl_targets.push(ts);
    }
    let mut out: BTreeSet<PathBuf> = BTreeSet::new();
    loop {
        let before = out.len();
        for (file, by) in &loaders {
            if !roots.contains(file)
                && !out.contains(file)
                && by.iter().all(|(from, live)| !live || out.contains(*from))
            {
                out.insert(file.clone());
            }
        }
        if out.len() == before {
            break;
        }
    }
    let mut cuts = Vec::new();
    for (d, ts) in decls.iter().zip(&decl_targets) {
        if d.live || !ts.iter().any(|t| out.contains(t)) || out.contains(d.from) {
            continue;
        }
        let (lines, keep) = &views[&d.from];
        let own = lines[..d.line]
            .iter()
            .rev()
            .map(|l| l.trim())
            .take_while(|t| t.starts_with("#[") || t.starts_with("//"))
            .find(|t| attr_never_on_wasm32(t));
        let indent = |l: &str| l.len() - l.trim_start().len();
        let enclosing = || {
            (0..d.line)
                .rev()
                .take_while(|j| !keep.get(*j).copied().unwrap_or(true))
                .map(|j| lines[j])
                .find(|l| indent(l) < indent(lines[d.line]) && attr_never_on_wasm32(l.trim()))
                .map(str::trim)
        };
        let gate = own.or_else(enclosing).unwrap_or("a gated item");
        cuts.push(Cut {
            file: d.from.to_path_buf(),
            line: d.line + 1,
            loader: lines[d.line].trim().to_string(),
            gate: gate.to_string(),
        });
    }
    Unloaded {
        files: out,
        cuts,
        unplaced: None,
    }
}

/// Whether [`unloaded_on_wasm32`] acts on the source line `raw`: an `include!(` in its code, or a
/// `mod` that is neither absent nor an inline module's header.
fn may_load(raw: &str) -> bool {
    // `code` is a prefix of `raw` with its literals blanked in place, so it spells nothing `raw`
    // does not.
    if !crate::mentions(raw, "mod") && !crate::mentions(raw, "include!(") {
        return false;
    }
    let code = crate::lock_order::mask_literals(crate::strip_line_comment(raw));
    code.contains("include!(")
        || matches!(mod_line(raw, &code), ModLine::File(_) | ModLine::Unplaced)
}

/// What a line says about a `mod` declaration.
enum ModLine<'a> {
    /// No `mod` keyword on this line.
    None,
    /// A body-less `[pub[(..)]] mod [r#]<name>;` declaration.
    File(&'a str),
    /// An inline module header (`[pub[(..)]] mod [r#]<name> {` or `{}`).
    InlineOpen,
    /// A `mod` token in any other form.
    Unplaced,
}

/// Classifies a source line for [`unloaded_on_wasm32`].
fn mod_line<'a>(raw: &'a str, code: &str) -> ModLine<'a> {
    if !code
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|word| word == "mod")
    {
        return ModLine::None;
    }
    let code = code.trim();
    let mut rest = code;
    if let Some(after) = rest.strip_prefix("pub") {
        let after = after.trim_start();
        rest = match after.strip_prefix('(') {
            Some(scope) => match scope.split_once(')') {
                Some((_, tail)) => tail.trim_start(),
                None => return ModLine::Unplaced,
            },
            None if after.starts_with("mod ") => after,
            None => return ModLine::Unplaced,
        };
    }
    let Some(rest) = rest.strip_prefix("mod") else {
        return ModLine::Unplaced;
    };
    if !rest.starts_with(char::is_whitespace) {
        return ModLine::Unplaced;
    }
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("r#").unwrap_or(rest);
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let (name, tail) = rest.split_at(end);
    if name.is_empty() {
        return ModLine::Unplaced;
    }
    match tail.trim() {
        ";" => {
            // Hand back the name as `raw` spells it (masking keeps byte offsets).
            match raw.find(name) {
                Some(at) => ModLine::File(&raw[at..at + name.len()]),
                None => ModLine::Unplaced,
            }
        }
        "{" | "{}" => ModLine::InlineOpen,
        _ => ModLine::Unplaced,
    }
}

/// What the `include!(..)` on `raw` names.
fn include_arg(raw: &str) -> IncludeArg<'_> {
    let Some(at) = raw.find("include!(") else {
        return IncludeArg::Unknown;
    };
    let arg = raw[at + "include!(".len()..].trim_start();
    if arg.starts_with("concat!(env!(\"OUT_DIR\")") {
        return IncludeArg::OutDir;
    }
    let plain = arg
        .strip_prefix('"')
        .and_then(|rest| rest.split_once('"'))
        .filter(|(p, tail)| {
            !p.is_empty() && !p.contains('\\') && tail.trim_start().starts_with(')')
        })
        .map(|(p, _)| p);
    plain.map_or(IncludeArg::Unknown, IncludeArg::Plain)
}

/// The file a `#[path = "<p>"]` attribute names, when it is that plain form.
fn path_attr(attr: &str) -> Option<&str> {
    let rest = attr
        .strip_prefix("#[path")?
        .trim_start()
        .strip_prefix('=')?
        .trim_start()
        .strip_prefix('"')?;
    let (p, tail) = rest.split_once('"')?;
    (tail.trim() == "]" && !p.contains('\\') && !p.is_empty()).then_some(p)
}

/// `p` with `.` and `..` resolved lexically (`a/b/../c` is `a/c`).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
#[path = "wasm_cfg_tests.rs"]
mod tests;
