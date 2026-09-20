// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fact about ANOTHER THREAD may not be passed as a constant.
//!
//! THE INCIDENT (2026-09-19, the late-park handoff). The update worker ended a
//! held successor by calling
//! `retire_ungranted_successor(job, proxy, nonce, held, outcome, detail, /*parked=*/ false)`.
//! `parked` means "the main thread has stopped its PTY readers", and a branch on
//! it decided whether `Wake::UpdateHandoffUnpark` was ever posted — i.e. whether
//! the other thread was ever woken. It was a literal at all six call sites. But
//! the main thread re-arms a park gate every 20 ms while a successor holds its
//! claim, so a park could land at any instant, including inside the worker's 2 s
//! stand-down grace; when it did, no wake was sent and the user's terminal
//! stayed frozen through the grace, a SIGKILL and an unbounded reap. An
//! adversarial review found it. `ty` did not, and could not have: see below.
//!
//! WHY THE MODEL CHECKER MISSED IT, because that is the reason this gate is a
//! source scan and not another invariant. The derived models ARE interleaving
//! models — `Next` is a disjunction over every action and `interp` fires every
//! enabled action from every reachable state, so the tooling was never the
//! limitation. The limitation was the model's own transition relation: the
//! late-park lane's guards encode the INTENDED serialization as a premise.
//! `RevokeUngrantedSuccessor` sets `failure = 1`; `ParkParentReaders` requires
//! `failure == 0`; nothing resets it. So after a revoke the park is permanently
//! disabled BY GUARD, and the racing state is a well-formed valuation that is
//! unreachable from `Init` by construction. The invariant that names the bug was
//! already present and already green —
//! `FailedPreCommitChildStaysReaderless` says `granted == 0 ⇒ parent_readers == 1`,
//! which is exactly the negation of what shipped — and it held VACUOUSLY because
//! the lane is pinned to the unparked phase. The property was right; the
//! transition relation was wrong, and a checker cannot find a behaviour its spec
//! forbids.
//!
//! So the rule is enforced over the SOURCE, like `ty_drivers_are_armed.rs` and
//! for the same stated reason: the failure mode is a premise nobody computes,
//! which no instrumentation inside the callee can observe, and which the next
//! model will assume away exactly as this one did.
//!
//! THE RULE. A function parameter is refused when BOTH hold:
//!   1. a branch on it guards a CROSS-AGENT POST — `send_event(`, a channel
//!      `send`, `notify_one`/`notify_all`, `unpark` — i.e. the parameter decides
//!      whether another agent is told anything; and
//!   2. every call site in the workspace passes a literal for it — so no caller
//!      ever measured the thing the name claims.
//!
//! Either half alone is ordinary code. Together they are a claim about another
//! agent's mutable state, frozen at compile time, deciding whether that agent is
//! woken.
//!
//! TWO-SIDED, because a ratchet with one side is not a ratchet. The live tree
//! finds zero candidates (the fix removed the parameter and made the post
//! unconditional), so a bare scan would be green over an empty set forever and
//! nobody would notice it rotting. `tests/fixtures/foreign_fact_negative_control.rs.txt`
//! carries the shipped pre-fix shape, and the scanner MUST still flag it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Call shapes that tell ANOTHER agent something. A branch on a parameter that
/// guards one of these is deciding whether a peer is woken at all.
const CROSS_AGENT_POSTS: &[&str] = &[
    "send_event(",
    ".send(",
    ".try_send(",
    "notify_one(",
    "notify_all(",
    ".unpark(",
    "post_event(",
];

/// Parameters this gate has been shown and which a human has ruled are NOT a
/// foreign fact, with the reason. An entry that STOPS firing fails the test just
/// as an unlisted hit does: a waiver for a defect that no longer exists is a
/// waiver nobody re-read.
const REVIEWED: &[(&str, &str, &str)] = &[
    // (file suffix, function, parameter) — empty on purpose. The one hit this
    // rule was built from is fixed; the fixture below is what keeps it honest.
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate>/.. is the workspace root")
        .to_path_buf()
}

/// Replace every comment and string byte with a space, preserving length and
/// newlines so byte offsets and line numbers still name the original. Scanning
/// raw source would match `send_event(` inside a doc comment describing one.
fn blank_noncode(src: &str) -> String {
    let b = src.as_bytes();
    let mut out: Vec<u8> = b.to_vec();
    let mut i = 0usize;
    let blank = |out: &mut Vec<u8>, i: usize| {
        if out[i] != b'\n' {
            out[i] = b' ';
        }
    };
    while i < b.len() {
        match b[i] {
            b'/' if i + 1 < b.len() && b[i + 1] == b'/' => {
                while i < b.len() && b[i] != b'\n' {
                    blank(&mut out, i);
                    i += 1;
                }
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                let mut depth = 0usize;
                while i < b.len() {
                    if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
                        depth += 1;
                        blank(&mut out, i);
                        blank(&mut out, i + 1);
                        i += 2;
                        continue;
                    }
                    if b[i] == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
                        depth -= 1;
                        blank(&mut out, i);
                        blank(&mut out, i + 1);
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                        continue;
                    }
                    blank(&mut out, i);
                    i += 1;
                }
            }
            b'"' => {
                blank(&mut out, i);
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' {
                        blank(&mut out, i);
                        if i + 1 < b.len() {
                            blank(&mut out, i + 1);
                        }
                        i += 2;
                        continue;
                    }
                    let done = b[i] == b'"';
                    blank(&mut out, i);
                    i += 1;
                    if done {
                        break;
                    }
                }
            }
            _ => i += 1,
        }
    }
    String::from_utf8(out).expect("blanking preserves UTF-8 boundaries of ASCII delimiters")
}

/// The index of the delimiter closing the one at `open`, or `None`.
fn match_delim(s: &[u8], open: usize, o: u8, c: u8) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < s.len() {
        if s[i] == o {
            depth += 1;
        } else if s[i] == c {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Split on commas at nesting depth zero — one entry per parameter or argument.
fn split_top_level(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for ch in s.chars() {
        match ch {
            '(' | '[' | '{' | '<' => depth += 1,
            // CLAMPED AT ZERO on purpose. `>` is not only a closing bracket:
            // a parameter list carries `-> T` in every fn-pointer and closure
            // type, and an unclamped decrement drives the depth negative there,
            // after which no comma splits and two parameters merge into one —
            // which would silently hide a candidate rather than report it.
            ')' | ']' | '}' | '>' => depth = (depth - 1).max(0),
            _ => {}
        }
        if ch == ',' && depth == 0 {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur);
    }
    parts
}

fn line_of(src: &str, idx: usize) -> usize {
    src[..idx].matches('\n').count() + 1
}

/// `fn` occurrences at a word boundary, with the name and the byte index of the
/// `(` opening its parameter list.
fn functions(src: &str) -> Vec<(String, usize, usize)> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(rel) = src[i..].find("fn ") {
        let at = i + rel;
        i = at + 3;
        if at > 0 && (b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_') {
            continue;
        }
        let rest = &src[at + 3..];
        let name: String = rest
            .chars()
            .skip_while(|c| c.is_whitespace())
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        let Some(paren_rel) = src[at..].find('(') else {
            continue;
        };
        let open = at + paren_rel;
        // A generic list before the parameters is fine; a `(` that far away is
        // not this function's parameter list.
        if open - at > 200 {
            continue;
        }
        out.push((name, at, open));
    }
    out
}

#[derive(Debug, Clone)]
struct Candidate {
    file: String,
    line: usize,
    function: String,
    parameter: String,
    index: usize,
    arity: usize,
}

/// Parameters that a branch guards a cross-agent post with.
fn candidates_in(file: &str, src: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    let b = src.as_bytes();
    for (name, start, open) in functions(src) {
        let Some(close) = match_delim(b, open, b'(', b')') else {
            continue;
        };
        let params = split_top_level(&src[open + 1..close]);
        let arity = params.len();
        let Some(body_open) = src[close..].find('{').map(|r| close + r) else {
            continue;
        };
        let Some(body_close) = match_delim(b, body_open, b'{', b'}') else {
            continue;
        };
        let body = &src[body_open..body_close];
        for (index, param) in params.iter().enumerate() {
            let trimmed = param.trim();
            if !trimmed.ends_with("bool") {
                continue;
            }
            let Some((lhs, _)) = trimmed.split_once(':') else {
                continue;
            };
            let pname = lhs.trim().trim_start_matches("mut ").trim();
            if pname.is_empty() || !pname.chars().all(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            if guards_a_post(body, pname) {
                out.push(Candidate {
                    file: file.to_string(),
                    line: line_of(src, start),
                    function: name.clone(),
                    parameter: pname.to_string(),
                    index,
                    arity,
                });
            }
        }
    }
    out
}

/// Does `if <param>` (or `if !<param>`) open a block that posts to another agent?
fn guards_a_post(body: &str, param: &str) -> bool {
    let b = body.as_bytes();
    let mut i = 0usize;
    while let Some(rel) = body[i..].find("if ") {
        let at = i + rel;
        i = at + 3;
        if at > 0 && (b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_') {
            continue;
        }
        let cond = body[at + 3..].trim_start();
        let cond = cond.strip_prefix('!').unwrap_or(cond).trim_start();
        if !cond.starts_with(param) {
            continue;
        }
        let after = &cond[param.len()..];
        if after
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        // The guarded block: the next `{` within a short reach of the condition.
        let Some(ob_rel) = body[at..].find('{') else {
            continue;
        };
        let ob = at + ob_rel;
        if ob - at > 120 {
            continue;
        }
        let block = match match_delim(b, ob, b'{', b'}') {
            Some(oe) => &body[ob..oe],
            None => &body[ob..(ob + 400).min(body.len())],
        };
        if CROSS_AGENT_POSTS.iter().any(|p| block.contains(p)) {
            return true;
        }
    }
    false
}

/// Every argument passed for `cand`'s parameter across the whole tree, as
/// `(file, line, argument text)`.
/// One place a candidate's parameter is passed: `(file, line, argument text)`.
type CallSite = (String, usize, String);

/// A refused parameter and every site that passes it a literal.
type ConstantPremise = (Candidate, Vec<CallSite>);

fn call_sites(cand: &Candidate, sources: &BTreeMap<String, String>) -> Vec<CallSite> {
    let mut out = Vec::new();
    for (file, src) in sources {
        let b = src.as_bytes();
        let mut i = 0usize;
        while let Some(rel) = src[i..].find(&cand.function) {
            let at = i + rel;
            i = at + cand.function.len();
            if at > 0 {
                let prev = b[at - 1];
                if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'.' {
                    continue;
                }
                if src[..at].trim_end().ends_with("fn") {
                    continue;
                }
            }
            let rest = &src[at + cand.function.len()..];
            let lead = rest.len() - rest.trim_start().len();
            if !rest[lead..].starts_with('(') {
                continue;
            }
            let open = at + cand.function.len() + lead;
            let Some(close) = match_delim(b, open, b'(', b')') else {
                continue;
            };
            let args = split_top_level(&src[open + 1..close]);
            if args.len() != cand.arity {
                continue;
            }
            out.push((
                file.clone(),
                line_of(src, at),
                args[cand.index].trim().to_string(),
            ));
        }
    }
    out
}

fn is_literal(arg: &str) -> bool {
    matches!(arg.trim(), "true" | "false")
}

/// `crates/**/src/**.rs`, skipping build output and vendored trees.
fn rust_sources(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.join("crates")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !matches!(name.as_ref(), "target" | "vendor" | ".git" | "node_modules") {
                    stack.push(path);
                }
                continue;
            }
            if path.extension().is_some_and(|e| e == "rs")
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                out.insert(rel, blank_noncode(&text));
            }
        }
    }
    out
}

/// Candidates whose parameter is a literal at EVERY call site.
fn constant_premises(sources: &BTreeMap<String, String>) -> Vec<ConstantPremise> {
    let mut out = Vec::new();
    for (file, src) in sources {
        for cand in candidates_in(file, src) {
            let sites = call_sites(&cand, sources);
            if sites.is_empty() {
                continue;
            }
            if sites.iter().all(|(_, _, a)| is_literal(a)) {
                out.push((cand, sites));
            }
        }
    }
    out
}

/// THE FIRING SIDE. The shipped pre-fix shape must still be caught — this is
/// what stops the gate from being green over an empty set.
#[test]
fn the_rule_still_catches_the_shape_it_was_written_for() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/foreign_fact_negative_control.rs.txt");
    let text = std::fs::read_to_string(&fixture).expect("the negative control is checked in");
    let mut sources = BTreeMap::new();
    sources.insert("fixture".to_string(), blank_noncode(&text));

    let hits = constant_premises(&sources);
    let named: Vec<_> = hits
        .iter()
        .map(|(c, sites)| (c.function.as_str(), c.parameter.as_str(), sites.len()))
        .collect();
    assert!(
        named
            .iter()
            .any(|(f, p, _)| *f == "retire_ungranted_successor" && *p == "parked"),
        "the scanner no longer catches the defect it was written for — it has rotted, \
         and the live tree's silence means nothing. Caught instead: {named:?}"
    );
    let sites = hits
        .iter()
        .find(|(c, _)| c.parameter == "parked")
        .map(|(_, s)| s.len())
        .unwrap_or(0);
    assert_eq!(
        sites, 6,
        "the fixture carries the six shipped call sites; the scanner found {sites}"
    );
}

/// Both halves of the rule are load-bearing: drop either and the shape is
/// ordinary code. A scanner that fired on one half alone would drown a
/// 1983-file tree in noise, and one that needed neither would fire on nothing.
#[test]
fn each_half_of_the_rule_is_load_bearing() {
    // (a) A literal-at-every-site bool that guards NO cross-agent post.
    let no_post = r#"
        fn set_quiet(flag: bool) { if flag { self.quiet = true; } }
        fn a() { set_quiet(true); }
    "#;
    let mut src = BTreeMap::new();
    src.insert("x".to_string(), blank_noncode(no_post));
    assert!(
        constant_premises(&src).is_empty(),
        "a constant that wakes nobody is not a foreign fact"
    );

    // (b) A bool that guards a post but is COMPUTED by its caller.
    let computed = r#"
        fn tell(flag: bool, proxy: &P) { if flag { let _ = proxy.send_event(W); } }
        fn a(app: &App) { tell(app.is_parked(), proxy); }
    "#;
    let mut src = BTreeMap::new();
    src.insert("y".to_string(), blank_noncode(computed));
    assert!(
        constant_premises(&src).is_empty(),
        "a caller that MEASURES the other agent is exactly what the rule asks for"
    );

    // (c) Both halves: caught.
    let both = r#"
        fn tell(flag: bool, proxy: &P) { if flag { let _ = proxy.send_event(W); } }
        fn a(proxy: &P) { tell(false, proxy); }
    "#;
    let mut src = BTreeMap::new();
    src.insert("z".to_string(), blank_noncode(both));
    assert_eq!(
        constant_premises(&src).len(),
        1,
        "the conjunction of both halves is the rule"
    );
}

/// THE LIVE SIDE. No parameter in the workspace may be a foreign fact unless a
/// human has reviewed it into `REVIEWED`.
#[test]
fn no_parameter_freezes_another_agents_state() {
    let root = workspace_root();
    let sources = rust_sources(&root);
    assert!(
        sources.len() > 500,
        "the scan found only {} sources under {}; it is looking in the wrong place, \
         and an empty scan proves nothing",
        sources.len(),
        root.display()
    );

    let mut refused = Vec::new();
    let mut seen_reviewed = Vec::new();
    for (cand, sites) in constant_premises(&sources) {
        let reviewed = REVIEWED.iter().find(|(f, fun, p)| {
            cand.file.ends_with(f) && cand.function == *fun && cand.parameter == *p
        });
        if let Some(entry) = reviewed {
            seen_reviewed.push(*entry);
            continue;
        }
        let where_ = sites
            .iter()
            .map(|(f, l, a)| format!("\n      {f}:{l} -> {a}"))
            .collect::<String>();
        refused.push(format!(
            "\n  {}:{} fn {} — parameter `{}` decides whether another agent is posted to, \
             and every call site passes a literal:{where_}",
            cand.file, cand.line, cand.function, cand.parameter
        ));
    }

    assert!(
        refused.is_empty(),
        "A FACT ABOUT ANOTHER AGENT IS BEING PASSED AS A CONSTANT.\n\
         A branch on this parameter decides whether a peer thread is woken, and no \
         caller ever measured it — so the claim is frozen at compile time while the \
         thing it names goes on changing. That is the late-park freeze (2026-09-19) \
         exactly: `parked: false` while the main thread parked anyway, and the wake \
         that would have released the user's terminal was never sent.\n\
         Compute it at the call site, or restructure so the peer is told \
         unconditionally and decides for itself.{}",
        refused.join("")
    );

    for entry in REVIEWED {
        assert!(
            seen_reviewed.contains(entry),
            "the reviewed waiver {entry:?} no longer matches anything — delete it, or \
             the next reader will believe a defect is still being tolerated"
        );
    }
}
