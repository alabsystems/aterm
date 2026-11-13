// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! closure-anchor: an action's only anchor sits in code the source-spec closure links.

use std::collections::BTreeSet;

use super::{Finding, Rule, SourceFile, Tree};

pub(super) const CLOSURE_ANCHOR: Rule = Rule {
    id: "closure-anchor",
    why: "aterm-models' `spec_xref_closure` (obligation 3: every action of a bound machine is \
          bound or waived) collects the anchors of the anchored crates linked as libraries, \
          where test code does not exist: a `refines` or a `spec_unmodeled` in a test module, in \
          a file only a test-gated `mod` loads, under a `cfg_attr` whose predicate needs `test` \
          (`test`, `all(unix, test)`), or in a target no dependent links (a crate's `tests/`, \
          `benches/`, `examples/`, `src/main.rs`, `src/bin/` or `build.rs`) binds or waives \
          nothing there, so the crate's own tests pass while the closure fails on main. Carry \
          the action's anchor on a non-test item beside the code it describes (a waiver on a \
          `#[doc(hidden)] pub fn <machine>_environment() {}` the closure test names)",
    scope: &[],
    check: closure_anchor,
};

/// The anchor attributes, as every crate spells them.
const ANCHORS: [&str; 2] = [
    "aterm_spec_anchor::refines",
    "aterm_spec_anchor::spec_unmodeled",
];

/// The crates whose own tests are the attributes' tests: their fixtures name fixture machines.
const ATTRIBUTE_CRATES: [&str; 2] = ["crates/aterm-spec-anchor/", "crates/aterm-spec-macros/"];

/// A crate's targets no dependent links: its integration tests, benches, examples, binaries and
/// build script.
const UNLINKED_TARGETS: [&str; 6] = [
    "tests/",
    "benches/",
    "examples/",
    "src/main.rs",
    "src/bin/",
    "build.rs",
];

/// One anchor naming a machine and an action.
struct Anchor<'a> {
    file: &'a SourceFile,
    line: usize,
    /// The machine as the anchor spells it.
    machine: &'a str,
    /// The machine as the closure resolves it: lower case, `_` dropped (`xref::machine_matches`).
    key: String,
    action: &'a str,
    /// Compiled into the library the closure links.
    linked: bool,
}

fn closure_anchor(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let mut anchors = Vec::new();
    for file in tree.corpus.under(&["crates/"]) {
        if ATTRIBUTE_CRATES.iter().any(|c| file.rel.starts_with(c)) || !file.mentions(&ANCHORS) {
            continue;
        }
        let Some((_, inner)) = crate_of(&file.rel) else {
            continue;
        };
        let unlinked = UNLINKED_TARGETS.iter().any(|t| inner.starts_with(t))
            || tree.corpus.test_code().holds(&file.rel);
        anchors.extend(anchors_in(file, unlinked));
    }
    let covered: BTreeSet<(&str, &str)> = anchors
        .iter()
        .filter(|a| a.linked)
        .map(|a| (a.key.as_str(), a.action))
        .collect();
    for a in anchors.iter().filter(|a| !a.linked) {
        if !covered.contains(&(a.key.as_str(), a.action)) {
            out.push(a.file.finding(
                CLOSURE_ANCHOR.id,
                a.line,
                format!(
                    "`{}`'s `{}` is anchored only where the source-spec closure never links it \
                     (test code, a binary or a build script)",
                    a.machine, a.action
                ),
            ));
        }
    }
}

/// The anchors `file` holds that name a machine and an action, each with whether it compiles
/// into the library (`unlinked`: the whole file is test code).
fn anchors_in(file: &SourceFile, unlinked: bool) -> Vec<Anchor<'_>> {
    let shipped: BTreeSet<usize> = if unlinked {
        BTreeSet::new()
    } else {
        file.shipped_lines().map(|l| l.no).collect()
    };
    let lexed = file.lexed();
    let mut found = Vec::new();
    for token in ANCHORS {
        for (at, _) in lexed.code.match_indices(token) {
            let after = at + token.len();
            let Some(args) = arguments(&lexed.code[after..]) else {
                continue;
            };
            let args = &lexed.text[after..after + args];
            let (Some(machine), Some(action)) = (value(args, "machine"), value(args, "action"))
            else {
                continue;
            };
            if machine.is_empty() || action.is_empty() {
                continue;
            }
            let line = lexed.code[..at].matches('\n').count() + 1;
            found.push(Anchor {
                file,
                line,
                machine,
                key: machine
                    .chars()
                    .filter(|c| *c != '_')
                    .flat_map(char::to_lowercase)
                    .collect(),
                action,
                linked: shipped.contains(&line) && !test_gated(&lexed.code[..at]),
            });
        }
    }
    found
}

/// The length of the bracketed argument list `code` starts with (blanks before it allowed),
/// through its closing delimiter.
fn arguments(code: &str) -> Option<usize> {
    let open = code.len() - code.trim_start().len();
    let (open_ch, close_ch) = match code[open..].chars().next()? {
        '(' => ('(', ')'),
        '[' => ('[', ']'),
        '{' => ('{', '}'),
        _ => return None,
    };
    let mut depth = 0usize;
    for (at, c) in code[open..].char_indices() {
        if c == open_ch {
            depth += 1;
        } else if c == close_ch {
            depth -= 1;
            if depth == 0 {
                return Some(open + at + 1);
            }
        }
    }
    None
}

/// The string `key = "…"` gives in `args`.
fn value<'a>(args: &'a str, key: &str) -> Option<&'a str> {
    args.match_indices(key).find_map(|(at, _)| {
        let edge = args[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        let rest = args[at + key.len()..].trim_start().strip_prefix('=')?;
        let rest = rest.trim_start().strip_prefix('"')?;
        edge.then(|| rest.split('"').next()).flatten()
    })
}

/// Does the code `before` an anchor end inside a `cfg_attr` whose predicate holds only under
/// `test` (`test`, `all(unix, test)`), read as the census reads a `cfg` that proves an item
/// test-only?
fn test_gated(before: &str) -> bool {
    let item_start = before.rfind([';', '}']).map_or(0, |p| p + 1);
    let slice = &before[item_start..];
    super::hits(slice, "cfg_attr", super::Bound::Word).any(|at| {
        let Some(inside) = super::after_ws(&slice[at + "cfg_attr".len()..], "(") else {
            return false;
        };
        let mut depth = 0usize;
        let mut end = None;
        for (i, c) in inside.char_indices() {
            match c {
                '(' => depth += 1,
                // The `cfg_attr` closed before the anchor: the anchor is not its attribute.
                ')' if depth == 0 => return false,
                ')' => depth -= 1,
                ',' if depth == 0 && end.is_none() => end = Some(i),
                _ => {}
            }
        }
        end.is_some_and(|end| crate::cfg_pred_entails_unshipped(&inside[..end]))
    })
}

/// `crates/<name>/` and the path inside it.
fn crate_of(rel: &str) -> Option<(&str, &str)> {
    let rest = rel.strip_prefix("crates/")?;
    let (name, inner) = rest.split_once('/')?;
    Some((&rel[.."crates/".len() + name.len() + 1], inner))
}

#[cfg(test)]
#[path = "closure_anchor_tests.rs"]
mod tests;
