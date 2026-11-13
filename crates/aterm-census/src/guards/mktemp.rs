// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! checked-mktemp: a scratch directory comes from a checked `mktemp`.

use super::rules::{CENSUS_CRATE, PRODUCT};
use super::{Finding, Rule, Tree};

pub(super) const CHECKED_MKTEMP: Rule = Rule {
    id: "checked-mktemp",
    why: "`$(cd \"$(mktemp -d …)\" && pwd -P)` is the caller's directory with status 0 when mktemp \
          fails (bash 3.2's `cd \"\"`), and the EXIT trap's `rm -rf` then deletes it; make the \
          directory and enter it in two checked statements",
    scope: &[],
    check: checked_mktemp,
};

fn checked_mktemp(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if file.rel.starts_with(CENSUS_CRATE) || !file.mentions(&["$(mktemp", "`mktemp"]) {
            continue;
        }
        for line in file.lines() {
            if fused_mktemp(line.text) {
                out.push(file.finding(CHECKED_MKTEMP.id, line.no, DETAIL.to_string()));
            }
        }
    }
    for script in tree.corpus.scripts() {
        if !script.raw.contains("$(mktemp") && !script.raw.contains("`mktemp") {
            continue;
        }
        for (no, line) in script.lines() {
            if fused_mktemp(line) {
                out.push(script.finding(CHECKED_MKTEMP.id, no, DETAIL.to_string()));
            }
        }
    }
}

const DETAIL: &str = "a scratch directory from an unchecked `$(cd \"$(mktemp …)\")`";

/// `$(cd` or `` `cd ``, blanks (and optional `cd` flags), an optional `"`, then `$(mktemp` or
/// `` `mktemp ``, before any shell `#` comment on the line.
fn fused_mktemp(line: &str) -> bool {
    let code = strip_shell_comment(line);
    ["$(cd", "`cd"].iter().any(|cd_open| {
        code.match_indices(cd_open).any(|(at, _)| {
            let rest = &code[at + cd_open.len()..];
            let mut after = rest.trim_start_matches([' ', '\t']);
            if after.len() == rest.len() {
                return false;
            }
            while let Some(flag_rest) = after
                .strip_prefix("--")
                .or_else(|| after.strip_prefix("-P"))
                .or_else(|| after.strip_prefix("-L"))
            {
                let trimmed = flag_rest.trim_start_matches([' ', '\t']);
                if trimmed.len() == flag_rest.len() {
                    break;
                }
                after = trimmed;
            }
            let after = after.strip_prefix('"').unwrap_or(after);
            ["$(mktemp", "`mktemp"].iter().any(|mk| {
                after
                    .strip_prefix(mk)
                    .is_some_and(|r| r.is_empty() || r.starts_with([' ', '\t', ')', '`']))
            })
        })
    })
}

fn strip_shell_comment(line: &str) -> &str {
    let trimmed = line.trim_start_matches([' ', '\t']);
    if trimmed.starts_with('#') {
        return "";
    }
    for (i, _) in line.match_indices('#') {
        if i > 0 && matches!(line.as_bytes()[i - 1], b' ' | b'\t') {
            return &line[..i];
        }
    }
    line
}

#[cfg(test)]
#[path = "mktemp_tests.rs"]
mod tests;
