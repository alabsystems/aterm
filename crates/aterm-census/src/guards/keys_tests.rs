// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

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
    use crate::scan_set::test_fixtures::disk_tree;
    let key = random_phrase();
    let fixture = "const PAPER: &str = \"0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0\";\n";
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
        (".cargo/machine.key".to_string(), String::new()),
        ("crates/x/.scratch/root.key".to_string(), String::new()),
        (".git/signing.key".to_string(), String::new()),
    ]);
    let root = disk_tree("aterm-census-guards", "key-material", &files);
    let corpus = super::super::Corpus::at(&root).expect("the synth tree reads");
    let tree = Tree::new(&root, &corpus, crate::guards::tests::NO_ENGINE);
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
            ".cargo/machine.key:0",
            "crates/aterm-release/fixtures/machine.key:0",
            "crates/aterm-update-core/src/pins.rs:0",
            "crates/atpkg-keys/src/new.rs:2",
            "crates/x/.scratch/root.key:0",
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
