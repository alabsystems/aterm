// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! key-material: no release-trust secret in the checkout.
//!
//! Two shapes nothing else scans for (the publication engine's secret scan
//! matches PEM headers, and only over the export):
//! * the paper master, written down as 52 bare base32 symbols
//!   (`0-9a-z` minus `i l o u`, bare or as 13 groups of 4), on the key surface:
//!   the trust anchors, the cutter's machine roster and prompt, the owner-side
//!   key tool and the key document. Path-scoped because truncated hashes of
//!   that length occur elsewhere. A phrase whose symbols step through the
//!   alphabet in order, in reverse or not at all (its last, check symbol aside)
//!   is a fixture, which no random master is;
//! * a key file: the machine key is binary pkcs8 with no text marker, so its
//!   name gives it away (`machine|master|release|signing|root.key`, `*.pkcs8`),
//!   anywhere in the checkout but build output and dot directories
//!   ([`super::walk`]).

use super::{Finding, Rule, Tree};

pub(super) const KEY_MATERIAL: Rule = Rule {
    id: "key-material",
    why: "the paper master signs the machine roster and a machine key signs every release; \
          either in the checkout is one commit from public, and no other scan sees them (a bare \
          base32 phrase and a binary pkcs8 file carry no PEM header)",
    scope: &[],
    check: key_material,
};

/// The files a master could be pasted into, where no phrase-shaped value belongs.
const KEY_SURFACE: &[&str] = &[
    "crates/aterm-update-core/src/pins.rs",
    "crates/aterm-update-core/src/roster.rs",
    "crates/aterm-release/src/machines.rs",
    "crates/aterm-release/src/provision.rs",
    "docs/RELEASE-KEYS.md",
];

/// The owner-side key tool: every file under it is key surface.
const KEY_TOOL: &str = "crates/atpkg-keys";

/// The phrase alphabet, in order.
const PHRASE_ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Symbols in a master phrase.
const PHRASE_LEN: usize = 52;

fn key_material(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let mut surface: Vec<String> = KEY_SURFACE.iter().map(|s| (*s).to_string()).collect();
    let tool_dir = format!("{KEY_TOOL}/");
    let tool: Vec<String> = tree
        .corpus
        .paths
        .iter()
        .filter(|rel| rel.starts_with(&tool_dir))
        .cloned()
        .collect();
    if tool.is_empty() {
        out.push(gone(KEY_TOOL));
    }
    surface.extend(tool);
    for rel in &surface {
        match std::fs::read(tree.root.join(rel)) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
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
    for rel in &tree.corpus.paths {
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

/// A file name only a secret key wears.
fn key_file_name(name: &str) -> bool {
    matches!(
        name,
        "machine.key" | "master.key" | "release.key" | "signing.key" | "root.key"
    ) || name.ends_with(".pkcs8")
}

/// The symbol value of a phrase character (either case), if it is one.
fn symbol(c: u8) -> Option<u8> {
    PHRASE_ALPHABET
        .iter()
        .position(|&a| a == c.to_ascii_lowercase())
        .and_then(|p| u8::try_from(p).ok())
}

/// Every master-phrase-shaped run in `line`, as its symbol values: 52 phrase
/// symbols bounded by non-alphanumerics, bare or as 13 groups of 4 each split
/// from the next by one space or dash.
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

/// Does the phrase step through the alphabet by one symbol up, one down or
/// none at all (its last, check symbol aside)? A fixture, never a random key.
fn synthetic(phrase: &[u8]) -> bool {
    let body = &phrase[..phrase.len().saturating_sub(1)];
    let step = |w: &[u8]| (w[1] + 32 - w[0]) % 32;
    let Some(first) = body.windows(2).next().map(step) else {
        return true;
    };
    matches!(first, 0 | 1 | 31) && body.windows(2).all(|w| step(w) == first)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A phrase-shaped run no fixture rule excuses, built at run time so no
    /// real-looking key is ever committed: symbols from a fixed LCG.
    fn random_phrase() -> String {
        let mut x: u32 = 0x2545_f491;
        (0..PHRASE_LEN)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                char::from(PHRASE_ALPHABET[((x >> 16) % 32) as usize])
            })
            .collect()
    }

    fn grouped(phrase: &str, sep: char) -> String {
        phrase
            .as_bytes()
            .chunks(4)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join(&sep.to_string())
    }

    #[test]
    fn a_phrase_is_52_bounded_symbols_bare_or_grouped() {
        let key = random_phrase();
        assert!(!synthetic(&phrases(&key)[0]));
        for line in [
            format!("const M: &str = \"{key}\";"),
            format!("paper: {}", key.to_ascii_uppercase()),
            format!("`{}`", grouped(&key, ' ')),
            format!("{}.", grouped(&key, '-')),
        ] {
            assert_eq!(phrases(&line).len(), 1, "{line}");
        }
        for line in [
            format!("{key}0"),
            key[1..].to_string(),
            format!("{}i", &key[1..]),
            format!("x{key}"),
            grouped(&key, '_'),
            grouped(&key, ' ').replacen(' ', "  ", 1),
        ] {
            assert!(phrases(&line).is_empty(), "{line}");
        }
    }

    #[test]
    fn an_alphabet_in_order_reversed_or_repeated_is_a_fixture() {
        let in_order = "0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0";
        let reversed = "zyxwvtsrqpnmkjhgfedcba9876543210zyxwvtsrqpnmkjhgfed0";
        let repeated = "2".repeat(PHRASE_LEN);
        for p in [in_order, reversed, &repeated, &grouped(in_order, ' ')] {
            let found = phrases(p);
            assert_eq!(found.len(), 1, "{p}");
            assert!(synthetic(&found[0]), "{p}");
        }
        let mut nudged = in_order.as_bytes().to_vec();
        nudged[20] = b'9';
        assert!(!synthetic(
            &phrases(std::str::from_utf8(&nudged).unwrap())[0]
        ));
    }

    #[test]
    fn key_file_names() {
        for name in ["machine.key", "root.key", "x.pkcs8"] {
            assert!(key_file_name(name), "{name}");
        }
        for name in ["machine.key.md", "keys.rs", "pkcs8.rs", "my-machine.key"] {
            assert!(!key_file_name(name), "{name}");
        }
    }

    #[test]
    fn the_rule_reads_the_key_surface_and_the_whole_checkout() {
        use crate::scan_set::test_fixtures::synth_tree;
        let key = random_phrase();
        let fixture =
            "const PAPER: &str = \"0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0\";\n";
        let mut files: Vec<(String, String)> = KEY_SURFACE[1..4]
            .iter()
            .map(|rel| ((*rel).to_string(), "fn ok() {}\n".to_string()))
            .collect();
        files.extend([
            (
                "docs/RELEASE-KEYS.md".to_string(),
                format!("# Keys\n\nWrite it as `{}`.\n", grouped(&key, ' ')),
            ),
            (
                "crates/atpkg-keys/src/master.rs".to_string(),
                fixture.to_string(),
            ),
            (
                "crates/atpkg-keys/src/new.rs".to_string(),
                format!("\nconst M: &str = \"{key}\";\n"),
            ),
            ("crates/atpkg-keys/target/x.rs".to_string(), key.clone()),
            ("crates/aterm-core/src/hash.rs".to_string(), key.clone()),
            (
                "crates/aterm-release/fixtures/machine.key".to_string(),
                String::new(),
            ),
            ("tools/owner.pkcs8".to_string(), String::new()),
            ("target/release.key".to_string(), String::new()),
            ("target-trust/x/machine.key".to_string(), String::new()),
            ("target.noindex/master.key".to_string(), String::new()),
            (".scratch/root.key".to_string(), String::new()),
        ]);
        let root = synth_tree("aterm-census-guards", "key-material", &files);
        let corpus = super::super::Corpus::read(&root).expect("the synth tree reads");
        let tree = Tree {
            root: &root,
            corpus: &corpus,
        };
        let mut found = Vec::new();
        key_material(&tree, &mut found);
        let _ = std::fs::remove_dir_all(&root);
        let mut spots: Vec<String> = found
            .iter()
            .map(|f| format!("{}:{}", f.path, f.line))
            .collect();
        spots.sort();
        assert_eq!(
            spots,
            [
                "crates/aterm-release/fixtures/machine.key:0",
                "crates/aterm-update-core/src/pins.rs:0",
                "crates/atpkg-keys/src/new.rs:2",
                "docs/RELEASE-KEYS.md:3",
                "tools/owner.pkcs8:0",
            ],
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|f| f.path.ends_with("pins.rs") && f.detail.contains("gone")),
            "{found:?}"
        );
    }
}
