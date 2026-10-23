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

/// The target's `name = "value"` cfgs: each name has exactly this value on
/// wasm32-unknown-unknown, so any other value is false.
const KEYED: &[(&str, &str)] = &[
    ("target_arch", "wasm32"),
    ("target_os", "unknown"),
    ("target_family", "wasm"),
    ("target_vendor", "unknown"),
    ("target_env", ""),
    ("target_pointer_width", "32"),
    ("target_endian", "little"),
];

/// `pred` (a `cfg` predicate, without its `cfg(..)` wrapper) on
/// wasm32-unknown-unknown in a shipped build: `test`, `unix` and `windows`
/// are false; [`KEYED`] names compare against the target's values;
/// `all`/`any`/`not` combine three-valued; anything else, or a predicate this
/// reader cannot parse, is [`Truth::Unknown`].
pub(crate) fn on_wasm32(pred: &str) -> Truth {
    let pred = pred.trim();
    if let Some(inner) = call(pred, "not") {
        let args = crate::split_top_level_commas(inner);
        return match args.as_slice() {
            [one] => on_wasm32(one).not(),
            _ => Truth::Unknown,
        };
    }
    for (head, is_all) in [("all", true), ("any", false)] {
        if let Some(inner) = call(pred, head) {
            let (mut any_unknown, absorbing) =
                (false, if is_all { Truth::False } else { Truth::True });
            for arg in crate::split_top_level_commas(inner) {
                match on_wasm32(arg) {
                    t if t == absorbing => return absorbing,
                    Truth::Unknown => any_unknown = true,
                    _ => {}
                }
            }
            return if any_unknown {
                Truth::Unknown
            } else {
                absorbing.not()
            };
        }
    }
    match pred {
        "test" | "unix" | "windows" => return Truth::False,
        _ => {}
    }
    if let Some((name, value)) = pred.split_once('=') {
        let (name, value) = (name.trim(), value.trim());
        let Some(value) = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .filter(|v| !v.contains(['"', '\\']))
        else {
            return Truth::Unknown;
        };
        return match KEYED.iter().find(|(key, _)| *key == name) {
            Some((_, ours)) if *ours == value => Truth::True,
            Some(_) => Truth::False,
            None => Truth::Unknown,
        };
    }
    Truth::Unknown
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

/// One loader of a file: a `mod <name>;` declaration (no body) or an
/// `include!("<path>")`.
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

/// The files of one crate's `src` tree (`files`: every `*.rs` in it, with its
/// text) that no shipped wasm32-unknown-unknown build compiles: every loader
/// that could load one — a `mod <name>;` declaration, an `include!` — is
/// false there (masked by [`attr_never_on_wasm32`], as
/// [`crate::lock_order::unmasked_lines`] masks it) or sits in such a file.
pub(crate) fn unloaded_on_wasm32(
    files: &BTreeMap<PathBuf, String>,
    roots: &BTreeSet<PathBuf>,
) -> Unloaded {
    let mut decls: Vec<ModDecl<'_>> = Vec::new();
    let mut unplaced: Option<String> = None;
    let mut cannot = |file: &Path, line: usize, why: &str| {
        unplaced.get_or_insert_with(|| format!("{}:{} {why}", file.display(), line + 1));
    };
    for (file, text) in files {
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
        let text = &files[d.from];
        let lines: Vec<&str> = text.lines().collect();
        let own = lines[..d.line]
            .iter()
            .rev()
            .map(|l| l.trim())
            .take_while(|t| t.starts_with("#[") || t.starts_with("//"))
            .find(|t| attr_never_on_wasm32(t));
        let indent = |l: &str| l.len() - l.trim_start().len();
        let keep = crate::lock_order::unmasked_lines(text, &attr_never_on_wasm32);
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

/// What a line says about a `mod` declaration.
enum ModLine<'a> {
    /// No `mod` keyword on this line.
    None,
    /// A body-less `[pub[(..)]] mod [r#]<name>;` declaration.
    File(&'a str),
    /// An inline module header (`[pub[(..)]] mod [r#]<name> {` or `{}`): its
    /// body lines are scanned on their own, and any `mod` inside it is caught
    /// there.
    InlineOpen,
    /// A `mod` token in any other form (`mod outer { mod live; }`, a `mod`
    /// sharing a line with another item).
    Unplaced,
}

/// Classify a line (`raw`, with its comments stripped and literals masked in
/// `code`) for [`unloaded_on_wasm32`].
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
mod tests {
    use super::*;

    #[test]
    fn the_platforms_wasm32_is_not_are_false_and_its_own_are_true() {
        for pred in [
            "unix",
            "windows",
            "test",
            "target_os = \"macos\"",
            "target_os = \"linux\"",
            "target_vendor = \"apple\"",
            "target_family = \"unix\"",
            "target_arch = \"aarch64\"",
            "not(target_arch = \"wasm32\")",
            "any(unix, windows)",
            "all(windows, any(test, debug_assertions))",
            "any(target_vendor = \"apple\", target_os = \"linux\")",
        ] {
            assert_eq!(on_wasm32(pred), Truth::False, "{pred}");
        }
        for pred in [
            "target_arch = \"wasm32\"",
            "target_family = \"wasm\"",
            "target_os = \"unknown\"",
            "not(unix)",
            "not(any(unix, windows))",
            "any(unix, target_arch = \"wasm32\")",
            "all(not(windows), target_pointer_width = \"32\")",
        ] {
            assert_eq!(on_wasm32(pred), Truth::True, "{pred}");
        }
    }

    #[test]
    fn what_the_target_does_not_settle_is_unknown() {
        for pred in [
            "debug_assertions",
            "feature = \"dev-seams\"",
            "trust_verify",
            "target_feature = \"atomics\"",
            "any(test, debug_assertions)",
            "not(feature = \"x\")",
            "all(target_arch = \"wasm32\", feature = \"x\")",
            "any(windows, feature = \"x\")",
            r#"target_arch = "wasm\u{33}2""#,
            r#"target_arch = "wasm\x33\x32""#,
            "not(unix, windows)",
            "target_os = macos",
            "",
        ] {
            assert_eq!(on_wasm32(pred), Truth::Unknown, "{pred}");
        }
    }

    #[test]
    fn a_target_table_links_unless_it_is_false_on_wasm32() {
        assert!(!target_links_on_wasm32("cfg(windows)"));
        assert!(!target_links_on_wasm32("cfg(any(unix, windows))"));
        assert!(!target_links_on_wasm32("cfg(target_os = \"macos\")"));
        assert!(!target_links_on_wasm32("x86_64-pc-windows-msvc"));
        assert!(target_links_on_wasm32("cfg(target_arch = \"wasm32\")"));
        assert!(target_links_on_wasm32("cfg(feature = \"x\")"));
        assert!(target_links_on_wasm32("cfg(not(unix))"));
        assert!(target_links_on_wasm32("wasm32-unknown-unknown"));
    }

    #[test]
    fn only_a_test_or_a_false_gate_drops_an_item() {
        assert!(attr_never_on_wasm32("#[test]"));
        assert!(attr_never_on_wasm32("#[cfg(test)]"));
        assert!(attr_never_on_wasm32("#[cfg(windows)]"));
        assert!(attr_never_on_wasm32(
            "#[cfg(not(target_arch = \"wasm32\"))]"
        ));
        assert!(!attr_never_on_wasm32("#[cfg(debug_assertions)]"));
        assert!(!attr_never_on_wasm32("#[cfg(not(windows))]"));
        assert!(!attr_never_on_wasm32(
            "#[cfg_attr(windows, allow(dead_code))]"
        ));
        assert!(!attr_never_on_wasm32("#[cfg(windows)] fn f() {}"));
    }

    /// A crate's `src` tree from (path under `src/`, text) pairs, rooted at
    /// `/c/src`, and its default roots.
    fn src_tree(files: &[(&str, &str)]) -> (BTreeMap<PathBuf, String>, BTreeSet<PathBuf>) {
        let map = files
            .iter()
            .map(|(p, t)| (Path::new("/c/src").join(p), (*t).to_string()))
            .collect();
        let roots = [Path::new("/c/src/lib.rs").to_path_buf()].into();
        (map, roots)
    }

    fn names(set: &BTreeSet<PathBuf>) -> Vec<String> {
        set.iter()
            .map(|p| p.strip_prefix("/c/src").unwrap().display().to_string())
            .collect()
    }

    /// A MODULE ONLY A FALSE GATE LOADS IS NO WASM32 FILE: a macOS-only
    /// `mod metal;` leaves out `metal/mod.rs` and, through it, the file its
    /// own (ungated) `mod worker;` loads; a non-mod-rs file's `#[cfg(windows)]
    /// mod b;` leaves out `a/b.rs`. CONTROLS: a feature-gated module (the
    /// target does not settle it), a plain one and its children, a sibling
    /// beside a cut one, and a file a false `#[path]` and a live `mod` both
    /// load all stay in.
    #[test]
    fn a_module_only_a_false_gate_loads_is_no_wasm32_file() {
        let (files, roots) = src_tree(&[
            (
                "lib.rs",
                "// The Metal arm.\n#[cfg(target_os = \"macos\")]\nmod metal;\n\
                 #[cfg(feature = \"x\")]\nmod featured;\npub(crate) mod plain;\n\
                 #[cfg(all(test, target_os = \"macos\"))]\n#[path = \"plain/shared.rs\"]\n\
                 mod shared_twin;\npub mod a;\n",
            ),
            ("metal/mod.rs", "mod worker;\npub fn f() {}\n"),
            (
                "metal/worker.rs",
                "pub fn go() {\n    std::thread::spawn(|| {});\n}\n",
            ),
            ("featured.rs", "pub fn g() {}\n"),
            ("plain/mod.rs", "mod shared;\n"),
            ("plain/shared.rs", "pub fn s() {}\n"),
            ("a.rs", "#[cfg(windows)]\nmod b;\nmod c;\n"),
            ("a/b.rs", "pub fn b() {}\n"),
            ("a/c.rs", "pub fn c() {}\n"),
        ]);
        let got = unloaded_on_wasm32(&files, &roots);
        assert_eq!(
            names(&got.files),
            ["a/b.rs", "metal/mod.rs", "metal/worker.rs"],
            "{got:?}"
        );
        assert_eq!(got.unplaced, None);
        let cuts: Vec<String> = got
            .cuts
            .iter()
            .map(|c| {
                format!(
                    "{}:{} `{}` under {}",
                    c.file.display(),
                    c.line,
                    c.loader,
                    c.gate
                )
            })
            .collect();
        assert_eq!(
            cuts,
            [
                "/c/src/a.rs:2 `mod b;` under #[cfg(windows)]",
                "/c/src/lib.rs:3 `mod metal;` under #[cfg(target_os = \"macos\")]",
            ]
        );
    }

    /// A root is never left out, even when a false declaration names it
    /// (a `main.rs` beside a custom lib root, `src/lib2.rs`), and a root that
    /// is not a mod-rs file loads its children from beside it, as rustc
    /// reads a crate root: `inner.rs`, which a false declaration also names,
    /// stays in by the root's live one.
    #[test]
    fn a_crate_root_is_never_left_out() {
        let (files, _) = src_tree(&[
            (
                "main.rs",
                "#[cfg(windows)]\nmod lib2;\n#[cfg(windows)]\nmod inner;\n",
            ),
            ("lib2.rs", "mod inner;\n"),
            ("inner.rs", "pub fn i() {}\n"),
        ]);
        let roots: BTreeSet<PathBuf> = [Path::new("/c/src/lib2.rs").to_path_buf()].into();
        let got = unloaded_on_wasm32(&files, &roots);
        assert!(got.files.is_empty(), "{got:?}");
    }

    /// A loader this reader cannot place leaves every file in, and says
    /// why: a live `mod` inside an inline module, a live `include!` of a
    /// computed path, a `cfg_attr` path, a `path` it cannot read. CONTROL:
    /// the same tree without it leaves the gated module out; a masked
    /// inline loader is no loader.
    #[test]
    fn a_module_tree_this_reader_cannot_place_leaves_every_file_in() {
        let gated = "#[cfg(windows)]\nmod win;\n";
        for (what, extra) in [
            ("inline", "mod outer {\n    mod win;\n}\n"),
            ("include", "include!(concat!(\"gen\", \".rs\"));\n"),
            ("cfg_attr", "#[cfg_attr(unix, path = \"u.rs\")]\nmod imp;\n"),
            ("path", "#[path = concat!(\"w\", \".rs\")]\nmod imp;\n"),
        ] {
            let lib = format!("{gated}{extra}");
            let (files, roots) = src_tree(&[("lib.rs", &lib), ("win.rs", "pub fn w() {}\n")]);
            let got = unloaded_on_wasm32(&files, &roots);
            assert!(got.files.is_empty(), "{what}: {got:?}");
            assert!(got.unplaced.is_some(), "{what}: says why");
        }
        let false_alias = "#[cfg(windows)]\n#[path = \"outer/live.rs\"]\nmod hidden;\n";
        for (extra, stays_in) in [
            ("mod outer { mod live; }\n", true),
            ("mod outer {\nmod live;\n}\n", true),
            ("mod outer {\n    mod live;\n}\n", true),
            (
                "mod outer {\nconst S: &str = r#\"\n}\n\"#;\n/* } */\nmod live;\n}\n",
                true,
            ),
            ("", false),
            ("mod outer {}\n", false),
            ("#[cfg(windows)]\nmod outer {\nmod live;\n}\n", false),
        ] {
            let lib = format!("{false_alias}{extra}");
            let (files, roots) =
                src_tree(&[("lib.rs", &lib), ("outer/live.rs", "pub fn live() {}\n")]);
            let got = unloaded_on_wasm32(&files, &roots);
            if stays_in {
                assert!(
                    got.files.is_empty(),
                    "live nested loader {extra:?}: {got:?}"
                );
                assert!(got.unplaced.is_some(), "nested loader is not placed");
            } else {
                assert_eq!(names(&got.files), ["outer/live.rs"], "CONTROL: {got:?}");
                assert_eq!(got.unplaced, None);
            }
        }
        let lib = format!(
            "{false_alias}mod harmless {{\n    fn f() {{}}\n}}\n\
             #[path = \"outer/live.rs\"]\nmod live;\n"
        );
        let (files, roots) = src_tree(&[("lib.rs", &lib), ("outer/live.rs", "pub fn live() {}\n")]);
        let got = unloaded_on_wasm32(&files, &roots);
        assert!(
            got.files.is_empty(),
            "root loader after inline module: {got:?}"
        );
        assert_eq!(got.unplaced, None, "the root loader is placed");
        for extra in [
            "",
            "#[cfg(windows)]\nmod outer {\n    mod win;\n}\n",
            "pub(crate) mod webgl {\n    fn f() {}\n}\n",
        ] {
            let lib = format!("{gated}{extra}");
            let (files, roots) = src_tree(&[("lib.rs", &lib), ("win.rs", "pub fn w() {}\n")]);
            let got = unloaded_on_wasm32(&files, &roots);
            assert_eq!(names(&got.files), ["win.rs"], "CONTROL {extra:?}: {got:?}");
        }
    }

    /// AN `include!` IS A LOADER: a file only a false `include!` splices in
    /// is out (and so is the file only it declares, through it: here none,
    /// since a `mod` in an included file is not placed); a file a live
    /// `include!` splices in stays in though a false `mod` names it too; an
    /// `OUT_DIR` include names no `src` file. A `mod` declared in an
    /// included file leaves every file in.
    #[test]
    fn an_include_is_a_loader_like_a_mod() {
        let (files, roots) = src_tree(&[
            (
                "lib.rs",
                "#[cfg(windows)]\nfn win() {\n    include!(\"win_gen.rs\");\n}\n\
                 fn table() {\n    include!(\"table_gen.rs\");\n}\n\
                 #[cfg(windows)]\nmod table_gen;\n\
                 include!(concat!(env!(\"OUT_DIR\"), \"/built.rs\"));\n",
            ),
            ("win_gen.rs", "pub fn w() {}\n"),
            ("table_gen.rs", "pub fn t() {}\n"),
        ]);
        let got = unloaded_on_wasm32(&files, &roots);
        assert_eq!(names(&got.files), ["win_gen.rs"], "{got:?}");
        assert_eq!(got.unplaced, None);
        assert_eq!(
            got.cuts
                .iter()
                .map(|c| (c.line, c.gate.as_str()))
                .collect::<Vec<_>>(),
            [(3, "#[cfg(windows)]")],
            "the masked fn's gate is the include's: {got:?}"
        );
        let (files, roots) = src_tree(&[
            (
                "lib.rs",
                "#[cfg(windows)]\nmod win;\ninclude!(\"parts.rs\");\n",
            ),
            ("parts.rs", "mod part;\n"),
            ("part.rs", "pub fn p() {}\n"),
            ("win.rs", "pub fn w() {}\n"),
        ]);
        let got = unloaded_on_wasm32(&files, &roots);
        assert!(got.files.is_empty(), "{got:?}");
        assert!(
            got.unplaced
                .is_some_and(|why| why.contains("included file"))
        );
    }
}
