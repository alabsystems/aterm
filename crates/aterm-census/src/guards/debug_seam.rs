// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! debug-seam: a debug-only seam's variable is named by no release build.
//!
//! A seam such as `ATERM_DEBUG_NETPLACE` (`aterm_netplace::facts::SEAM`) scripts what the app
//! believes of the Mac (where it is on the network), so the lab can put one instance on a phone
//! hotspot. A release build that read it would let an environment variable lie to every reporter
//! and to the vault's admission, so every mention of it sits in code a release build never
//! compiles: under a `cfg` that entails `debug_assertions` or `test` (`all(debug_assertions,
//! not(test))`, `any(test, debug_assertions)`), never `dev_seam!` (whose `dev-seams` feature a
//! release build can turn on). The rule reads mentions, not reads, since every way to read the
//! variable starts at one (`env::var_os(SEAM)`, an imported `var_os(SEAM)`, `vars_os()` searched
//! for the name, a wrapper handed `facts::SEAM`, a `use` or re-export of it): the seam's quoted
//! name, and each `const` or `static` bound to it, by its bare name in its own crate or in a file
//! that names that crate, and by its module's path (`facts::SEAM`) anywhere. A binding itself is
//! no mention. It fails a mention a release build compiles, and a seam it finds no mention of
//! outside its bindings (renamed: name it here again).
//!
//! `ATERM_TEST_NEGATIVE_CONTROL` is the other seam: it turns a negative control on for a whole
//! lab process (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §9), restoring an old trust rule (the
//! gap rule that read an omitted seat as revoked, a batch read as settled before it is). Its one
//! reader is `aterm_types::negative_control::on`, under `cfg(any(test, debug_assertions))`; a
//! lab lane that sets it on a child names it under the same kind of `cfg`.
//!
//! `ATERM_DEBUG_TOKEN_RUN_SPARE` makes a debug build's token wrapper hand Claude Code a second,
//! unread socket holding the token (`aterm_agent::harness::accounts_token_run::SPARE_SEAM`): the
//! lab's probe of whether a descriptor left open in a session reaches Claude Code's tool children
//! (DESIGN-claude-setup-token S9 lane C). A release build that read it would leave the token open,
//! unread, in a session's descriptors.

use super::rules::{CENSUS_CRATE, PRODUCT};
use super::{Finding, Rule, Tree};
use crate::lex::Lexed;

/// The seams only a debug build may read: each variable's name.
const DEBUG_ONLY: &[&str] = &[
    "ATERM_DEBUG_NETPLACE",
    "ATERM_TEST_NEGATIVE_CONTROL",
    "ATERM_DEBUG_TOKEN_RUN_SPARE",
];

pub(super) const DEBUG_SEAM: Rule = Rule {
    id: "debug-seam",
    why: "a debug-only seam (ATERM_DEBUG_NETPLACE scripts where this Mac is on the network; \
          ATERM_TEST_NEGATIVE_CONTROL restores an old trust rule for a lab process; \
          ATERM_DEBUG_TOKEN_RUN_SPARE leaves an unread token socket open in a session) named by a \
          release build lets an environment variable lie to every reporter and to the vault's \
          admission; name it only under a cfg that entails debug_assertions or test (never \
          dev_seam!, whose dev-seams feature a release build can turn on)",
    scope: &[
        "crates/aterm-netplace/src/facts.rs",
        "crates/aterm-types/src/negative_control.rs",
        "crates/aterm-agent/src/harness/accounts_token_run.rs",
    ],
    check: debug_seam,
};

fn debug_seam(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let bound = bound_names(tree);
    let test_code = tree.corpus.test_code();
    let mut seen = vec![false; DEBUG_ONLY.len()];
    let words: Vec<&str> = DEBUG_ONLY
        .iter()
        .copied()
        .chain(bound.iter().map(|b| b.name.as_str()))
        .collect();
    for file in tree.corpus.under(PRODUCT) {
        if file.rel.starts_with(CENSUS_CRATE)
            || test_code.holds(&file.rel)
            || !file.mentions(&words)
        {
            continue;
        }
        let lexed = file.lexed();
        let compiled = crate::lock_order::unmasked_lines(&lexed.code, &release_never_compiles);
        for (line, seam) in mentions(&file.rel, lexed, &bound) {
            seen[seam] = true;
            if compiled.get(line).copied().unwrap_or(true) {
                out.push(file.finding(
                    DEBUG_SEAM.id,
                    line + 1,
                    format!(
                        "names `{}` in code a release build compiles",
                        DEBUG_ONLY[seam]
                    ),
                ));
            }
        }
    }
    for (seam, seen) in DEBUG_ONLY.iter().zip(seen) {
        if !seen {
            out.push(Finding {
                rule: DEBUG_SEAM.id,
                path: String::new(),
                line: 0,
                detail: format!(
                    "no mention of `{seam}` was found outside its bindings, so the rule holds \
                     nothing of it (renamed? name its variable here again)"
                ),
            });
        }
    }
}

/// Each line (0-based) of the file `rel`, lexed as `lexed`, that names a seam of [`DEBUG_ONLY`],
/// with the seam's index: its quoted name in code, or a constant of `bound` (bare in its own
/// crate or in a file whose code names that crate, by its module's path anywhere). A line that
/// binds a seam is none.
fn mentions(rel: &str, lexed: &Lexed, bound: &[Bound]) -> Vec<(usize, usize)> {
    let line_of = |at: usize| lexed.code[..at].matches('\n').count();
    let mut out = Vec::new();
    for (seam, name) in DEBUG_ONLY.iter().enumerate() {
        let quoted = format!("\"{name}\"");
        out.extend(
            lexed
                .text
                .match_indices(&quoted)
                .map(|(at, _)| (line_of(at), seam)),
        );
    }
    for b in bound {
        let Some(seam) = DEBUG_ONLY.iter().position(|seam| *seam == b.seam) else {
            continue;
        };
        let bare = rel.starts_with(&b.krate) || names(&lexed.code, &b.crate_ident);
        let qualified = format!("{}::", b.module);
        for (at, _) in lexed.code.match_indices(&b.name) {
            let before = &lexed.code[..at];
            if !whole_word(&lexed.code, at, b.name.len()) {
                continue;
            }
            let by_path = before
                .strip_suffix(&qualified)
                .is_some_and(|head| !head.ends_with(is_ident));
            if bare || by_path {
                out.push((line_of(at), seam));
            }
        }
    }
    out.retain(|(line, _)| !bound.iter().any(|b| b.file == rel && b.line == *line));
    out.sort_unstable();
    out.dedup();
    out
}

/// A `const` or `static` bound to a seam of [`DEBUG_ONLY`] (`pub const SEAM: &str =
/// "ATERM_DEBUG_NETPLACE";`).
struct Bound {
    /// Its name.
    name: String,
    /// The seam.
    seam: &'static str,
    /// The file that binds it, and the line (0-based) that does.
    file: String,
    line: usize,
    /// The module that file is (`facts`), as another file's path names the constant.
    module: String,
    /// The crate's directory (`crates/aterm-netplace/`) and its name in a path
    /// (`aterm_netplace`).
    krate: String,
    crate_ident: String,
}

/// Every `const` or `static` a shipped file binds to a seam of [`DEBUG_ONLY`].
fn bound_names(tree: &Tree<'_>) -> Vec<Bound> {
    let mut out = Vec::new();
    for file in tree.corpus.under(PRODUCT) {
        if file.rel.starts_with(CENSUS_CRATE) || !file.mentions(DEBUG_ONLY) {
            continue;
        }
        for line in file.lines() {
            let Some((head, value)) = line.text.split_once('=') else {
                continue;
            };
            let Some(seam) = DEBUG_ONLY
                .iter()
                .find(|seam| value.trim().trim_end_matches(';').trim() == format!("\"{seam}\""))
            else {
                continue;
            };
            let Some(name) = ["const ", "static "]
                .iter()
                .find_map(|word| {
                    head.match_indices(word)
                        .find(|(at, _)| !head[..*at].ends_with(is_ident))
                        .map(|(at, _)| &head[at + word.len()..])
                })
                .and_then(|rest| {
                    rest.trim_start()
                        .trim_start_matches("mut ")
                        .split(':')
                        .next()
                })
                .map(str::trim)
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
            let (krate, crate_ident) = crate_of(&file.rel);
            out.push(Bound {
                name: name.to_owned(),
                seam,
                file: file.rel.clone(),
                line: line.no - 1,
                module: module_of(&file.rel),
                krate,
                crate_ident,
            });
        }
    }
    out
}

/// The crate a source file is in: its directory (`crates/aterm-netplace/`) and its name in a
/// path (`aterm_netplace`).
pub(super) fn crate_of(rel: &str) -> (String, String) {
    let mut parts = rel.split('/');
    let (top, name) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    (format!("{top}/{name}/"), name.replace('-', "_"))
}

/// The module a source file is (`crates/aterm-netplace/src/facts.rs` is `facts`; a `mod.rs` its
/// directory's; a crate root, `lib.rs` or `main.rs`, its crate's, `-` spelled `_`).
pub(super) fn module_of(rel: &str) -> String {
    let mut parts = rel.trim_end_matches(".rs").rsplit('/');
    let stem = parts.next().unwrap_or_default();
    match stem {
        "mod" => parts.next().unwrap_or_default().to_owned(),
        "lib" | "main" => {
            let _src = parts.next();
            parts.next().unwrap_or_default().replace('-', "_")
        }
        stem => stem.to_owned(),
    }
}

/// Whether `c` can be part of an identifier.
pub(super) fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether the `len` bytes of `text` at `at` are a whole word (`SEAM`; not `SEAMS` or
/// `NO_SEAM`).
pub(super) fn whole_word(text: &str, at: usize, len: usize) -> bool {
    !text[..at].ends_with(is_ident) && !text[at + len..].starts_with(is_ident)
}

/// Whether `text` names the identifier `name` as a whole word.
pub(super) fn names(text: &str, name: &str) -> bool {
    text.match_indices(name)
        .any(|(at, _)| whole_word(text, at, name.len()))
}

/// Whether `attr` (`#[cfg(…)]`) gates what follows out of every release build: its predicate
/// entails `debug_assertions` or `test`.
pub(super) fn release_never_compiles(attr: &str) -> bool {
    attr.strip_prefix("#[cfg(")
        .and_then(|rest| rest.strip_suffix(")]"))
        .is_some_and(entails_debug_or_test)
}

/// Whether the `cfg` predicate `pred` can hold only in a debug or test build: `debug_assertions`
/// or `test` itself, an `all` with one such child, an `any` whose every child is one. A `not`, a
/// feature or a platform proves nothing.
fn entails_debug_or_test(pred: &str) -> bool {
    let pred = pred.trim();
    if pred == "test" || pred == "debug_assertions" {
        return true;
    }
    for (head, every) in [("all(", false), ("any(", true)] {
        if let Some(inner) = pred
            .strip_prefix(head)
            .and_then(|rest| rest.strip_suffix(')'))
        {
            let children = crate::split_top_level_commas(inner);
            if children.is_empty() {
                return false;
            }
            return if every {
                children.iter().all(|child| entails_debug_or_test(child))
            } else {
                children.iter().any(|child| entails_debug_or_test(child))
            };
        }
    }
    false
}

#[cfg(test)]
#[path = "debug_seam_tests.rs"]
mod tests;
