// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

const ROOT: &str = "[workspace]\nmembers = [\"crates/*\"]\n# vendored forks\n\
                    exclude = [\"vendor/x\", # a fork\n  \"crates/forked\"]\n\
                    resolver = \"2\"\n\n[workspace.metadata.x]\nmembers = [\"other\"]\n";

fn layout() -> Layout {
    Layout::new(
        Some(ROOT),
        [
            "Cargo.toml",
            "crates/a/Cargo.toml",
            "crates/forked/Cargo.toml",
            "crates/a/sub/Cargo.toml",
            "docs/sub/Cargo.toml",
            "vendor/x/Cargo.toml",
        ],
    )
}

/// The three classes (root Markdown includes CHANGELOG.md), and every rejection beside them: other
/// file types, a fork's or an excluded crate's Markdown, a SPECS.md that is not at a member's
/// root, docs Markdown a package owns, and paths with empty, `.` or `..` segments.
#[test]
fn the_class_is_root_docs_and_member_spec_markdown() {
    let layout = layout();
    for path in [
        "AGENTS.md",
        "README.md",
        "CHANGELOG.md",
        "docs/KNOWN_ISSUES.md",
        "docs/measured/x/README.md",
        "crates/a/SPECS.md",
    ] {
        assert!(class(path, &layout), "{path}");
    }
    for path in [
        "Cargo.toml",
        "docs/shot.png",
        "docs/conf.toml",
        "deny.toml",
        "vendor/x/README.md",
        "vendor/x/SPECS.md",
        "crates/forked/SPECS.md",
        "crates/a/README.md",
        "crates/a/src/SPECS.md",
        "crates/a/sub/SPECS.md",
        "crates/none/SPECS.md",
        "docs/sub/x.md",
        "docs/sub/deeper/x.md",
        "docs/../AGENTS.md",
        "./AGENTS.md",
        "docs//x.md",
        "/AGENTS.md",
        "docs/x.MD",
    ] {
        assert!(!class(path, &layout), "{path}");
    }
}

/// The workspace arrays read exactly, or not at all: a root manifest whose arrays hold
/// anything else, or none at all, puts no spec table in the class. Docs and root Markdown
/// need no manifest.
#[test]
fn an_unreadable_workspace_admits_no_spec_table() {
    assert_eq!(
        workspace_arrays(ROOT),
        Some((
            vec!["crates/*".to_owned()],
            vec!["vendor/x".to_owned(), "crates/forked".to_owned()]
        ))
    );
    for manifest in [
        None,
        Some("[package]\nname = \"x\"\n"),
        Some("[workspace]\nmembers = [\"crates/*\", { path = \"x\" }]\n"),
        Some("[workspace]\nmembers = \"crates/*\"\n"),
        Some("[workspace]\nmembers = [\"crates/\\u002a\"]\n"),
        Some("[workspace]\nmembers = [\"crates/*\"\n"),
    ] {
        let layout = Layout::new(manifest, ["crates/a/Cargo.toml"]);
        assert!(!class("crates/a/SPECS.md", &layout), "{manifest:?}");
        assert!(class("docs/x.md", &layout) && class("AGENTS.md", &layout));
    }
    let globbed = Layout::new(
        Some("[workspace]\nmembers = [\"crates/a*\"]\n"),
        ["crates/a/Cargo.toml"],
    );
    assert!(!class("crates/a/SPECS.md", &globbed));
    let exact = Layout::new(
        Some("[workspace]\nmembers = [\"crates/a\"]\n"),
        ["crates/a/Cargo.toml", "crates/b/Cargo.toml"],
    );
    assert!(class("crates/a/SPECS.md", &exact) && !class("crates/b/SPECS.md", &exact));
}

/// A path is named where a string, an include or a `#[path]` holds it, in shipped or test
/// code, and where the text does not lex. NEGATIVE CONTROLS: a comment or doc comment alone
/// names nothing, nor does text that does not hold the path.
#[test]
fn a_path_is_named_by_code_and_never_by_a_comment() {
    let path = "docs/KNOWN_ISSUES.md";
    for text in [
        "fn f() -> &'static str { \"docs/KNOWN_ISSUES.md\" }\n",
        "#[cfg(test)]\nmod t { const P: &str = \"../../docs/KNOWN_ISSUES.md\"; }\n",
        "const S: &str = include_str!(\"../../docs/KNOWN_ISSUES.md\");\n",
        "#[path = \"docs/KNOWN_ISSUES.md\"]\nmod m;\n",
        "const R: &str = r#\"see docs/KNOWN_ISSUES.md\"#;\n",
        "fn f() { let s = \"docs/KNOWN_ISSUES.md\n",
    ] {
        assert!(names(text, path), "{text}");
    }
    for text in [
        "// docs/KNOWN_ISSUES.md lists them\nfn f() {}\n",
        "/// Read docs/KNOWN_ISSUES.md.\nfn f() {}\n/* docs/KNOWN_ISSUES.md */\n",
        "fn f() -> &'static str { \"docs/OTHER.md\" }\n",
    ] {
        assert!(!names(text, path), "{text}");
    }
}

/// A doctest is code: a string in a Rust block of a `///`, `//!` or `/** */` doc comment, its
/// hidden lines included, names the path, as a `#[doc]`-included README's Rust block does.
/// NEGATIVE CONTROLS: doc prose, a block of another language, an `ignore` block, a plain
/// comment's fence name nothing.
#[test]
fn a_doctest_names_a_path_and_doc_prose_does_not() {
    let path = "docs/KNOWN_ISSUES.md";
    let read = "let k = std::fs::read_to_string(\"../../docs/KNOWN_ISSUES.md\");";
    for text in [
        format!("/// ```\n/// {read}\n/// ```\nfn f() {{}}\n"),
        format!("//! ```rust,no_run\n//! # {read}\n//! ```\n"),
        format!("/** Read it.\n * ```\n * {read}\n * ```\n */\nfn f() {{}}\n"),
        format!("/// ~~~edition2021\n/// {read}\n/// ~~~\nfn f() {{}}\n"),
    ] {
        assert!(names(&text, path), "{text}");
    }
    for text in [
        format!("/// See docs/KNOWN_ISSUES.md.\n/// ```text\n/// {read}\n/// ```\nfn f() {{}}\n"),
        format!("/// ```ignore\n/// {read}\n/// ```\nfn f() {{}}\n"),
        format!("// ```\n// {read}\n// ```\nfn f() {{}}\n"),
    ] {
        assert!(!names(&text, path), "{text}");
    }
    let doc = format!("/// x\n/// ```\n/// {read}\n/// ```\nfn f() {{}}\n");
    let blocks = doctests(&doc);
    assert_eq!(blocks.len(), doc.len(), "byte for byte");
    assert_eq!(blocks.lines().nth(2).map(str::trim), Some(read));
    assert_eq!(blocks.lines().filter(|l| !l.trim().is_empty()).count(), 1);
    let readme = format!("# x\n\n```rust\n{read}\n```\n\n```sh\ncat {path}\n```\n");
    assert!(may_read("crates/x/README.md", &readme, path));
    assert!(!may_read(
        "crates/x/README.md",
        &readme.replace("rust", "text"),
        path
    ));
}

/// The land-time backstop is conservative where the guard is exact: a file name, a docs
/// directory, a crate's own `SPECS.md` from its own code, a walk's bare spec table name, each
/// in code, may read the path. NEGATIVE CONTROLS: a comment, census's own sources, another
/// crate's own table name, a file that is neither Rust nor a crate's Markdown, and docs
/// Markdown name nothing.
#[test]
fn a_reader_is_named_by_file_name_directory_or_own_table() {
    assert_eq!(
        needles("docs/measured/arena/TEMPLATE.md"),
        ["TEMPLATE.md", "docs/measured", "docs/measured/arena"]
    );
    assert_eq!(needles("AGENTS.md"), ["AGENTS.md"]);
    assert_eq!(needles("crates/b/SPECS.md"), ["SPECS.md"]);
    let ki = "docs/KNOWN_ISSUES.md";
    let reads = [
        (
            "crates/b/src/t.rs",
            "fn t() { repo().join(\"docs\").join(\"KNOWN_ISSUES.md\"); }",
            ki,
        ),
        (
            "crates/b/src/t.rs",
            "fn t() { workspace_root().join(\"docs/measured/arena\").join(T); }",
            "docs/measured/arena/TEMPLATE.md",
        ),
        (
            "crates/b/tests/t.rs",
            "fn t() { read_to_string(\"SPECS.md\"); }",
            "crates/b/SPECS.md",
        ),
        (
            "crates/b/build.rs",
            "const S: &str = \"cargo:rerun-if-changed=SPECS.md\";",
            "crates/b/SPECS.md",
        ),
        (
            "crates/a/src/t.rs",
            "fn t() { read(root().join(\"crates/b/SPECS.md\")); }",
            "crates/b/SPECS.md",
        ),
        (
            "crates/a/src/t.rs",
            "fn t() { for e in d { e.path().join(\"SPECS.md\"); } }",
            "crates/b/SPECS.md",
        ),
        (
            "crates/a/src/t.rs",
            "fn t() { format!(\"{dir}/SPECS.md\"); }",
            "crates/b/SPECS.md",
        ),
    ];
    for (file, text, path) in reads {
        assert!(may_read(file, text, path), "{file}: {text}");
    }
    let none = [
        (
            "crates/b/src/t.rs",
            "// docs/KNOWN_ISSUES.md lists them\nfn t() {}\n",
            ki,
        ),
        (
            "crates/aterm-census/src/g.rs",
            "const P: &str = \"docs/KNOWN_ISSUES.md\";",
            ki,
        ),
        (
            "crates/a/src/t.rs",
            "fn t() { read(\"cat SPECS.md\"); }",
            "crates/b/SPECS.md",
        ),
        (
            "crates/a/src/t.rs",
            "fn t() { s.ends_with(\"/SPECS.md\"); }",
            "crates/b/SPECS.md",
        ),
        ("crates/b/assets/x.txt", "KNOWN_ISSUES.md", ki),
        (
            "docs/DESIGN.md",
            "```rust\nread(\"docs/KNOWN_ISSUES.md\")\n```\n",
            ki,
        ),
        (
            "CHANGELOG.md",
            "```rust\nread(\"docs/KNOWN_ISSUES.md\")\n```\n",
            ki,
        ),
    ];
    for (file, text, path) in none {
        assert!(!may_read(file, text, path), "{file}: {text}");
    }
}

/// [`SOURCE`] is this file's text, which `verify land` compares with the merge's.
#[test]
fn the_rule_source_is_this_file() {
    let here = std::fs::read_to_string(aterm_tempfile::manifest_dir().join("src/no_reach.rs"))
        .expect("this file");
    assert_eq!(SOURCE, here);
}

/// The live tree's root manifest reads whole: a spec table of a member is in the class, and
/// the excluded fork's is not.
#[test]
fn the_live_workspace_reads_whole() {
    let root = aterm_tempfile::manifest_dir().join("../..");
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let (members, exclude) = workspace_arrays(&manifest).expect("the live [workspace]");
    assert_eq!(members, ["crates/*"]);
    assert!(
        exclude
            .iter()
            .any(|entry| entry == "crates/aterm-windows-sys")
    );
    let layout = Layout::new(
        Some(&manifest),
        [
            "crates/aterm-verify/Cargo.toml",
            "crates/aterm-windows-sys/Cargo.toml",
        ],
    );
    assert!(class("crates/aterm-verify/SPECS.md", &layout));
    assert!(!class("crates/aterm-windows-sys/SPECS.md", &layout));
}
