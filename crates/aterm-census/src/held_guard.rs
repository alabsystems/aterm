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
mod tests {
    use super::*;

    /// Shapes the detector must flag, each with at least one finding.
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
    let raw = aterm_lz4::decompress_size_prepended(t.cold_bytes()).unwrap();
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
        (
            "a control read's whole-frame hold is the same guard, chained",
            r##"
fn read_then_resize(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    crate::term_lock_whole(term).resize(rows, cols);
}
"##,
        ),
        (
            "a control read's whole-frame hold is the same guard, bound",
            r##"
fn read_then_resize(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let mut t = term_lock_whole(term);
    t.grid_mut().resize(rows, cols);
}
"##,
        ),
        (
            "a read's whole-frame hold that decides when to wait is the same guard, chained",
            r##"
fn read_then_resize(term: &Mutex<Terminal>, rows: usize, cols: usize, on: ReadOn) {
    crate::term_lock_whole_when(term, |_| on == ReadOn::Control).resize(rows, cols);
}
"##,
        ),
        (
            "a read's whole-frame hold that decides when to wait is the same guard, bound",
            r##"
fn read_then_resize(term: &Mutex<Terminal>, rows: usize, cols: usize, on: ReadOn) {
    let mut t = term_lock_whole_when(term, |t| on == ReadOn::Control && t.rows() > 0);
    t.grid_mut().resize(rows, cols);
}
"##,
        ),
        (
            "the host's hold of the terminal as it stands is the same guard",
            r##"
fn verb_rewrap(host: &impl SessionHost, sid: SessionId) {
    host.with_terminal_as_it_stands(sid, |t: &Terminal| {
        t.rewrap(0);
    });
}
"##,
        ),
    ];

    /// Documented-safe shapes that must draw no finding.
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

    /// The walk fails closed: a part of the scan set it cannot read refuses
    /// the scan, never a clean scan of what it could read, in a scope
    /// directory (an entry its listing cannot read) and in a split-out
    /// library (a source directory it may not list). Negative control: the
    /// same tree read whole is scanned.
    #[test]
    fn a_scan_set_it_cannot_read_whole_refuses_the_scan() {
        let files = [
            "crates/aterm-gui/src/main.rs",
            "crates/aterm-gui/src/pane/mod.rs",
            "crates/aterm-control/src/lib.rs",
            "crates/aterm-gui-term/src/lib.rs",
        ]
        .map(|rel| (rel.to_string(), String::new()));
        let scan = |fault: fn(&crate::tree::memory::Tree)| {
            let root =
                crate::scan_set::test_fixtures::synth_tree("aterm-census-held", "unread", &files);
            fault(&root);
            scan_tree(&root).map(|scan| scan.files)
        };
        assert_eq!(scan(|_| {}), Ok(4));
        assert!(scan(|root| root.unreadable("crates/aterm-gui/src/pane/mod.rs")).is_err());
        assert!(scan(|root| root.unlistable("crates/aterm-gui-term/src")).is_err());
    }
}
