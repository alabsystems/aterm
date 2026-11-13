// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! generated-seal: a generated file changes only when its generator reruns.
//!
//! `.gitattributes` names the generated files (`linguist-generated`, which GitHub also folds
//! in a diff and `git check-attr` reports), and each one carries its generator's seal
//! ([`crate::seal`]): a line naming the generator and a digest of the file as the generator
//! wrote it. A hand edit, such as a comment prune, breaks the seal; the next run of the
//! generator would revert it, and in the meantime the edit can cut something the file means
//! (the libc oracle's sibling marker, a cell's `!! … ABSENT` lines). The rule needs no
//! generator to run, so it holds a generator that takes a network or a cross toolchain too.
//!
//! Three findings: a declared file without a seal or whose seal no longer holds; a
//! `linguist-generated` row that names no file (a moved output); and a Rust file that seals
//! itself but is not declared, so a prune that took its whole header would go unseen.

use super::{Finding, Rule, Tree};
use crate::seal::{self, HEAD_LINES, MARK, Seal};

pub(super) const GENERATED_SEAL: Rule = Rule {
    id: "generated-seal",
    why: "a generated file is changed by rerunning its generator, never by hand: the next run \
          reverts a hand edit (a comment prune), and the edit can cut what the file means; \
          every file .gitattributes marks linguist-generated carries its generator's seal, \
          and the seal holds",
    scope: &[],
    check: generated_seal,
};

/// The file every declaration is read from.
const ATTRIBUTES: &str = ".gitattributes";

/// The attribute that declares a file generated.
const ATTRIBUTE: &str = "linguist-generated";

fn generated_seal(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let rows = tree
        .text(ATTRIBUTES)
        .map(|t| declarations(&t))
        .unwrap_or_default();
    let mut declared = vec![false; tree.corpus.paths().len()];
    for row in &rows {
        let mut named = false;
        for (at, rel) in tree.corpus.paths().iter().enumerate() {
            if matches(&row.pattern, rel) {
                declared[at] = row.set;
                named = true;
            }
        }
        if row.set && !named {
            out.push(Finding {
                rule: GENERATED_SEAL.id,
                path: ATTRIBUTES.to_string(),
                line: row.line,
                detail: format!(
                    "`{}` marks no file {ATTRIBUTE}: the generator moved its output, so point \
                     the row at where it writes now",
                    row.pattern
                ),
            });
        }
    }
    for (rel, declared) in tree.corpus.paths().iter().zip(declared) {
        let rust = rel.ends_with(".rs") && !rel.starts_with("vendor/");
        if !declared && !rust {
            continue;
        }
        let Some(body) = tree.text(rel) else {
            continue;
        };
        let finding = |line: usize, detail: String| Finding {
            rule: GENERATED_SEAL.id,
            path: rel.clone(),
            line,
            detail,
        };
        match seal::check(&body) {
            Seal::Absent if declared => out.push(finding(
                1,
                format!(
                    "{ATTRIBUTES} marks it {ATTRIBUTE}, but no `{MARK} … seal` line sits in \
                     its first {HEAD_LINES} lines: its generator writes one \
                     (aterm_census::seal::line) and stamps it (seal::stamp)"
                ),
            )),
            Seal::Absent => {}
            Seal::Broken { line, sealed, now } => out.push(finding(
                line,
                format!(
                    "edited since its generator wrote it (sealed {sealed}, now {now}): rerun \
                     the generator this line names, or revert the edit"
                ),
            )),
            Seal::Holds { line } if !declared => out.push(finding(
                line,
                format!(
                    "seals itself, but {ATTRIBUTES} does not mark it {ATTRIBUTE}: declare it, \
                     so a prune of its whole header is seen"
                ),
            )),
            Seal::Holds { .. } => {}
        }
    }
}

/// One `.gitattributes` row that sets or unsets [`ATTRIBUTE`].
struct Declaration {
    pattern: String,
    set: bool,
    /// 1-based.
    line: usize,
}

/// The rows of `attributes` that name [`ATTRIBUTE`], in file order (a later row wins).
fn declarations(attributes: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    for (index, row) in attributes.lines().enumerate() {
        let mut words = row.split_whitespace();
        let Some(pattern) = words.next().filter(|p| !p.starts_with('#')) else {
            continue;
        };
        for word in words {
            let set = match word {
                ATTRIBUTE => true,
                _ if word.strip_prefix(ATTRIBUTE) == Some("=true") => true,
                _ if word.strip_prefix(['-', '!']) == Some(ATTRIBUTE) => false,
                _ if word.strip_prefix(ATTRIBUTE) == Some("=false") => false,
                _ => continue,
            };
            out.push(Declaration {
                pattern: pattern.to_string(),
                set,
                line: index + 1,
            });
        }
    }
    out
}

/// Does the `.gitattributes` pattern `pattern` match the root-relative path `rel`? A pattern
/// with no `/` matches the last component at any depth; one with a `/` matches from the root,
/// where `**` spans any number of components and `*` and `?` stay within one.
fn matches(pattern: &str, rel: &str) -> bool {
    let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
    if !pattern.contains('/') {
        return rel
            .rsplit('/')
            .next()
            .is_some_and(|name| component(pattern, name));
    }
    components(pattern, rel)
}

fn split_component(path: &str) -> Option<(&str, &str)> {
    match path.split_once('/') {
        Some((head, tail)) => Some((head, tail)),
        None if !path.is_empty() => Some((path, "")),
        None => None,
    }
}

fn components(want: &str, have: &str) -> bool {
    match (split_component(want), split_component(have)) {
        (None, None) => true,
        (Some(("**", rest)), _) => {
            components(rest, have)
                || split_component(have).is_some_and(|(_, tail)| components(want, tail))
        }
        (Some((w, rest)), Some((h, tail))) => component(w, h) && components(rest, tail),
        _ => false,
    }
}

/// `*` and `?` glob match of one path component.
fn component(want: &str, have: &str) -> bool {
    fn go(want: &[u8], have: &[u8]) -> bool {
        match want.split_first() {
            None => have.is_empty(),
            Some((b'*', rest)) => (0..=have.len()).any(|skip| go(rest, &have[skip..])),
            Some((b'?', rest)) => !have.is_empty() && go(rest, &have[1..]),
            Some((w, rest)) => have.first() == Some(w) && go(rest, &have[1..]),
        }
    }
    go(want.as_bytes(), have.as_bytes())
}

#[cfg(test)]
#[path = "generated_tests.rs"]
mod tests;
