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
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    /// A literal naming the keg, or Homebrew's link to it in its `bin`, is found in shipped
    /// and test code alike; prose about it, a scratch keg a test lays under its own
    /// directory, another Homebrew path a row lists as a fallback, and this crate's own tables
    /// are not.
    #[test]
    fn no_source_names_homebrews_rustup_keg() {
        let src = "pub const HOMEBREW_RUSTUP: &str = \"/opt/homebrew/opt/rustup/bin/rustup\";\n\
                   // Homebrew's keg (/opt/homebrew/opt/rustup) is never looked at.\n\
                   const SHIM: &str = \"/usr/local/opt/rustup/bin\";\n\
                   let zsh = \"/opt/homebrew/bin/zsh\";\n\
                   let link = Path::new(\"/opt/homebrew/bin/rustup\");\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                       fn t(tmp: &Path) { tmp.join(\"opt/homebrew/opt/rustup/bin/rustup\"); }\n\
                       fn u() { let c = \"/x/Cellar/rustup/1.0\"; }\n\
                   }\n";
        let found = run(
            &RUSTUP_KEG,
            &[
                ("crates/aterm-toolchain/src/rustup.rs", src),
                ("crates/aterm-census/src/guards/rustup_keg.rs", src),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-toolchain/src/rustup.rs:1",
                "crates/aterm-toolchain/src/rustup.rs:3",
                "crates/aterm-toolchain/src/rustup.rs:5",
                "crates/aterm-toolchain/src/rustup.rs:9",
            ]
        );
    }
}
