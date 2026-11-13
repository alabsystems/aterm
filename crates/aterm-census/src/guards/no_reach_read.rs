// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! no-reach-read: no member reads the Markdown `verify land` composes after rerunning only fmt,
//! census and export-scan ([`crate::no_reach`]).
//!
//! A read is one of these, in a member's code or its doctests (the Rust blocks of its doc
//! comments, and of a crate Markdown file a `#[doc = include_str!(…)]` pulls in; comments and
//! every other literal are passed over):
//! - an `include!`, `include_str!` or `include_bytes!` whose argument resolves to a no-reach path;
//! - a string literal that starts `../` (from the file's directory or its crate's), `/../` or
//!   `{}/../` (from its crate's, as after `CARGO_MANIFEST_DIR`), and resolves outside its crate;
//! - a relative literal a test or build script reads from its crate's directory, where Cargo runs
//!   it: the one argument of `read_to_string`, `read`, `read_dir`, `open`, `metadata`,
//!   `canonicalize`, `Path::new` or `PathBuf::from`, or a `cargo:rerun-if-changed=` line;
//! - a path its crate's directory is the root of: `manifest_dir()`, `env!("CARGO_MANIFEST_DIR")`,
//!   `std::env::var("CARGO_MANIFEST_DIR")` (or `var_os`), a `concat!` of the macro and literals,
//!   or a relative `Path::new`/`PathBuf::from` literal (`Path::new`, `PathBuf::from` and
//!   `canonicalize` around one change nothing), followed by `.join(<literal or a const of the
//!   crate>)`, `.parent()`, `.and_then(Path::parent)`, `.unwrap()`, `.expect(…)`, `.into()`, `?`
//!   and the like, or as the first argument of a `format!("{}/…", …)` (or a binding named in a
//!   literal's leading `{name}/…`): in one chain, through a `let` binding used later in its block
//!   (a rebinding's own initializer included), or through a function of the same file that takes
//!   nothing and returns one.
//!
//! It reads a no-reach file the tree holds, or a directory under `docs/` that holds one (a bare
//! repository root is no read of its own, and a path the tree does not hold reads nothing). What
//! the lexer cannot follow is not caught: a helper of another file or one with parameters, a const
//! in another crate, a path joined from a variable, or a `git show`. `verify land`'s own search
//! for each shared path's file name (`aterm_census::no_reach::may_read`) backs this up there.

use std::collections::{BTreeMap, BTreeSet};

use super::export_includes::{crate_dir, includes, resolved};
use super::{Bound, Finding, Rule, SourceFile, Tree, hits, is_word, literal, open_delim, token};
use crate::no_reach::{Layout, class};

pub(super) const NO_REACH_READ: Rule = Rule {
    id: "no-reach-read",
    why: "`verify land` composes main's edits to root Markdown, docs/**/*.md and a member's \
          SPECS.md after rerunning only fmt, census and export-scan on the merge \
          (aterm_census::no_reach), because no build, include or test reads one; a member that \
          reads one runs a test on text no landing judges again. Read a fixture of the crate's \
          own, or move the text under the crate",
    scope: &[],
    check: no_reach_read,
};

/// What a file must mention to hold a read the rule sees.
const NEEDLES: [&str; 8] = [
    "include",
    "../",
    "manifest_dir",
    "CARGO_MANIFEST_DIR",
    "SPECS.md",
    "Path::new",
    "PathBuf::from",
    "rerun-if-changed",
];

/// The calls a relative literal argument of reads from the working directory (a test's or build
/// script's is its crate's).
const CWD_CALLS: [&str; 8] = [
    "read_to_string",
    "read",
    "read_dir",
    "open",
    "metadata",
    "canonicalize",
    "new",
    "from",
];

fn no_reach_read(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let paths = tree.corpus.paths();
    let present: BTreeSet<&str> = paths.iter().map(String::as_str).collect();
    let layout = Layout::new(
        tree.text("Cargo.toml").as_deref(),
        paths.iter().map(String::as_str),
    );
    let reads = Reads {
        present: &present,
        layout: &layout,
    };
    let consts = crate_consts(tree, &present);
    let none = BTreeMap::new();
    for file in tree.corpus.under(&[""]) {
        if !file.mentions(&NEEDLES) {
            continue;
        }
        let dir = file.rel.rsplit_once('/').map_or("", |(dir, _)| dir);
        let Some(krate) = crate_dir(dir, &present) else {
            continue;
        };
        let consts = consts.get(krate).unwrap_or(&none);
        let mut found = BTreeSet::new();
        read_file(file, dir, krate, consts, &reads, &mut found);
        let raw = file.raw();
        if raw.contains("```") || raw.contains("~~~") {
            let doctests = SourceFile::outside(file.rel.clone(), crate::no_reach::doctests(raw));
            read_file(&doctests, dir, krate, consts, &reads, &mut found);
        }
        report(file, found, out);
        for (at, target) in includes(file, reads.present) {
            if !target.ends_with(".md") || class(&target, reads.layout) || !doc_include(file, at) {
                continue;
            }
            let Some(markdown) = tree.text(&target) else {
                continue;
            };
            let blocks = SourceFile::outside(target.clone(), crate::no_reach::fenced(&markdown));
            let mut found = BTreeSet::new();
            read_file(&blocks, dir, krate, consts, &reads, &mut found);
            report(&blocks, found, out);
        }
    }
}

/// Each read `found` in `file`, as a finding.
fn report(file: &SourceFile, found: BTreeSet<(usize, String)>, out: &mut Vec<Finding>) {
    for (line, target) in found {
        out.push(file.finding(
            NO_REACH_READ.id,
            line,
            format!(
                "reads `{target}`, which `verify land` composes after rerunning only fmt, census \
                 and export-scan"
            ),
        ));
    }
}

/// Whether the include macro at `at` of `file` is a `#[doc = …]` (or `#![doc = …]`) value: the
/// Markdown it reads is documentation, whose Rust blocks are doctests.
fn doc_include(file: &SourceFile, at: usize) -> bool {
    let code = &file.lexed().code;
    let Some(head) = code[..at].trim_end().strip_suffix('=') else {
        return false;
    };
    let Some(head) = head.trim_end().strip_suffix("doc") else {
        return false;
    };
    let head = head.trim_end();
    // `#[doc = …]`, or a `cfg_attr`'s `doc = …` argument.
    head.ends_with("#[") || head.ends_with("#![") || head.ends_with(',') || head.ends_with('(')
}

/// The tree's paths and the class's layout: what a resolved path is judged against.
struct Reads<'a> {
    present: &'a BTreeSet<&'a str>,
    layout: &'a Layout,
}

impl Reads<'_> {
    /// Whether reading root-relative `path` reads no-reach text: a no-reach file the tree holds,
    /// or a directory under `docs/` that holds one. A path the tree does not hold reads nothing
    /// (a scratch repository's fixture, a symlink's target), until a tree holds it.
    fn reaches(&self, path: &str) -> bool {
        if self.present.contains(path) {
            return class(path, self.layout);
        }
        if !(path == "docs" || path.starts_with("docs/")) {
            return false;
        }
        let under = format!("{path}/");
        self.present
            .range(under.as_str()..)
            .take_while(|p| p.starts_with(&under))
            .any(|p| class(p, self.layout))
    }
}

/// Each `const NAME: &str = "…";` of each crate (by its directory), by name: `None` where the
/// crate gives one name two values.
fn crate_consts(
    tree: &Tree<'_>,
    present: &BTreeSet<&str>,
) -> BTreeMap<String, BTreeMap<String, Option<String>>> {
    let mut out: BTreeMap<String, BTreeMap<String, Option<String>>> = BTreeMap::new();
    for file in tree.corpus.under(&[""]) {
        if !file.mentions(&["const"]) {
            continue;
        }
        let dir = file.rel.rsplit_once('/').map_or("", |(dir, _)| dir);
        let Some(krate) = crate_dir(dir, present) else {
            continue;
        };
        let lexed = file.lexed();
        let (code, text) = (lexed.code.as_str(), lexed.text.as_str());
        for at in hits(code, "const", Bound::Word) {
            let Some((name, value)) = string_const(code, text, at + "const".len()) else {
                continue;
            };
            out.entry(krate.to_owned())
                .or_default()
                .entry(name.to_owned())
                .and_modify(|held| {
                    if held.as_deref() != Some(value) {
                        *held = None;
                    }
                })
                .or_insert_with(|| Some(value.to_owned()));
        }
    }
    out
}

/// After `const`: `NAME: <type> = "<literal>";`, its name and literal.
fn string_const<'a>(code: &'a str, text: &'a str, at: usize) -> Option<(&'a str, &'a str)> {
    let (name, at) = word(code, at)?;
    let at = token(code, at, ":")?;
    let eq = at + code[at..].find('=')?;
    if code[at..eq].contains([';', '{', '}']) || eq - at > 40 {
        return None;
    }
    let (value, next) = literal(code, text, eq + 1)?;
    token(code, next, ";")?;
    Some((name, value))
}

/// The identifier that is the next thing after blanks from `at`, and the offset past it.
fn word(code: &str, at: usize) -> Option<(&str, usize)> {
    let rest = code.get(at..)?;
    let start = at + (rest.len() - rest.trim_start().len());
    let len = code[start..].bytes().take_while(|b| is_word(*b)).count();
    (len > 0).then(|| (&code[start..start + len], start + len))
}

/// Every read of `file` (in directory `dir`, of the crate at `krate`) the rule sees, by the
/// 1-based line where it starts and the root-relative path it reads.
fn read_file(
    file: &SourceFile,
    dir: &str,
    krate: &str,
    consts: &BTreeMap<String, Option<String>>,
    reads: &Reads<'_>,
    found: &mut BTreeSet<(usize, String)>,
) {
    let lexed = file.lexed();
    let (code, text) = (lexed.code.as_str(), lexed.text.as_str());
    let line = |at: usize| text[..at].matches('\n').count() + 1;
    for (at, target) in includes(file, reads.present) {
        if reads.reaches(&target) {
            found.insert((line(at), target));
        }
    }
    let inside = |path: &str| {
        krate.is_empty()
            || path == krate
            || path
                .strip_prefix(krate)
                .is_some_and(|rest| rest.starts_with('/'))
    };
    let mut work: Vec<Source> = anchors(code, text, krate);
    let mut at = 0;
    while let Some(open) = code[at..].find('"').map(|i| at + i) {
        let Some(close) = code[open + 1..].find('"').map(|i| open + 1 + i) else {
            break;
        };
        at = close + 1;
        let lit = &text[open + 1..close];
        let bases: Vec<(&str, &str)> = if lit.starts_with("../") {
            vec![(dir, lit), (krate, lit)]
        } else if let Some(rest) = lit.strip_prefix("{}").filter(|r| r.starts_with("/../")) {
            vec![(krate, rest)]
        } else if lit.starts_with("/../") {
            vec![(krate, lit)]
        } else {
            Vec::new()
        };
        for (base, path) in bases {
            if let Some(target) = resolved(base, path)
                && !inside(&target)
                && reads.reaches(&target)
            {
                found.insert((line(open), target));
            }
        }
        // What a test or build script reads from its crate's directory, where Cargo runs it.
        let rerun = ["cargo:rerun-if-changed=", "cargo::rerun-if-changed="]
            .into_iter()
            .find_map(|key| lit.strip_prefix(key));
        if let Some(path) = rerun.or_else(|| cwd_argument(code, open, close).then_some(lit))
            && !path.starts_with(['/', '{'])
            && !path.contains("://")
            && let Some(target) = resolved(krate, path)
        {
            if reads.reaches(&target) {
                found.insert((line(open), target.clone()));
            }
            if rerun.is_none()
                && let Some((start, end)) = path_constructor(code, open, close)
            {
                work.push(Source {
                    start,
                    end,
                    value: target,
                });
            }
        }
    }
    let mut bound = BTreeSet::new();
    while let Some(source) = work.pop() {
        let (mut value, mut end) = chain(code, text, source.end, Some(source.value), consts);
        let mut start = source.start;
        while value.is_some()
            && let Some((outer, inner_end)) = wrapper(code, start, end)
        {
            (value, end) = chain(code, text, inner_end, value, consts);
            start = outer;
        }
        let Some(value) = value else { continue };
        if reads.reaches(&value) {
            found.insert((line(source.start), value.clone()));
        }
        if let Some((lit_at, tail)) = format_tail(code, text, start)
            && let Some(target) = resolved(&value, tail)
            && reads.reaches(&target)
        {
            found.insert((line(lit_at), target));
        }
        if let Some(name) = helper(code, start, end)
            && bound.insert((name.to_owned(), start))
        {
            for call in hits(code, name, Bound::Word) {
                let before = code[..call].trim_end();
                if (before.ends_with("fn") && word_ends(before, "fn")) || before.ends_with('.') {
                    continue;
                }
                let Some(end) =
                    token(code, call + name.len(), "(").and_then(|p| token(code, p, ")"))
                else {
                    continue;
                };
                work.push(Source {
                    start: path_start(code, call),
                    end,
                    value: value.clone(),
                });
            }
        }
        let Some((name, scope)) = binding(code, start, end) else {
            continue;
        };
        if !bound.insert((name.to_owned(), start)) {
            continue;
        }
        for use_at in hits(&code[scope.0..scope.1], name, Bound::Word) {
            let use_at = scope.0 + use_at;
            let before = code[..use_at].trim_end();
            let before_let = before.strip_suffix("mut").map_or(before, str::trim_end);
            if before.ends_with('.')
                || before.ends_with("::")
                || (before_let.ends_with("let") && word_ends(before_let, "let"))
            {
                continue;
            }
            work.push(Source {
                start: use_at,
                end: use_at + name.len(),
                value: value.clone(),
            });
        }
        named_in_literals(code, text, scope, name, &value, reads, &line, found);
    }
}

/// Whether the literal at `[open, close]` (its quotes) is the one argument of a call that reads
/// a path from the working directory ([`CWD_CALLS`]; `new` and `from` only after `::`).
fn cwd_argument(code: &str, open: usize, close: usize) -> bool {
    let Some(head) = code[..open].trim_end().strip_suffix('(') else {
        return false;
    };
    let head = head.trim_end();
    if token(code, close + 1, ")").is_none() {
        return false;
    }
    CWD_CALLS.into_iter().any(|name| {
        head.ends_with(name)
            && word_ends(head, name)
            && (!matches!(name, "new" | "from")
                || head[..head.len() - name.len()].trim_end().ends_with("::"))
    })
}

/// Where the literal at `[open, close]` is the argument of `Path::new` or `PathBuf::from` (any
/// path before it): where that call starts and the offset past its `)`, a root to follow.
fn path_constructor(code: &str, open: usize, close: usize) -> Option<(usize, usize)> {
    let head = code[..open].trim_end().strip_suffix('(')?.trim_end();
    let name = ["new", "from"]
        .into_iter()
        .find(|name| head.ends_with(name) && word_ends(head, name))?;
    let name_at = head.len() - name.len();
    let owner = head[..name_at].trim_end().strip_suffix("::")?.trim_end();
    if !(owner.ends_with("Path") && word_ends(owner, "Path"))
        && !(owner.ends_with("PathBuf") && word_ends(owner, "PathBuf"))
    {
        return None;
    }
    Some((path_start(code, name_at), token(code, close + 1, ")")?))
}

/// Where the path expression at `start` is the first argument after a format string that starts
/// `{}` (`format!("{}/SPECS.md", root)`): the literal's offset and the path after its `{}`.
fn format_tail<'a>(code: &str, text: &'a str, start: usize) -> Option<(usize, &'a str)> {
    let head = code[..start].trim_end().strip_suffix(',')?.trim_end();
    let close = head.len().checked_sub(1)?;
    (head.as_bytes()[close] == b'"').then_some(())?;
    let open = head[..close].rfind('"')?;
    let lit = text.get(open + 1..close)?;
    let tail = lit.strip_prefix("{}")?;
    (!tail.contains(['{', '}'])).then_some((open, tail))
}

/// Where the path expression at `[start, end)` is the whole body of a function that takes
/// nothing (`fn root() -> PathBuf { <it> }`): its name.
fn helper(code: &str, start: usize, end: usize) -> Option<&str> {
    token(code, end, "}")?;
    let head = code[..start].trim_end().strip_suffix('{')?;
    let fn_at = head.rfind("fn ")?;
    if fn_at > 0 && is_word(head.as_bytes()[fn_at - 1]) {
        return None;
    }
    let signature = head[fn_at + 3..].trim();
    let (name, rest) = signature.split_once('(')?;
    let name = name.trim();
    let rest = rest.trim_start().strip_prefix(')')?.trim();
    if name.is_empty()
        || !name.bytes().all(is_word)
        || !(rest.is_empty() || rest.starts_with("->"))
        || rest.contains(['{', '}', ';'])
    {
        return None;
    }
    Some(name)
}

/// Each literal in `scope` that starts `{name}/` (an inline format argument of the binding
/// `name`, which holds `value`) and reads no-reach text, into `found`.
#[expect(
    clippy::too_many_arguments,
    reason = "one step of read_file's worklist, over its locals"
)]
fn named_in_literals(
    code: &str,
    text: &str,
    scope: (usize, usize),
    name: &str,
    value: &str,
    reads: &Reads<'_>,
    line: &dyn Fn(usize) -> usize,
    found: &mut BTreeSet<(usize, String)>,
) {
    let lead = format!("{{{name}}}");
    let mut at = scope.0;
    while let Some(open) = code[at..scope.1].find('"').map(|i| at + i) {
        let Some(close) = code[open + 1..scope.1].find('"').map(|i| open + 1 + i) else {
            break;
        };
        at = close + 1;
        if let Some(tail) = text[open + 1..close].strip_prefix(&lead)
            && !tail.contains(['{', '}'])
            && let Some(target) = resolved(value, tail)
            && reads.reaches(&target)
        {
            found.insert((line(open), target));
        }
    }
}

/// Where a path value begins: the offset of its expression, the offset past its root, and the
/// root-relative path it holds there.
struct Source {
    start: usize,
    end: usize,
    value: String,
}

/// Every place `code` names the crate's directory (the module doc's roots), each with what it
/// holds.
fn anchors(code: &str, text: &str, krate: &str) -> Vec<Source> {
    let mut out = Vec::new();
    let mut push = |at: usize, end: usize, value: String| {
        out.push(Source {
            start: path_start(code, at),
            end,
            value,
        });
    };
    for at in hits(code, "manifest_dir", Bound::Word) {
        if let Some(end) =
            token(code, at + "manifest_dir".len(), "(").and_then(|p| token(code, p, ")"))
        {
            push(at, end, krate.to_owned());
        }
    }
    for at in hits(code, "env", Bound::Word) {
        if let Some(end) = manifest_env(code, text, at) {
            push(at, end, krate.to_owned());
        }
    }
    for name in ["var", "var_os"] {
        for at in hits(code, name, Bound::Word) {
            if let Some(end) = token(code, at + name.len(), "(").and_then(|open| {
                let (var, next) = literal(code, text, open)?;
                (var == "CARGO_MANIFEST_DIR").then_some(())?;
                token(code, next, ")")
            }) {
                push(at, end, krate.to_owned());
            }
        }
    }
    for at in hits(code, "concat", Bound::Word) {
        if let Some((tail, end)) = manifest_concat(code, text, at)
            && let Some(value) = resolved(krate, &tail)
        {
            push(at, end, value);
        }
    }
    out
}

/// `env!("CARGO_MANIFEST_DIR")` at `at`: the offset past it.
fn manifest_env(code: &str, text: &str, at: usize) -> Option<usize> {
    let at = token(code, at + "env".len(), "!")?;
    let (open, close) = open_delim(code, at)?;
    let (var, next) = literal(code, text, open)?;
    (var == "CARGO_MANIFEST_DIR").then_some(())?;
    token(code, next, close)
}

/// `concat!(env!("CARGO_MANIFEST_DIR"), "…", …)` at `at`: its literals joined, and the offset
/// past it.
fn manifest_concat(code: &str, text: &str, at: usize) -> Option<(String, usize)> {
    let at = token(code, at + "concat".len(), "!")?;
    let (open, close) = open_delim(code, at)?;
    let (_, env_at) = word(code, open).filter(|(w, _)| *w == "env")?;
    let mut at = manifest_env(code, text, env_at - "env".len())?;
    let mut tail = String::new();
    loop {
        if let Some(end) = token(code, at, close) {
            return Some((tail, end));
        }
        at = token(code, at, ",")?;
        if let Some(end) = token(code, at, close) {
            return Some((tail, end));
        }
        let (part, next) = literal(code, text, at)?;
        tail.push_str(part);
        at = next;
    }
}

/// Where the path expression at `[start, end)` is the one argument of a call that changes no
/// path (`Path::new(…)`, `PathBuf::from(…)`, `canonicalize(…)`, each with any path before it):
/// where that call starts, and the offset past its `)`.
fn wrapper(code: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let open = code[..start].trim_end().strip_suffix('(')?.trim_end();
    let name = ["new", "from", "canonicalize"]
        .into_iter()
        .find(|name| open.ends_with(name) && word_ends(open, name))?;
    let name_at = open.len() - name.len();
    if matches!(name, "new" | "from") && !open[..name_at].trim_end().ends_with("::") {
        return None;
    }
    let close = token(code, end, ")")?;
    Some((path_start(code, name_at), close))
}

/// Whether `s` ends with the whole word `name`.
fn word_ends(s: &str, name: &str) -> bool {
    s.len() == name.len() || !is_word(s.as_bytes()[s.len() - name.len() - 1])
}

/// The start of the path `segment::segment::…` that ends with the word at `at`.
fn path_start(code: &str, at: usize) -> usize {
    let mut start = at;
    loop {
        let before = code[..start].trim_end();
        let Some(rest) = before.strip_suffix("::") else {
            return start;
        };
        let rest = rest.trim_end();
        let len = rest.bytes().rev().take_while(|b| is_word(*b)).count();
        if len == 0 {
            return start;
        }
        start = rest.len() - len;
    }
}

/// The path `value` holds after the method chain from `at`, and the offset where the chain ends;
/// `None` once a step makes it unknown (a `join` of anything but a literal or a const of the
/// crate, a `parent` of the root).
fn chain(
    code: &str,
    text: &str,
    mut at: usize,
    mut value: Option<String>,
    consts: &BTreeMap<String, Option<String>>,
) -> (Option<String>, usize) {
    while let Some(held) = value.clone() {
        if let Some(next) = token(code, at, "?") {
            at = next;
            continue;
        }
        let Some(dot) = token(code, at, ".") else {
            break;
        };
        let Some((method, after)) = word(code, dot) else {
            break;
        };
        let Some(open) = token(code, after, "(") else {
            break;
        };
        match method {
            "join" => {
                let arg = literal(code, text, open)
                    .map(|(lit, next)| (Some(lit.to_owned()), next))
                    .or_else(|| {
                        let (name, next) = word(code, path_start_forward(code, open)?)?;
                        Some((consts.get(name).cloned().flatten(), next))
                    });
                let Some((arg, next)) = arg else {
                    return (None, at);
                };
                let Some(close) = token(code, next, ")") else {
                    return (None, at);
                };
                value = arg
                    .filter(|path| !path.starts_with('/'))
                    .and_then(|path| resolved(&held, &path));
                at = close;
            }
            "parent" => {
                let Some(close) = token(code, open, ")") else {
                    break;
                };
                value = held
                    .rsplit_once('/')
                    .map(|(up, _)| up.to_owned())
                    .or_else(|| (!held.is_empty()).then(String::new));
                at = close;
            }
            "and_then" | "map"
                if code[open..]
                    .trim_start()
                    .strip_prefix("Path::parent")
                    .or_else(|| {
                        code[open..]
                            .trim_start()
                            .strip_prefix("std::path::Path::parent")
                    })
                    .is_some_and(|rest| rest.trim_start().starts_with(')')) =>
            {
                let Some(close) = code[open..].find(')').map(|i| open + i + 1) else {
                    break;
                };
                value = held
                    .rsplit_once('/')
                    .map(|(up, _)| up.to_owned())
                    .or_else(|| (!held.is_empty()).then(String::new));
                at = close;
            }
            "unwrap" | "to_path_buf" | "as_path" | "to_owned" | "clone" | "canonicalize"
            | "into" | "as_ref" => {
                let Some(close) = token(code, open, ")") else {
                    break;
                };
                at = close;
            }
            "expect" => {
                let Some(close) = literal(code, text, open).and_then(|(_, n)| token(code, n, ")"))
                else {
                    break;
                };
                at = close;
            }
            _ => break,
        }
    }
    (value, at)
}

/// From `at`, past a path's `segment::` prefixes: where its last segment starts.
fn path_start_forward(code: &str, at: usize) -> Option<usize> {
    let mut at = at;
    loop {
        let (_, after) = word(code, at)?;
        match token(code, after, "::") {
            Some(next) => at = next,
            None => return Some(at),
        }
    }
}

/// Where the expression at `[start, end)` is a `let`'s whole initializer (`let [mut] name[: T] =
/// <it>;`): the name, and the rest of its block up to the name's next `let`.
fn binding(code: &str, start: usize, end: usize) -> Option<(&str, (usize, usize))> {
    let semi = token(code, end, ";")?;
    let head = code[..start].trim_end().strip_suffix('=')?;
    let boundary = head.rfind([';', '{', '}']).map_or(0, |at| at + 1);
    let statement = head[boundary..].trim();
    let rest = statement.strip_prefix("let")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix("mut")
        .filter(|r| r.starts_with(char::is_whitespace))
        .map_or(rest, str::trim_start);
    let len = rest.bytes().take_while(|b| is_word(*b)).count();
    let name = &rest[..len];
    let tail = rest[len..].trim();
    if name.is_empty() || !(tail.is_empty() || tail.starts_with(':')) {
        return None;
    }
    let mut depth = 0usize;
    let mut open = None;
    for (i, b) in code.as_bytes()[..start].iter().enumerate().rev() {
        match b {
            b'}' => depth += 1,
            b'{' if depth == 0 => {
                open = Some(i);
                break;
            }
            b'{' => depth -= 1,
            _ => {}
        }
    }
    let mut scope_end = open
        .and_then(|open| super::brace_close(code, open))
        .unwrap_or(code.len());
    for again in hits(&code[semi..scope_end], name, Bound::Word) {
        let before = code[..semi + again].trim_end();
        let before = before.strip_suffix("mut").map_or(before, str::trim_end);
        if before.ends_with("let") && word_ends(before, "let") {
            // A rebinding's own initializer still reads this one (`let dir = dir.join(…);`).
            scope_end = code[semi + again..scope_end]
                .find(';')
                .map_or(scope_end, |at| semi + again + at);
            break;
        }
    }
    Some((name, (semi, scope_end)))
}

#[cfg(test)]
#[path = "no_reach_read_tests.rs"]
mod tests;
