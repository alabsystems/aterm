// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! m6-names (the learner's design, §13.3): only the learner's consumers name its module, and the
//! real sets' capability, its mints and the real pairs' flag stay in the files that list them.
//!
//! A rule over the whole tree, so it runs with every other guard rather than with
//! `aterm-effects`'s tests: that crate's tests alone do not see a new name of them. It reads the
//! corpus, `crates/` and `apps/`; the top-level oracles (`libc-oracle`, `win32-oracle`) depend on
//! no crate that declares these names, and are not read.

use super::{Finding, Rule, Tree};

pub(super) const M6_NAMES: Rule = Rule {
    id: "m6-names",
    why: "a file outside the learner's consumers names its module, or the real sets' capability, \
          a mint or the real pairs' flag stands outside the files the design lists (a lab that \
          reaches the learner past its door)",
    scope: &[M4_RS, M4_TESTS_RS, M6_RS],
    check: m6_names,
};

fn m6_names(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in &tree.corpus.files {
        let path = file.rel.as_str();
        for refusal in scan_confinement(path, &file.raw) {
            out.push(file.finding(M6_NAMES.id, 0, refusal));
        }
        if !M6_NAMERS.contains(&path) {
            for refusal in scan_workspace_file(path, &file.raw) {
                out.push(file.finding(M6_NAMES.id, 0, refusal));
            }
        }
    }
    // m4.rs: the declaration, its impl, `new`'s return and body, and the three real-set mints;
    // m6.rs: the import, the ledger's field, the real-set open and the private load it calls.
    let real = concat!("Real", "Sets");
    for (want, n) in [(M4_RS, 7), (M6_RS, 4)] {
        let Some(file) = tree.corpus.get(want) else {
            continue;
        };
        let named = words(&file.raw).filter(|word| *word == real).count();
        if named != n {
            out.push(file.finding(
                M6_NAMES.id,
                0,
                format!("names {real} {named} times, where the design lists {n}"),
            ));
        }
    }
}

/// Whether `word` is among `text`'s words.
fn has_word(text: &str, word: &str) -> bool {
    words(text).any(|w| w == word)
}

/// `src`'s identifier-like words.
fn words(src: &str) -> impl Iterator<Item = &str> {
    src.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
}

/// `src` with the whitespace around each `::` removed (`super ::x` is `super::x`).
fn tight(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(at) = rest.find("::") {
        out.push_str(rest[..at].trim_end());
        out.push_str("::");
        rest = rest[at + 2..].trim_start();
    }
    out.push_str(rest);
    out
}

const M4_RS: &str = "crates/aterm-effects/src/aos_momentum/m4.rs";
const M4_TESTS_RS: &str = "crates/aterm-effects/src/aos_momentum/m4_tests.rs";
const M6_RS: &str = "crates/aterm-effects/src/aos_momentum/m6.rs";
const LEARN_RS: &str = "crates/aterm-lab/src/aos_learn.rs";
const LEARN_BENCH_RS: &str = "crates/aterm-lab/src/aos_learn_bench.rs";
const MOMENTUM_RS: &str = "crates/aterm-effects/src/aos_momentum.rs";

/// The files that may name m6 (design §13.3), by path: its consumers and tests, and this guard.
const M6_NAMERS: &[&str] = &[
    LEARN_RS,
    LEARN_BENCH_RS,
    M6_RS,
    "crates/aterm-effects/src/aos_momentum/m6_tests.rs",
    "crates/aterm-effects/src/aos_momentum/locks_tests.rs",
    "crates/aterm-census/src/guards/m6_names.rs",
    "crates/aterm-census/src/guards/m6_names_tests.rs",
];

/// `src`'s refusals under the workspace scan of m6's path, for the file at `path`.
fn scan_workspace_file(path: &str, src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let m6 = concat!("m", "6");
    if !src.contains(m6) && !src.contains("aos_momentum") {
        return out;
    }
    // Paths with the whitespace around `::` removed, each matched as whole words.
    let paths = tight(src);
    let word = |c: char| c.is_alphanumeric() || c == '_';
    for token in [concat!("aos_momentum::", "m6"), concat!("super::", "m6")] {
        for (i, _) in paths.match_indices(token) {
            if !paths[..i].ends_with(word) && !paths[i + token.len()..].starts_with(word) {
                out.push(format!("the path {token}"));
            }
        }
    }
    let in_crate = path.starts_with("crates/aterm-effects/src/");
    let mut flat = String::with_capacity(src.len());
    for (i, w) in src.split_whitespace().enumerate() {
        if i > 0 {
            flat.push(' ');
        }
        flat.push_str(w);
    }
    let mut rest = flat.as_str();
    while let Some(at) = rest.find("use ") {
        let item = &rest[at..];
        let led = !rest[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_');
        let end = item.find(';').unwrap_or(item.len());
        let body = &item[..end];
        let rooted = ["use super::", "use self::", "use crate::"]
            .iter()
            .any(|r| body.starts_with(r));
        if led && body.contains("aos_momentum") {
            if has_word(body, m6) {
                out.push(format!("{body}: names m6"));
            }
            if body.contains('*') {
                out.push(format!("{body}: a glob"));
            }
            if has_word(body, concat!("a", "s")) {
                out.push(format!("{body}: an `as` rename"));
            }
        } else if led && in_crate && rooted && has_word(body, m6) {
            out.push(format!("{body}: names m6"));
        }
        rest = &item[end..];
    }
    // In this crate, m6's own path however it is reached (`use super::*;`, then `m6::`).
    let bare = concat!("m", "6::");
    if in_crate
        && paths
            .match_indices(bare)
            .any(|(i, _)| !paths[..i].ends_with(word))
    {
        out.push(format!("the path {bare}"));
    }
    // The parent declares m6 and names it nowhere else, so it re-exports none of it.
    if path == MOMENTUM_RS {
        let squeezed: String = src.chars().filter(|c| !c.is_whitespace()).collect();
        if words(src).filter(|w| *w == m6).count() > squeezed.matches(concat!("mod", "m6;")).count()
        {
            out.push("aos_momentum.rs names m6 past its declaration".to_owned());
        }
    }
    out
}

/// The length of the item or statement `s` begins: to its first `;` outside brackets, the close of
/// its first block when `blocks` (an item's body ends it), or a closer of the block around it.
/// Strings, character literals and line comments are skipped.
fn item_end(s: &str, blocks: bool) -> usize {
    let b = s.as_bytes();
    let mut depth = 0_i32;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'\'' if b.get(i + 1) == Some(&b'\\') => {
                i += 3;
                while i < b.len() && b[i] != b'\'' {
                    i += 1;
                }
            }
            b'\'' if b.get(i + 2) == Some(&b'\'') => i += 2,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth < 0 || (blocks && depth == 0 && b[i] == b'}') {
                    return i + 1;
                }
            }
            b';' if depth == 0 => return i + 1,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// What a test gate covers in `s`, the text after it: an item (`fn`, `mod`, `impl`, `trait`,
/// `struct`, `enum`, `union`, `extern` or a macro) to its body's end; a statement, field or
/// expression past its own blocks (`if … {} else {}`, `S { … }.f()`) to its `;` or the block
/// around it.
fn gated_end(s: &str) -> usize {
    let mut rest = s;
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("pub(") {
            rest = r.split_once(')').map_or("", |(_, r)| r);
        } else if rest.starts_with("#[") {
            rest = &rest[bracketed(&rest[1..]).map_or(rest.len(), |n| n + 1)..];
        } else if rest.starts_with("//") {
            rest = rest.split_once('\n').map_or("", |(_, r)| r);
        } else {
            break;
        }
    }
    let stop = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c.is_whitespace()))
        .unwrap_or(rest.len());
    let item = rest[stop..].starts_with('!')
        || rest[..stop].split_whitespace().any(|w| {
            [
                "fn", "mod", "impl", "trait", "struct", "enum", "union", "extern",
            ]
            .contains(&w)
        });
    item_end(s, item)
}

/// The length of the bracketed text `s` begins (`[…]`), to its closer.
fn bracketed(s: &str) -> Option<usize> {
    let mut depth = 0_i32;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// The length of the attribute `s` begins (`[…]`, after its `#`) when it is a test gate:
/// `#[test]`, or a `cfg` whose predicate requires `test`.
fn test_gate(s: &str) -> Option<usize> {
    let len = bracketed(s)?;
    let attr: String = s[1..len - 1].split_whitespace().collect();
    let gate = attr == "test"
        || attr
            .strip_prefix("cfg(")
            .and_then(|p| p.strip_suffix(')'))
            .is_some_and(requires_test);
    gate.then_some(len)
}

/// Whether the `cfg` predicate `p` (whitespace removed) requires `test`: `test` itself, or an
/// `all(…)` one of whose arguments does. (`any(test, …)` and `not(test)` do not: m4's mints
/// are `any(test, debug_assertions, …)` items.)
fn requires_test(p: &str) -> bool {
    p == "test"
        || p.strip_prefix("all(")
            .and_then(|r| r.strip_suffix(')'))
            .is_some_and(|args| {
                let mut depth = 0_i32;
                let mut start = 0;
                let mut parts = Vec::new();
                for (i, c) in args.char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        ',' if depth == 0 => {
                            parts.push(&args[start..i]);
                            start = i + 1;
                        }
                        _ => {}
                    }
                }
                parts.push(&args[start..]);
                parts.into_iter().any(requires_test)
            })
}

/// The test code of the file at `path`: all of it in a test file (`*_tests.rs`, `tests.rs`,
/// under a `tests/` directory, or under `#![cfg(test)]`), and elsewhere what each test gate
/// covers and each doctest's lines.
fn test_parts<'s>(path: &str, src: &'s str) -> Vec<&'s str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.ends_with("_tests.rs")
        || name == "tests.rs"
        || path.contains("/tests/")
        || src
            .match_indices("#![")
            .any(|(at, _)| test_gate(&src[at + 2..]).is_some())
    {
        return vec![src];
    }
    let mut out = Vec::new();
    let mut fenced = false;
    for line in src.lines() {
        let doc = line.trim_start();
        let Some(text) = doc.strip_prefix("///").or_else(|| doc.strip_prefix("//!")) else {
            fenced = false;
            continue;
        };
        if text.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if fenced {
            out.push(line);
        }
    }
    for (at, _) in src.match_indices("#[") {
        let attr = &src[at + 1..];
        if let Some(len) = test_gate(attr) {
            let item = &attr[len..];
            out.push(&item[..gated_end(item)]);
        }
    }
    out
}

/// Whether `text` holds the flag `flag` itself, not a longer one (`--pairs` is another).
fn has_flag(text: &str, flag: &str) -> bool {
    text.match_indices(flag).any(|(i, _)| {
        !text[i + flag.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '-')
    })
}

/// `text`'s refusals under the confinement of the real sets' capability, the mints and the real
/// pairs' flag (design §13.3), for the workspace file at `path`. The flag is refused in every
/// file but the non-test code of the lab's learner and runner, test sources included however
/// they are named or gated.
fn scan_confinement(path: &str, text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let flag = concat!("--", "pair");
    if !text.contains(concat!("Real", "Sets"))
        && !text.contains(concat!("mint_", "real"))
        && !text.contains(concat!("mint_", "practice"))
        && !text.contains(flag)
    {
        return out;
    }
    let tests = test_parts(path, text);
    let in_tests = |word: &str| tests.iter().any(|t| has_word(t, word));
    let rules: [(&str, &[&str], &[&str]); 3] = [
        // Named in m4.rs's, m6.rs's and aos_learn.rs's non-test items alone.
        (concat!("Real", "Sets"), &[M4_RS, M6_RS, LEARN_RS], &[]),
        // Declared in m4.rs and called in m6.rs's non-test items alone.
        (concat!("mint_", "real"), &[M4_RS, M6_RS], &[]),
        // Those, and the tests of the mints in m4_tests.rs.
        (
            concat!("mint_", "practice"),
            &[M4_RS, M6_RS],
            &[M4_TESTS_RS],
        ),
    ];
    for (word, items, files) in rules {
        let named = if files.contains(&path) {
            false
        } else if items.contains(&path) {
            in_tests(word)
        } else {
            has_word(text, word)
        };
        if named {
            out.push(format!("names {word}"));
        }
    }
    let entry = [LEARN_RS, LEARN_BENCH_RS].contains(&path);
    if tests.iter().any(|t| has_flag(t, flag)) || (!entry && has_flag(text, flag)) {
        out.push(format!("{flag} outside the lab's learner and runner"));
    }
    out
}

#[cfg(test)]
#[path = "m6_names_tests.rs"]
mod tests;
