// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! rustup-keg: no source looks at Homebrew's rustup, by its keg or by the link in Homebrew's
//! `bin`.

use super::rules::{CENSUS_CRATE, PRODUCT};
use super::{Finding, Rule, Tree};

pub(super) const RUSTUP_KEG: Rule = Rule {
    id: "rustup-keg",
    why: "rustup is the `rustup` row of aterm_uds::host::TOOLS, found by that row's resolver alone \
          ($ATERM_TEST_RUSTUP, the stamped copy `verify provision` places, then $PATH); a path into \
          Homebrew's rustup keg, or to the link Homebrew lays for it in its `bin`, is a second \
          finder, answering a copy the row never provided",
    scope: &[],
    check: rustup_keg,
};

/// Homebrew's rustup as a path spells it: its keg's `opt` link under either Homebrew prefix or
/// Linuxbrew's, its Cellar, and the link Homebrew lays in each prefix's `bin` (a person's
/// Homebrew rustup is reached only as an entry of `$PATH`).
const KEGS: &[&str] = &[
    "/opt/homebrew/opt/rustup",
    "/usr/local/opt/rustup",
    "/.linuxbrew/opt/rustup",
    "Cellar/rustup",
    "/opt/homebrew/bin/rustup",
    "/usr/local/bin/rustup",
    "/.linuxbrew/bin/rustup",
];

fn rustup_keg(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if file.rel.starts_with(CENSUS_CRATE) || !file.mentions(KEGS) {
            continue;
        }
        for line in file.lines() {
            if let Some(keg) = KEGS.iter().find(|keg| line.text.contains(*keg)) {
                out.push(file.finding(
                    RUSTUP_KEG.id,
                    line.no,
                    format!("names Homebrew's rustup (`{keg}`)"),
                ));
            }
        }
    }
}

#[cfg(test)]
#[path = "rustup_keg_tests.rs"]
mod tests;
