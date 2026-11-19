// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn git(root: &Path, args: &[&str]) -> String {
    identity(Path::new("git"), root, args).unwrap()
}

fn commit(root: &Path) {
    git(
        root,
        &[
            "-c",
            "user.name=Andrew Yates",
            "-c",
            "user.email=test@aterm.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "source",
        ],
    );
}

/// Identity evidence must be complete, including at the collector's inclusive bound. Overflow
/// is a refusal, not a truncated compiler identity; a failed child still supplies no identity.
#[cfg(unix)]
#[test]
fn identity_keeps_complete_output_and_refuses_overflow_or_failed_children() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let source = dir.path().join("identity");
    let source_arg = source.to_str().unwrap();
    let cat = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
    let cap = aterm_uds::deadline::OUTPUT_KEEP;
    for len in [0, cap, cap + 1] {
        let expected = "i".repeat(len);
        std::fs::write(&source, &expected).unwrap();
        let read = identity(&cat, dir.path(), &[source_arg]);
        if len <= cap {
            assert_eq!(read.unwrap(), expected);
        } else {
            assert!(read.is_err(), "an oversized identity cannot be evidence");
        }
    }
    std::fs::write(&source, b" \ncompiler identity\n\t ").unwrap();
    assert_eq!(
        identity(&cat, dir.path(), &[source_arg]).unwrap(),
        "compiler identity"
    );
    let missing = dir.path().join("missing-identity");
    let failed = identity(&cat, dir.path(), &[missing.to_str().unwrap()]).unwrap_err();
    assert!(failed.to_string().contains("missing-identity"), "{failed}");
}

#[test]
fn dirty_sources_never_supply_head_tree_evidence() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    std::fs::write(root.join("source"), b"original").unwrap();
    git(root, &["add", "source"]);
    commit(root);
    let compiled_tree = clean_tree(root).unwrap();
    std::fs::write(root.join("source"), b"changed").unwrap();
    assert!(clean_tree(root).is_err());
    std::fs::write(root.join("source"), b"original").unwrap();
    assert_eq!(clean_tree(root).unwrap(), compiled_tree);

    for (hide, reveal) in [
        ("--assume-unchanged", "--no-assume-unchanged"),
        ("--skip-worktree", "--no-skip-worktree"),
    ] {
        git(root, &["update-index", hide, "source"]);
        std::fs::write(root.join("source"), b"changed").unwrap();
        assert!(git(root, &["status", "--porcelain"]).is_empty());
        assert!(clean_tree(root).is_err());
        assert_ne!(git(root, &["ls-files", "-v"]), "H source");
        std::fs::write(root.join("source"), b"original").unwrap();
        git(root, &["update-index", reveal, "source"]);
        assert_eq!(clean_tree(root).unwrap(), compiled_tree);
    }
    std::fs::write(root.join("untracked"), b"new input").unwrap();
    assert!(clean_tree(root).is_err());
}

#[test]
fn unread_git_index_never_supplies_head_tree_evidence() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    std::fs::write(root.join("source"), b"original").unwrap();
    git(root, &["add", "source"]);
    commit(root);
    assert!(clean_tree(root).is_ok());

    std::fs::write(root.join(".git/index"), b"invalid index").unwrap();
    assert!(identity(Path::new("git"), root, &["rev-parse", "HEAD^{tree}"]).is_ok());
    assert!(clean_tree(root).is_err());
}

#[test]
fn test_index_requires_clean_populated_submodules_at_their_saved_commit() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin");
    let root = dir.path().join("root");
    for path in [&origin, &root] {
        std::fs::create_dir(path).unwrap();
        git(path, &["init", "-q"]);
    }
    std::fs::write(origin.join("source"), b"original").unwrap();
    git(&origin, &["add", "source"]);
    commit(&origin);
    git(
        &root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--",
            origin.to_str().unwrap(),
            "nested",
        ],
    );
    commit(&root);
    git(&root, &["config", "submodule.nested.ignore", "all"]);
    let tree = clean_tree(&root).unwrap();
    let nested = root.join("nested");

    std::fs::write(nested.join("source"), b"changed").unwrap();
    assert!(git(&root, &["status", "--porcelain"]).is_empty());
    assert!(clean_tree(&root).is_err());
    git(&nested, &["add", "source"]);
    commit(&nested);
    assert!(git(&root, &["status", "--porcelain"]).is_empty());
    assert!(clean_tree(&root).is_err());
    git(&nested, &["checkout", "--detach", "HEAD~1"]);
    assert_eq!(clean_tree(&root).unwrap(), tree);

    git(&nested, &["update-index", "--skip-worktree", "source"]);
    std::fs::write(nested.join("source"), b"changed").unwrap();
    assert!(git(&nested, &["status", "--porcelain"]).is_empty());
    assert!(clean_tree(&root).is_err());
    std::fs::write(nested.join("source"), b"original").unwrap();
    git(&nested, &["update-index", "--no-skip-worktree", "source"]);
    assert_eq!(clean_tree(&root).unwrap(), tree);

    git(&root, &["submodule", "deinit", "-f", "--", "nested"]);
    assert!(clean_tree(&root).is_err());
}

#[test]
fn partial_compiles_keep_other_targets_only_on_the_same_tree_and_compiler() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("test-executables.json");
    let entry = |name: &str| Entry {
        package: name.into(),
        kind: "lib".into(),
        name: name.into(),
        manifest_path: PathBuf::from(name),
        path: PathBuf::from(name),
        sha256: String::new(),
    };
    let index = |tree: &str, compiler: &str, name: &str| Index {
        tree: tree.into(),
        toolchain: compiler.into(),
        binaries: vec![entry(name)],
    };
    index("tree", "compiler", "first").write(&path).unwrap();
    index("tree", "compiler", "second").write(&path).unwrap();
    assert_eq!(Index::read(&path).unwrap().binaries.len(), 2);
    index("other", "compiler", "third").write(&path).unwrap();
    assert_eq!(Index::read(&path).unwrap().binaries.len(), 1);
    index("other", "new compiler", "fourth")
        .write(&path)
        .unwrap();
    assert_eq!(Index::read(&path).unwrap().binaries.len(), 1);
}

#[test]
fn only_the_same_tree_compiler_target_and_bytes_are_reused() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    let binary = root.join("aterm_gui-0123456789abcdef");
    std::fs::write(&binary, b"binary").unwrap();
    let messages = aterm_json::json!({"reason":"compiler-artifact", "package_id":"aterm-gui", "manifest_path":root.join("crates/aterm-gui/Cargo.toml"), "target":{"kind":["lib"],"name":"aterm_gui"}, "profile":{"test":true},"executable":binary}).to_string();
    let index = Index::from_messages("tree".into(), "compiler".into(), &messages).unwrap();
    let path = root.join("verify/test-executables.json");
    index.write(&path).unwrap();
    let mut index = Index::read(&path).unwrap();
    assert!(index.fixture_binary("tree", "compiler", root).is_some());
    assert!(index.fixture_binary("other", "compiler", root).is_none());
    assert!(index.fixture_binary("tree", "other", root).is_none());
    index.binaries[0].kind = "bin".into();
    assert!(index.fixture_binary("tree", "compiler", root).is_none());
    index.binaries[0].kind = "lib".into();
    std::fs::write(&binary, b"changed").unwrap();
    assert!(index.fixture_binary("tree", "compiler", root).is_none());
    std::fs::remove_file(&binary).unwrap();
    assert!(index.fixture_binary("tree", "compiler", root).is_none());
}
