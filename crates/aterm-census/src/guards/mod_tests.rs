// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// No engine baseline: what a test's tree is judged with unless it says otherwise.
pub(crate) const NO_ENGINE: &crate::EngineBaseline = &Err(String::new());

/// The rule's findings over `files`, with no checkout behind them.
pub(crate) fn run(rule: &Rule, files: &[(&str, &str)]) -> Vec<Finding> {
    let corpus = Corpus::of(files);
    let tree = Tree::new(
        Path::new("/nonexistent-aterm-census-root"),
        &corpus,
        NO_ENGINE,
    );
    let walk = Walk {
        root: tree.root,
        engine: NO_ENGINE,
        source: &std::sync::OnceLock::from(Arc::new(memo::Snapshot::empty())),
        run: None,
    };
    run_rules(&tree, std::slice::from_ref(rule), &walk)
}

/// Each finding as `path:line`.
pub(crate) fn spots(found: &[Finding]) -> Vec<String> {
    found
        .iter()
        .map(|f| format!("{}:{}", f.path, f.line))
        .collect()
}

#[test]
fn calls_are_the_identifiers_before_each_paren() {
    assert_eq!(
        calls("let x = a::b(c.d (1), Some(e)) + f[0](g);"),
        ["b", "d", "Some"]
    );
    assert!(calls("(1, 2)").is_empty());
}

#[test]
fn word_hits_respect_identifier_edges() {
    assert!(has("use libc::c_int;", "libc", Bound::Word));
    assert!(!has("use aterm_libc::x;", "libc", Bound::Word));
    assert!(!has("MetalLayer", "Metal", Bound::Word));
    assert!(has("MetalLayer", "Metal", Bound::Sub));
}

#[test]
fn shipped_lines_drop_test_items_in_both_views() {
    let file = SourceFile::outside(
        "crates/x/src/lib.rs".to_string(),
        "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn b() {}\n}\nfn c() {}\n",
    );
    let kept: Vec<usize> = file.shipped_lines().map(|l| l.no).collect();
    assert_eq!(kept, [1, 6]);
}

#[test]
fn a_panicking_rule_is_one_finding_not_a_crash() {
    fn boom(_: &Tree<'_>, _: &mut Vec<Finding>) {
        panic!("planted");
    }
    let rule = Rule {
        id: "boom",
        why: "planted",
        scope: &[],
        check: boom,
    };
    let found = run(&rule, &[]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].rule, "boom");
}

/// The files of `corpus` that are test code nothing ships ([`rules::TestCode::holds`]).
fn held(corpus: &Corpus) -> Vec<String> {
    let test_code = corpus.test_code();
    let held: std::collections::BTreeSet<String> = corpus
        .under(&[""])
        .filter(|f| test_code.holds(&f.rel))
        .map(|f| f.rel.clone())
        .collect();
    held.into_iter().collect()
}

/// A file is test code when its parent declares it under a test `cfg`,
/// by its own name, its `mod.rs` or a `#[path]`, or when a test-only file
/// declares it.
#[test]
fn a_module_its_parent_gates_on_test_is_test_code() {
    let corpus = Corpus::of(&[
        (
            "crates/x/src/lib.rs",
            "#[cfg(test)]\nmod a;\n#[cfg(all(test, unix))]\n#[path = \"rig_conformance.rs\"]\n\
             pub(crate) mod rig;\n#[cfg(not(test))]\nmod live;\nmod plain;\n",
        ),
        ("crates/x/src/a.rs", "mod b;\n"),
        ("crates/x/src/a/b/mod.rs", ""),
        ("crates/x/src/rig_conformance.rs", ""),
        ("crates/x/src/live.rs", ""),
        (
            "crates/x/src/plain.rs",
            "#[cfg(test)]\nmod tests;\nmod inner;\n",
        ),
        ("crates/x/src/plain/tests.rs", ""),
        ("crates/x/src/plain/inner.rs", ""),
    ]);
    assert_eq!(
        held(&corpus),
        [
            "crates/x/src/a.rs",
            "crates/x/src/a/b/mod.rs",
            "crates/x/src/plain/tests.rs",
            "crates/x/src/rig_conformance.rs",
        ]
    );
}

/// A file a shipped line also loads ships, and so does what it declares; a file only a
/// test-only file loads stays test code, and so does one a test-only inline module
/// `include!`s. Each load resolves as rustc resolves it: an attribute on the `mod`'s own
/// line, a `cfg_attr` path and the file found by name where its predicate is false, an inline
/// module's directory, an `include!` literal.
///
/// NEGATIVE CONTROL: reading a `cfg_attr` path as unconditional loses `outer/inner.rs`.
#[test]
fn a_test_module_a_shipped_line_also_loads_ships() {
    let corpus = Corpus::of(&[
        (
            "crates/x/src/lib.rs",
            "#[cfg(test)]\nmod a;\n#[cfg(test)]\nmod b;\n#[path = \"b.rs\"] mod b_again;\n\
             mod outer {\n    #[cfg_attr(unix, path = \"o.rs\")]\n    mod inner;\n    \
             mod plain;\n}\ninclude!(\"inc.rs\");\n#[cfg(test)]\nmod c {\n    \
             include!(\"c_inc.rs\");\n}\n",
        ),
        ("crates/x/src/a.rs", "mod a_child;\n"),
        ("crates/x/src/a/a_child.rs", ""),
        ("crates/x/src/b.rs", "mod b_child;\n"),
        ("crates/x/src/b/b_child.rs", ""),
        ("crates/x/src/outer/o.rs", ""),
        ("crates/x/src/outer/plain.rs", ""),
        ("crates/x/src/inc.rs", ""),
        ("crates/x/src/c_inc.rs", ""),
    ]);
    assert_eq!(
        held(&corpus),
        [
            "crates/x/src/a.rs",
            "crates/x/src/a/a_child.rs",
            "crates/x/src/c_inc.rs",
        ]
    );
    let lib = corpus.get("crates/x/src/lib.rs").map(SourceFile::loads);
    let loads: Vec<(Vec<String>, bool)> = lib
        .unwrap_or_default()
        .iter()
        .map(|l| (l.files.clone(), l.gated))
        .collect();
    let s = |p: &str| p.to_string();
    assert_eq!(
        loads,
        [
            (
                vec![s("crates/x/src/a.rs"), s("crates/x/src/a/mod.rs")],
                true
            ),
            (
                vec![s("crates/x/src/b.rs"), s("crates/x/src/b/mod.rs")],
                true
            ),
            (vec![s("crates/x/src/b.rs")], false),
            (vec![s("crates/x/src/outer/o.rs")], false),
            (
                vec![
                    s("crates/x/src/outer/inner.rs"),
                    s("crates/x/src/outer/inner/mod.rs")
                ],
                false
            ),
            (
                vec![
                    s("crates/x/src/outer/plain.rs"),
                    s("crates/x/src/outer/plain/mod.rs")
                ],
                false
            ),
            (vec![s("crates/x/src/inc.rs")], false),
            (vec![s("crates/x/src/c_inc.rs")], true),
        ]
    );
}

/// The corpus fails closed: a source directory its `.rs` walk may not list
/// refuses the read, never a corpus of what it could read.
#[test]
fn a_source_directory_it_cannot_list_refuses_the_corpus() {
    let read = |fault: fn(&crate::tree::memory::Tree)| {
        let root = crate::scan_set::test_fixtures::synth_tree(
            "aterm-census-guards",
            "unlisted",
            &[("crates/a/src/notes.md".to_string(), String::new())],
        );
        fault(&root);
        Corpus::at(&root).map(|corpus| corpus.len())
    };
    assert_eq!(read(|_| {}), Ok(0));
    assert!(read(|root| root.unlistable("crates/a/src")).is_err());
}

/// The walk enters dot directories for paths (never `.git`, nor one
/// `.gitignore` keeps out by directory), and reads scripts only outside
/// them.
#[test]
fn the_walk_lists_dot_directories_but_reads_no_script_there() {
    use crate::scan_set::test_fixtures::disk_tree;
    let files: Vec<(String, String)> = [
        (
            ".gitignore",
            "debug\n.claude/\n/.trust-cache/\n/publish/.out/\n**/mutants.out*/\n\
             !vendor/**/debug/\n",
        ),
        ("tools/install.sh", "#!/bin/sh\n"),
        (".cargo/config.toml", ""),
        (".cargo/machine.key", ""),
        (".config/hooks/stop.sh", "#!/bin/sh\n"),
        (".claude/worktrees/w/.cargo/machine.key", ""),
        (".trust-cache/f.json", ""),
        ("crates/a/.trust-cache/f.json", ""),
        ("publish/.out/x.sh", "#!/bin/sh\n"),
        (".git/hooks/pre-commit.sh", "#!/bin/sh\n"),
        ("target/debug/x.sh", "#!/bin/sh\n"),
        (
            "crates/a/src/lib.rs",
            "// SPDX-License-Identifier: Apache-2.0\n",
        ),
    ]
    .iter()
    .map(|(p, c)| ((*p).to_string(), (*c).to_string()))
    .collect();
    let root = disk_tree("aterm-census-guards", "walk-dot-dirs", &files);
    let corpus = Corpus::at(&root);
    let _ = std::fs::remove_dir_all(&root);
    let corpus = corpus.expect("the synth tree reads");
    assert_eq!(
        corpus.paths(),
        [
            ".cargo/config.toml",
            ".cargo/machine.key",
            ".config/hooks/stop.sh",
            ".gitignore",
            "crates/a/.trust-cache/f.json",
            "crates/a/src/lib.rs",
            "tools/install.sh",
        ]
    );
    let scripts: Vec<&str> = corpus.scripts().iter().map(|s| s.rel.as_str()).collect();
    assert_eq!(scripts, ["tools/install.sh"]);
}

#[test]
fn a_scope_no_file_lies_under_is_a_finding() {
    fn nothing(_: &Tree<'_>, _: &mut Vec<Finding>) {}
    let rule = Rule {
        id: "scoped",
        why: "planted",
        scope: &["crates/aterm-grid/", "crates/aterm-renamed/"],
        check: nothing,
    };
    let corpus = Corpus::of(&[("crates/aterm-grid/src/lib.rs", "")]);
    let found = scope_findings(&corpus, std::slice::from_ref(&rule));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, "crates/aterm-renamed/");
}

/// A module's folder is rustc's: in a file that does not own its folder, a child found by name
/// resolves under `<stem>/`, a `#[path]` and an inline module's `#[path]` against the file's own
/// folder, and an inline module without one takes the stem on for what it holds; `mod r#type;`
/// names `type.rs`.
///
/// NEGATIVE CONTROLS: joining an inline module's `#[path]` under the stem gives
/// `src/a/foo/y.rs`; reading `r#` as part of no name loses `src/a/type.rs`.
#[test]
fn a_module_folder_is_the_one_rustc_resolves() {
    let corpus = Corpus::of(&[(
        "crates/x/src/a.rs",
        "#[path = \"foo\"]\npub mod m {\n    pub mod y;\n}\npub mod n {\n    pub mod z;\n}\n\
         pub mod r#type;\n",
    )]);
    let a = corpus.get("crates/x/src/a.rs").unwrap();
    let loads: Vec<Vec<String>> = a
        .loads_in(false, None)
        .iter()
        .map(|l| l.files.clone())
        .collect();
    let s = |p: &str| p.to_string();
    assert_eq!(
        loads,
        [
            vec![s("crates/x/src/foo/y.rs"), s("crates/x/src/foo/y/mod.rs")],
            vec![s("crates/x/src/a/n/z.rs"), s("crates/x/src/a/n/z/mod.rs")],
            vec![s("crates/x/src/a/type.rs"), s("crates/x/src/a/type/mod.rs")],
        ]
    );
}
