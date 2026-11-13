// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

const ROOT: &str = "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/forked\"]\n";

/// The files every case's crate sits among: the root manifest, two members and an excluded
/// one, and the Markdown and other files the reads name.
fn world<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut files = vec![
        ("Cargo.toml", ROOT),
        ("crates/a/Cargo.toml", "[package]\nname = \"a\"\n"),
        ("crates/a/SPECS.md", ""),
        ("crates/a/assets/x.md", ""),
        ("crates/a/docs/x.md", ""),
        ("crates/b/Cargo.toml", "[package]\nname = \"b\"\n"),
        ("crates/b/SPECS.md", ""),
        ("crates/forked/Cargo.toml", "[package]\nname = \"forked\"\n"),
        ("crates/forked/SPECS.md", ""),
        ("AGENTS.md", ""),
        ("CHANGELOG.md", ""),
        (".gitleaks.toml", ""),
        ("docs/KNOWN_ISSUES.md", ""),
        ("docs/notes.md", ""),
        ("docs/measured/x/README.md", ""),
    ];
    files.extend_from_slice(extra);
    files
}

fn found(source: &str) -> Vec<String> {
    found_in("crates/a/src/lib_tests.rs", source)
}

fn found_in(rel: &str, source: &str) -> Vec<String> {
    let found = run(&NO_REACH_READ, &world(&[(rel, source)]));
    found
        .iter()
        .map(|f| {
            let target = f.detail.split('`').nth(1).unwrap_or_default();
            format!("{}:{} {target}", f.path, f.line)
        })
        .collect()
}

/// Each read the rule names: an include, a `../` literal leaving the crate, a crate-rooted
/// chain to the root and on (to CHANGELOG.md as to any root Markdown), a folder under docs/,
/// another member's spec table, a build script's literal, a const of the crate, the crate's own
/// SPECS.md by every root, and a root followed through `let` bindings.
#[test]
fn each_read_of_no_reach_text_is_named() {
    let cases: [(&str, &str, &str); 13] = [
        (
            "include_str!(\"../../../docs/notes.md\")",
            "docs/notes.md",
            "",
        ),
        (
            "include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../AGENTS.md\"))",
            "AGENTS.md",
            "",
        ),
        (
            "fn t() { manifest_dir().join(\"../..\").join(\"CHANGELOG.md\"); }",
            "CHANGELOG.md",
            "",
        ),
        (
            "std::fs::read_to_string(aterm_tempfile::manifest_dir().join(\"../..\")\
             .join(\"docs/KNOWN_ISSUES.md\"))",
            "docs/KNOWN_ISSUES.md",
            "",
        ),
        (
            "std::fs::read_dir(aterm_tempfile::manifest_dir().join(\"../../docs\"))",
            "docs",
            "",
        ),
        (
            "std::fs::read_dir(manifest_dir().join(\"../..\").join(\"docs\").join(\"measured\"))",
            "docs/measured",
            "",
        ),
        (
            "std::fs::read_to_string(\"../b/SPECS.md\")",
            "crates/b/SPECS.md",
            "",
        ),
        (
            "const NOTES: &str = \"docs/notes.md\";\nfn t() { manifest_dir().join(\"../..\").join(NOTES); }",
            "docs/notes.md",
            "",
        ),
        (
            "std::fs::read_to_string(aterm_tempfile::manifest_dir().join(\"SPECS.md\"))",
            "crates/a/SPECS.md",
            "",
        ),
        (
            "std::fs::read_to_string(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/SPECS.md\"))",
            "crates/a/SPECS.md",
            "",
        ),
        (
            "std::path::PathBuf::from(env!(\"CARGO_MANIFEST_DIR\")).join(\"SPECS.md\")",
            "crates/a/SPECS.md",
            "",
        ),
        (
            "fn t() {\n    let root = aterm_tempfile::manifest_dir().join(\"../..\");\n    \
             let other = 1;\n    std::fs::read(root.join(\"docs/KNOWN_ISSUES.md\"));\n}",
            "docs/KNOWN_ISSUES.md",
            "4",
        ),
        (
            "fn t() {\n    let mut root = std::fs::canonicalize(manifest_dir().join(\"..\"))\
             .unwrap();\n    let up: std::path::PathBuf = root.parent().unwrap().to_path_buf();\n    \
             up.join(\"AGENTS.md\").exists();\n}",
            "AGENTS.md",
            "4",
        ),
    ];
    for (source, target, line) in cases {
        let found = found(source);
        let line = if line.is_empty() {
            (source.matches('\n').count() + 1).to_string()
        } else {
            line.to_owned()
        };
        assert_eq!(
            found,
            [format!("crates/a/src/lib_tests.rs:{line} {target}")],
            "{source}"
        );
    }
    assert_eq!(
        found_in(
            "crates/a/build.rs",
            "fn main() { std::fs::read(\"../../docs/notes.md\"); }"
        ),
        ["crates/a/build.rs:1 docs/notes.md"]
    );
    assert_eq!(
        found_in(
            "crates/a/tests/it.rs",
            "const X: &str = include_str!(\"../SPECS.md\");"
        ),
        ["crates/a/tests/it.rs:1 crates/a/SPECS.md"],
        "a test target's include of its crate's own spec table"
    );
}

/// The reads a test, build script or doctest makes without a `../` literal: a bare literal
/// from the crate's directory (where Cargo runs it), a build script's rerun line, a
/// `format!` of a root, a rebinding, `.into()`, `var_os`, a same-file helper root, a relative
/// `Path::new` root, `and_then(Path::parent)`, a doctest, and a `#[doc]`-included README's
/// Rust block. NEGATIVE CONTROLS sit in [`what_reads_no_no_reach_text_is_no_finding`].
#[test]
fn a_read_from_the_crate_directory_a_helper_or_a_doctest_is_named() {
    let spec = "crates/a/SPECS.md";
    let cases: [(&str, &str, &str); 11] = [
        ("std::fs::read_to_string(\"SPECS.md\")", spec, ""),
        (
            "std::fs::read(format!(\"{}/SPECS.md\", env!(\"CARGO_MANIFEST_DIR\")))",
            spec,
            "",
        ),
        (
            "fn t() {\n    let dir = env!(\"CARGO_MANIFEST_DIR\");\n    \
             std::fs::read(format!(\"{dir}/SPECS.md\"));\n}",
            spec,
            "3",
        ),
        (
            "fn t() {\n    let dir = aterm_tempfile::manifest_dir();\n    \
             let dir = dir.canonicalize().unwrap();\n    std::fs::read(dir.join(\"SPECS.md\"));\n}",
            spec,
            "4",
        ),
        (
            "fn t() {\n    let dir: std::path::PathBuf = env!(\"CARGO_MANIFEST_DIR\").into();\n    \
             std::fs::read(dir.join(\"SPECS.md\"));\n}",
            spec,
            "3",
        ),
        (
            "std::path::PathBuf::from(std::env::var_os(\"CARGO_MANIFEST_DIR\").expect(\"set\"))\
             .join(\"SPECS.md\")",
            spec,
            "",
        ),
        (
            "fn repo() -> std::path::PathBuf {\n    aterm_tempfile::manifest_dir().join(\"../..\")\n}\n\
             fn t() { std::fs::read(repo().join(\"docs\").join(\"KNOWN_ISSUES.md\")); }",
            "docs/KNOWN_ISSUES.md",
            "",
        ),
        (
            "std::fs::read(std::path::Path::new(\"../..\").join(\"docs\").join(\"KNOWN_ISSUES.md\"))",
            "docs/KNOWN_ISSUES.md",
            "",
        ),
        (
            "std::path::Path::new(\".\").join(\"SPECS.md\").exists()",
            spec,
            "",
        ),
        (
            "fn t() {\n    let m = manifest_dir();\n    \
             let root = m.parent().and_then(Path::parent).unwrap();\n    root.join(\"AGENTS.md\");\n}",
            "AGENTS.md",
            "4",
        ),
        (
            "/// ```\n/// let s = std::fs::read_to_string(concat!(env!(\"CARGO_MANIFEST_DIR\"), \
             \"/../../docs/KNOWN_ISSUES.md\"));\n/// ```\npub fn f() {}",
            "docs/KNOWN_ISSUES.md",
            "2",
        ),
    ];
    for (source, target, line) in cases {
        let line = if line.is_empty() {
            (source.matches('\n').count() + 1).to_string()
        } else {
            line.to_owned()
        };
        assert_eq!(
            found(source),
            [format!("crates/a/src/lib_tests.rs:{line} {target}")],
            "{source}"
        );
    }
    assert_eq!(
        found_in(
            "crates/a/build.rs",
            "fn main() { println!(\"cargo:rerun-if-changed=SPECS.md\"); }"
        ),
        ["crates/a/build.rs:1 crates/a/SPECS.md"]
    );
    let readme = run(
        &NO_REACH_READ,
        &world(&[
            (
                "crates/a/src/lib.rs",
                "#![doc = include_str!(\"../README.md\")]\n",
            ),
            (
                "crates/a/README.md",
                "# a\n\n```rust\nlet s = std::fs::read(\"../../docs/notes.md\");\n```\n\n\
                 ```sh\ncat ../../AGENTS.md\n```\n",
            ),
        ]),
    );
    assert_eq!(spots(&readme), ["crates/a/README.md:4"]);
}

/// NEGATIVE CONTROLS: what reads nothing no-reach. A tempdir's README.md, a provenance or
/// fixture literal, a crate-owned asset, a `../docs` path that stays inside the crate, a
/// comment, a root read of a file outside the class, a directory under docs/ holding none, an
/// excluded crate's spec table, a binding shadowed before its use, a const another crate
/// defines, a join of an unknown value, and a no-reach name the tree does not hold (a
/// scratch repository's symlink target).
#[test]
fn what_reads_no_no_reach_text_is_no_finding() {
    for source in [
        "fn t() { let root = aterm_tempfile::tempdir().unwrap(); \
         std::fs::write(root.path().join(\"README.md\"), \"\"); }",
        "const P: &str = \"docs/measured/x/README.md\";",
        "fn t() { std::fs::write(dir.join(\"AGENTS.md\"), \"\"); }",
        "const X: &str = include_str!(\"../assets/x.md\");",
        "fn t() { std::fs::read(\"../docs/x.md\"); }",
        "// std::fs::read(manifest_dir().join(\"../../docs/x.md\"))",
        "/* include_str!(\"../../../AGENTS.md\") */",
        "fn t() { manifest_dir().join(\"../..\").join(\".gitleaks.toml\"); }",
        "fn t() { manifest_dir().join(\"../..\").join(\"docs/empty\"); }",
        "fn t() { std::fs::read(\"../forked/SPECS.md\"); }",
        "fn t() { let root = manifest_dir().join(\"../..\"); let root = other(); \
         root.join(\"AGENTS.md\"); }",
        "fn t() { manifest_dir().join(\"../..\").join(ELSEWHERE); }",
        "fn t() { manifest_dir().join(\"../..\").join(format!(\"docs/{}\", x)); }",
        "fn t() { manifest_dir().join(\"../..\"); }",
        "fn t() { symlink(\"../../hop.md\", work.join(\"docs/out.md\")); }",
        "fn t() { tab_title(\"SPECS.md\"); }",
        "fn t() { std::fs::read(format!(\"{}/SPECS.md\", other)); }",
        "fn repo(at: u8) -> PathBuf { manifest_dir().join(\"../..\") }\n\
         fn t() { repo(1).join(\"AGENTS.md\"); }",
        "fn root() -> PathBuf { manifest_dir().join(\"../..\") }\n\
         fn t() { x.root().join(\"AGENTS.md\"); }",
        "/// Reads docs/KNOWN_ISSUES.md.\npub fn f() {}",
        "/// ```text\n/// std::fs::read(\"../../../docs/notes.md\");\n/// ```\npub fn f() {}",
        "fn t() { let s = \"SPECS.md\"; }",
    ] {
        assert_eq!(found(source), Vec::<String>::new(), "{source}");
    }
    let elsewhere = run(
        &NO_REACH_READ,
        &world(&[
            (
                "crates/b/src/lib.rs",
                "pub const ELSEWHERE: &str = \"AGENTS.md\";",
            ),
            (
                "crates/a/src/lib.rs",
                "fn t() { manifest_dir().join(\"../..\").join(ELSEWHERE); }",
            ),
        ]),
    );
    assert_eq!(spots(&elsewhere), Vec::<String>::new());
    let twice = run(
        &NO_REACH_READ,
        &world(&[
            ("crates/a/src/one.rs", "const P: &str = \"AGENTS.md\";"),
            ("crates/a/src/two.rs", "const P: &str = \"README.txt\";"),
            (
                "crates/a/src/lib.rs",
                "fn t() { manifest_dir().join(\"../..\").join(P); }",
            ),
        ]),
    );
    assert_eq!(
        spots(&twice),
        Vec::<String>::new(),
        "a name the crate gives two values is unknown"
    );
}

/// A planted reader against the live tree's paths and root manifest: one added test file
/// that reads docs/KNOWN_ISSUES.md through a bound root is exactly one finding. (`verify
/// census` judges the tree as it stands.)
#[test]
fn a_planted_reader_in_the_live_tree_is_one_finding() {
    let root = aterm_tempfile::manifest_dir().join("../..");
    let corpus = super::super::Corpus::at(&root).expect("the live corpus reads");
    let tree = Tree::new(&root, &corpus, super::super::tests::NO_ENGINE);
    let planted = SourceFile::outside(
        "crates/aterm-census/src/planted_tests.rs".to_owned(),
        "fn t() { let root = aterm_tempfile::manifest_dir().join(\"../..\");\n\
         std::fs::read(root.join(\"docs/KNOWN_ISSUES.md\")); }\n",
    );
    let paths = corpus.paths();
    let present: BTreeSet<&str> = paths.iter().map(String::as_str).collect();
    let layout = Layout::new(
        tree.text("Cargo.toml").as_deref(),
        paths.iter().map(String::as_str),
    );
    let reads = Reads {
        present: &present,
        layout: &layout,
    };
    let mut found = BTreeSet::new();
    read_file(
        &planted,
        "crates/aterm-census/src",
        "crates/aterm-census",
        &BTreeMap::new(),
        &reads,
        &mut found,
    );
    assert_eq!(
        found.into_iter().collect::<Vec<_>>(),
        [(2, "docs/KNOWN_ISSUES.md".to_owned())]
    );
}
