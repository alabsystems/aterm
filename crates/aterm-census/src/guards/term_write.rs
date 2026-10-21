// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! term-write: a host writes into a session's terminal only under a
//! `TermGuard`.
//!
//! A write into a session's engine reaches its `screen`, `cursor` and `cells`
//! subscribers only by a ring. The PTY reader rings after each burst; every
//! other write rings as its hold ends, from the drop of the `TermGuard` that
//! `term_lock`, `term_try_lock` and `term_lock_ui` return
//! (`crates/aterm-gui/src/lib.rs`), which compares the frame key across the
//! hold. Nothing reads a content watch on a timer, so a write made through a
//! raw `MutexGuard` is pushed to nobody until something else rings.
//!
//! A raw `.lock()`, `.try_lock()` or `.lock_or_recover()` of a session's
//! terminal (a receiver named `term`: `term`, `s.term`, `pool.get(id)?.term`,
//! the one name the crate gives that mutex) is held to reading by the
//! compiler while its guard is bound without `mut` or borrowed `&`. Read over
//! the GUI's shipped code ([`Corpus::test_only`] files aside), each of these
//! can write, and is a finding:
//!
//! - a `let`, `if let` or `while let` that binds it `mut` (`let mut t =`,
//!   `let Ok(mut t) =`, `Some(mut t)`), or a `match` on it with an arm that
//!   does;
//! - a closure that takes it `mut` (`.map(|mut t| …)`);
//! - a `&mut` borrow of it (`&mut term.lock()…`, `&mut *term.lock()…`);
//! - a method called on the guard itself (`term.lock().unwrap().resize(…)`),
//!   which may take it `&mut`, or an assignment through it;
//! - `MutexGuard<…Terminal>` named outside lib.rs: a function that returns or
//!   takes one hands a raw guard to code no lock site shows.
//!
//! Not detected: a guard passed by value or returned through a call
//! (`patiently(|| term.try_lock())`), a deferred `let t; t = …`, and a
//! terminal mutex under another name.
//!
//! [`Corpus::test_only`]: super::Corpus::test_only

use super::rules::test_path;
use super::{Bound, Finding, Rule, Tree, has, hits, is_word};

/// The crate whose code holds the session terminals.
const GUI: &str = "crates/aterm-gui/src/";

/// Where `TermGuard` wraps the raw guard.
const GUARD_HOME: &str = "crates/aterm-gui/src/lib.rs";

/// The `Mutex` methods that return a raw guard, and whether the guard comes
/// back bare (not in a `Result`).
const LOCKS: &[(&str, bool)] = &[
    ("lock", false),
    ("try_lock", false),
    ("lock_or_recover", true),
];

/// The methods that keep the chain's value the guard: unwrapping its
/// `Result`, or a `PoisonError`'s.
const UNWRAPS: &[&str] = &["unwrap", "expect", "unwrap_or_else", "into_inner"];

pub(super) const TERM_WRITE: Rule = Rule {
    id: "term-write",
    why: "a host's write into a session's terminal rings its screen, cursor and cells \
          subscribers only as a TermGuard hold ends (term_lock, term_try_lock, \
          term_lock_ui); a write through a raw guard is pushed to nobody until something \
          else rings",
    scope: &[GUI, GUARD_HOME],
    check: term_write,
};

fn term_write(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let test_only = tree.corpus.test_only(GUI);
    for file in tree.corpus.under(&[GUI]) {
        if test_path(&file.rel)
            || test_only.contains(&file.rel)
            || !file.mentions(&["term", "MutexGuard"])
        {
            continue;
        }
        let code = file.shipped_code();
        let mut found = writes(&code);
        if file.rel != GUARD_HOME {
            found.extend(raw_guard_types(&code));
        }
        found.sort_unstable();
        let mut last = 0;
        for (at, what) in found {
            let no = code[..at].matches('\n').count() + 1;
            if no != last {
                out.push(file.finding(TERM_WRITE.id, no, what.to_string()));
                last = no;
            }
        }
    }
}

/// Each raw lock of a `term` in `code` whose guard can be written through:
/// the offset to report and what the lock site does.
fn writes(code: &str) -> Vec<(usize, &'static str)> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    for &(method, bare) in LOCKS {
        for at in hits(code, method, Bound::Word) {
            let dot = ws_back(b, at);
            if dot == 0 || b[dot - 1] != b'.' {
                continue;
            }
            let recv_end = ws_back(b, dot - 1);
            let recv = word_start(b, recv_end);
            if &code[recv..recv_end] != "term" {
                continue;
            }
            let open = ws_fwd(b, at + method.len());
            let close = ws_fwd(b, open + 1);
            if b.get(open) != Some(&b'(') || b.get(close) != Some(&b')') {
                continue;
            }
            let start = receiver_start(b, recv);
            match chain(code, close + 1, bare) {
                Chain::Writes(at, what) => out.push((at, what)),
                Chain::Read => {}
                Chain::Flows(end) => {
                    if let Some(what) = flows_into(code, start, end) {
                        out.push((start, what));
                    }
                }
            }
        }
    }
    out
}

/// Where the chain after a raw lock leaves its guard.
enum Chain {
    /// The chain writes through it (at this offset).
    Writes(usize, &'static str),
    /// The chain ends in a read the compiler holds to reading (a closure
    /// that takes it immutably, a test of the `Result`).
    Read,
    /// The chain's value is still the guard, or its `Result`, up to here.
    Flows(usize),
}

/// Follow the method chain from `from`, just past the lock's `()`.
fn chain(code: &str, mut from: usize, bare: bool) -> Chain {
    let b = code.as_bytes();
    let mut guard = bare;
    loop {
        let at = ws_fwd(b, from);
        match b.get(at) {
            Some(b'?') => {
                guard = true;
                from = at + 1;
                continue;
            }
            Some(b'.') => {}
            _ => return Chain::Flows(from),
        }
        let name = ws_fwd(b, at + 1);
        let name_end = word_end(b, name);
        let open = ws_fwd(b, name_end);
        if name_end == name || b.get(open) != Some(&b'(') {
            return Chain::Flows(from);
        }
        let Some(close) = closer(b, open) else {
            return Chain::Flows(from);
        };
        let method = &code[name..name_end];
        if UNWRAPS.contains(&method) {
            guard = true;
        } else if method == "ok" {
            guard = false;
        } else if guard {
            return Chain::Writes(name, "calls a method on the raw guard itself");
        } else if closure_takes_mut(&code[open + 1..close]) {
            return Chain::Writes(name, "hands the raw guard to a closure that takes it `mut`");
        } else {
            return Chain::Read;
        }
        from = close + 1;
    }
}

/// What the expression `code[start..end]`, a raw guard or its `Result`, is
/// bound or borrowed as, when that can write.
fn flows_into(code: &str, start: usize, end: usize) -> Option<&'static str> {
    let b = code.as_bytes();
    let after = ws_fwd(b, end);
    if b.get(after) == Some(&b'=') && !matches!(b.get(after + 1), Some(b'=' | b'>')) {
        return Some("assigns through the raw guard");
    }
    let mut lead = ws_back(b, start);
    while lead > 0 && b[lead - 1] == b'*' {
        lead = ws_back(b, lead - 1);
    }
    if ends_with_word(code, lead, "mut") && b[..ws_back(b, lead - 3)].ends_with(b"&") {
        return Some("borrows the raw guard `&mut`");
    }
    while lead > 0 && matches!(b[lead - 1], b'&' | b'(') {
        lead = ws_back(b, lead - 1);
    }
    if ends_with_word(code, lead, "match") {
        if let Some(arm) = arm_binding_mut(code, ws_fwd(b, end)) {
            return Some(arm);
        }
        lead = ws_back(b, lead - "match".len());
    }
    let eq = lead.checked_sub(1)?;
    if b[eq] != b'=' || eq == 0 || b"=!<>+-*/%^&|".contains(&b[eq - 1]) {
        return None;
    }
    let head = code[..eq].rfind([';', '{', '}']).map_or(0, |at| at + 1);
    let pattern = code[head..eq].split_once("let ")?.1;
    binds_mut(pattern).then_some("binds the raw guard `mut`")
}

/// Whether a `match` whose block opens at `open` has an arm whose pattern
/// binds `mut`.
fn arm_binding_mut(code: &str, open: usize) -> Option<&'static str> {
    let b = code.as_bytes();
    if b.get(open) != Some(&b'{') {
        return None;
    }
    let close = closer(b, open)?;
    let mut depth = 0usize;
    let mut in_pattern = true;
    let mut pattern = open + 1;
    let mut i = open + 1;
    while i < close {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' => depth = depth.saturating_sub(1),
            b'}' => {
                depth = depth.saturating_sub(1);
                // A block arm ends with its `}`, unless an `else` follows.
                if depth == 0 && !in_pattern && !word_at(code, ws_fwd(b, i + 1), "else") {
                    in_pattern = true;
                    pattern = i + 1;
                }
            }
            b',' if depth == 0 && !in_pattern => {
                in_pattern = true;
                pattern = i + 1;
            }
            b'=' if depth == 0 && in_pattern && b.get(i + 1) == Some(&b'>') => {
                if binds_mut(&code[pattern..i]) {
                    return Some("matches the raw guard with an arm that binds it `mut`");
                }
                in_pattern = false;
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Whether `args` hold a closure whose parameters bind `mut`.
fn closure_takes_mut(args: &str) -> bool {
    let b = args.as_bytes();
    let mut i = 0;
    while let Some(rel) = args[i..].find('|') {
        let bar = i + rel;
        let before = ws_back(b, bar);
        let opens = before == 0 || matches!(b[before - 1], b'(' | b',') || {
            ends_with_word(args, before, "move")
        };
        let Some(len) = args[bar + 1..].find('|') else {
            return false;
        };
        if opens && binds_mut(&args[bar + 1..bar + 1 + len]) {
            return true;
        }
        i = bar + 1 + len + 1;
    }
    false
}

/// Whether a pattern binds `mut` (a `&mut` in it matches a reference).
fn binds_mut(pattern: &str) -> bool {
    let b = pattern.as_bytes();
    hits(pattern, "mut", Bound::Word).any(|at| !b[..ws_back(b, at)].ends_with(b"&"))
}

/// `MutexGuard<…Terminal…>` named in `code`.
fn raw_guard_types(code: &str) -> Vec<(usize, &'static str)> {
    let b = code.as_bytes();
    hits(code, "MutexGuard", Bound::Word)
        .filter(|&at| {
            let open = ws_fwd(b, at + "MutexGuard".len());
            b.get(open) == Some(&b'<')
                && angle_closer(b, open)
                    .is_some_and(|close| has(&code[open..close], "Terminal", Bound::Word))
        })
        .map(|at| {
            (
                at,
                "names a raw guard of a Terminal outside lib.rs's TermGuard",
            )
        })
        .collect()
}

/// The start of the receiver whose last segment starts at `start`: back over
/// each `segment.` before it, a segment being a path, a call, an index or a
/// `?` (`self.pool.get(id)?.term`).
fn receiver_start(b: &[u8], mut start: usize) -> usize {
    loop {
        let dot = ws_back(b, start);
        if dot == 0 || b[dot - 1] != b'.' || (dot >= 2 && b[dot - 2] == b'.') {
            return start;
        }
        let seg_end = ws_back(b, dot - 1);
        let mut at = seg_end;
        while at > 0 {
            match b[at - 1] {
                b'?' => at -= 1,
                b')' | b']' => match opener(b, at - 1) {
                    Some(open) => at = open,
                    None => return start,
                },
                _ => break,
            }
        }
        loop {
            at = word_start(b, at);
            if at >= 2 && &b[at - 2..at] == b"::" {
                at -= 2;
            } else {
                break;
            }
        }
        if at == seg_end {
            return start;
        }
        start = at;
    }
}

fn ws_back(b: &[u8], mut at: usize) -> usize {
    while at > 0 && b[at - 1].is_ascii_whitespace() {
        at -= 1;
    }
    at
}

fn ws_fwd(b: &[u8], mut at: usize) -> usize {
    while b.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    at
}

fn word_start(b: &[u8], mut at: usize) -> usize {
    while at > 0 && is_word(b[at - 1]) {
        at -= 1;
    }
    at
}

fn word_end(b: &[u8], mut at: usize) -> usize {
    while b.get(at).is_some_and(|&c| is_word(c)) {
        at += 1;
    }
    at
}

/// Whether `code[..end]` ends with the whole word `word`.
fn ends_with_word(code: &str, end: usize, word: &str) -> bool {
    code[..end].ends_with(word) && word_start(code.as_bytes(), end) == end - word.len()
}

/// Whether the whole word `word` starts at `at`.
fn word_at(code: &str, at: usize, word: &str) -> bool {
    code[at..].starts_with(word) && word_end(code.as_bytes(), at) == at + word.len()
}

/// The `)` or `]` that closes the bracket at `open`.
fn closer(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &c) in b.iter().enumerate().skip(open) {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The `(` or `[` that opens the bracket closed at `close`.
fn opener(b: &[u8], close: usize) -> Option<usize> {
    let mut depth = 0usize;
    for i in (0..=close).rev() {
        match b[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The `>` that closes the generic list opened at `open`.
fn angle_closer(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &c) in b.iter().enumerate().skip(open) {
        match c {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b';' | b'{' => return None,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    const AT: &str = "crates/aterm-gui/src/app_input.rs";

    fn found(src: &str) -> Vec<String> {
        spots(&run(&TERM_WRITE, &[(AT, src)]))
    }

    /// Each shape of a write through a raw guard, with the line it fires on.
    /// The first is the review's: Kitty's step was this shape until it moved
    /// to `term_try_lock`.
    #[test]
    fn every_shape_of_a_write_through_a_raw_guard_is_found() {
        let cases: &[(&str, &str, usize)] = &[
            (
                "let Ok(mut …) = try_lock",
                "fn f(s: &S) {\n    let Ok(mut t) = s.term.try_lock() else {\n        \
                 return;\n    };\n    t.text_selection_mut().clear();\n}\n",
                2,
            ),
            (
                "let mut … = lock().unwrap()",
                "fn f(term: &Mutex<Terminal>) {\n    let mut t = term.lock().unwrap();\n    \
                 t.resize(2, 3);\n}\n",
                2,
            ),
            (
                "if let Some(mut …) after ok()",
                "fn f(s: &S) {\n    if let Some(mut t) = s.term.try_lock().ok() {\n        \
                 t.scroll_display(1);\n    }\n}\n",
                2,
            ),
            (
                "a match arm binding mut",
                "fn f(h: &H) {\n    match h.term.try_lock() {\n        Ok(mut t) => t.reset(),\n        \
                 Err(_) => {}\n    }\n}\n",
                2,
            ),
            (
                "a later match arm, after a block arm",
                "fn f(h: &H) {\n    match self.pool.get(id)?.term.try_lock() {\n        \
                 Err(TryLockError::WouldBlock) => {\n            return;\n        }\n        \
                 Err(TryLockError::Poisoned(mut p)) => p.get_mut().reset(),\n        \
                 Ok(t) => t.rows(),\n    };\n}\n",
                2,
            ),
            (
                "a mut binding of the match's value",
                "fn f(h: &H) {\n    let mut t = match h.term.try_lock() {\n        \
                 Ok(t) => t,\n        Err(_) => return,\n    };\n}\n",
                2,
            ),
            (
                "a closure taking it mut",
                "fn f(s: &S) {\n    s.term.try_lock().map(|mut t| t.reset());\n}\n",
                2,
            ),
            (
                "&mut of the guard",
                "fn f(term: &Mutex<Terminal>) {\n    apply(&mut term.lock().unwrap(), 3);\n}\n",
                2,
            ),
            (
                "&mut * of the guard",
                "fn f(term: &Mutex<Terminal>) {\n    apply(&mut *term.lock_or_recover());\n}\n",
                2,
            ),
            (
                "a method on the guard itself, wrapped by rustfmt",
                "fn f(s: &S) {\n    s.term\n        .lock()\n        .unwrap_or_else(|p| \
                 p.into_inner())\n        .process(b\"x\");\n}\n",
                5,
            ),
            (
                "a method on the guard after ok()?",
                "fn f(s: &S) -> Option<()> {\n    s.term.try_lock().ok()?.resize(1, 2);\n    \
                 Some(())\n}\n",
                2,
            ),
            (
                "an assignment through it",
                "fn f(term: &Mutex<Terminal>, other: Terminal) {\n    *term.lock().unwrap() = \
                 other;\n}\n",
                2,
            ),
            (
                "a raw guard type handed out",
                "fn engine(&self) -> MutexGuard<'_, Terminal> {\n    todo!()\n}\n",
                1,
            ),
        ];
        for (why, src, line) in cases {
            assert_eq!(found(src), [format!("{AT}:{line}")], "MISS — {why}:\n{src}");
        }
    }

    /// A raw guard bound immutably, borrowed `&` or read in a closure is held
    /// to reading by the compiler; so is every lock that is not a `term`'s, the
    /// guard type in lib.rs, test code by its own gate or its parent's, and
    /// prose.
    #[test]
    fn reads_other_locks_test_code_and_prose_are_silent() {
        let reads = "fn f(s: &S, term: &Mutex<Terminal>) -> Option<u16> {\n\
                     let Ok(t) = s.term.try_lock() else { return None };\n\
                     let fresh = match s.term.try_lock() {\n    Ok(t) => Some(label(&t)),\n    \
                     Err(TryLockError::Poisoned(p)) => Some(label(&p.into_inner())),\n    \
                     Err(TryLockError::WouldBlock) => None,\n};\n\
                     let vi = s.term.try_lock().is_ok_and(|t| t.vi_is_active());\n\
                     let e = s.term.try_lock().ok().map(|t| t.title_epoch());\n\
                     let t = h.term.try_lock().ok()?;\n\
                     let k = Fingerprint::of(&term.lock().unwrap());\n\
                     let r = term.try_lock().map_or(fallback, |t| (t.rows(), t.cols()));\n\
                     if let Some(t) = patiently(p, || s.term.try_lock()) {}\n\
                     let mut n = term_lock(term);\n\
                     let mut m = self.meta.lock().unwrap();\n\
                     let mut c = s.cast.try_lock().unwrap();\n\
                     let e = term.lock().unwrap() == other;\n\
                     // let mut t = term.lock().unwrap();\n\
                     let s = \"let Ok(mut t) = s.term.try_lock()\";\n\
                     None\n}\n\
                     #[cfg(test)]\nmod tests {\n    fn t(term: &Mutex<Terminal>) {\n        \
                     term.lock().unwrap().process(b\"x\");\n    }\n}\n";
        let lib = "pub(crate) fn term_lock(term: &Mutex<Terminal>) -> TermGuard<'_> {\n    \
                   let guard = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());\n    \
                   TermGuard { guard }\n}\n\
                   pub(crate) struct TermGuard<'a> {\n    guard: MutexGuard<'a, Terminal>,\n}\n\
                   #[cfg(all(test, unix))]\n#[path = \"rig_conformance.rs\"]\nmod rig;\n";
        let write =
            "fn f(term: &Mutex<Terminal>) {\n    term.lock().unwrap().process(b\"x\");\n}\n";
        let found = run(
            &TERM_WRITE,
            &[
                (AT, reads),
                (GUARD_HOME, lib),
                ("crates/aterm-gui/src/rig_conformance.rs", write),
                ("crates/aterm-gui/src/lib_tests.rs", write),
                ("crates/aterm-control/src/conformance.rs", write),
            ],
        );
        assert!(found.is_empty(), "{found:?}");
        // The negative control: the same writer, declared without a gate.
        let found = run(
            &TERM_WRITE,
            &[
                (GUARD_HOME, "#[path = \"rig_conformance.rs\"]\nmod rig;\n"),
                ("crates/aterm-gui/src/rig_conformance.rs", write),
            ],
        );
        assert_eq!(spots(&found), ["crates/aterm-gui/src/rig_conformance.rs:2"]);
    }
}
