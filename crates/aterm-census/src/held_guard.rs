// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OB-22 — NO UNBOUNDED WORK UNDER A HELD `term` GUARD, anywhere in the GUI's
//! shipped source: the per-SITE half of the L0 whole-Mac-freeze class, beside
//! OB-5's per-PATH half.
//!
//! OB-5 asks whether a main-thread ROOT reaches `.resize(` on a `term_lock`
//! guard. This asks, of every line of shipped `crates/aterm-gui/src` and
//! `crates/aterm-control/src`, whether blocklisted O(history) work —
//! `.resize(` `.reflow(` `.rewrap(` `.take_scrollback(` `.decompress(`
//! `.hydrate(` `.rebuild_index(`, or the `zstd` cold tier — runs while a guard
//! is held, whoever calls it. The safe offload (`resize_offloading_scrollback`
//! under the lock, `pending.reflow()` off it, `finish_resize_offload` under a
//! second brief lock) names no blocklisted method on a held guard, so it stays
//! green; the tokens are METHOD-CALL-anchored for exactly that reason.
//!
//! What counts as HELD, each shape pinned by a `must_fire` unit case below:
//!
//! * a line that acquires (`term_lock(`, `term_lock_ui(`, and aterm-control's
//!   `with_terminal(` / `with_terminal_mut(`, whose closure the host runs
//!   under the guard) holds it while the acquiring call's parens stay open —
//!   across a whole multi-line closure body;
//! * a `let` whose right-hand side IS the guard (only `&`/`mut`/`*`/`(` ahead of
//!   the acquire, nothing chained after its closing paren) binds every
//!   identifier its pattern binds (typed and destructuring binds included)
//!   until its block closes, it is `drop`ped, or it is shadowed;
//! * an explicit reborrow of a bound guard (`&mut *t`, `t.deref_mut()`) is the
//!   guard too; a plain accessor (`let alt = t.is_alternate_screen()`) reads a
//!   VALUE out and is not;
//! * the receiver may be paren-dereffed or reached through zero-arg accessor
//!   hops, and survives the line break rustfmt puts in a long chain.
//!
//! Out of reach for a line walk, and not faked: an acquire hidden in a macro,
//! an acquire whose own call spans lines, and a deferred `let t; t = …`.
//!
//! Code is read through a lexer that blanks comments and every string, raw
//! string and char literal in place, across lines, and THEN the shared
//! unshipped-item mask (`#[cfg(test)]` and its conjunctive kin) — so prose and
//! log text naming `.resize(` are never work, a `)` in a literal or trailing
//! comment cannot close a region early, and a `}` in a literal cannot end a
//! masked item early (the order is load-bearing: see [`blank_non_code`]).

use std::path::Path;

/// The source the GUI process runs under the per-session `term` mutex.
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

/// Scan [`HELD_GUARD_SCOPE`] under `root`. A scope directory that is missing is
/// an `Err`: a guard whose scan set shrank has proven nothing about what left.
pub(crate) fn scan_tree(root: &Path) -> Result<HeldGuardScan, String> {
    let mut files = Vec::new();
    for dir in HELD_GUARD_SCOPE {
        let abs = root.join(dir);
        if !abs.is_dir() {
            return Err(format!("the scan set lost `{dir}` (no such directory)"));
        }
        crate::collect_rs_files(&abs, &mut files)
            .map_err(|e| format!("cannot walk `{dir}`: {e}"))?;
    }
    files.retain(|p| !crate::is_test_file(p));
    files.sort();
    let mut scan = HeldGuardScan {
        files: files.len(),
        acquires: 0,
        findings: Vec::new(),
    };
    for file in &files {
        let text = std::fs::read_to_string(file)
            .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let rel = file
            .strip_prefix(root)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
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

/// The 1-based lines of one file's SHIPPED code that do blocklisted work under
/// a held guard, and how many lines acquire one.
pub(crate) fn scan_source(text: &str) -> (Vec<usize>, usize) {
    // Lex FIRST, then mask: see `blank_non_code` for why the order is load-bearing.
    let code = crate::mask_unshipped_items(&blank_non_code(text));
    let mut walk = Walk::default();
    let mut hits = Vec::new();
    let mut acquires = 0;
    for (i, line) in code.lines().enumerate() {
        // A line with no code carries no token; skipping it keeps a comment
        // inside a wrapped chain from ending the chain.
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

// ---------------------------------------------------------------------------
// The lexer: comments and literal contents blanked in place, BEFORE the mask
// ---------------------------------------------------------------------------

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The whole file with every comment and every string, raw string and char
/// literal (its `b`/`c`/`r` prefix and `#`s included) blanked to spaces IN
/// PLACE, tracked ACROSS lines: the line count and every column stay put, and
/// each line's trailing whitespace is dropped, so a gate with a trailing comment
/// still reads as the gate. A string that opens and closes on one line keeps
/// its two `"`, so `#[cfg(all(test, feature = "x"))]` stays a parseable gate; a
/// string that spans lines loses them, because a quote left alone on a line is
/// what the item mask's per-line literal masker would re-read as an OPENER
/// (blanking, say, the `;` that ends a `const X: &str = r#"…"#;` item).
///
/// This runs BEFORE the unshipped-item mask, never after it. The mask reads one
/// line at a time and ends an item at its first `<indent>}` line, so on raw
/// text a `}` inside a test module's raw string (`aterm-gui/src/app_native.rs`
/// has one) ended the module there and left the string's closer `"#;` visible,
/// and a lexer run over the masked text then read that `"` as an opener: every
/// later line of the file, shipped code included, became string contents.
/// Blanked first, a literal can hold no `}` to end an item and no `#[cfg(test)]`
/// to start one, and the mask can never strand half of it.
pub(crate) fn blank_non_code(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = chars.clone();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let (start, lit) = if is_ident_start(c) && (i == 0 || !is_ident(chars[i - 1])) {
            // An identifier is skipped whole, unless it prefixes a literal.
            let mut w = i;
            while w < chars.len() && is_ident(chars[w]) {
                w += 1;
            }
            let word: String = chars[i..w].iter().collect();
            let lit = match (word.as_str(), chars.get(w)) {
                ("r" | "br" | "cr", _) => raw_string(&chars, w),
                ("b" | "c", Some('"')) => cooked_string(&chars, w),
                ("b", Some('\'')) => char_literal(&chars, w),
                _ => None,
            };
            if lit.is_none() {
                i = w;
                continue;
            }
            (i, lit)
        } else {
            let lit = match (c, chars.get(i + 1)) {
                ('/', Some('/')) => Some(Literal {
                    end: chars[i..]
                        .iter()
                        .position(|&c| c == '\n')
                        .map_or(chars.len(), |p| i + p),
                    quotes: None,
                }),
                ('/', Some('*')) => Some(block_comment(&chars, i)),
                ('"', _) => cooked_string(&chars, i),
                ('\'', _) => char_literal(&chars, i),
                _ => None,
            };
            (i, lit)
        };
        let Some(lit) = lit else {
            i += 1;
            continue;
        };
        let end = lit.end.min(chars.len());
        for slot in &mut out[start..end] {
            if *slot != '\n' {
                *slot = ' ';
            }
        }
        if let Some((open, close)) = lit.quotes
            && !chars[start..end].contains(&'\n')
        {
            out[open] = '"';
            out[close] = '"';
        }
        i = end;
    }
    let blanked: String = out.into_iter().collect();
    let mut lexed = String::with_capacity(blanked.len());
    for line in blanked.lines() {
        lexed.push_str(line.trim_end());
        lexed.push('\n');
    }
    lexed
}

/// One comment or literal: where it ends (exclusive), and for a string, where
/// its opening and closing `"` sit.
struct Literal {
    end: usize,
    quotes: Option<(usize, usize)>,
}

/// A `"…"` string whose opening quote is at `q`, honouring `\` escapes.
fn cooked_string(chars: &[char], q: usize) -> Option<Literal> {
    let mut j = q + 1;
    while j < chars.len() {
        match chars[j] {
            '\\' => j += 2,
            '"' => {
                return Some(Literal {
                    end: j + 1,
                    quotes: Some((q, j)),
                });
            }
            _ => j += 1,
        }
    }
    Some(Literal {
        end: chars.len(),
        quotes: None,
    })
}

/// A raw string whose `#`s (if any) start at `at`, right after its `r` prefix;
/// `None` when no `"` follows them (`r#ident` is a raw identifier).
fn raw_string(chars: &[char], at: usize) -> Option<Literal> {
    let hashes = chars[at..].iter().take_while(|&&c| c == '#').count();
    let q = at + hashes;
    if chars.get(q) != Some(&'"') {
        return None;
    }
    let close = (q + 1..chars.len())
        .find(|&j| chars[j] == '"' && (1..=hashes).all(|k| chars.get(j + k) == Some(&'#')));
    Some(match close {
        Some(close) => Literal {
            end: close + 1 + hashes,
            quotes: Some((q, close)),
        },
        None => Literal {
            end: chars.len(),
            quotes: None,
        },
    })
}

/// A char literal at `i` (`'x'`, `'\n'`, `'\u{1F600}'`); `None` for anything
/// else a `'` starts (a lifetime, a label).
fn char_literal(chars: &[char], i: usize) -> Option<Literal> {
    let next = chars.get(i + 1).copied();
    if next == Some('\\') {
        let close = (i + 3..chars.len().min(i + 12))
            .find(|&j| chars[j] == '\'' || chars[j] == '\n')
            .filter(|&j| chars[j] == '\'')?;
        return Some(Literal {
            end: close + 1,
            quotes: None,
        });
    }
    (next.is_some_and(|c| c != '\n') && chars.get(i + 2) == Some(&'\'')).then_some(Literal {
        end: i + 3,
        quotes: None,
    })
}

/// A block comment opening at `i`, nested ones included.
fn block_comment(chars: &[char], i: usize) -> Literal {
    let mut depth = 0usize;
    let mut j = i;
    while j < chars.len() {
        match (chars[j], chars.get(j + 1)) {
            ('/', Some('*')) => {
                depth += 1;
                j += 2;
            }
            ('*', Some('/')) => {
                depth -= 1;
                j += 2;
                if depth == 0 {
                    break;
                }
            }
            _ => j += 1,
        }
    }
    Literal {
        end: j,
        quotes: None,
    }
}

// ---------------------------------------------------------------------------
// The walk: which lines do blocklisted work while a guard is held
// ---------------------------------------------------------------------------

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
        // Bind BEFORE the hit test so a same-line alias-then-call still counts.
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
        if live && bal.contains("zstd") {
            hit = true;
        }
        // A chain rustfmt broke across lines puts the receiver and the work on
        // separate lines — the one bypass the formatter introduces on its own.
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

    /// The identifiers a line binds to a REBORROW of a tracked guard: `&`/`*` in
    /// front with at most zero-arg hops after, or a spelled-out
    /// `.deref()`/`.deref_mut()`. An accessor alone reads a VALUE out, and
    /// treating those as guards once cascaded onto 100+ plain locals.
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
        "with_terminal(",
        "with_terminal_mut(",
    ]
    .iter()
    .filter_map(|needle| bal.find(needle))
    .min()
}

/// A blocklisted method call anywhere on the line, or the `zstd` cold tier.
fn has_blocked_work(bal: &str) -> bool {
    bal.contains("zstd")
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
    // `(<anything>*<ws>id)` — a paren-deref.
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

/// A line that is ONLY a receiver and zero-arg hops (`t.grid_mut()`): its first
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

/// The identifiers a line binds to the GUARD ITSELF: a right-hand side whose top
/// is the acquire (a path to it allowed) and whose acquire call closes on this
/// line with nothing chained after it — a chain reads a VALUE out, which is
/// what lets the detached `let pending = term_lock(t).resize_offloading_…` go.
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
    // A call spanning lines is the region walk's.
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

/// The identifiers a pattern BINDS: not `let`/`mut`/`ref`/`_`, and not a path
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
mod tests {
    use super::*;

    /// Every shape that has walked through this class, plus the ones closed
    /// since: each must draw AT LEAST ONE finding. A shape that stops firing is
    /// a whole-Mac freeze shipped green.
    const MUST_FIRE: &[(&str, &str)] = &[
        (
            "the original un-fixed cross_resize: the whole reflow on the held guard",
            r##"
fn cross_resize(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    term_lock(term).resize(rows, cols);
}
"##,
        ),
        (
            "one zero-arg accessor between the guard and the work",
            r##"
fn cross_resize_hop(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let mut t = term_lock(term);
    t.grid_mut().resize(rows, cols);
}
"##,
        ),
        (
            "alias and work on ONE line: the binding registers before the hit test",
            r##"
fn hydrate_same_line(term: &Mutex<Terminal>, rows: usize) {
    let mut t = term_lock(term);
    let u = &mut *t; u.hydrate(rows);
}
"##,
        ),
        (
            "a `{` inside a test-module string must not hold the mask open past it",
            r##"
#[cfg(test)]
mod tests {
    #[test]
    fn reports_progress() {
        emit("rewrap {of the scrollback");
    }
}

fn cross_resize(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    term_lock(term).resize(rows, cols);
}
"##,
        ),
        (
            "an unbalanced paren inside a char literal must not close the region",
            r##"
fn verb_hydrate(host: &mut dyn SessionHost, sid: SessionId, rows: usize) {
    host.with_terminal_mut(sid, |t| {
        if t.last_byte() == b')' {
            t.hydrate(rows);
        }
    });
}
"##,
        ),
        (
            "aterm-control borrows through the host closure: no `term_lock` to anchor on",
            r##"
fn verb_resize(host: &mut dyn SessionHost, sid: SessionId, rows: usize, cols: usize) {
    host.with_terminal_mut(sid, |t| {
        t.resize(rows, cols);
    });
}
"##,
        ),
        (
            "an unbalanced paren in a trailing comment must not close the region",
            r##"
fn verb_decompress(host: &mut dyn SessionHost, sid: SessionId, rows: usize) {
    host.with_terminal_mut(sid, |t| {
        let n = t.rows(); // clamp) to the visible window
        t.decompress(n + rows);
    });
}
"##,
        ),
        (
            "DerefMut is a spelling of the reborrow",
            r##"
fn take_via_deref(term: &Mutex<Terminal>) {
    let mut t = term_lock(term);
    let inner = t.deref_mut();
    inner.take_scrollback();
}
"##,
        ),
        (
            "a pattern binds the guard as well as a bare identifier",
            r##"
fn cross_resize_destructured(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let (mut t, limit) = (term_lock(term), cols);
    t.resize(rows, limit);
}
"##,
        ),
        (
            "the guard and the work in an inner block",
            r##"
fn resize_in_branch(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    if rows > 0 {
        let mut t = term_lock(term);
        t.take_scrollback();
    }
}
"##,
        ),
        (
            "an explicit deref through parens",
            r##"
fn cross_resize_deref(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let mut t = term_lock(term);
    (*t).resize(rows, cols);
}
"##,
        ),
        (
            "`crate::term_lock(..)` is the same acquire",
            r##"
fn rebuild_path_qualified(term: &Mutex<Terminal>) {
    let mut t = crate::term_lock(term);
    t.rebuild_index();
}
"##,
        ),
        (
            "a reborrow keeps the lock held; the alias inherits the guard's scope",
            r##"
fn cross_resize_reborrow(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let mut t = term_lock(term);
    let u = &mut *t;
    u.resize(rows, cols);
}
"##,
        ),
        (
            "`&mut` in front of the acquire still binds the guard",
            r##"
fn cross_resize_refmut(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let t = &mut term_lock(term);
    t.resize(rows, cols);
}
"##,
        ),
        (
            "rustfmt breaks a long chain: receiver and work on different lines",
            r##"
fn cross_resize_wrapped(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let mut t = term_lock(term);
    t.grid_mut()
        .resize(rows, cols);
}
"##,
        ),
        (
            "the UI thread's registering acquire hands back the same guard",
            r##"
fn redraw_reflow(term: &Mutex<Terminal>, waiting: &AtomicU32, rows: usize, cols: usize) {
    let site = TermWaitSite::Redraw;
    let mut t = crate::term_lock_ui(term, waiting, site);
    t.resize(rows, cols);
}
"##,
        ),
        (
            "a type annotation used to hide the binding",
            r##"
fn cross_resize_typed(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let mut t: TermGuard<'_> = term_lock(term);
    t.resize(rows, cols);
}
"##,
        ),
        (
            "touching the compressed cold tier while the guard lives",
            r##"
fn decode_cold(term: &Mutex<Terminal>) {
    let mut t = term_lock(term);
    let raw = zstd::decode_all(t.cold_bytes()).unwrap();
    consume(raw);
}
"##,
        ),
        (
            "a `}` inside a test module's raw string must not end the module and strand \
             the string's closer",
            r##"
fn production(term: &Mutex<Terminal>) {
    record(term_lock(term).cols());
}

#[cfg(test)]
mod tests {
    const PLANT: &str = r#"
fn f() {
}
"#;
}

fn cross_resize(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    term_lock(term).resize(rows, cols);
}
"##,
        ),
        (
            "a shipped raw string quoting `#[cfg(test)]` gates nothing after it",
            r##"
const TEMPLATE: &str = r#"
#[cfg(test)]
fn helper() {
"#;

fn cross_resize(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    term_lock(term).resize(rows, cols);
}
"##,
        ),
    ];

    /// The documented-safe shapes a careless widening eats first: each must
    /// draw NO finding. A detector that cries wolf gets ignored until it
    /// protects nothing.
    const MUST_SILENT: &[(&str, &str)] = &[
        (
            "balanced single-line reads open no region",
            r##"
fn dims(term: &Mutex<Terminal>) -> (usize, usize) {
    (term_lock(term).rows(), term_lock(term).cols())
}
"##,
        ),
        (
            "a test resizes a terminal under a lock it owns",
            r##"
fn production(term: &Mutex<Terminal>) {
    record(term_lock(term).cols());
}

#[cfg(test)]
mod tests {
    #[test]
    fn resize_reflows() {
        let term = Mutex::new(Terminal::new(24, 80));
        let mut t = term_lock(&term);
        t.resize(48, 200);
    }
}
"##,
        ),
        (
            "a multi-line raw string in a test module ends no block early",
            r##"
#[cfg(test)]
mod tests {
    #[test]
    fn replays_a_recorded_index() {
        write(
            path,
            br#"{"frames":[
                {"n":1,"file":"frame_0001.png"}
            ]}"#,
        );
    }

    #[test]
    fn reflows_after_replay() {
        let term = Mutex::new(Terminal::new(24, 80));
        let mut t = term_lock(&term);
        t.reflow(200);
    }
}
"##,
        ),
        (
            "a format string's braces are not a test-only statement's braces",
            r##"
fn truncate_front_lines(term: &Mutex<Terminal>, n: usize) {
    #[cfg(test)]
    debug_assert!(
        n <= line_count(),
        "truncate_front_lines({n}) exceeds line_count({})",
        term_lock(term).hydrate(n)
    );
    record(n);
}
"##,
        ),
        (
            "prose naming the hazard, on its own line and trailing",
            r##"
fn documented(term: &Mutex<Terminal>) {
    let mut t = term_lock(term);
    // t.resize(rows, cols) here would freeze the whole machine.
    record(t.rows()); // ...and so would t.reflow(), which is why it is offloaded
}
"##,
        ),
        (
            "`drop` releases the lock before the expensive part",
            r##"
fn cross_resize_dropped(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let mut t = term_lock(term);
    let pending = t.resize_offloading_scrollback(rows, cols);
    drop(t);
    let reflowed = pending.reflow();
    term_lock(term).finish_resize_offload(reflowed);
}
"##,
        ),
        (
            "the guard's block closed before the work",
            r##"
fn resize_after_scope(term: &Mutex<Terminal>, pending: Pending) {
    {
        let t = term_lock(term);
        record(t.cols());
    }
    pending.reflow();
}
"##,
        ),
        (
            "unbounded work with no terminal lock anywhere",
            r##"
fn reflow_detached(job: &mut Pending, grid: &mut Grid, rows: usize, cols: usize) {
    grid.resize(rows, cols);
    job.reflow();
    job.rebuild_index();
}
"##,
        ),
        (
            "THE FIX ITSELF: the rewrap off the lock, between two brief holds",
            r##"
fn cross_resize(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let pending = term_lock(term).resize_offloading_scrollback(rows, cols);
    let reflowed = pending.reflow();
    term_lock(term).finish_resize_offload(reflowed);
}
"##,
        ),
        (
            "the same offload through a bound guard whose scope closes first",
            r##"
fn cross_resize_bound(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let pending = {
        let mut t = term_lock(term);
        t.resize_offloading_scrollback(rows, cols)
    };
    let reflowed = pending.reflow();
    term_lock(term).finish_resize_offload(reflowed);
}
"##,
        ),
        (
            "the name rebound to a detached job",
            r##"
fn reflow_shadowed(term: &Mutex<Terminal>) {
    let mut t = term_lock(term);
    record(t.cols());
    let mut t = detached_job();
    t.reflow();
}
"##,
        ),
        (
            "a TYPED re-let shadows the guard name too",
            r##"
fn reflow_shadowed_typed(term: &Mutex<Terminal>) {
    let mut t = term_lock(term);
    record(t.cols());
    let mut t: Pending = detached_job();
    t.reflow();
}
"##,
        ),
        (
            "`.resize(` inside a string is prose",
            r##"
fn log_resize(term: &Mutex<Terminal>) {
    let mut t = term_lock(term);
    log(t.rows(), "resize( is named here but never called");
}
"##,
        ),
        (
            "the registering acquire's chain reads a VALUE out",
            r##"
fn resize_from_ui_read(term: &Mutex<Terminal>, waiting: &AtomicU32, grid: &mut Grid, rows: usize) {
    let cols = crate::term_lock_ui(term, waiting, TermWaitSite::Press).cols();
    grid.resize(rows, cols);
}
"##,
        ),
        (
            "a VALUE read out of the guard is not the guard",
            r##"
fn rebuild_detached(term: &Mutex<Terminal>) {
    let t = term_lock(term);
    let snapshot = t.cold_snapshot();
    drop(t);
    snapshot.rebuild_index();
}
"##,
        ),
        (
            "a chain reads a VALUE out; the guard is gone by the semicolon",
            r##"
fn resize_from_read(term: &Mutex<Terminal>, grid: &mut Grid, rows: usize) {
    let cols = term_lock(&term).cols();
    grid.resize(rows, cols);
}
"##,
        ),
        (
            "a wrapped chain ending in the SAFE offload, and a bare tail expression",
            r##"
fn cross_resize_wrapped_safe(term: &Mutex<Terminal>, rows: usize, cols: usize) -> Pending {
    let mut t = term_lock(term);
    let cols_now = t
        .cols();
    t.grid_mut()
        .resize_offloading_scrollback(rows, cols_now.min(cols))
}
"##,
        ),
        (
            "a test-module raw string holding a column-0 `}`, then more test code \
             (app_native.rs's unix-only-scan PLANT)",
            r##"
fn production(term: &Mutex<Terminal>) {
    record(term_lock(term).cols());
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_scan_goes_red_on_a_plant() {
        const PLANT: &str = r#"
#[cfg(unix)]
enum Overflow {
    Fail,
}
"#;
        assert_eq!(scan(PLANT), 1);
    }

    #[test]
    fn names_the_hazard() {
        let mut t = term_lock(&term);
        t.resize(48, 200);
        log("never call term_lock(t).resize(1, 2) on the main thread");
    }
}
"##,
        ),
        (
            "a block comment and a doc comment naming the work",
            r##"
fn cold_len(term: &Mutex<Terminal>) -> usize {
    let mut t = term_lock(term);
    /* t.resize(rows, cols) would
       t.reflow() here */
    /// t.hydrate(rows)
    t.cold_len()
}
"##,
        ),
    ];

    #[test]
    fn every_must_fire_shape_draws_a_finding() {
        for (why, src) in MUST_FIRE {
            assert!(
                !scan_source(src).0.is_empty(),
                "MISS — {why}: expected at least one finding in\n{src}"
            );
        }
    }

    #[test]
    fn every_must_silent_shape_draws_none() {
        for (why, src) in MUST_SILENT {
            let hits = scan_source(src).0;
            assert!(
                hits.is_empty(),
                "CRIES WOLF — {why}: expected no finding, got lines {hits:?} in\n{src}"
            );
        }
    }

    /// The finding names the line the work is on, so the diagnostic points at
    /// the site to repair, and the acquire count sees every acquiring line.
    #[test]
    fn a_finding_is_the_line_of_the_work() {
        let (hits, acquires) = scan_source(MUST_FIRE[14].1);
        assert_eq!(hits, vec![5], "the wrapped chain's `.resize(` line");
        assert_eq!(acquires, 1);
    }

    /// Every literal and comment is blanked where it stands, columns and line
    /// count kept; a one-line string keeps its quotes, a multi-line one does not.
    #[test]
    fn the_lexer_blanks_every_literal_and_comment_in_place() {
        let src = "let a = \"x // y\"; // z\nlet b = r#\"multi\nline\"#; /* c\nd */ e\n\
                   'x' '\\'' 'a b\n#[cfg(all(test, feature = \"x\"))] // why\nlet c = br\"}\";";
        let lexed = blank_non_code(src);
        assert_eq!(
            lexed.lines().collect::<Vec<_>>(),
            [
                "let a = \"      \";",
                "let b =",
                "      ;",
                "     e",
                "         'a b",
                "#[cfg(all(test, feature = \" \"))]",
                "let c =   \" \";",
            ]
        );
        assert_eq!(lexed.lines().count(), src.lines().count());
        assert_eq!(blank_non_code("no newline").lines().count(), 1);
        assert_eq!(
            blank_non_code("a\n\n").lines().count(),
            "a\n\n".lines().count()
        );
    }

    /// The real tree: the scope exists, the walk sees acquires (a blind walk is
    /// not a clean one), and nothing is held across unbounded work.
    #[test]
    fn this_tree_holds_no_guard_across_unbounded_work() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-census lives two levels under the repo root");
        let scan = scan_tree(root).expect("the OB-22 scope exists");
        assert!(
            scan.acquires >= 100,
            "a blind walk: {} acquires",
            scan.acquires
        );
        let found: Vec<String> = scan
            .findings
            .iter()
            .map(|f| format!("{}: {}", f.span, f.line))
            .collect();
        assert!(found.is_empty(), "{found:#?}");
    }
}
