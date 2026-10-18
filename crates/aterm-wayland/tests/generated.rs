// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The committed protocol modules are what the generator writes from the
//! committed protocol files: the generator's own code (scan/src, built
//! outside the workspace) compiled here and run in process.
//!
//! Negative control: a protocol file one attribute away generates other
//! text.

#[allow(dead_code, reason = "the generator's binary entry is not used here")]
#[path = "../scan/src/generate.rs"]
mod generate;
#[allow(dead_code, reason = "as above")]
#[path = "../scan/src/xml.rs"]
mod xml;

use std::path::Path;

#[test]
fn the_committed_modules_are_the_generators_output() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = generate::generate(root).expect("the protocol files parse");
    assert_eq!(
        files.len(),
        generate::FILES.len() + 2,
        "a module per file, stubs, mod.rs"
    );
    for (rel, text) in files {
        let committed =
            std::fs::read_to_string(root.join(&rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert!(
            committed == text,
            "{rel} differs from a fresh run: regenerate it (crates/aterm-wayland/scan)"
        );
    }
}

#[test]
fn a_protocol_one_attribute_away_generates_other_text() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = aterm_tempfile::tempdir().expect("a scratch directory");
    std::fs::create_dir(scratch.path().join("protocols")).unwrap();
    for (file, _) in generate::FILES {
        let mut text = std::fs::read_to_string(root.join("protocols").join(file)).unwrap();
        if *file == "viewporter.xml" {
            text = text.replacen(
                "<interface name=\"wp_viewport\" version=\"1\">",
                "<interface name=\"wp_viewport\" version=\"2\">",
                1,
            );
        }
        std::fs::write(scratch.path().join("protocols").join(file), text).unwrap();
    }
    let fresh = generate::generate(root).unwrap();
    let moved = generate::generate(scratch.path()).unwrap();
    let differ: Vec<_> = fresh
        .iter()
        .zip(&moved)
        .filter(|(a, b)| a.1 != b.1)
        .map(|(a, _)| a.0.clone())
        .collect();
    assert_eq!(differ, ["src/protocols/viewporter.rs"]);
}
