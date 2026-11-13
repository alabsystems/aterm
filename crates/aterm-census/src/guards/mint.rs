// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! launcher-mint: ensures `root_authority` and the `launcher-mint` feature are only used in launcher crates outside tests.

use super::rules::PRODUCT;
use super::{Bound, Finding, Rule, Tree, has};

/// The trusted launchers: the only crates whose shipped code mints.
const LAUNCHERS: &[&str] = &["crates/aterm-gui/", "crates/aterm-cli/"];
/// The mint's name, and the crate and feature that seal it.
const MINT: &str = "root_authority";
const CAP: &str = "aterm-cap";
const FEATURE: &str = "launcher-mint";

pub(super) const LAUNCHER_MINT: Rule = Rule {
    id: "launcher-mint",
    why: "the root-authority mint makes every capability, and cargo's feature unification puts \
          it within reach of every crate in the shipped build once a launcher enables \
          `launcher-mint`: only aterm-gui and aterm-cli may name it or enable the feature \
          outside tests",
    scope: &[
        "crates/aterm-cap/",
        "crates/aterm-gui/",
        "crates/aterm-cli/",
    ],
    check: launcher_mint,
};

fn launcher_mint(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if LAUNCHERS.iter().any(|l| file.rel.starts_with(l))
            || tree.corpus.test_code().holds(&file.rel)
            || !file.mentions(&[MINT])
        {
            continue;
        }
        for line in file.shipped_lines() {
            if has(line.code, MINT, Bound::Word) && !has(line.code, "fn root_authority", Bound::Sub)
            {
                out.push(file.finding(
                    LAUNCHER_MINT.id,
                    line.no,
                    "names the root-authority mint outside a launcher".to_string(),
                ));
            }
        }
    }
    for rel in manifests(tree.corpus.paths()) {
        let text = match crate::tree::read_to_string(&tree.root.join(rel)) {
            Ok(text) if text.contains(FEATURE) => text,
            Ok(_) => continue,
            Err(e) => {
                out.push(manifest_finding(rel, format!("unreadable manifest: {e}")));
                continue;
            }
        };
        match crate::scan_set::shipped_feature_enablers(&text, rel, CAP, FEATURE) {
            Ok(found) => {
                for at in found {
                    out.push(manifest_finding(
                        rel,
                        format!("{at}: only a launcher enables the mint"),
                    ));
                }
            }
            Err(why) => out.push(manifest_finding(rel, why)),
        }
    }
}

/// The root manifest and every `crates/<name>/Cargo.toml` but the launchers'.
fn manifests(paths: &[String]) -> impl Iterator<Item = &str> {
    paths.iter().map(String::as_str).filter(|rel| {
        *rel == "Cargo.toml"
            || rel.strip_prefix("crates/").is_some_and(|r| {
                r.strip_suffix("/Cargo.toml")
                    .is_some_and(|name| !name.contains('/'))
                    && !LAUNCHERS.iter().any(|l| rel.starts_with(l))
            })
    })
}

fn manifest_finding(rel: &str, detail: String) -> Finding {
    Finding {
        rule: LAUNCHER_MINT.id,
        path: rel.to_string(),
        line: 0,
        detail,
    }
}

#[cfg(test)]
#[path = "mint_tests.rs"]
mod tests;
