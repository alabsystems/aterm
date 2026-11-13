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

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::debug_seam::{crate_of, is_ident, module_of, names, release_never_compiles};
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

/// The files under [`SOURCE`], in corpus order, each with the lines of its code view a release
/// build compiles ([`unmasked_lines`] under [`release_never_compiles`]), worked out at most once
/// per file, and only for a file whose line a pass asks about.
struct Sources<'a> {
    files: Vec<&'a SourceFile>,
    compiled: Vec<OnceCell<Vec<bool>>>,
}

impl<'a> Sources<'a> {
    fn of(tree: &Tree<'a>) -> Self {
        let files: Vec<&SourceFile> = tree.corpus.under(SOURCE).collect();
        let compiled = files.iter().map(|_| OnceCell::new()).collect();
        Self { files, compiled }
    }

    /// Whether a release build compiles line `at` (0-based) of file `i`'s code view: a line past
    /// the mask's end is compiled.
    fn compiled(&self, i: usize, at: usize) -> bool {
        self.compiled[i]
            .get_or_init(|| unmasked_lines(&self.files[i].lexed().code, &release_never_compiles))
            .get(at)
            .copied()
            .unwrap_or(true)
    }
}

fn seam_caller(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let sources = Sources::of(tree);
    let shipped = released_files(&sources);
    let read =
        |file: &SourceFile| shipped.contains(&file.rel) && !file.rel.starts_with(CENSUS_CRATE);
    let mut gated = Vec::new();
    for (i, file) in sources.files.iter().enumerate() {
        if !read(file)
            || !file.mentions(&["#[cfg("])
            || !file.mentions(&["debug_assertions", "test"])
        {
            continue;
        }
        let code = &file.lexed().code;
        let lines: Vec<&str> = code.lines().collect();
        for (at, line) in lines.iter().enumerate() {
            let Some((reach, name)) = item(line) else {
                continue;
            };
            if !seam_gate(&lines, at) || sources.compiled(i, at) {
                continue;
            }
            let (krate, crate_ident) = crate_of(&file.rel);
            gated.push(Seam {
                name: name.to_owned(),
                file: file.rel.clone(),
                line: at,
                reach,
                krate,
                crate_ident,
                module: module_of(&file.rel),
                root: crate_root(&file.rel),
            });
        }
    }
    if gated.is_empty() {
        return;
    }
    // A name a gated crate also declares on a top-level line a release build compiles has a
    // release twin, and is no seam.
    let gated_crates: BTreeSet<&str> = gated.iter().map(|s| s.krate.as_str()).collect();
    let gated_names: HashSet<&str> = gated.iter().map(|s| s.name.as_str()).collect();
    let mut released: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (i, file) in sources.files.iter().enumerate() {
        if !read(file) || !gated_crates.iter().any(|k| file.rel.starts_with(k)) {
            continue;
        }
        for (at, line) in file.lexed().code.lines().enumerate() {
            let Some((_, name)) = item(line) else {
                continue;
            };
            if gated_names.contains(name) && sources.compiled(i, at) {
                released
                    .entry(crate_of(&file.rel).0)
                    .or_default()
                    .insert(name.to_owned());
            }
        }
    }
    gated.retain(|s| !released.get(&s.krate).is_some_and(|n| n.contains(&s.name)));
    if gated.is_empty() {
        return;
    }
    let index = Index::of(&gated);
    for (i, file) in sources.files.iter().enumerate() {
        if !read(file) {
            continue;
        }
        let code = &file.lexed().code;
        let Some(mentions) = index.mentions(code) else {
            continue;
        };
        let newlines: Vec<usize> = code.match_indices('\n').map(|(at, _)| at).collect();
        // Each seam in turn, each of its mentions in the order the code holds them.
        for &(s, at) in &mentions.hits {
            let seam = &gated[s];
            let same_file = file.rel == seam.file;
            let same_crate = file.rel.starts_with(&seam.krate);
            let reaches = same_file
                || (seam.reach != Reach::File && same_crate)
                || (seam.reach == Reach::Dependents && mentions.names_crate(&index, s, seam, code));
            if !reaches {
                continue;
            }
            let line = newlines.partition_point(|&nl| nl < at);
            let before = &code[..at];
            let after = &code[at + seam.name.len()..];
            if method(before)
                || after.starts_with('!')
                || declares(before)
                || binds(after)
                || gated_on_its_line(before)
                || !names_the_seam(path_of(before), seam, same_file, same_crate)
                || !sources.compiled(i, line)
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

/// The seams by the identifier a mention of each is, and the crates the dependents' seams are
/// named through. A name is an identifier ([`item`] cuts it at the first character [`is_ident`]
/// refuses), so its whole-word mentions are exactly the identifiers of a code view equal to it.
struct Index<'s> {
    /// Each name or crate's identifier: the seams that name is (in seam order) and the crate's
    /// slot, if it is one.
    words: HashMap<&'s str, Word>,
    /// Per seam, the slot of the crate it is named through from another crate; `None` when that
    /// name is not one identifier (a directory named `a.b`), which [`names`] reads instead.
    crate_slot: Vec<Option<usize>>,
    crates: usize,
    /// Per first byte, the lengths of the words that start with it (bit `min(len, 63)`): most
    /// identifiers of a file are none of them and are passed over without a lookup.
    lengths: [u64; 256],
}

/// What one identifier is to the index: the seams so named, and the crate it names.
#[derive(Default)]
struct Word {
    seams: Vec<usize>,
    crate_slot: Option<usize>,
}

/// The mentions of seams one code view holds, as `(seam, offset)` sorted, and which crates'
/// identifiers it holds as a whole word.
struct Mentions {
    hits: Vec<(usize, usize)>,
    crates: Vec<bool>,
}

impl<'s> Index<'s> {
    fn of(gated: &'s [Seam]) -> Self {
        let mut words: HashMap<&str, Word> = HashMap::new();
        for (s, seam) in gated.iter().enumerate() {
            words.entry(seam.name.as_str()).or_default().seams.push(s);
        }
        let mut crates = 0;
        let mut crate_slot = Vec::with_capacity(gated.len());
        for seam in gated {
            let ident = seam.crate_ident.as_str();
            if ident.is_empty() || !ident.chars().all(is_ident) {
                crate_slot.push(None);
                continue;
            }
            let word = words.entry(ident).or_default();
            let slot = *word.crate_slot.get_or_insert_with(|| {
                crates += 1;
                crates - 1
            });
            crate_slot.push(Some(slot));
        }
        let mut lengths = [0u64; 256];
        for word in words.keys() {
            lengths[usize::from(word.as_bytes()[0])] |= 1 << word.len().min(63);
        }
        Self {
            words,
            crate_slot,
            crates,
            lengths,
        }
    }

    /// The seams `code` mentions as a whole word, or `None` when it mentions none.
    fn mentions(&self, code: &str) -> Option<Mentions> {
        let mut hits = Vec::new();
        let mut crates = vec![false; self.crates];
        for (at, ident) in identifiers(code) {
            let first = usize::from(ident.as_bytes()[0]);
            if self.lengths[first] & (1 << ident.len().min(63)) == 0 {
                continue;
            }
            let Some(word) = self.words.get(ident) else {
                continue;
            };
            hits.extend(word.seams.iter().map(|&s| (s, at)));
            if let Some(slot) = word.crate_slot {
                crates[slot] = true;
            }
        }
        if hits.is_empty() {
            return None;
        }
        hits.sort_unstable();
        Some(Mentions { hits, crates })
    }
}

impl Mentions {
    /// Whether `code`, the view these mentions were read from, names the crate of seam `s`
    /// (`seam`) as a whole word, as [`names`] reads it.
    fn names_crate(&self, index: &Index<'_>, s: usize, seam: &Seam, code: &str) -> bool {
        match index.crate_slot[s] {
            Some(slot) => self.crates[slot],
            None => names(code, &seam.crate_ident),
        }
    }
}

/// Each identifier of `code` with its byte offset, in order: a maximal run of the bytes
/// [`is_ident`] accepts (all ASCII, so no run splits a character).
fn identifiers(code: &str) -> impl Iterator<Item = (usize, &str)> {
    let bytes = code.as_bytes();
    let ident = |b: u8| is_ident(char::from(b));
    let mut at = 0;
    std::iter::from_fn(move || {
        while at < bytes.len() && !ident(bytes[at]) {
            at += 1;
        }
        if at == bytes.len() {
            return None;
        }
        let start = at;
        while at < bytes.len() && ident(bytes[at]) {
            at += 1;
        }
        Some((start, &code[start..at]))
    })
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
fn released_files(sources: &Sources<'_>) -> BTreeSet<String> {
    let mut loads: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (i, file) in sources.files.iter().enumerate() {
        if !file.mentions(&["mod "]) {
            continue;
        }
        let Lexed { code, text } = file.lexed();
        let mut texts: Option<Vec<&str>> = None;
        for (at, line) in code.lines().enumerate() {
            let Some(name) = mod_declaration(line) else {
                continue;
            };
            if !sources.compiled(i, at) {
                continue;
            }
            let texts = texts.get_or_insert_with(|| text.lines().collect());
            let (dir, base) = module_dirs(&file.rel);
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
    let present: BTreeSet<&str> = sources.files.iter().map(|f| f.rel.as_str()).collect();
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
#[path = "seam_caller_tests.rs"]
mod tests;
