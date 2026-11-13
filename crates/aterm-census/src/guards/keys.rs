// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! key-material: no release-trust secret in the checkout.

use super::{Finding, Rule, Tree};

pub(super) const KEY_MATERIAL: Rule = Rule {
    id: "key-material",
    why: "the paper master signs the machine roster and a machine key signs every release; \
          either in the checkout is one commit from public, and no other scan sees them (a bare \
          base32 phrase and a binary pkcs8 file carry no PEM header)",
    scope: &[],
    check: key_material,
};

const KEY_SURFACE: &[&str] = &[
    "crates/aterm-update-core/src/pins.rs",
    "crates/aterm-update-core/src/roster.rs",
    "crates/aterm-release/src/machines.rs",
    "crates/aterm-release/src/provision.rs",
    "docs/RELEASE-KEYS.md",
];

const KEY_TOOL: &str = "crates/atpkg-keys";

const PHRASE_ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

const PHRASE_LEN: usize = 52;

fn key_material(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let mut surface: Vec<String> = KEY_SURFACE.iter().map(|s| (*s).to_string()).collect();
    let tool_dir = format!("{KEY_TOOL}/");
    let tool: Vec<String> = tree
        .corpus
        .paths()
        .iter()
        .filter(|rel| rel.starts_with(&tool_dir))
        .cloned()
        .collect();
    if tool.is_empty() {
        out.push(gone(KEY_TOOL));
    }
    surface.extend(tool);
    for rel in &surface {
        let loaded = tree
            .corpus
            .get(rel)
            .map(|f| Ok(std::borrow::Cow::Borrowed(f.raw())))
            .unwrap_or_else(|| {
                crate::tree::read_bytes(&tree.root.join(rel)).map(|bytes| {
                    std::borrow::Cow::Owned(String::from_utf8_lossy(&bytes).into_owned())
                })
            });
        match loaded {
            Ok(text) => {
                for (i, line) in text.lines().enumerate() {
                    if phrases(line).iter().any(|p| !synthetic(p)) {
                        out.push(Finding {
                            rule: KEY_MATERIAL.id,
                            path: rel.clone(),
                            line: i + 1,
                            detail: "a master-phrase-shaped run (52 base32 symbols) on the key \
                                     surface"
                                .to_string(),
                        });
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => out.push(gone(rel)),
            Err(e) => out.push(unreadable(rel, &e.to_string())),
        }
    }
    for rel in tree.corpus.paths() {
        if key_file_name(rel.rsplit('/').next().unwrap_or(rel)) {
            out.push(Finding {
                rule: KEY_MATERIAL.id,
                path: rel.clone(),
                line: 0,
                detail: "a file named like a secret key".to_string(),
            });
        }
    }
}

fn gone(rel: &str) -> Finding {
    Finding {
        rule: KEY_MATERIAL.id,
        path: rel.to_string(),
        line: 0,
        detail: "this key-surface path is gone, so the rule reads nothing there (moved? name its \
                 new path in the rule)"
            .to_string(),
    }
}

fn unreadable(rel: &str, why: &str) -> Finding {
    Finding {
        rule: KEY_MATERIAL.id,
        path: rel.to_string(),
        line: 0,
        detail: format!("unreadable: {why}"),
    }
}

fn key_file_name(name: &str) -> bool {
    matches!(
        name,
        "machine.key" | "master.key" | "release.key" | "signing.key" | "root.key"
    ) || name.ends_with(".pkcs8")
}

fn symbol(c: u8) -> Option<u8> {
    PHRASE_ALPHABET
        .iter()
        .position(|&a| a == c.to_ascii_lowercase())
        .and_then(|p| u8::try_from(p).ok())
}

fn phrases(line: &str) -> Vec<Vec<u8>> {
    let b = line.as_bytes();
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_alphanumeric() {
            let start = i;
            while i < b.len() && b[i].is_ascii_alphanumeric() {
                i += 1;
            }
            runs.push((start, i));
        } else {
            i += 1;
        }
    }
    let symbols = |&(s, e): &(usize, usize)| -> Option<Vec<u8>> {
        b[s..e].iter().map(|&c| symbol(c)).collect()
    };
    let mut out: Vec<Vec<u8>> = runs
        .iter()
        .filter(|(s, e)| e - s == PHRASE_LEN)
        .filter_map(symbols)
        .collect();
    for group in runs.windows(PHRASE_LEN / 4) {
        let spaced = group.iter().all(|(s, e)| e - s == 4)
            && group
                .windows(2)
                .all(|p| p[1].0 == p[0].1 + 1 && matches!(b[p[0].1], b' ' | b'-'));
        if spaced && let Some(parts) = group.iter().map(symbols).collect::<Option<Vec<_>>>() {
            out.push(parts.concat());
        }
    }
    out
}

fn synthetic(phrase: &[u8]) -> bool {
    let body = &phrase[..phrase.len().saturating_sub(1)];
    let step = |w: &[u8]| (w[1] + 32 - w[0]) % 32;
    let Some(first) = body.windows(2).next().map(step) else {
        return true;
    };
    matches!(first, 0 | 1 | 31) && body.windows(2).all(|w| step(w) == first)
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
