// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tripwire: std's clock must never reach a wasm build.
//!
//! On `wasm32-unknown-unknown` BOTH `std::time::Instant::now()` and
//! `std::time::SystemTime::now()` PANIC — "time not implemented on this
//! platform". `aterm_time::Instant` / `aterm_time::SystemTime` shim to
//! `performance.now()` / `Date.now()` there and re-export std's types natively,
//! so swapping is byte-identical for native builds and correct for the web ones.
//!
//! Native suites cannot see the failure: the panic unwinds out of a `&mut self`
//! wasm-bindgen method, the object's `RefCell` is never released, and every later
//! access throws "recursive use of an object detected", far from the clock.
//! The embedding app's wasm source patch keys on variable names, so a newly
//! named clock slips past it: that patch is a backstop, this is the guard.
//!
//! The closure is walked from the wasm entry points' manifests on every run, not
//! hand-listed: a hand-list of a transitive closure goes stale with the first new
//! dependency.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// The wasm entry points. Everything reachable from these through normal
/// (non-dev, non-build) dependency edges is code that can be compiled into a
/// browser bundle.
const WASM_ROOTS: &[&str] = &["aterm-wasm", "aterm-gpu-web"];

/// The std-clock constructors that panic on `wasm32-unknown-unknown`.
///
/// BOTH spellings, deliberately. `SystemTime::now()` panics exactly as
/// `Instant::now()` does, and it was the `SystemTime` half that this campaign
/// had to migrate by hand (`aterm-types::shell_types::current_time_ms`) because
/// nothing would have flagged it.
const FORBIDDEN: &[&str] = &["std::time::Instant::now()", "std::time::SystemTime::now()"];

/// The in-source opt-out marker.
///
/// A site that genuinely cannot reach wasm — the clearest case being one inside
/// a `#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]` arm whose
/// wasm twin sits right beside it — carries this marker plus a reason on one of
/// the lines directly above it. Deliberately a VISIBLE annotation at the call
/// site rather than a crate name on a list at the top of this file: an exemption
/// you have to open another file to discover is how the original bug survived
/// review, and this way the reason is read by whoever next edits the line.
const ALLOW_MARKER: &str = "wasm-clock-guard: allow";

/// How many lines above a site the marker may sit — enough for a marker with its
/// reason on the following line, not enough to reach an unrelated function.
const ALLOW_LOOKBACK: usize = 4;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

/// The workspace path dependencies one manifest declares through NORMAL edges.
///
/// `[dependencies]` and every `[target.'cfg(…)'.dependencies]` section count;
/// `dev-` and `build-` sections do not, because neither reaches a bundle. cfg
/// predicates are NOT evaluated — a cfg-gated edge counts on every platform, the
/// same fail-closed rule `aterm-census`'s scan-set derivation uses: this guard
/// would rather walk a file the browser never links than miss one it does.
fn normal_workspace_deps(root: &Path, crate_name: &str) -> Vec<String> {
    let manifest = root.join("crates").join(crate_name).join("Cargo.toml");
    let Ok(text) = fs::read_to_string(&manifest) else {
        return vec![];
    };
    let mut out = vec![];
    let mut in_deps = false;
    for line in text.lines() {
        let t = line.trim();
        if let Some(header) = t.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            let h = header.trim();
            in_deps =
                h == "dependencies" || (h.starts_with("target.") && h.ends_with(".dependencies"));
            continue;
        }
        if !in_deps || t.is_empty() || t.starts_with('#') {
            continue;
        }
        // `name = { … }`, `name = "…"` or `name.workspace = true`.
        let key_end = t
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(t.len());
        let name = &t[..key_end];
        if !name.is_empty() && root.join("crates").join(name).join("Cargo.toml").is_file() {
            out.push(name.to_string());
        }
    }
    out
}

/// Every workspace crate reachable from the wasm entry points.
fn wasm_closure(root: &Path) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<String> = WASM_ROOTS.iter().map(|s| (*s).to_string()).collect();
    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        stack.extend(normal_workspace_deps(root, &name));
    }
    seen
}

fn rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(rs_files(&p));
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Test scope: which lines, and which whole files, are test-only code.
//
// Test modules and `#[test]` functions never reach wasm, so their clocks are
// legitimate. Scope is read off the item the attribute is on (a production
// clock can sit BELOW a test module): from the attribute to the end of that
// item (its matching `}` or its `;`), with comments and literals blanked
// first so a `'}'` or a `"{"` cannot move a boundary. A file is test code as
// a whole when it says `#![cfg(test)]`, or when every `mod`/`include!` that
// pulls it in sits inside test scope in its parent.
// ---------------------------------------------------------------------------

/// What one source file says about its own test scope.
struct Scan {
    /// Per 0-based line: inside a test-only item.
    test_line: Vec<bool>,
    /// The file opens with a test-implying inner `#![cfg(…)]`.
    whole_file_test: bool,
    /// Files this one pulls in by `mod name;` or `include!("…")`, resolved,
    /// each with the 0-based line of the declaration.
    children: Vec<(PathBuf, usize)>,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `text` with every comment and every string/char literal's CONTENTS blanked
/// to spaces (newlines kept, byte length unchanged), so the braces, brackets
/// and `;` left are Rust's own. String contents come back beside it, keyed by
/// the byte offset of the opening quote, for `#[path = "…"]` and `include!`.
fn strip_source(text: &str) -> (Vec<u8>, BTreeMap<usize, String>) {
    fn blank(out: &mut [u8], from: usize, to: usize) {
        for b in &mut out[from..to] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }
    let src = text.as_bytes();
    let n = src.len();
    let mut out = src.to_vec();
    let mut strings = BTreeMap::new();
    let mut i = 0;
    while i < n {
        let b = src[i];
        let next = src.get(i + 1).copied();
        if b == b'/' && next == Some(b'/') {
            let end = src[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(n, |p| i + p);
            blank(&mut out, i, end);
            i = end;
        } else if b == b'/' && next == Some(b'*') {
            // Block comments nest in Rust.
            let (mut depth, mut j) = (0usize, i);
            while j < n {
                if src[j] == b'/' && src.get(j + 1) == Some(&b'*') {
                    depth += 1;
                    j += 2;
                } else if src[j] == b'*' && src.get(j + 1) == Some(&b'/') {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            let j = j.min(n);
            blank(&mut out, i, j);
            i = j;
        } else if b == b'r'
            && (i == 0
                || !is_ident_byte(src[i - 1])
                || (matches!(src[i - 1], b'b' | b'c') && (i < 2 || !is_ident_byte(src[i - 2]))))
            && matches!(next, Some(b'"' | b'#'))
        {
            // Raw string `r"…"` / `r#"…"#` (also `br`, `cr`); `r#ident` is not one.
            let mut j = i + 1;
            while j < n && src[j] == b'#' {
                j += 1;
            }
            let hashes = j - (i + 1);
            if src.get(j) != Some(&b'"') {
                i += 1;
                continue;
            }
            let start = j + 1;
            let mut k = start;
            while k < n
                && !(src[k] == b'"'
                    && src
                        .get(k + 1..k + 1 + hashes)
                        .is_some_and(|h| h.iter().all(|&c| c == b'#')))
            {
                k += 1;
            }
            let k = k.min(n);
            strings.insert(j, text[start..k].to_string());
            blank(&mut out, start, k);
            i = k + 1 + hashes;
        } else if b == b'"' {
            let mut j = i + 1;
            while j < n && src[j] != b'"' {
                j += if src[j] == b'\\' { 2 } else { 1 };
            }
            let j = j.min(n);
            strings.insert(i, text[i + 1..j].to_string());
            blank(&mut out, i + 1, j);
            i = j + 1;
        } else if b == b'\'' {
            if next == Some(b'\\') {
                // Escaped char literal: '\n', '\'', '\u{7B}'.
                let mut j = i + 3;
                while j < n && src[j] != b'\'' {
                    j += 1;
                }
                let j = j.min(n);
                blank(&mut out, i + 1, j);
                i = j + 1;
            } else {
                // 'x' is a char literal; 'a without a closing quote is a lifetime.
                let len = match next {
                    Some(c) if c >= 0xF0 => 4,
                    Some(c) if c >= 0xE0 => 3,
                    Some(c) if c >= 0xC0 => 2,
                    _ => 1,
                };
                if src.get(i + 1 + len) == Some(&b'\'') {
                    blank(&mut out, i + 1, i + 1 + len);
                    i += len + 2;
                } else {
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }
    (out, strings)
}

fn skip_ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Whether the keyword `kw` starts at `i` as a whole word.
fn keyword_at(s: &[u8], i: usize, kw: &[u8]) -> bool {
    s[i..].starts_with(kw)
        && (i == 0 || !is_ident_byte(s[i - 1]))
        && s.get(i + kw.len()).is_none_or(|&b| !is_ident_byte(b))
}

/// The attribute starting at `i` (`#[…]` or `#![…]`), as (is inner, content
/// with all whitespace removed, offset just past the closing `]`).
fn attribute_at(s: &[u8], i: usize) -> Option<(bool, String, usize)> {
    if s.get(i) != Some(&b'#') {
        return None;
    }
    let mut j = i + 1;
    let inner = s.get(j) == Some(&b'!');
    if inner {
        j += 1;
    }
    let j = skip_ws(s, j);
    if s.get(j) != Some(&b'[') {
        return None;
    }
    let mut depth = 0usize;
    for (k, &c) in s.iter().enumerate().skip(j) {
        if c == b'[' {
            depth += 1;
        } else if c == b']' {
            depth -= 1;
            if depth == 0 {
                let content = s[j + 1..k]
                    .iter()
                    .filter(|c| !c.is_ascii_whitespace())
                    .map(|&c| c as char)
                    .collect();
                return Some((inner, content, k + 1));
            }
        }
    }
    None
}

/// Split a cfg argument list at its top-level commas.
fn split_top(args: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (vec![], 0i32, 0);
    for (k, c) in args.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&args[start..k]);
                start = k + 1;
            }
            _ => {}
        }
    }
    out.push(&args[start..]);
    out.retain(|a| !a.is_empty());
    out
}

/// Whether a cfg predicate can only hold in a test build: `test` itself,
/// `all(…)` with such an argument, or `any(…)` whose arguments all are.
/// `not(…)` and everything else is production.
fn predicate_implies_test(p: &str) -> bool {
    if p == "test" {
        return true;
    }
    if let Some(args) = p.strip_prefix("all(").and_then(|a| a.strip_suffix(')')) {
        return split_top(args).into_iter().any(predicate_implies_test);
    }
    if let Some(args) = p.strip_prefix("any(").and_then(|a| a.strip_suffix(')')) {
        let args = split_top(args);
        return !args.is_empty() && args.into_iter().all(predicate_implies_test);
    }
    false
}

/// Whether an attribute's content marks its item test-only: `#[test]`, or a
/// `cfg` whose predicate implies `test`.
fn attribute_is_test(content: &str) -> bool {
    content == "test"
        || content
            .strip_prefix("cfg(")
            .and_then(|c| c.strip_suffix(')'))
            .is_some_and(predicate_implies_test)
}

/// Keywords that open an item or statement whose own syntax may carry
/// top-level commas (generics, `use` lists), so a `,` does not end it.
const ITEM_KEYWORDS: &[&str] = &[
    "fn",
    "impl",
    "struct",
    "enum",
    "trait",
    "type",
    "mod",
    "use",
    "const",
    "static",
    "unsafe",
    "extern",
    "async",
    "macro_rules",
    "union",
    "let",
];

fn word_at(s: &[u8], i: usize) -> &str {
    let end = s[i..]
        .iter()
        .position(|&b| !is_ident_byte(b))
        .map_or(s.len(), |p| i + p);
    std::str::from_utf8(&s[i..end]).unwrap_or("")
}

/// The offset of the last byte of the item that starts at or after `from`
/// (just past an attribute): its matching `}`, its `;`, or — for a field,
/// variant, argument or match arm — the `,` that ends it. A `}`, `)` or `]`
/// that closes the ENCLOSING block or list ends it too (the attributed element
/// was the last one in it, with no trailing comma).
fn item_end(s: &[u8], from: usize) -> usize {
    let n = s.len();
    let mut j = skip_ws(s, from);
    while let Some((_, _, end)) = attribute_at(s, j) {
        j = skip_ws(s, end);
    }
    let mut w = j;
    if word_at(s, w) == "pub" {
        w = skip_ws(s, w + 3);
        if s.get(w) == Some(&b'(') {
            w = s[w..]
                .iter()
                .position(|&b| b == b')')
                .map_or(n, |p| w + p + 1);
            w = skip_ws(s, w);
        }
    }
    let comma_ends = w < n && !ITEM_KEYWORDS.contains(&word_at(s, w));
    let mut paren = 0i32;
    let mut k = j;
    while k < n {
        match s[k] {
            b'(' | b'[' => paren += 1,
            b')' | b']' => {
                paren -= 1;
                if paren < 0 {
                    return k.saturating_sub(1);
                }
            }
            b';' if paren <= 0 => return k,
            b',' if paren <= 0 && comma_ends => return k,
            b'}' if paren <= 0 => return k.saturating_sub(1),
            b'{' if paren <= 0 => {
                let mut depth = 0usize;
                for (m, &c) in s.iter().enumerate().skip(k) {
                    if c == b'{' {
                        depth += 1;
                    } else if c == b'}' {
                        depth -= 1;
                        if depth == 0 {
                            return m;
                        }
                    }
                }
                return n.saturating_sub(1);
            }
            _ => {}
        }
        k += 1;
    }
    n.saturating_sub(1)
}

/// The `#[path = "…"]` among the attributes directly in front of the `mod`
/// keyword at `pos` (visibility allowed between them).
fn path_attr_before(s: &[u8], strings: &BTreeMap<usize, String>, pos: usize) -> Option<String> {
    let mut k = pos;
    loop {
        while k > 0 && s[k - 1].is_ascii_whitespace() {
            k -= 1;
        }
        if k >= 3 && &s[k - 3..k] == b"pub" && (k == 3 || !is_ident_byte(s[k - 4])) {
            k -= 3;
            continue;
        }
        if k == 0 || !matches!(s[k - 1], b']' | b')') {
            return None;
        }
        // Walk back to the matching opener.
        let close = s[k - 1];
        let open = if close == b']' { b'[' } else { b'(' };
        let mut depth = 0usize;
        let mut m = k - 1;
        loop {
            if s[m] == close {
                depth += 1;
            } else if s[m] == open {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            if m == 0 {
                return None;
            }
            m -= 1;
        }
        if close == b')' {
            // `pub(crate)` / `pub(in …)`: step over it, the `pub` is next.
            k = m;
            continue;
        }
        let content: String = s[m + 1..k - 1]
            .iter()
            .filter(|c| !c.is_ascii_whitespace())
            .map(|&c| c as char)
            .collect();
        if content.starts_with("path=") {
            let quote = s[m..k].iter().position(|&b| b == b'"')? + m;
            return strings.get(&quote).cloned();
        }
        // Another attribute: step over it and its `#` (or `#!`).
        k = m;
        while k > 0 && (s[k - 1] == b'#' || s[k - 1] == b'!' || s[k - 1].is_ascii_whitespace()) {
            k -= 1;
            if s[k] == b'#' {
                break;
            }
        }
    }
}

/// Where `mod name;` in `file` loads from: `#[path]` is relative to the
/// declaring file's directory; otherwise `name.rs` / `name/mod.rs` beside a
/// `lib.rs`/`main.rs`/`mod.rs`/`src/bin/*.rs` root, or under `stem/` for any
/// other file.
fn resolve_mod(file: &Path, name: &str, path_attr: Option<String>) -> Option<PathBuf> {
    let dir = file.parent()?;
    if let Some(p) = path_attr {
        return Some(dir.join(p));
    }
    let fname = file.file_name()?.to_str()?;
    let mod_rs = matches!(fname, "lib.rs" | "main.rs" | "mod.rs")
        || dir.file_name().is_some_and(|d| d == "bin");
    let base = if mod_rs {
        dir.to_path_buf()
    } else {
        dir.join(file.file_stem()?)
    };
    [
        base.join(format!("{name}.rs")),
        base.join(name).join("mod.rs"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// Read `text`'s test scope. `file` (its path) is needed only to resolve the
/// files it pulls in.
fn scan_source(text: &str, file: Option<&Path>) -> Scan {
    let (s, strings) = strip_source(text);
    let n = s.len();
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(
            s.iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(k, _)| k + 1),
        )
        .collect();
    let line_of = |off: usize| line_starts.partition_point(|&st| st <= off) - 1;
    let mut test_line = vec![false; line_starts.len()];
    let mut whole_file_test = false;
    let mut children = vec![];
    let mut depth = 0usize;
    let mut i = 0;
    while i < n {
        match s[i] {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b'#' => {
                if let Some((inner, content, end)) = attribute_at(&s, i) {
                    if attribute_is_test(&content) {
                        if inner {
                            // `#![cfg(test)]` at file level; inside an inline
                            // block it is left alone (fail closed).
                            whole_file_test |= depth == 0;
                        } else {
                            for l in &mut test_line[line_of(i)..=line_of(item_end(&s, end))] {
                                *l = true;
                            }
                        }
                    }
                    i = end;
                    continue;
                }
            }
            b'm' if depth == 0 && keyword_at(&s, i, b"mod") => {
                let name_start = skip_ws(&s, i + 3);
                let name = word_at(&s, name_start);
                let after = skip_ws(&s, name_start + name.len());
                if let Some(f) = file
                    && !name.is_empty()
                    && s.get(after) == Some(&b';')
                    && let Some(p) = resolve_mod(f, name, path_attr_before(&s, &strings, i))
                {
                    children.push((p, line_of(i)));
                }
            }
            b'i' if keyword_at(&s, i, b"include") && s.get(i + 7) == Some(&b'!') => {
                let open = skip_ws(&s, i + 8);
                if s.get(open) == Some(&b'(')
                    && let Some(lit) = strings.get(&skip_ws(&s, open + 1))
                    && let Some(dir) = file.and_then(Path::parent)
                {
                    children.push((dir.join(lit), line_of(i)));
                }
            }
            _ => {}
        }
        i += 1;
    }
    Scan {
        test_line,
        whole_file_test,
        children,
    }
}

fn canonical(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Every `mod`/`include!` under `src_root` that pulls in `target`, as (the
/// declaring file, whether the declaration sits in test scope there). Only
/// files that mention the target's name are parsed; `#[path]` can point
/// anywhere (`../x.rs` from a child directory is live in this tree), so the
/// whole `src/` tree is a candidate.
fn declarations_of(target: &Path, src_root: &Path) -> Vec<(PathBuf, bool)> {
    let needle = if target.file_name().is_some_and(|f| f == "mod.rs") {
        target.parent().and_then(Path::file_name)
    } else {
        target.file_stem()
    }
    .and_then(|s| s.to_str())
    .unwrap_or_default()
    .to_string();
    let mut out = vec![];
    for p in rs_files(src_root) {
        let p = canonical(&p);
        if p == target {
            continue;
        }
        let Ok(text) = fs::read_to_string(&p) else {
            continue;
        };
        if !text.contains(&needle) {
            continue;
        }
        let scan = scan_source(&text, Some(&p));
        for (child, line) in &scan.children {
            if canonical(child) == target {
                out.push((p.clone(), scan.test_line[*line]));
            }
        }
    }
    out
}

/// Whether ALL of `file` is test code: it says so itself (`#![cfg(test)]`), or
/// it is pulled in at least once and every declaration that pulls it in is in
/// test scope (or lives in a file that is test code). A file nothing declares
/// is production — fail closed.
fn file_is_test(file: &Path, src_root: &Path, memo: &mut BTreeMap<PathBuf, bool>) -> bool {
    let key = canonical(file);
    if let Some(&v) = memo.get(&key) {
        return v;
    }
    // Provisional answer for a declaration cycle: production.
    memo.insert(key.clone(), false);
    let text = fs::read_to_string(&key).unwrap_or_default();
    let verdict = scan_source(&text, Some(&key)).whole_file_test || {
        let decls = declarations_of(&key, src_root);
        !decls.is_empty()
            && decls
                .iter()
                .all(|(parent, in_test)| *in_test || file_is_test(parent, src_root, memo))
    };
    memo.insert(key, verdict);
    verdict
}

/// A line that is entirely a comment names the clock, it does not call it —
/// and this guard's own subject matter means the forbidden spellings turn up in
/// prose (the doc that explains WHY they are forbidden) more often than in code.
fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// Whether the site at `idx` carries an [`ALLOW_MARKER`] just above it.
fn is_allowed(lines: &[&str], idx: usize) -> bool {
    let start = idx.saturating_sub(ALLOW_LOOKBACK);
    lines[start..idx].iter().any(|l| l.contains(ALLOW_MARKER))
}

/// The 0-based line indices of `text` that call a [`FORBIDDEN`] clock outside
/// the test scope `scan` found in it: not a comment, not test code, not
/// allow-marked. A line holding both spellings is reported once per spelling.
/// Whether the whole FILE is test code is the caller's question
/// ([`file_is_test`]), because it depends on the files that pull this one in.
fn offending_lines(text: &str, scan: &Scan) -> Vec<usize> {
    if scan.whole_file_test {
        return vec![];
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut out = vec![];
    for (idx, line) in lines.iter().enumerate() {
        for forbidden in FORBIDDEN {
            if line.contains(forbidden)
                && !is_comment(line)
                && !scan.test_line.get(idx).copied().unwrap_or(false)
                && !is_allowed(&lines, idx)
            {
                out.push(idx);
            }
        }
    }
    out
}

/// [`offending_lines`] of a standalone source text (no files around it).
fn offending_lines_in(text: &str) -> Vec<usize> {
    offending_lines(text, &scan_source(text, None))
}

/// The derived closure must actually contain the crates the migration proved are
/// in it, and it must be walked rather than assumed. Pinned as a LOWER BOUND, not
/// an equality: a new dependency legitimately grows the set, and a guard that
/// fails on growth is a guard people delete.
#[test]
fn the_derived_wasm_closure_covers_the_crates_the_migration_found() {
    let root = workspace_root();
    let closure = wasm_closure(&root);
    // The nine the hand-list had, plus the five it was missing — every one of
    // which reaches a browser bundle through `aterm-wasm`.
    for expected in [
        "aterm-core",
        "aterm-effects",
        "aterm-gpu",
        "aterm-gpu-web",
        "aterm-grid",
        "aterm-render",
        "aterm-scrollback",
        "aterm-search",
        "aterm-wasm",
        "aterm-policy",
        "aterm-predict",
        "aterm-selection",
        "aterm-shell-integration",
        "aterm-types",
    ] {
        assert!(
            closure.contains(expected),
            "the derived wasm closure lost `{expected}` — the derivation or the \
             manifests drifted. Derived set: {closure:?}"
        );
    }
}

#[test]
fn no_wasm_reachable_crate_calls_the_std_clock_in_production_code() {
    let root = workspace_root();
    let closure = wasm_closure(&root);
    assert!(
        closure.len() > 9,
        "the derived closure collapsed to {} crate(s) — the manifest walk broke",
        closure.len()
    );
    let mut offenders = vec![];

    for name in &closure {
        let src = root.join("crates").join(name).join("src");
        if !src.is_dir() {
            continue;
        }
        let src_root = canonical(&src);
        let mut memo = BTreeMap::new();
        for file in rs_files(&src) {
            let text = fs::read_to_string(&file).expect("read src");
            if !FORBIDDEN.iter().any(|f| text.contains(f)) {
                continue;
            }
            let sites = offending_lines(&text, &scan_source(&text, Some(&file)));
            if sites.is_empty() || file_is_test(&file, &src_root, &mut memo) {
                continue;
            }
            let rel = file.strip_prefix(&root).unwrap_or(&file);
            for idx in sites {
                let line = text.lines().nth(idx).unwrap_or_default();
                offenders.push(format!("{}:{}: {}", rel.display(), idx + 1, line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "std's clock PANICS on wasm32 (\"time not implemented on this platform\") and these \
         crates compile into the wasm bundles. Use `aterm_time::Instant::now()` / \
         `aterm_time::SystemTime::now()` — byte-identical natively, shimmed to \
         performance.now()/Date.now() on wasm. A panic here does not merely lose a frame: it \
         poisons the wasm-bindgen RefCell and every later access throws \"recursive use of an \
         object detected\". If a site genuinely cannot reach wasm, mark it `{ALLOW_MARKER}` \
         with the reason on the lines directly above it. Offenders:\n  {}",
        offenders.join("\n  ")
    );
}

/// The scan's own semantics, on a synthetic file. Test scope is the ITEM an
/// attribute is on, not the rest of the file: the clock in `later()` sits
/// below a test module and is production — the shape that trapped the web
/// renderer's first frame while a "rest of the file" rule called it test code.
/// Braces inside char/string literals and comments do not move a boundary.
#[test]
fn the_scan_scopes_test_code_to_the_attributed_item() {
    let src = r#"fn prod() {
    let a = std::time::Instant::now();
    let c = (std::time::Instant::now(), std::time::SystemTime::now());
    // std::time::Instant::now() in prose
    // wasm-clock-guard: allow (native-only arm)
    let b = std::time::SystemTime::now();
}
#[cfg(test)]
mod tests {
    fn t() { let _ = std::time::Instant::now(); }
    const BRACE: char = '}';
    const S: &str = "}}} {";
    /* } */
    fn lt<'a>(x: &'a str) -> &'a str { x }
    #[test] fn u() { let _ = std::time::SystemTime::now(); }
}
fn later() {
    let z = std::time::Instant::now();
}
#[cfg(all(test, feature = "x"))]
fn gated() { std::time::Instant::now(); }
#[cfg(any(test, feature = "x"))]
fn either() { std::time::Instant::now(); }
#[cfg(not(test))]
fn not_test() { std::time::Instant::now(); }
#[cfg(test)]
use std::time::Instant as _T;
fn after_use() { std::time::Instant::now(); }
struct Fields {
    #[cfg(test)]
    probe: u8,
    live: [u8; { std::time::Instant::now(); 1 }],
}
fn last_param(a: u8, #[cfg(test)] probe: u8) -> u64 {
    let t = std::time::Instant::now();
    0
}
fn last_param_ml(
    a: u8,
    #[cfg(test)]
    probe: u8
) {
    std::time::Instant::now();
}
fn call_site() {
    f(a, #[cfg(test)] b);
    std::time::Instant::now();
}
"#;
    assert_eq!(
        offending_lines_in(src),
        vec![1, 2, 2, 17, 22, 24, 27, 31, 34, 42, 46]
    );
}

/// A file-level `#![cfg(test)]` makes the whole file test code.
#[test]
fn an_inner_cfg_test_exempts_the_whole_file() {
    let src = "#![cfg(test)]\nfn f() { let _ = std::time::Instant::now(); }\n";
    assert!(offending_lines_in(src).is_empty());
}

/// A file is test code as a whole exactly when what pulls it in is in test
/// scope — `#[cfg(test)] mod x;`, a `#[path]` module, an `include!` inside a
/// test module, and anything those declare in turn. A file pulled in by a
/// production `mod`/`include!`, or by nothing at all, is production.
#[test]
fn a_file_is_test_code_when_every_declaration_of_it_is() {
    let dir = std::env::temp_dir().join(format!("aterm-wasm-clock-guard-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let src = dir.join("src");
    fs::create_dir_all(src.join("unit_tests")).unwrap();
    let body = "pub fn f() { let _ = std::time::Instant::now(); }\n";
    let files = [
        (
            "lib.rs",
            "mod prod;\n#[cfg(test)]\nmod unit_tests;\n#[cfg(test)]\n#[path = \"elsewhere_tests.rs\"]\npub(crate) mod elsewhere;\n#[cfg(test)]\nmod inline {\n    include!(\"included.rs\");\n}\ninclude!(\"prod_included.rs\");\n",
        ),
        ("prod.rs", body),
        ("unit_tests.rs", "mod deeper;\n"),
        ("unit_tests/deeper.rs", body),
        ("elsewhere_tests.rs", body),
        ("included.rs", body),
        ("prod_included.rs", body),
        ("orphan.rs", body),
    ];
    for (name, text) in files {
        fs::write(src.join(name), text).unwrap();
    }
    let root = canonical(&src);
    let mut memo = BTreeMap::new();
    let verdicts: Vec<(&str, bool)> = files[1..]
        .iter()
        .map(|(name, _)| (*name, file_is_test(&src.join(name), &root, &mut memo)))
        .collect();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        verdicts,
        vec![
            ("prod.rs", false),
            ("unit_tests.rs", true),
            ("unit_tests/deeper.rs", true),
            ("elsewhere_tests.rs", true),
            ("included.rs", true),
            ("prod_included.rs", false),
            ("orphan.rs", false),
        ]
    );
}
