// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! seam-caller: an item only a debug build compiles is named by no code a release build compiles.
//!
//! A test seam other crates' tests call is `#[cfg(any(test, debug_assertions))]`: tests, benches
//! and every dev check build it, and a release build does not. Code a release build compiles that
//! names one passes every dev check and fails only the first optimized build, the release
//! cutter's own (0.151.0's cut, 2026-11-03: a sign-in called
//! `aterm_pty::spawn_session_leader_on_pty`). The rule reads each item a file declares at its top
//! level (a `fn`, `struct`, `enum`, `union`, `trait`, `type`, `const` or `static`; a method, a
//! field, a module or a `use` is not read) under a one-line `cfg` that no release build satisfies
//! and a debug build that is not a test build may (`test` alone is left to the dev build, which
//! fails on it). It fails each whole-word mention of that item's name, in code a release build
//! compiles, that can name it: in its own file, bare or through `self`, `super` or its module; for
//! a `pub(…)` item, in another file of its crate through its module, the crate (`crate::x` for a
//! root's item, `aterm_pty::x`) or `super`; for a `pub` one, also in another crate through a path
//! or a `use` that starts at its crate (`aterm_pty::x`, `use aterm_pty::{x}`). A bare word outside
//! its file (a local, or an import's later uses, which the import line already names), a path
//! through another module (`nss::x`) or a type (`Self::x`, `Plan::X`: an associated item or a
//! variant) is not one, and neither is a method call (`.name`), a macro (`name!`), a declaration
//! (`fn x`, `let x`), a field or parameter (`x: T`), or a mention after a gate on its own line (a
//! parameter's `#[cfg(debug_assertions)]`). A name the crate also declares at a top level a
//! release build compiles (a release twin under `not(debug_assertions)`, or an unrelated item) is
//! no seam. Only files a release build compiles are read: each crate root and what a `mod` on a
//! release-compiled line loads from one, so a test file, an integration test, a debug-only module
//! and the census's own source are not. A glob import (`use aterm_pty::*`) is not followed.

use std::collections::{BTreeMap, BTreeSet};

use super::debug_seam::{crate_of, is_ident, module_of, names, release_never_compiles, whole_word};
use super::rules::CENSUS_CRATE;
use super::{Finding, Rule, SourceFile, Tree};
use crate::lex::Lexed;
use crate::lock_order::unmasked_lines;

/// The crates the rule reads.
const SOURCE: &[&str] = &["crates/"];

pub(super) const SEAM_CALLER: Rule = Rule {
    id: "seam-caller",
    why: "an item declared under a cfg no release build satisfies (a test seam, \
          `cfg(any(test, debug_assertions))`) is absent from every release build, so code a release \
          build compiles that names it passes every dev check and fails only the first optimized \
          build, the release's own; gate the caller the same way or make the item production code",
    scope: SOURCE,
    check: seam_caller,
};

/// Where an item's name can be written: its file, its crate, or also its crate's dependents.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    File,
    Crate,
    Dependents,
}

/// An item only a debug build compiles.
struct Seam {
    name: String,
    /// The file that declares it, and the line (0-based) that does.
    file: String,
    line: usize,
    reach: Reach,
    /// Its crate's directory (`crates/aterm-pty/`) and name in a path (`aterm_pty`).
    krate: String,
    crate_ident: String,
    /// The module its file is (`unix`), and whether that file is its crate's root.
    module: String,
    root: bool,
}

fn seam_caller(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let shipped = released_files(tree);
    let read =
        |file: &SourceFile| shipped.contains(&file.rel) && !file.rel.starts_with(CENSUS_CRATE);
    let mut gated = Vec::new();
    for file in tree.corpus.under(SOURCE).filter(|f| read(f)) {
        if !file.mentions(&["#[cfg("]) || !file.mentions(&["debug_assertions", "test"]) {
            continue;
        }
        let (krate, crate_ident) = crate_of(&file.rel);
        let code = &file.lexed().code;
        let compiled = unmasked_lines(code, &release_never_compiles);
        let lines: Vec<&str> = code.lines().collect();
        for (at, line) in lines.iter().enumerate() {
            if compiled.get(at).copied().unwrap_or(true) {
                continue;
            }
            let Some((reach, name)) = item(line) else {
                continue;
            };
            if seam_gate(&lines, at) {
                gated.push(Seam {
                    name: name.to_owned(),
                    file: file.rel.clone(),
                    line: at,
                    reach,
                    krate: krate.clone(),
                    crate_ident: crate_ident.clone(),
                    module: module_of(&file.rel),
                    root: crate_root(&file.rel),
                });
            }
        }
    }
    if gated.is_empty() {
        return;
    }
    let gated_crates: BTreeSet<&str> = gated.iter().map(|s| s.krate.as_str()).collect();
    let gated_names: Vec<&str> = gated.iter().map(|s| s.name.as_str()).collect();
    let mut released: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in tree.corpus.under(SOURCE).filter(|f| read(f)) {
        if !gated_crates.iter().any(|k| file.rel.starts_with(k)) || !file.mentions(&gated_names) {
            continue;
        }
        let (krate, _) = crate_of(&file.rel);
        let code = &file.lexed().code;
        let compiled = unmasked_lines(code, &release_never_compiles);
        for (at, line) in code.lines().enumerate() {
            if !compiled.get(at).copied().unwrap_or(true) {
                continue;
            }
            let Some((_, name)) = item(line) else {
                continue;
            };
            if gated_names.contains(&name) {
                released
                    .entry(krate.clone())
                    .or_default()
                    .insert(name.to_owned());
            }
        }
    }
    gated.retain(|s| !released.get(&s.krate).is_some_and(|n| n.contains(&s.name)));
    if gated.is_empty() {
        return;
    }
    let words: Vec<&str> = gated.iter().map(|s| s.name.as_str()).collect();
    for file in tree.corpus.under(SOURCE).filter(|f| read(f)) {
        if !file.mentions(&words) {
            continue;
        }
        let code = &file.lexed().code;
        let compiled = unmasked_lines(code, &release_never_compiles);
        let line_of = |at: usize| code[..at].matches('\n').count();
        for seam in &gated {
            let same_file = file.rel == seam.file;
            let same_crate = file.rel.starts_with(&seam.krate);
            let reaches = same_file
                || (seam.reach != Reach::File && same_crate)
                || (seam.reach == Reach::Dependents && names(code, &seam.crate_ident));
            if !reaches {
                continue;
            }
            for (at, _) in code.match_indices(&seam.name) {
                let line = line_of(at);
                let before = &code[..at];
                let after = &code[at + seam.name.len()..];
                if !whole_word(code, at, seam.name.len())
                    || !compiled.get(line).copied().unwrap_or(true)
                    || method(before)
                    || after.starts_with('!')
                    || declares(before)
                    || binds(after)
                    || gated_on_its_line(before)
                    || !names_the_seam(path_of(before), seam, same_file, same_crate)
                {
                    continue;
                }
                out.push(file.finding(
                    SEAM_CALLER.id,
                    line + 1,
                    format!(
                        "names `{}` ({}:{}), which only a debug build compiles, in code a release \
                         build compiles",
                        seam.name,
                        seam.file,
                        seam.line + 1
                    ),
                ));
            }
        }
    }
}

/// The item a top-level line declares, and how far its name reaches: `pub fn x(` is
/// `(Dependents, "x")`, `pub(crate) struct Y;` `(Crate, "Y")`, `const fn z` `(File, "z")`. An
/// indented line, an `impl`, a `mod`, a `use`, a macro or a `const _` declares none.
fn item(line: &str) -> Option<(Reach, &str)> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let (reach, rest) = if let Some(rest) = line.strip_prefix("pub(") {
        (Reach::Crate, rest.split_once(')')?.1)
    } else if let Some(rest) = line.strip_prefix("pub ") {
        (Reach::Dependents, rest)
    } else {
        (Reach::File, line)
    };
    let mut words = rest.split_whitespace().peekable();
    while let Some(word) = words.next() {
        match word {
            "unsafe" | "async" | "extern" | "mut" => {}
            "const" if words.peek() == Some(&"fn") => {}
            "fn" | "struct" | "enum" | "union" | "trait" | "type" | "const" | "static" => {
                let next = words.next()?;
                let next = if next == "mut" { words.next()? } else { next };
                let end = next.find(|c: char| !is_ident(c)).unwrap_or(next.len());
                let name = &next[..end];
                return (!name.is_empty() && name != "_").then_some((reach, name));
            }
            // The ABI's string, its contents blanked in the code view.
            word if word.starts_with('"') => {}
            _ => return None,
        }
    }
    None
}

/// Whether the attributes above line `at` of `lines` (the code view) hold a one-line `cfg` that no
/// release build satisfies and a debug build that is not a test build may.
fn seam_gate(lines: &[&str], at: usize) -> bool {
    lines[..at]
        .iter()
        .rev()
        .take_while(|l| l.starts_with("#[") || l.trim().is_empty())
        .any(|l| {
            let attr = l.trim_end();
            release_never_compiles(attr)
                && attr
                    .strip_prefix("#[cfg(")
                    .and_then(|rest| rest.strip_suffix(")]"))
                    .is_some_and(|pred| in_a_debug_build(pred) != Some(false))
        })
}

/// What the `cfg` predicate `pred` is in a debug build that is not a test build: `Some` when the
/// predicate decides it there, `None` when a feature, a platform or another option leaves it open.
fn in_a_debug_build(pred: &str) -> Option<bool> {
    let pred = pred.trim();
    match pred {
        "debug_assertions" => return Some(true),
        "test" => return Some(false),
        _ => {}
    }
    if let Some(inner) = pred
        .strip_prefix("not(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return in_a_debug_build(inner).map(|holds| !holds);
    }
    for (head, any) in [("all(", false), ("any(", true)] {
        if let Some(inner) = pred
            .strip_prefix(head)
            .and_then(|rest| rest.strip_suffix(')'))
        {
            let mut open = false;
            for child in crate::split_top_level_commas(inner) {
                match in_a_debug_build(child) {
                    Some(holds) if holds == any => return Some(any),
                    Some(_) => {}
                    None => open = true,
                }
            }
            return if open { None } else { Some(!any) };
        }
    }
    None
}

/// The files a release build compiles: each crate root (`src/lib.rs`, `src/main.rs`, a `src/bin/`
/// target, `build.rs`) and each file a `mod` declaration on a line a release build compiles loads
/// from one (`x.rs` or `x/mod.rs` beside it, or its `#[path]`). A `mod` in an inline module names
/// a file this does not look for, so that file is not read.
fn released_files(tree: &Tree<'_>) -> BTreeSet<String> {
    let mut loads: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for file in tree.corpus.under(SOURCE) {
        if !file.mentions(&["mod "]) {
            continue;
        }
        let Lexed { code, text } = file.lexed();
        let compiled = unmasked_lines(code, &release_never_compiles);
        let texts: Vec<&str> = text.lines().collect();
        let (dir, base) = module_dirs(&file.rel);
        for (at, line) in code.lines().enumerate() {
            let Some(name) = mod_declaration(line) else {
                continue;
            };
            if !compiled.get(at).copied().unwrap_or(true) {
                continue;
            }
            let path = texts[..at]
                .iter()
                .rev()
                .take_while(|l| l.trim_start().starts_with("#["))
                .find_map(|l| path_attribute(l));
            let targets = path.map_or_else(
                || vec![format!("{base}/{name}.rs"), format!("{base}/{name}/mod.rs")],
                |path| vec![normalized(&format!("{dir}/{path}"))],
            );
            loads.entry(file.rel.as_str()).or_default().extend(targets);
        }
    }
    let present: BTreeSet<&str> = tree.corpus.under(SOURCE).map(|f| f.rel.as_str()).collect();
    let mut shipped: BTreeSet<String> = present
        .iter()
        .filter(|rel| crate_root(rel))
        .map(|rel| (*rel).to_owned())
        .collect();
    let mut queue: Vec<String> = shipped.iter().cloned().collect();
    while let Some(rel) = queue.pop() {
        for target in loads.get(rel.as_str()).into_iter().flatten() {
            if present.contains(target.as_str()) && shipped.insert(target.clone()) {
                queue.push(target.clone());
            }
        }
    }
    shipped
}

/// Whether `rel` is a crate's root: `src/lib.rs`, `src/main.rs`, `build.rs`, or a `src/bin/`
/// target (`src/bin/x.rs`, `src/bin/x/main.rs`).
fn crate_root(rel: &str) -> bool {
    let (krate, _) = crate_of(rel);
    let Some(inner) = rel.strip_prefix(&krate) else {
        return false;
    };
    matches!(inner, "src/lib.rs" | "src/main.rs" | "build.rs")
        || inner.strip_prefix("src/bin/").is_some_and(|bin| {
            bin.ends_with(".rs")
                && (!bin.contains('/')
                    || (bin.matches('/').count() == 1 && bin.ends_with("/main.rs")))
        })
}

/// A file's directory, and the directory its `mod x;` declarations load from: the same for a crate
/// root or a `mod.rs`, else the file's own stem (`src/a.rs` loads from `src/a/`).
fn module_dirs(rel: &str) -> (String, String) {
    let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
    let base = if name == "mod.rs" || crate_root(rel) {
        dir.to_owned()
    } else {
        rel.trim_end_matches(".rs").to_owned()
    };
    (dir.to_owned(), base)
}

/// The module a `mod x;` line declares from a file (`pub(crate) mod x;` too); an inline module
/// (`mod x {`) loads no file.
fn mod_declaration(line: &str) -> Option<&str> {
    let line = line.trim();
    let line = match line.strip_prefix("pub(") {
        Some(rest) => rest.split_once(')')?.1.trim_start(),
        None => line.strip_prefix("pub ").unwrap_or(line),
    };
    let name = line.strip_prefix("mod ")?.strip_suffix(';')?.trim();
    (!name.is_empty() && name.chars().all(is_ident)).then_some(name)
}

/// The path a `#[path = "…"]` attribute line names.
fn path_attribute(line: &str) -> Option<&str> {
    let rest = line
        .trim()
        .strip_prefix("#[path")?
        .trim_start()
        .strip_prefix('=')?;
    rest.trim()
        .strip_prefix('"')?
        .split_once('"')
        .map(|(path, _)| path)
}

/// `path` with its `.` and `..` components resolved (`a/b/../c.rs` is `a/c.rs`).
fn normalized(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

/// Whether a gate a release build never satisfies opens the line before a mention
/// (`#[cfg(debug_assertions)] hook: Option<…>` names its type after it).
fn gated_on_its_line(before: &str) -> bool {
    let line = before.rsplit('\n').next().unwrap_or(before);
    line.match_indices("#[cfg(").any(|(at, _)| {
        line[at..]
            .find(")]")
            .is_some_and(|end| release_never_compiles(&line[at..at + end + 2]))
    })
}

/// Whether a mention whose path is `path` (`(head, prev)`, `None` for a bare word) can name
/// `seam`: bare only in its own file; through a type (`Self::x`, `Plan::X`) never; in its crate
/// through its module, the crate, `super`, or in its own file `self`; from another crate through a
/// path that starts at its crate.
fn names_the_seam(
    path: Option<(&str, &str)>,
    seam: &Seam,
    same_file: bool,
    same_crate: bool,
) -> bool {
    let Some((head, prev)) = path else {
        return same_file;
    };
    if prev.starts_with(|c: char| c.is_ascii_uppercase()) {
        return false;
    }
    if !same_crate {
        return head == seam.crate_ident;
    }
    prev == seam.module
        || prev == seam.crate_ident
        || prev == "super"
        || (prev == "self" && same_file)
        || (prev == "crate" && seam.root)
}

/// The path a mention is named through, as `(head, prev)`: the path's first segment and the one
/// just before the name (`a::b::x` is `("a", "b")`); in a `use`, `head` is the `use` path's
/// first segment and `prev` the one before the name or its group (`use a::{b::{c, x}}` is
/// `("a", "b")`). `None` for a bare word, outside a path and a `use` group.
fn path_of(before: &str) -> Option<(&str, &str)> {
    let head = use_head(before);
    let prev = match before.strip_suffix("::") {
        Some(rest) => last_ident(rest),
        None if head.is_some() => grouped_prev(before),
        None => None,
    }?;
    Some((head.or_else(|| path_head(before))?, prev))
}

/// The first segment of the `use` declaration a mention is inside (`use ::a::b` is `a`): since a
/// `use` ends at its `;`, one opens on a line after the last `;` before the mention.
fn use_head(before: &str) -> Option<&str> {
    let statement = before.rsplit(';').next().unwrap_or(before);
    statement.lines().rev().find_map(|line| {
        let mut rest = line.trim_start();
        while rest.starts_with("#[") {
            rest = rest.split_once(']')?.1.trim_start();
        }
        let rest = match rest.strip_prefix("pub(") {
            Some(after) => after.split_once(')')?.1.trim_start(),
            None => rest.strip_prefix("pub ").unwrap_or(rest),
        };
        let path = rest.strip_prefix("use ")?.trim_start();
        let path = path.strip_prefix("::").unwrap_or(path);
        let end = path.find(|c: char| !is_ident(c)).unwrap_or(path.len());
        Some(&path[..end]).filter(|head| !head.is_empty())
    })
}

/// The segment before the innermost open group of a `use` (`use a::{b::{c, ` is `b`).
fn grouped_prev(before: &str) -> Option<&str> {
    let statement = before.rsplit(';').next().unwrap_or(before);
    let mut depth = 0usize;
    for (at, c) in statement.char_indices().rev() {
        match c {
            '}' => depth += 1,
            '{' if depth == 0 => {
                return last_ident(statement[..at].trim_end().strip_suffix("::")?);
            }
            '{' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The first segment of the path that ends in `::` at the end of `before` (`f(::a::b::` is `a`).
fn path_head(before: &str) -> Option<&str> {
    let mut rest = before.strip_suffix("::")?;
    let mut head = last_ident(rest)?;
    loop {
        let start = rest.len() - head.len();
        let Some(more) = rest[..start].strip_suffix("::") else {
            return Some(head);
        };
        let Some(segment) = last_ident(more) else {
            return Some(head);
        };
        rest = more;
        head = segment;
    }
}

/// The identifier `text` ends with, if any.
fn last_ident(text: &str) -> Option<&str> {
    let start = text
        .char_indices()
        .rev()
        .find(|&(_, c)| !is_ident(c))
        .map_or(0, |(at, c)| at + c.len_utf8());
    Some(&text[start..]).filter(|ident| !ident.is_empty())
}

/// Whether the code after a mention makes it a binding's name: a field, a parameter or a
/// struct literal's field (`x: T`, `x: value`), not a path (`x::y`).
fn binds(after: &str) -> bool {
    let after = after.trim_start();
    after.starts_with(':') && !after.starts_with("::")
}

/// Whether the code before a mention makes it a method or a field (`x.name`), not a range's end
/// (`0..NAME`).
fn method(before: &str) -> bool {
    let before = before.trim_end();
    before.ends_with('.') && !before.ends_with("..")
}

/// Whether the code before a mention ends in a declaring keyword (`fn x`, `struct x`, `let x`,
/// `macro_rules! x`): the mention is an item's own name, not a use of one.
fn declares(before: &str) -> bool {
    if before.trim_end().ends_with("macro_rules!") {
        return true;
    }
    let word = before
        .trim_end_matches(char::is_whitespace)
        .rsplit(|c: char| !is_ident(c))
        .next()
        .unwrap_or_default();
    before.ends_with(char::is_whitespace)
        && matches!(
            word,
            "fn" | "struct"
                | "enum"
                | "union"
                | "trait"
                | "type"
                | "const"
                | "static"
                | "mut"
                | "let"
        )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::tests::{run, spots};
    use super::*;

    /// Code a release build compiles that names a debug-only item is a finding: a public seam
    /// another crate's production code imports (one `use`, a grouped `use`, a nested group) or
    /// calls by a path from that crate, a private one called from its own file (bare or through
    /// `self`), a `pub(crate)` one named through its module from another file of its crate, a
    /// re-export a release build compiles. A mention under a gate entailing `test` or
    /// `debug_assertions` (an item's, a test module's, a parameter's on its own line), in a test
    /// file, an integration test or a debug-only module, a bare word outside its file (a local, an
    /// import's later use), a path through another module or crate or a type, a field, another
    /// crate's item of the same name in a file that never names this crate, a release twin, a
    /// method call, a macro, a declaration, a longer word and prose are not, nor is an item gated
    /// by `test` alone or by a feature, nor `const _`.
    /// NEGATIVE CONTROLS: `seam_gate` answering `false` leaves no finding; reading every file
    /// (not only what a release build compiles) adds `fx/sim.rs:1`, `thing_tests.rs:1` and
    /// `tests/it.rs:1`; dropping the release-twin filter adds `twin.rs:5`; dropping the
    /// method-call skip adds a second `unix.rs:12`; without the gate-on-its-line skip, `unix.rs:16`
    /// is one, and without the binding skip `unix.rs:18`; reading `const _` as an item adds
    /// `unix.rs:22`; counting bare words outside their file adds `agy_signin.rs:2` and two more
    /// `other.rs:2`; and without the type-segment skip, `agy_signin.rs:7` is one.
    #[test]
    fn a_debug_only_item_named_by_code_a_release_build_compiles_is_found() {
        let unix = "#[cfg(any(test, debug_assertions))]\n\
                    pub fn spawn_session_leader_on_pty() {}\n\
                    #[cfg(any(test, debug_assertions))]\n\
                    fn ensure_above_stdio() {}\n\
                    #[cfg(debug_assertions)]\n\
                    pub(crate) struct Probe;\n\
                    pub fn shipped() { ensure_above_stdio(); }\n\
                    #[cfg(any(test, debug_assertions))]\n\
                    pub fn gated_caller() { ensure_above_stdio(); let _ = Probe; }\n\
                    #[cfg(test)]\n\
                    fn only_test() {}\n\
                    fn other(x: X) { let p = Probe; x.spawn_session_leader_on_pty(); }\n\
                    #[cfg(feature = \"dev-seams\")]\n\
                    pub fn featured() {}\n\
                    fn uses() { featured(); only_test(); ensure_above_stdio_twin(); }\n\
                    fn param(#[cfg(debug_assertions)] probe: Probe) {}\n\
                    // ensure_above_stdio() in prose\n\
                    struct V { ensure_above_stdio: fn() }\n\
                    fn n() { nss::ensure_above_stdio(); Plan::Probe; self::ensure_above_stdio(); }\n\
                    #[cfg(any(test, debug_assertions))]\n\
                    const _: () = ();\n\
                    fn m(x: u8) { match x { _ => {} } }\n";
        let lib = "pub mod unix;\n\
                   mod other;\n\
                   pub use unix::shipped;\n\
                   fn f() { let _ = unix::Probe; }\n\
                   #[cfg(any(test, debug_assertions))]\n\
                   pub use unix::spawn_session_leader_on_pty;\n\
                   #[cfg(test)]\n\
                   mod tests { fn t() { super::unix::Probe; } }\n\
                   pub(crate) use unix::Probe as Gate;\n";
        let other = "fn g() { ensure_above_stdio(); }\n\
                     fn h() -> Probe { Probe::new(); super::unix::Probe; crate::other::Probe }\n";
        let uds = "mod twin;\n";
        let twin = "#[cfg(debug_assertions)]\n\
                    fn sweep_at_exit() {}\n\
                    #[cfg(not(debug_assertions))]\n\
                    fn sweep_at_exit() {}\n\
                    fn exit() { sweep_at_exit(); }\n";
        let agent = "pub mod harness;\n";
        let harness = "pub mod agy_signin;\nmod grouped;\n";
        let signin = "use aterm_pty::spawn_session_leader_on_pty;\n\
                      pub fn run() { spawn_session_leader_on_pty(); }\n\
                      fn path() { aterm_pty::spawn_session_leader_on_pty(); }\n\
                      macro_rules! spawn_session_leader_on_pty { () => {} }\n\
                      fn mac() { spawn_session_leader_on_pty!(); }\n\
                      fn local() { let spawn_session_leader_on_pty = 1; }\n\
                      fn heads() { crate::local::spawn_session_leader_on_pty(); \
                      Plan::spawn_session_leader_on_pty(); \
                      aterm_pty::Plan::spawn_session_leader_on_pty(); }\n";
        let grouped = "fn before() {}\n\
                       use aterm_pty::{\n\
                       \x20   shipped,\n\
                       \x20   spawn_session_leader_on_pty,\n\
                       };\n\
                       use crate::spawn_folder::spawn_session_leader_on_pty;\n\
                       use aterm_pty::{other::{x, spawn_session_leader_on_pty}};\n";
        let gui = "mod thing;\n\
                   #[cfg(test)]\n\
                   mod thing_tests;\n";
        let thing = "fn f() { aterm_pty::shipped(); }\n\
                     #[cfg(test)]\n\
                     mod tests {\n\
                     \x20   fn t() { aterm_pty::spawn_session_leader_on_pty(); }\n\
                     }\n\
                     #[cfg(any(test, debug_assertions))]\n\
                     fn seam_user() { aterm_pty::spawn_session_leader_on_pty(); }\n";
        let calls = "fn t() { aterm_pty::spawn_session_leader_on_pty(); }\n";
        let fx = "pub mod child;\n\
                  #[cfg(any(test, debug_assertions))]\n\
                  mod sim;\n";
        let child = "#[cfg(any(test, debug_assertions))]\n\
                     pub struct ScriptedChild;\n";
        let sim = "use super::child::ScriptedChild;\n";
        let net = "mod x;\n";
        let unrelated = "fn spawn_session_leader_on_pty() {}\n\
                         fn h() { spawn_session_leader_on_pty(); }\n";
        let found = run(
            &SEAM_CALLER,
            &[
                ("crates/aterm-pty/src/unix.rs", unix),
                ("crates/aterm-pty/src/lib.rs", lib),
                ("crates/aterm-pty/src/other.rs", other),
                ("crates/aterm-uds/src/lib.rs", uds),
                ("crates/aterm-uds/src/twin.rs", twin),
                ("crates/aterm-agent/src/lib.rs", agent),
                ("crates/aterm-agent/src/harness/mod.rs", harness),
                ("crates/aterm-agent/src/harness/agy_signin.rs", signin),
                ("crates/aterm-agent/src/harness/grouped.rs", grouped),
                ("crates/aterm-gui/src/lib.rs", gui),
                ("crates/aterm-gui/src/thing.rs", thing),
                ("crates/aterm-gui/src/thing_tests.rs", calls),
                ("crates/aterm-gui/tests/it.rs", calls),
                ("crates/agh/src/lib.rs", "pub mod fx;\n"),
                ("crates/agh/src/fx/mod.rs", fx),
                ("crates/agh/src/fx/child.rs", child),
                ("crates/agh/src/fx/sim.rs", sim),
                ("crates/aterm-net/src/lib.rs", net),
                ("crates/aterm-net/src/x.rs", unrelated),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-agent/src/harness/agy_signin.rs:1",
                "crates/aterm-agent/src/harness/agy_signin.rs:3",
                "crates/aterm-agent/src/harness/grouped.rs:4",
                "crates/aterm-agent/src/harness/grouped.rs:7",
                "crates/aterm-pty/src/lib.rs:4",
                "crates/aterm-pty/src/lib.rs:9",
                "crates/aterm-pty/src/other.rs:2",
                "crates/aterm-pty/src/unix.rs:7",
                "crates/aterm-pty/src/unix.rs:12",
                "crates/aterm-pty/src/unix.rs:19",
            ],
            "{found:#?}"
        );
    }

    /// What a release build compiles: each crate root, and what a `mod` on a release-compiled line
    /// loads from one (beside a root or a `mod.rs`, under a file's stem, or at its `#[path]`).
    /// NEGATIVE CONTROL: resolving every `mod` beside its file (no stem directory) loses
    /// `crates/a/src/a/b.rs`.
    #[test]
    fn a_release_build_compiles_its_roots_and_what_their_mods_load() {
        let corpus = super::super::Corpus::of(&[
            (
                "crates/a/src/lib.rs",
                "pub mod a;\n#[cfg(test)]\nmod t;\n#[path = \"../gen/g.rs\"]\nmod g;\n",
            ),
            (
                "crates/a/src/a.rs",
                "mod b;\n#[cfg(debug_assertions)]\nmod d;\n",
            ),
            ("crates/a/src/a/b.rs", "mod c;\n"),
            ("crates/a/src/a/b/c/mod.rs", ""),
            ("crates/a/src/a/d.rs", ""),
            ("crates/a/src/t.rs", ""),
            ("crates/a/gen/g.rs", ""),
            ("crates/a/src/bin/tool.rs", ""),
            ("crates/a/src/bin/big/main.rs", "mod part;\n"),
            ("crates/a/src/bin/big/part.rs", ""),
            ("crates/a/src/orphan.rs", ""),
            ("crates/a/build.rs", ""),
        ]);
        let tree = Tree {
            root: Path::new("/nonexistent-aterm-census-root"),
            corpus: &corpus,
            engine: crate::guards::tests::NO_ENGINE,
        };
        let shipped: Vec<String> = released_files(&tree).into_iter().collect();
        assert_eq!(
            shipped,
            [
                "crates/a/build.rs",
                "crates/a/gen/g.rs",
                "crates/a/src/a.rs",
                "crates/a/src/a/b.rs",
                "crates/a/src/a/b/c/mod.rs",
                "crates/a/src/bin/big/main.rs",
                "crates/a/src/bin/big/part.rs",
                "crates/a/src/bin/tool.rs",
                "crates/a/src/lib.rs",
            ]
        );
    }

    /// Which top-level lines declare an item, and how far its name reaches.
    /// NEGATIVE CONTROL: skipping `const` before `fn` reads `const fn z` as a `const` named `fn`.
    #[test]
    fn a_top_level_line_names_its_item_and_reach() {
        for (line, want) in [
            ("pub fn x(a: u8) {", Some((Reach::Dependents, "x"))),
            ("pub(crate) struct Y;", Some((Reach::Crate, "Y"))),
            ("pub(in crate::a) enum E {", Some((Reach::Crate, "E"))),
            ("const fn z() -> u8 {", Some((Reach::File, "z"))),
            (
                "pub unsafe extern \" \" fn w() {",
                Some((Reach::Dependents, "w")),
            ),
            ("static mut COUNT: u8 = 0;", Some((Reach::File, "COUNT"))),
            (
                "pub type Alias<T> = Vec<T>;",
                Some((Reach::Dependents, "Alias")),
            ),
            ("    pub fn method(&self) {", None),
            ("impl Foo {", None),
            ("pub mod testing;", None),
            ("pub use a::b;", None),
            ("macro_rules! m {", None),
        ] {
            assert!(item(line) == want, "{line}");
        }
    }

    /// A gate is a seam's when no release build satisfies it and a debug build that is not a
    /// test build may.
    /// NEGATIVE CONTROL: `in_a_debug_build` answering `None` for `test` makes `#[cfg(test)]` a
    /// seam's gate.
    #[test]
    fn a_seams_gate_admits_a_debug_build_and_no_release_build() {
        for (attr, seam) in [
            ("#[cfg(any(test, debug_assertions))]", true),
            ("#[cfg(debug_assertions)]", true),
            ("#[cfg(all(debug_assertions, not(test)))]", true),
            ("#[cfg(all(debug_assertions, unix))]", true),
            ("#[cfg(test)]", false),
            ("#[cfg(all(test, debug_assertions))]", false),
            ("#[cfg(not(debug_assertions))]", false),
            ("#[cfg(feature = \"dev-seams\")]", false),
            (
                "#[cfg(any(debug_assertions, feature = \"dev-seams\"))]",
                false,
            ),
        ] {
            assert_eq!(seam_gate(&[attr, "fn x() {}"], 1), seam, "{attr}");
        }
    }
}
