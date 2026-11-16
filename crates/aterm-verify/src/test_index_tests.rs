// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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
    identity(Path::new("git"), root, &["init", "-q"]).unwrap();
    std::fs::write(root.join("source"), b"original").unwrap();
    identity(Path::new("git"), root, &["add", "source"]).unwrap();
    identity(
        Path::new("git"),
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
    )
    .unwrap();
    assert!(clean_tree(root).is_ok());
    std::fs::write(root.join("source"), b"changed").unwrap();
    assert!(clean_tree(root).is_err());
    std::fs::write(root.join("source"), b"original").unwrap();
    assert!(clean_tree(root).is_ok());
    std::fs::write(root.join("untracked"), b"new input").unwrap();
    assert!(clean_tree(root).is_err());
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
