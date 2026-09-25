// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Platform-cfg parity: a `crate::<module>::<item>` reference must still resolve on every
//! target `atpkg` is compiled for.
//!
//! `#[cfg(unix)]` on a definition is invisible from a Unix box — the crate builds, the tests
//! pass, `tippy` is quiet, and the only reader that disagrees is a compiler this macOS fleet
//! runs at a release cut. A helper gated `unix` with no `not(unix)` twin, called from an
//! ungated caller, stops `atpkg` compiling for `x86_64-pc-windows-msvc`; a test that loses
//! its `#[cfg(target_os = "macos")]` while calling a macOS-only helper costs the whole
//! unit-test binary on Linux and Windows. Both shapes have been live here.
//!
//! The law: for every reference spelled `crate::<module>::<name>` resolving to an item
//! declared in that module's own file, at least one declaration of the name must survive
//! wherever the reference site survives. A name whose declarations cover both sides of a
//! predicate (`unix` + `not(unix)`, the twin shape `seam.rs` uses four times over), or the
//! exact three-way `unix` / `windows` / `not(any(unix, windows))` partition, is exempt,
//! as is one with any declaration carrying no platform cfg.
//!
//! `cargo xtask gate cells` really cross-compiles and stays the authority; this rides along
//! with `cargo test -p atpkg`, reading only committed sources under `CARGO_MANIFEST_DIR`.
//! Only `crate::`-qualified paths are judged (a bare name needs a real resolver), only
//! `atpkg`, and the cfg algebra is shallow: anything subtler than `all`/`any` split into
//! conjuncts and disjuncts is treated as covered rather than as a violation, because a false
//! red teaches people to delete the guard. Hence `the_scan_still_sees_the_crate`: a scanner
//! that quietly stopped finding anything would otherwise pass for ever.
//! violation, because a false red teaches people to delete the guard; a construct whose
//! extent the lexer cannot pin over-gates rather than under-gates, for the same reason. Those
//! limits are why `the_scan_still_sees_the_crate` exists: a scanner that quietly stopped
//! finding anything would otherwise pass for ever.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

// The law

/// `cfg` keys that name a target rather than a build choice. A predicate mentioning none of
/// these (`test`, `feature = "x"`, `debug_assertions`) is still tracked — it is part of what
/// a reference site survives — but never by itself makes a declaration this file's business.
const PLATFORM_KEYS: &[&str] = &[
    "unix",
    "windows",
    "target_os",
    "target_family",
    "target_arch",
    "target_env",
    "target_vendor",
    "target_pointer_width",
    "target_endian",
];

/// One reference that does not resolve everywhere its own line does.
struct Violation {
    file: String,
    line: usize,
    path: String,
    declared: Vec<String>,
    active: Vec<String>,
    source: String,
}

/// The law over an in-memory tree: keys are paths relative to `src/` (`seam.rs`,
/// `platform/mod.rs`), values the file's source. Taking a map rather than a directory is what
/// lets the red fixtures below be literals.
fn violations(tree: &BTreeMap<String, String>) -> Vec<Violation> {
    let scans: BTreeMap<&str, Scan> = tree
        .iter()
        .map(|(rel, src)| (rel.as_str(), Scan::of(src)))
        .collect();
    let modules = module_map(tree);
    let mut found = Vec::new();
    for (rel, scan) in &scans {
        for (i, code) in scan.code.iter().enumerate() {
            let trimmed = code.trim();
            if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
                continue;
            }
            for (module, name) in references(code) {
                let Some(home) = modules.get(module.as_str()) else {
                    continue;
                };
                let Some(target) = scans.get(home.as_str()) else {
                    continue;
                };
                let gates: Vec<&Vec<String>> = target
                    .items
                    .iter()
                    .filter(|d| d.name == name)
                    .map(|d| &d.gates)
                    .collect();
                if gates.is_empty() {
                    continue;
                }
                let every: Vec<&String> = gates.iter().copied().flatten().collect();
                if !every.iter().any(|p| mentions_platform(p)) {
                    continue;
                }
                if twinned(&every) || unix_windows_other_partition(&gates) {
                    continue;
                }
                let active = &scan.active[i];
                if covered(&gates, active) {
                    continue;
                }
                found.push(Violation {
                    file: (*rel).to_string(),
                    line: i + 1,
                    path: if module.is_empty() {
                        format!("crate::{name}")
                    } else {
                        format!("crate::{module}::{name}")
                    },
                    declared: gates.iter().map(|g| render(g)).collect(),
                    active: active.clone(),
                    source: scan.raw[i].trim().to_string(),
                });
            }
        }
    }
    found
}

/// `src/`-relative path -> the module path a `crate::…` reference spells it with. `lib.rs` is
/// the crate root (the empty path); `main.rs` is a separate binary target and is not a module
/// of the library at all.
fn module_map(tree: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for rel in tree.keys() {
        if rel == "main.rs" {
            continue;
        }
        let module = if rel == "lib.rs" {
            String::new()
        } else if let Some(dir) = rel.strip_suffix("/mod.rs") {
            dir.replace('/', "::")
        } else if let Some(stem) = rel.strip_suffix(".rs") {
            stem.replace('/', "::")
        } else {
            continue;
        };
        map.insert(module, rel.clone());
    }
    map
}

/// Whether some declaration of the name survives everywhere the reference site does: each of
/// that declaration's conjuncts must be met by a predicate the reference site already carries.
fn covered(declarations: &[&Vec<String>], active: &[String]) -> bool {
    let mut held: Vec<String> = Vec::new();
    for pred in active {
        held.extend(conjuncts(pred));
    }
    declarations.iter().any(|stacked| {
        let mut needed: Vec<String> = Vec::new();
        for pred in stacked.iter() {
            needed.extend(conjuncts(pred));
        }
        needed
            .iter()
            .all(|need| disjuncts(need).iter().any(|d| held.iter().any(|h| h == d)))
    })
}

/// Both sides of one predicate are declared — the `unix` / `not(unix)` twin shape. Such a name
/// resolves on every target by construction and is not this file's business.
fn twinned(preds: &[&String]) -> bool {
    preds.iter().any(|a| {
        preds
            .iter()
            .any(|b| **a == format!("not({b})") || **b == format!("not({a})"))
    })
}

/// The portable third branch of a Unix/Windows/other implementation is not a
/// simple `not(unix)` twin, but these three *bare* gates cover every target.
/// Requiring each whole gate stack avoids treating a feature-gated Windows
/// branch as complete coverage.
fn unix_windows_other_partition(declarations: &[&Vec<String>]) -> bool {
    let has = |pred: &str| {
        declarations
            .iter()
            .any(|gates| gates.len() == 1 && gates[0] == pred)
    };
    has("unix") && has("windows") && has("not(any(unix,windows))")
}

fn mentions_platform(pred: &str) -> bool {
    PLATFORM_KEYS.iter().any(|key| contains_word(pred, key))
}

// The scan

/// One item declaration and the `cfg` predicates standing over it, outermost first.
struct Decl {
    name: String,
    gates: Vec<String>,
}

/// One lexed source file.
struct Scan {
    /// Per line: the code with comments and every literal blanked out, so a brace inside a
    /// string or a doc comment cannot move the nesting.
    code: Vec<String>,
    /// Per line: the line as written, for diagnostics only — a blanked line has lost the very
    /// literals (`"macos"`, `"bin"`) a reader needs to recognize the site.
    raw: Vec<String>,
    /// Per line: every `cfg` predicate in force on it.
    active: Vec<Vec<String>>,
    items: Vec<Decl>,
}

/// A `cfg` frame: what it asserts, the brace depth it was opened at, and whether the construct
/// it decorates turned out to have a body. An item or a block ends when the depth comes back;
/// a braceless statement (`#[cfg(unix)] use std::…;`) ends at its `;`.
struct Frame {
    preds: Vec<String>,
    depth0: i32,
    opened: bool,
}

impl Scan {
    fn of(src: &str) -> Self {
        let mut lex = Lex::Code;
        let raw: Vec<String> = src.lines().map(str::to_string).collect();
        let code: Vec<String> = raw.iter().map(|l| blank_line(l, &mut lex)).collect();
        let mut active = Vec::with_capacity(code.len());
        let mut items = Vec::new();
        let mut stack: Vec<Frame> = Vec::new();
        let mut pending: Vec<String> = Vec::new();
        let mut depth: i32 = 0;
        let mut parens: i32 = 0;
        for (line, written) in code.iter().zip(&raw) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                active.push(flatten(&stack));
                continue;
            }
            if let Some(pred) = cfg_predicate(trimmed) {
                // Read the predicate off the line as written where that parses: the blanked
                // copy has lost `"macos"` out of `target_os = "macos"`, which a diagnostic
                // needs to name a site a reader can go and look at.
                pending.push(cfg_predicate(written.trim()).unwrap_or(pred));
                active.push(flatten(&stack));
                continue;
            }
            if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
                active.push(flatten(&stack));
                continue;
            }
            if !pending.is_empty() {
                stack.push(Frame {
                    preds: std::mem::take(&mut pending),
                    depth0: depth,
                    opened: false,
                });
            }
            if let Some(name) = item_name(line) {
                items.push(Decl {
                    name,
                    gates: flatten(&stack),
                });
            }
            // Recorded before the line's own braces are consumed, so the first line of a
            // gated statement counts as standing inside its own gate.
            active.push(flatten(&stack));
            for (at, ch) in line.char_indices() {
                match ch {
                    '(' | '[' => parens += 1,
                    ')' | ']' => parens -= 1,
                    '{' => {
                        depth += 1;
                        for frame in &mut stack {
                            if !frame.opened && frame.depth0 == depth - 1 {
                                frame.opened = true;
                            }
                        }
                    }
                    '}' => {
                        depth -= 1;
                        while let Some(top) = stack.last() {
                            let done = if top.opened {
                                top.depth0 >= depth
                            } else {
                                top.depth0 > depth
                            };
                            if !done {
                                break;
                            }
                            // `#[cfg(unix)] if … { … } else { … }`: the gate covers the
                            // whole chain, not just the first arm.
                            if top.opened && line[at + 1..].trim_start().starts_with("else") {
                                break;
                            }
                            stack.pop();
                        }
                    }
                    ';' if parens == 0 => {
                        while let Some(top) = stack.last() {
                            if top.opened || top.depth0 != depth {
                                break;
                            }
                            stack.pop();
                        }
                    }
                    _ => {}
                }
            }
        }
        Self {
            code,
            raw,
            active,
            items,
        }
    }
}

fn flatten(stack: &[Frame]) -> Vec<String> {
    stack.iter().flat_map(|f| f.preds.iter().cloned()).collect()
}

/// `#[cfg(<pred>)]` alone on a line -> the predicate, whitespace squeezed out so
/// `target_os = "macos"` and `target_os="macos"` are one spelling. A `cfg_attr` or a `cfg` in
/// a longer line is not a gate over what follows and is ignored.
fn cfg_predicate(trimmed: &str) -> Option<String> {
    let body = trimmed.strip_prefix("#[cfg(")?.strip_suffix(")]")?;
    Some(body.chars().filter(|c| !c.is_whitespace()).collect())
}

/// The name an item declaration binds, at any indentation: the first item keyword followed by
/// an identifier. A spurious hit (an associated `type` in an `impl`, a nested `fn`) can only
/// add a declaration, and an extra declaration only ever makes the law more permissive.
fn item_name(code: &str) -> Option<String> {
    const KEYWORDS: &[&str] = &[
        "fn", "struct", "enum", "trait", "type", "const", "static", "union", "mod",
    ];
    let mut tokens = code
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty())
        .peekable();
    while let Some(token) = tokens.next() {
        if KEYWORDS.contains(&token) {
            let next = *tokens.peek()?;
            if !KEYWORDS.contains(&next) {
                return Some(next.to_string());
            }
        }
    }
    None
}

/// Every `crate::<module…>::<name>` in one line of code. Module segments are the
/// lowercase-leading ones, so `crate::seam::ViewJob::Root` names `seam`'s `ViewJob` and not a
/// module called `ViewJob`.
fn references(code: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !starts_word(&chars, i, "crate::") {
            i += 1;
            continue;
        }
        let mut j = i + "crate::".len();
        let mut segments: Vec<String> = Vec::new();
        loop {
            let start = j;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            if j == start {
                break;
            }
            let segment: String = chars[start..j].iter().collect();
            let followed = chars.get(j) == Some(&':') && chars.get(j + 1) == Some(&':');
            let is_module = segment.starts_with(|c: char| c.is_lowercase() || c == '_');
            if followed && is_module {
                segments.push(segment);
                j += 2;
                continue;
            }
            let module = segments.join("::");
            out.push((module, segment));
            break;
        }
        i = j.max(i + 1);
    }
    out
}

fn starts_word(chars: &[char], at: usize, word: &str) -> bool {
    if at > 0 && (chars[at - 1].is_alphanumeric() || chars[at - 1] == '_') {
        return false;
    }
    chars[at..]
        .iter()
        .zip(word.chars())
        .filter(|(a, b)| *a == b)
        .count()
        == word.chars().count()
        && chars.len() - at >= word.chars().count()
}

// The lexer

/// What the scanner is in the middle of at a line boundary. Carried across lines on purpose:
/// this crate writes multi-line `\`-continued string literals, and a line-at-a-time lexer
/// would read their prose as code and miscount every brace after them.
#[derive(Clone, Copy)]
enum Lex {
    Code,
    BlockComment,
    Str,
    RawStr(usize),
}

/// One line with comments and every literal removed — what is left is the only text whose
/// braces, semicolons and identifiers mean anything here.
fn blank_line(line: &str, state: &mut Lex) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    loop {
        match *state {
            Lex::BlockComment => match find_pair(&chars, i, '*', '/') {
                Some(end) => {
                    *state = Lex::Code;
                    i = end;
                }
                None => return out,
            },
            Lex::Str => match find_string_end(&chars, i) {
                Some(end) => {
                    *state = Lex::Code;
                    i = end;
                }
                None => return out,
            },
            Lex::RawStr(hashes) => match find_raw_end(&chars, i, hashes) {
                Some(end) => {
                    *state = Lex::Code;
                    i = end;
                }
                None => return out,
            },
            Lex::Code => {
                if i >= chars.len() {
                    return out;
                }
                let c = chars[i];
                if c == '/' && chars.get(i + 1) == Some(&'/') {
                    return out;
                }
                if c == '/' && chars.get(i + 1) == Some(&'*') {
                    *state = Lex::BlockComment;
                    i += 2;
                    continue;
                }
                if let Some((next, opened)) = literal_open(&chars, i) {
                    *state = opened;
                    i = next;
                    continue;
                }
                if c == '\'' {
                    i = char_literal_end(&chars, i);
                    continue;
                }
                out.push(c);
                i += 1;
            }
        }
    }
}

/// A string/byte-string/raw-string opening at `at`: the index just past the opening quote and
/// the state to continue in. `None` when nothing opens here.
fn literal_open(chars: &[char], at: usize) -> Option<(usize, Lex)> {
    if at > 0 && (chars[at - 1].is_alphanumeric() || chars[at - 1] == '_') {
        return None;
    }
    let mut j = at;
    if chars.get(j) == Some(&'b') {
        j += 1;
    }
    let raw = chars.get(j) == Some(&'r');
    if raw {
        j += 1;
    }
    let mut hashes = 0;
    while chars.get(j) == Some(&'#') {
        hashes += 1;
        j += 1;
    }
    if chars.get(j) != Some(&'"') {
        return None;
    }
    if hashes > 0 && !raw {
        return None;
    }
    Some((j + 1, if raw { Lex::RawStr(hashes) } else { Lex::Str }))
}

fn find_pair(chars: &[char], from: usize, a: char, b: char) -> Option<usize> {
    (from..chars.len().saturating_sub(1))
        .find(|&j| chars[j] == a && chars[j + 1] == b)
        .map(|j| j + 2)
}

/// The index just past a normal string's closing quote, or `None` when the string runs on to
/// the next line.
fn find_string_end(chars: &[char], from: usize) -> Option<usize> {
    let mut j = from;
    while j < chars.len() {
        match chars[j] {
            '\\' => j += 2,
            '"' => return Some(j + 1),
            _ => j += 1,
        }
    }
    None
}

fn find_raw_end(chars: &[char], from: usize, hashes: usize) -> Option<usize> {
    let mut j = from;
    while j < chars.len() {
        if chars[j] == '"' && (1..=hashes).all(|k| chars.get(j + k) == Some(&'#')) {
            return Some(j + 1 + hashes);
        }
        j += 1;
    }
    None
}

/// Past a char literal, or past the tick of a lifetime — either way the next index to read.
fn char_literal_end(chars: &[char], at: usize) -> usize {
    if chars.get(at + 1) == Some(&'\\') {
        let mut j = at + 2;
        while j < chars.len() && chars[j] != '\'' {
            j += 1;
        }
        return j + 1;
    }
    if chars.get(at + 2) == Some(&'\'') {
        return at + 3;
    }
    at + 1
}

// cfg algebra (shallow on purpose — see the header)

fn split_top(list: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut current = String::new();
    for c in list.chars() {
        match c {
            '(' => {
                depth += 1;
                current.push(c);
            }
            ')' => {
                depth -= 1;
                current.push(c);
            }
            ',' if depth == 0 => parts.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn unwrap_list<'a>(pred: &'a str, head: &str) -> Option<&'a str> {
    pred.strip_prefix(head)?
        .strip_prefix('(')?
        .strip_suffix(')')
}

fn disjuncts(pred: &str) -> Vec<String> {
    unwrap_list(pred, "any").map_or_else(|| vec![pred.to_string()], split_top)
}

fn conjuncts(pred: &str) -> Vec<String> {
    unwrap_list(pred, "all").map_or_else(|| vec![pred.to_string()], split_top)
}

fn contains_word(haystack: &str, word: &str) -> bool {
    let hay = haystack.as_bytes();
    let needle = word.as_bytes();
    if needle.is_empty() || hay.len() < needle.len() {
        return false;
    }
    let ident = |b: u8| b == b'_' || b.is_ascii_alphanumeric();
    (0..=hay.len() - needle.len()).any(|start| {
        let end = start + needle.len();
        &hay[start..end] == needle
            && (start == 0 || !ident(hay[start - 1]))
            && (end == hay.len() || !ident(hay[end]))
    })
}

fn render(preds: &[String]) -> String {
    if preds.is_empty() {
        "<ungated>".to_string()
    } else {
        preds.join(" + ")
    }
}

// The tree

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read_tree(root: &Path) -> BTreeMap<String, String> {
    let mut tree = BTreeMap::new();
    collect(root, root, &mut tree);
    tree
}

fn collect(root: &Path, dir: &Path, tree: &mut BTreeMap<String, String>) {
    let listing =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {} ({e})", dir.display()));
    for entry in listing {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, tree);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let rel = path
                .strip_prefix(root)
                .expect("under src/")
                .to_str()
                .expect("utf-8 path")
                .replace('\\', "/");
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {} ({e})", path.display()));
            tree.insert(rel, text);
        }
    }
}

fn report(found: &[Violation]) -> String {
    let mut out = String::new();
    for v in found {
        let _ = write!(
            out,
            "\n  src/{}:{}  {}\n      declared: {}\n      the reference site carries: {}\n      | {}\n",
            v.file,
            v.line,
            v.path,
            v.declared.join("  |  "),
            render(&v.active),
            v.source,
        );
    }
    out
}

// The tests

/// The law over the real crate.
#[test]
fn every_crate_qualified_reference_resolves_on_every_target() {
    let tree = read_tree(&src_root());
    let found = violations(&tree);
    assert!(
        found.is_empty(),
        "{} reference(s) under crates/atpkg/src name an item that is configured OUT where the \
         reference itself is still compiled. Each one is a crate that does not BUILD on some \
         target aterm ships — the shape that stopped `atpkg` compiling for \
         x86_64-pc-windows-msvc on 2026-09-16. Fix it at the declaration (widen the cfg, or \
         give it a twin carrying the complementary one — never a stub that returns a lie), or \
         at the reference (gate it the same way its callee is gated).\n{}",
        found.len(),
        report(&found),
    );
}

/// Non-vacuity, half one: the two defects this file was born from, replayed as literals. A
/// guard nobody has watched go red is a guard nobody should believe.
#[test]
fn the_law_goes_red_on_the_two_defects_it_was_born_from() {
    // 1. `is_real_dir` — `#[cfg(unix)]`, no twin, called from an ungated fn.
    let mut tree = BTreeMap::new();
    tree.insert(
        "seam.rs".to_string(),
        r#"
/// `lstat` says a real directory — a symlink to one is not.
#[cfg(unix)]
pub(crate) fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}
"#
        .to_string(),
    );
    tree.insert(
        "compat.rs".to_string(),
        r#"
pub fn ensure_root_with(layout: &Layout) -> io::Result<Ensured> {
    let standing = trust_build_of(layout)
        .is_some_and(|n| crate::seam::is_real_dir(&root_dir(layout, n).join("bin")));
    Ok(Ensured::Present)
}
"#
        .to_string(),
    );
    let found = violations(&tree);
    assert_eq!(found.len(), 1, "{}", report(&found));
    assert_eq!(found[0].path, "crate::seam::is_real_dir");
    assert_eq!(found[0].file, "compat.rs");

    // 2. `set_xattr_for_test` — `all(test, target_os = "macos")`, called from a `#[cfg(test)]`
    //    module that forgot the platform half.
    let mut tree = BTreeMap::new();
    tree.insert(
        "provenance.rs".to_string(),
        r#"
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn set_xattr_for_test(path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
    Ok(())
}
"#
        .to_string(),
    );
    let doctor = r#"
#[cfg(test)]
mod tests {
    GATE
    #[test]
    fn a_tagged_file_is_counted() {
        crate::provenance::set_xattr_for_test(&trust_exe, "user.aterm.probe", b"1").unwrap();
    }
}
"#;
    tree.insert(
        "doctor.rs".to_string(),
        doctor.replace("GATE", "").to_string(),
    );
    let found = violations(&tree);
    assert_eq!(found.len(), 1, "{}", report(&found));
    assert_eq!(found[0].path, "crate::provenance::set_xattr_for_test");

    // …and the one-attribute repair is what turns it green.
    tree.insert(
        "doctor.rs".to_string(),
        doctor.replace("GATE", "#[cfg(target_os = \"macos\")]"),
    );
    let found = violations(&tree);
    assert!(found.is_empty(), "{}", report(&found));
}

/// Non-vacuity, half two: every shape that must stay green. A guard that cannot tell a twin
/// from a defect is worse than none — it gets deleted the first week.
#[test]
fn the_shapes_that_are_not_defects_stay_green() {
    let mut tree = BTreeMap::new();
    tree.insert(
        "seam.rs".to_string(),
        r#"
#[cfg(unix)]
pub(crate) fn bin_mismatch(build: &Path, view: &Path) -> Option<PathBuf> {
    None
}
#[cfg(not(unix))]
pub(crate) fn bin_mismatch(_build: &Path, view: &Path) -> Option<PathBuf> {
    None
}
#[cfg(unix)]
pub(crate) fn unix_only(path: &Path) -> bool {
    true
}
#[cfg(target_os = "macos")]
pub(crate) fn run_view_job(
    helper: &Path,
    layout: &Layout,
    job: &ViewJob,
) -> Result<String, String> {
    Err(String::new())
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn run_view_job(
    _helper: &Path,
    _layout: &Layout,
    _job: &ViewJob,
) -> Result<String, String> {
    Err(String::new())
}
"#
        .to_string(),
    );
    tree.insert(
        "compat.rs".to_string(),
        r#"
// A twin is always resolvable.
pub fn a(build: &Path, view: &Path) -> bool {
    crate::seam::bin_mismatch(build, view).is_none() && crate::seam::run_view_job(p, l, j).is_ok()
}
// The call stands inside the same gate the callee does — as a whole item…
#[cfg(unix)]
pub fn b(path: &Path) -> bool {
    crate::seam::unix_only(path)
}
// …as a bare block inside an ungated fn…
pub fn c(path: &Path) -> bool {
    #[cfg(unix)]
    {
        return crate::seam::unix_only(path);
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}
// …and as a single gated statement, the shape `root_first_mismatch` uses.
pub fn d(root: &Path) -> Option<PathBuf> {
    #[cfg(unix)]
    if crate::seam::unix_only(root) {
        return Some(root.to_path_buf());
    }
    None
}
// A multi-line signature must not end its own gate at the first comma.
#[cfg(unix)]
pub fn e(
    one: &Path,
    two: &Path,
) -> bool {
    crate::seam::unix_only(one) && crate::seam::unix_only(two)
}
"#
        .to_string(),
    );
    let found = violations(&tree);
    assert!(found.is_empty(), "{}", report(&found));
}

/// The real `metadata_io::open_regular` shape: all three platform branches
/// cover the ungated lock-release probe, but an extra gate on one branch does
/// not. This is the scanner's historical false positive and its negative
/// control, run through the same source parser as the crate-wide law.
#[test]
fn the_unix_windows_other_partition_needs_three_bare_branches() {
    let mut tree = BTreeMap::new();
    let declarations = r#"
#[cfg(unix)]
pub(crate) fn open_regular(path: &Path) -> io::Result<File> { todo!() }
#[cfg(windows)]
pub(crate) fn open_regular(path: &Path) -> io::Result<File> { todo!() }
#[cfg(not(any(unix, windows)))]
pub(crate) fn open_regular(path: &Path) -> io::Result<File> { todo!() }
"#;
    tree.insert("metadata_io.rs".to_string(), declarations.to_string());
    tree.insert(
        "lock.rs".to_string(),
        "pub fn released(path: &Path) { let _ = crate::metadata_io::open_regular(path); }"
            .to_string(),
    );
    assert!(violations(&tree).is_empty(), "all targets have one branch");

    tree.insert(
        "metadata_io.rs".to_string(),
        declarations.replace(
            "#[cfg(windows)]",
            "#[cfg(all(windows, feature = \"extra\"))]",
        ),
    );
    let found = violations(&tree);
    assert_eq!(
        found.len(),
        1,
        "an extra gate makes Windows coverage partial"
    );
    assert_eq!(found[0].path, "crate::metadata_io::open_regular");
}

/// Non-vacuity, half three: the scan must still see the crate. Every limit in this file's
/// header narrows what it judges, and the failure mode of a narrowed guard is a lexer that
/// quietly matches nothing and passes for ever. These floors sit far below the tree's real
/// numbers — they are here to catch a scanner that stopped working, not to pin a shape.
#[test]
fn the_scan_still_sees_the_crate() {
    let tree = read_tree(&src_root());
    assert!(
        tree.len() >= 30,
        "only {} source files found under crates/atpkg/src",
        tree.len()
    );
    let modules = module_map(&tree);
    assert!(
        modules.contains_key("seam") && modules.contains_key("compat"),
        "the two modules the defects landed in must resolve"
    );

    let scans: BTreeMap<&str, Scan> = tree
        .iter()
        .map(|(rel, src)| (rel.as_str(), Scan::of(src)))
        .collect();

    let gated = scans
        .values()
        .flat_map(|s| &s.items)
        .filter(|d| d.gates.iter().any(|p| mentions_platform(p)))
        .count();
    assert!(
        gated >= 300,
        "only {gated} platform-gated declarations seen"
    );

    let mut resolved = 0_usize;
    let mut reaching_gated = 0_usize;
    for scan in scans.values() {
        for code in &scan.code {
            for (module, name) in references(code) {
                let Some(home) = modules.get(module.as_str()) else {
                    continue;
                };
                let Some(target) = scans.get(home.as_str()) else {
                    continue;
                };
                let mut decls = target.items.iter().filter(|d| d.name == name).peekable();
                if decls.peek().is_none() {
                    continue;
                }
                resolved += 1;
                if decls.any(|d| d.gates.iter().any(|p| mentions_platform(p))) {
                    reaching_gated += 1;
                }
            }
        }
    }
    assert!(resolved >= 1000, "only {resolved} references resolved");
    assert!(
        reaching_gated >= 10,
        "only {reaching_gated} references reach a platform-gated declaration — the law judges \
         those and nothing else, so this is the number that must not quietly go to zero"
    );

    // The lexer's own obligation: `seam.rs` carries four-line `\`-continued string literals,
    // and a lexer that read their prose as code would miscount every brace after them.
    let seam = scans.get("seam.rs").expect("seam.rs");
    assert!(
        seam.items.iter().any(|d| d.name == "is_real_dir"),
        "the declaration the Windows defect was found at must still be seen"
    );
    assert!(
        seam.items
            .iter()
            .any(|d| d.name == "bin_mismatch" && d.gates.iter().any(|p| p == "not(unix)")),
        "the `not(unix)` twin arm must still be seen with its own gate"
    );
}
