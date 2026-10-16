// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! checked-mktemp: a scratch directory comes from a CHECKED `mktemp`.
//!
//! Under /bin/bash 3.2 `cd ""` succeeds and stays put, so the fused
//! `T="$(cd "$(mktemp -d …)" && pwd -P)"` sets T to the caller's directory with
//! status 0 when mktemp fails (a TMPDIR that is gone, a full disk), and the EXIT
//! trap's `rm -rf "$T"` then deletes that directory — the checkout, when a suite
//! runs from the repository root. Write two statements, each checked. Read over
//! every script and every Rust file's text view (a script in a Rust string
//! counts); a match after a `#` on its line is a comment.

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
        if file.rel.starts_with(CENSUS_CRATE) || !file.mentions(&["$(mktemp"]) {
            continue;
        }
        for line in file.lines() {
            if fused_mktemp(line.text) {
                out.push(file.finding(CHECKED_MKTEMP.id, line.no, DETAIL.to_string()));
            }
        }
    }
    for script in &tree.corpus.scripts {
        if !script.raw.contains("$(mktemp") {
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

/// `$(cd`, blanks, an optional `"`, then `$(mktemp`, before any `#` on the line.
fn fused_mktemp(line: &str) -> bool {
    let code = line.split('#').next().unwrap_or(line);
    code.match_indices("$(cd").any(|(at, _)| {
        let rest = &code[at + "$(cd".len()..];
        let after = rest.trim_start_matches([' ', '\t']);
        after.len() < rest.len()
            && after
                .strip_prefix('"')
                .unwrap_or(after)
                .starts_with("$(mktemp")
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    #[test]
    fn a_scratch_directory_comes_from_a_checked_mktemp() {
        let script = "#!/bin/bash\n\
                      T=\"$(cd \"$(mktemp -d \"${TMPDIR:-/tmp}/x.XXXXXX\")\" && pwd -P)\"\n\
                      U=$(cd $(mktemp -d) && pwd)\n\
                      # T=\"$(cd \"$(mktemp -d)\" && pwd -P)\" is the shape to avoid\n\
                      T=\"$(mktemp -d)\" || exit 1\ncd \"$T\" || exit 1\n";
        let rust = "const SCRIPT: &str = r#\"\nT=\"$(cd \"$(mktemp -d)\" && pwd -P)\"\n\"#;\n\
                    // T=\"$(cd \"$(mktemp -d)\")\"\nfn f() {}\n";
        let found = run(
            &CHECKED_MKTEMP,
            &[
                ("tools/test-x.sh", script),
                ("crates/aterm-verify/tests/t.rs", rust),
                ("crates/aterm-census/src/guards/mktemp.rs", rust),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-verify/tests/t.rs:2",
                "tools/test-x.sh:2",
                "tools/test-x.sh:3",
            ]
        );
    }
}
