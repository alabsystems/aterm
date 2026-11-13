// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::spots;
use super::*;

const MANIFEST_TEXT: &str = "Cargo.toml\ncrates\nvendor\n!vendor/astream\n\
                             !crates/b/tests/withheld.rs\n!crates/b/tests/corpus\n";

fn run(files: &[(&str, &str)]) -> Vec<Finding> {
    let corpus = Corpus::of(files);
    let mut found = Vec::new();
    held_to(&corpus, Some(MANIFEST_TEXT), &mut found);
    found
}

#[test]
fn an_exported_file_includes_only_what_the_export_carries() {
    let lib = "#[cfg(test)]\nmod t {\n\
               const A: &str = include_str!(concat!(\n    env!(\"CARGO_MANIFEST_DIR\"),\n    \
               \"/../../vendor/astream/crates/astream-x/tests/fixtures/a.txt\"\n));\n\
               const B: &str = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \
               \"/../../vendor/astream/\", \"evidence/manifest.toml\",));\n\
               const C: &[u8] = include_bytes!(\"../../../vendor/astream/crates/astream-x/src/data.bin\");\n\
               const D: &str = include_str!(\"../tests/corpus/one.txt\");\n\
               const E: &str = include_str!(\"fixtures/ok.txt\");\n\
               const F: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\n\
               // include_str!(\"../tests/corpus/one.txt\") in a comment is no include\n\
               const G: &str = \"include_str!(\\\"../tests/corpus/one.txt\\\")\";\n\
               const H: &str = include_str!(\"../../../docs/notes.md\");\n\
               const I: &str = include_str!(\"../../../vendor/astream/Cargo.toml\");\n\
               const J: &str = include_str!(\"../../../../../outside.txt\");\n\
               const K: &str = include_str![\"../tests/corpus/one.txt\"];\n\
               const L: &str = include_str!{concat![env!(\"CARGO_MANIFEST_DIR\"), \"/../../vendor/astream/evidence/manifest.toml\"]};\n}\n";
    let found = run(&[
        ("Cargo.toml", ""),
        ("crates/b/Cargo.toml", ""),
        ("crates/b/src/lib.rs", lib),
        ("crates/b/src/fixtures/ok.txt", ""),
        ("crates/b/tests/corpus/one.txt", ""),
        // A withheld test reads the withheld corpus: neither is exported.
        (
            "crates/b/tests/withheld.rs",
            "const X: &str = include_str!(\"corpus/one.txt\");\n",
        ),
        ("docs/notes.md", ""),
        ("vendor/astream/Cargo.toml", ""),
        ("vendor/astream/evidence/manifest.toml", ""),
        ("vendor/astream/crates/astream-x/Cargo.toml", ""),
        ("vendor/astream/crates/astream-x/src/data.bin", ""),
        ("vendor/astream/crates/astream-x/tests/fixtures/a.txt", ""),
    ]);
    assert_eq!(
        spots(&found),
        [
            "crates/b/src/lib.rs:3",
            "crates/b/src/lib.rs:7",
            "crates/b/src/lib.rs:9",
            "crates/b/src/lib.rs:14",
            "crates/b/src/lib.rs:17",
            "crates/b/src/lib.rs:18",
        ],
        "{found:#?}"
    );
    assert!(
        found[0].detail.contains(
            "`vendor/astream/crates/astream-x/tests/fixtures/a.txt`, which the export leaves \
             out: the export's vendor/astream carries"
        ),
        "{}",
        found[0].detail
    );
    assert!(
        found[1]
            .detail
            .contains("`vendor/astream/evidence/manifest.toml`"),
        "{}",
        found[1].detail
    );
    assert!(
        found[2].detail.contains(
            "`crates/b/tests/corpus/one.txt`, which the export leaves out: \
             publish/manifest.txt withholds it"
        ),
        "{}",
        found[2].detail
    );
    assert!(
        found[3].detail.contains("`docs/notes.md`"),
        "{}",
        found[3].detail
    );
}

#[test]
fn the_export_carries_astreams_root_manifest_and_each_crates_tree_but_its_targets() {
    for carried in [
        "Cargo.toml",
        "crates/astream-x/Cargo.toml",
        "crates/astream-x/src/lib.rs",
        "crates/astream-x/build.rs",
        "crates/astream-x/src/tests/fixture.txt",
    ] {
        assert!(astream_carries(carried), "{carried}");
    }
    for left in [
        "README.md",
        "evidence/manifest.toml",
        "crates/astream-x/tests/fixtures/a.txt",
        "crates/astream-x/examples/e.rs",
        "crates/astream-x/benches/b.rs",
        "crates/Cargo.toml",
        "crates/astream-x",
    ] {
        assert!(!astream_carries(left), "{left}");
    }
}
