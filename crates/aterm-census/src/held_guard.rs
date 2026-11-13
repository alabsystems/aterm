// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Census check disallowing unbounded O(history) work under a held `term` guard.

use std::path::Path;

/// The source the GUI process runs under the per-session `term` mutex, with
/// the window crate's split-out libraries ([`crate::gui_split_src_dirs`]).
pub(crate) const HELD_GUARD_SCOPE: &[&str] = &["crates/aterm-gui/src", "crates/aterm-control/src"];

/// The O(history) methods that may never run on a held guard.
const BLOCKED_METHODS: &[&str] = &[
    "resize",
    "reflow",
    "rewrap",
    "take_scrollback",
    "decompress",
    "hydrate",
    "rebuild_index",
];

/// The cold tier's codec, by path: a call into the aterm-lz4 crate
/// (`aterm_lz4::`). It is the only codec path a line in scope can name: the
/// store's cold-page encode and decode are private to aterm-scrollback, and so
/// is its `lz4` module outside the fuzz build. Neither scope crate depends on
/// aterm-lz4 today, so this fires only once one decodes cold pages itself.
/// A path rather than a codec name, so the zstd archive decoder (`aterm_zstd`)
/// is not mistaken for the cold tier.
const COLD_CODEC: &str = "lz4::";

/// One blocklisted-work-under-guard line.
pub(crate) struct HeldGuardFinding {
    /// Repo-relative `file:line`.
    pub(crate) span: String,
    /// The source line, trimmed.
    pub(crate) line: String,
}

/// OB-22 over the tree: the files read, the acquire lines seen (a walk that
/// sees none is blind, not clean), and the findings.
pub(crate) struct HeldGuardScan {
    pub(crate) files: usize,
    pub(crate) acquires: usize,
    pub(crate) findings: Vec<HeldGuardFinding>,
}

/// Scan [`HELD_GUARD_SCOPE`] and the window crate's split-out libraries under
/// `root`. A scope directory that is missing is an `Err`: a guard whose scan
/// set shrank has proven nothing about what left.
pub(crate) fn scan_tree(root: &Path) -> Result<HeldGuardScan, String> {
    let mut files = Vec::new();
    for dir in HELD_GUARD_SCOPE {
        let abs = root.join(dir);
        if !crate::tree::is_dir(&abs) {
            return Err(format!("the scan set lost `{dir}` (no such directory)"));
        }
        crate::collect_rs_files(&abs, &mut files)
            .map_err(|e| format!("cannot walk `{dir}`: {e}"))?;
    }
    for src in crate::gui_split_src_dirs(root) {
        crate::collect_rs_files(&src, &mut files)
            .map_err(|e| format!("cannot walk `{}`: {e}", src.display()))?;
    }
    files.retain(|p| !crate::is_test_file(p));
    files.sort();
    let mut scan = HeldGuardScan {
        files: files.len(),
        acquires: 0,
        findings: Vec::new(),
    };
    for file in &files {
        let text = crate::tree::read_to_string(file)
            .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let rel = crate::rel_path(root, file);
        let (hits, acquires) = scan_source(&text);
        scan.acquires += acquires;
        let raw: Vec<&str> = text.lines().collect();
        for n in hits {
            scan.findings.push(HeldGuardFinding {
                span: format!("{rel}:{n}"),
                line: raw.get(n - 1).map_or("", |l| l.trim()).to_string(),
            });
        }
    }
    Ok(scan)
}

/// The 1-based lines of one file's shipped code that do blocklisted work under
/// a held guard, and how many lines acquire one.
pub(crate) fn scan_source(text: &str) -> (Vec<usize>, usize) {
    let code = crate::mask_unshipped_items(&crate::lex::blank_non_code(text));
    let mut walk = Walk::default();
    let mut hits = Vec::new();
    let mut acquires = 0;
    for (i, line) in code.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if find_acquire(line).is_some() {
            acquires += 1;
        }
        if walk.step(line, i) {
            hits.push(i + 1);
        }
    }
    (hits, acquires)
}

const fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

const fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// A guard bound by a `let`: the name, the brace depth it was bound at, and
/// the line that bound it.
struct Binding {
    name: String,
    depth: i64,
    line: usize,
}

#[derive(Default)]
struct Walk {
    /// Open parens of an acquiring call still unclosed (its closure body).
    region: i64,
    /// Brace depth, for a binding's scope.
    braces: i64,
    bindings: Vec<Binding>,
    /// A tracked receiver whose chain rustfmt broke onto the following lines.
    pending: Option<String>,
}

impl Walk {
    fn tracked(&self, id: &str) -> bool {
        self.bindings.iter().any(|b| b.name == id)
    }

    /// Advance over one line of code; `true` when it is a finding.
    fn step(&mut self, bal: &str, line: usize) -> bool {
        let mut hit = false;
        if let Some((lhs, rhs)) = split_let(bal) {
            let ids = bind_guard(lhs, rhs).or_else(|| self.bind_alias(lhs, rhs));
            for name in ids.unwrap_or_default() {
                self.bindings.push(Binding {
                    name,
                    depth: self.braces,
                    line,
                });
            }
        }
        let live = !self.bindings.is_empty();
        if self.bindings.iter().any(|b| receives_work(bal, &b.name)) {
            hit = true;
        }
        if live && touches_cold_codec(bal) {
            hit = true;
        }
        if self.pending.is_some() {
            let t = bal.trim_start();
            if t.starts_with('.') && method_call_at(t, 0).is_some() {
                hit = true;
            }
            if bal.contains(';') || !t.starts_with('.') {
                self.pending = None;
            }
        }
        if self.pending.is_none()
            && let Some(first) = bare_receiver_line(bal)
            && self.tracked(first)
        {
            self.pending = Some(first.to_string());
        }
        if self.region > 0 {
            hit |= has_blocked_work(bal);
            self.region = (self.region + paren_balance(bal)).max(0);
        } else if let Some(at) = find_acquire(bal) {
            hit |= has_blocked_work(bal);
            let open = paren_balance(&bal[at..]);
            if open > 0 {
                self.region = open;
            }
        }
        self.braces += count(bal, '{') - count(bal, '}');
        let braces = self.braces;
        self.bindings.retain(|b| {
            b.line == line || (b.depth <= braces && !drops(bal, &b.name) && !shadows(bal, &b.name))
        });
        hit
    }

    /// Identifiers a line binds to a reborrow of a tracked guard: `&`/`*` in
    /// front with at most zero-arg hops after, or `.deref()`/`.deref_mut()`.
    /// A bare accessor reads a value out and is not a guard.
    fn bind_alias(&self, lhs: &str, rhs: &str) -> Option<Vec<String>> {
        let rest = rhs.trim_start();
        let borrow = rest.starts_with(['&', '*']);
        let rest = strip_prefixes(rest, false);
        let id = leading_ident(rest)?;
        if !self.tracked(id) {
            return None;
        }
        let tail = &rest[id.len()..];
        let ok = if borrow {
            let mut t = tail;
            while let Some(after) = zero_arg_hop(t) {
                t = after;
            }
            statement_end(t)
        } else {
            ["deref()", "deref_mut()"].iter().any(|m| {
                tail.strip_prefix('.')
                    .and_then(|t| t.strip_prefix(m))
                    .is_some_and(statement_end)
            })
        };
        ok.then(|| pattern_idents(strip_type(lhs)))
    }
}

fn count(s: &str, c: char) -> i64 {
    s.chars().filter(|x| *x == c).count() as i64
}

fn paren_balance(s: &str) -> i64 {
    count(s, '(') - count(s, ')')
}

/// Where the first acquire on the line starts, if any.
pub(crate) fn find_acquire(bal: &str) -> Option<usize> {
    [
        "term_lock(",
        "term_lock_ui(",
        "term_lock_whole(",
        "term_lock_whole_when(",
        "with_terminal(",
        "with_terminal_mut(",
        "with_terminal_as_it_stands(",
    ]
    .iter()
    .filter_map(|needle| bal.find(needle))
    .min()
}

/// A cold-codec call on the line.
fn touches_cold_codec(bal: &str) -> bool {
    bal.contains(COLD_CODEC)
}

/// A blocklisted method call anywhere on the line, or a cold-codec call.
fn has_blocked_work(bal: &str) -> bool {
    touches_cold_codec(bal)
        || bal
            .match_indices('.')
            .any(|(at, _)| method_call_at(bal, at).is_some())
}

/// `.<blocked>(` starting at byte `at`: the method name.
fn method_call_at(s: &str, at: usize) -> Option<&'static str> {
    let rest = s.get(at..)?.strip_prefix('.')?;
    BLOCKED_METHODS
        .iter()
        .find(|m| rest.strip_prefix(**m).is_some_and(|r| r.starts_with('(')))
        .copied()
}

/// `.<ident>()` at the start of `s` — a zero-arg accessor hop — and what follows.
fn zero_arg_hop(s: &str) -> Option<&str> {
    let rest = s.strip_prefix('.')?;
    let id = leading_ident(rest)?;
    rest[id.len()..].strip_prefix("()")
}

/// Only whitespace and at most one `;` left.
fn statement_end(s: &str) -> bool {
    let t = s.trim();
    t.is_empty() || t == ";"
}

fn leading_ident(s: &str) -> Option<&str> {
    let first = s.chars().next()?;
    if !is_ident_start(first) {
        return None;
    }
    let end = s.find(|c: char| !is_ident(c)).unwrap_or(s.len());
    Some(&s[..end])
}

/// Does `bal` do blocklisted work on the guard `id` — `id.m(`, `(*id).m(`, or
/// either through zero-arg hops (`id.grid_mut().resize(`)?
fn receives_work(bal: &str, id: &str) -> bool {
    let boundary = |at: usize| {
        bal[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !is_ident(c) && c != '.')
    };
    let chain_works = |mut t: &str| loop {
        if method_call_at(t, 0).is_some() {
            return true;
        }
        match zero_arg_hop(t) {
            Some(after) => t = after,
            None => return false,
        }
    };
    for (at, _) in bal.match_indices(id) {
        if boundary(at) && chain_works(&bal[at + id.len()..]) {
            return true;
        }
    }
    for (at, _) in bal.match_indices('(') {
        if !boundary(at) {
            continue;
        }
        let inner = &bal[at + 1..];
        let Some(close) = inner.find([')', '(']) else {
            continue;
        };
        if inner.as_bytes()[close] != b')' {
            continue;
        }
        let content = &inner[..close];
        let Some(before) = content.strip_suffix(id) else {
            continue;
        };
        if before.trim_end_matches([' ', '\t']).ends_with('*') && chain_works(&inner[close + 1..]) {
            return true;
        }
    }
    false
}

/// A line that is only a receiver and zero-arg hops (`t.grid_mut()`): its first
/// identifier, for a chain that continues on the next line.
fn bare_receiver_line(bal: &str) -> Option<&str> {
    let t = bal.trim();
    let id = leading_ident(t)?;
    let mut rest = &t[id.len()..];
    while let Some(after) = zero_arg_hop(rest) {
        rest = after;
    }
    rest.is_empty().then_some(id)
}

/// A `let` statement's pattern and right-hand side (up to and including its
/// first `;`), split at its top-level `=`.
fn split_let(bal: &str) -> Option<(&str, &str)> {
    let at = bal.match_indices("let").find_map(|(at, _)| {
        let before_ok = bal[..at].chars().next_back().is_none_or(|c| !is_ident(c));
        let after_ok = bal[at + 3..].starts_with([' ', '\t']);
        (before_ok && after_ok).then_some(at)
    })?;
    let s = &bal[at..];
    let b = s.as_bytes();
    for i in 0..b.len() {
        if b[i] != b'=' {
            continue;
        }
        let prev = if i > 0 { b[i - 1] } else { b' ' };
        let next = b.get(i + 1).copied().unwrap_or(b' ');
        if next == b'=' || matches!(prev, b'=' | b'!' | b'<' | b'>') {
            continue;
        }
        let rhs = &s[i + 1..];
        let rhs = rhs.find(';').map_or(rhs, |j| &rhs[..=j]);
        return Some((&s[..i], rhs));
    }
    None
}

/// Strip leading `&`, `*`, `mut ` (and, for an acquire, `(`) with whitespace.
fn strip_prefixes(mut s: &str, parens: bool) -> &str {
    loop {
        let t = s.trim_start();
        if let Some(r) = t.strip_prefix(['&', '*']) {
            s = r;
        } else if parens && let Some(r) = t.strip_prefix('(') {
            s = r;
        } else if let Some(r) = t.strip_prefix("mut").filter(|r| r.starts_with([' ', '\t'])) {
            s = r;
        } else {
            return t;
        }
    }
}

/// Identifiers a line binds to the guard itself: the right-hand side's top is
/// the acquire, closing on this line with nothing chained after. A chain reads
/// a value out (e.g. `let pending = term_lock(t).resize_offloading_…`).
fn bind_guard(lhs: &str, rhs: &str) -> Option<Vec<String>> {
    let mut rest = strip_prefixes(rhs, true);
    while let Some(id) = leading_ident(rest)
        && let Some(after) = rest[id.len()..].strip_prefix("::")
    {
        rest = after;
    }
    let open = if rest.starts_with("term_lock(") {
        "term_lock".len()
    } else if rest.starts_with("term_lock_ui(") {
        "term_lock_ui".len()
    } else if rest.starts_with("term_lock_whole(") {
        "term_lock_whole".len()
    } else if rest.starts_with("term_lock_whole_when(") {
        "term_lock_whole_when".len()
    } else {
        return None;
    };
    let mut depth = 0i64;
    let mut close = None;
    for (i, c) in rest[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let tail = rest[close? + 1..].trim_start();
    if tail.starts_with(['.', '?']) {
        return None;
    }
    Some(pattern_idents(strip_type(lhs)))
}

/// The pattern with its type annotation dropped: everything before the first
/// `:` that is not half of a `::` path.
fn strip_type(lhs: &str) -> &str {
    let b = lhs.as_bytes();
    for i in 0..b.len() {
        if b[i] == b':' && (i == 0 || b[i - 1] != b':') && b.get(i + 1).copied() != Some(b':') {
            return &lhs[..i];
        }
    }
    lhs
}

/// The identifiers a pattern binds: not `let`/`mut`/`ref`/`_`, and not a path
/// or constructor (an identifier followed by `(`, `[`, `{` or `::`).
fn pattern_idents(lhs: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = lhs;
    while let Some(start) = rest.find(is_ident_start) {
        let word = leading_ident(&rest[start..]).unwrap_or("");
        let after = &rest[start + word.len()..];
        rest = after;
        if matches!(word, "let" | "mut" | "ref" | "_") {
            continue;
        }
        if after.starts_with(['(', '[', '{']) || after.starts_with("::") {
            continue;
        }
        out.push(word.to_string());
    }
    out
}

/// `drop(<id>)` on this line.
fn drops(bal: &str, id: &str) -> bool {
    bal.match_indices("drop").any(|(at, _)| {
        bal[..at].chars().next_back().is_none_or(|c| !is_ident(c))
            && bal[at + 4..]
                .trim_start_matches([' ', '\t'])
                .strip_prefix('(')
                .and_then(|r| r.trim_start_matches([' ', '\t']).strip_prefix(id))
                .is_some_and(|r| r.trim_start_matches([' ', '\t']).starts_with(')'))
    })
}

/// `let [mut] <id>` followed by `:` or `=` — a shadowing re-bind.
fn shadows(bal: &str, id: &str) -> bool {
    bal.match_indices("let").any(|(at, _)| {
        if !bal[..at].chars().next_back().is_none_or(|c| !is_ident(c)) {
            return false;
        }
        let Some(r) = bal[at + 3..].strip_prefix([' ', '\t']) else {
            return false;
        };
        let mut r = r.trim_start_matches([' ', '\t']);
        if let Some(m) = r.strip_prefix("mut").filter(|m| m.starts_with([' ', '\t'])) {
            r = m.trim_start_matches([' ', '\t']);
        }
        r.strip_prefix(id)
            .is_some_and(|r| r.trim_start_matches([' ', '\t']).starts_with([':', '=']))
    })
}

#[cfg(test)]
#[path = "held_guard_tests.rs"]
mod tests;
