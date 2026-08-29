// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The differential gate: for every `.toml` file in this repository, this
//! crate's parse and the `toml` oracle's must be the same value.
//!
//! The corpus is not a synthetic fixture set — it is 300-odd real files: every
//! crate manifest, every vendored upstream's manifest, `deny.toml`,
//! `ownership.toml`, `vendor/forge.toml`, the publish set, the art assets, and
//! the config files aterm actually ships. If the replacement disagrees with the
//! crate it replaces on any of them, the change is not safe to make.

mod common;

use common::{corpus, values_agree};

#[test]
fn every_toml_file_in_the_repository_parses_identically() {
    let files = corpus();
    let mut checked = 0usize;
    let mut failures = Vec::new();

    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let theirs = toml::from_str::<toml::Value>(&text);
        let ours = aterm_toml::from_str::<aterm_toml::Value>(&text);
        let display = path.display();

        match (&ours, &theirs) {
            (Ok(a), Ok(b)) => {
                if let Err(why) = values_agree(a, b) {
                    failures.push(format!("{display}: {why}"));
                }
            }
            (Err(a), Err(_)) => {
                // Both refuse. The corpus has no such file today, but a
                // vendored manifest could gain one and the gate should not
                // fail over two implementations phrasing a rejection
                // differently.
                let _ = a;
            }
            (Ok(_), Err(b)) => {
                failures.push(format!("{display}: we accept, the oracle rejects: {b}"))
            }
            (Err(a), Ok(_)) => {
                failures.push(format!("{display}: we reject, the oracle accepts: {a}"))
            }
        }
        checked += 1;
    }

    assert!(
        failures.is_empty(),
        "{} of {checked} files disagreed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    eprintln!("differential: {checked} .toml files parsed identically to the `toml` oracle");
}

#[test]
fn the_corpus_covers_the_files_the_task_named() {
    let files = corpus();
    let names: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    for required in [
        "/Cargo.toml",
        "/deny.toml",
        "/ownership.toml",
        "/vendor/forge.toml",
        "/crates/aterm-toml/Cargo.toml",
    ] {
        assert!(
            names.iter().any(|n| n.ends_with(required)),
            "the corpus is missing {required}"
        );
    }
}

/// The oracle's own reader, driven over the corpus, has to agree that what we
/// print is what we read. This catches an encoder that round-trips its own
/// parse but emits something the rest of the world reads differently.
#[test]
fn what_we_print_the_oracle_reads_back_unchanged() {
    let mut checked = 0usize;
    for path in corpus() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(theirs) = toml::from_str::<toml::Value>(&text) else {
            continue;
        };
        let document: aterm_toml::edit::DocumentMut = text
            .parse()
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let printed = document.to_string();
        let reparsed: toml::Value = toml::from_str(&printed)
            .unwrap_or_else(|e| panic!("{}: the oracle rejected our output: {e}", path.display()));
        assert_eq!(
            theirs,
            reparsed,
            "{}: our output means something else",
            path.display()
        );
        checked += 1;
    }
    eprintln!("differential: {checked} documents re-read identically by the oracle");
}
