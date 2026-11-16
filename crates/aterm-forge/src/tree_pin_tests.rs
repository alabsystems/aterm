// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::attest::tests::git;
use std::path::PathBuf;

const DIR: &str = "vendor/up";
const PIN: &str = "vendor/up.pin";
const TAG: &str = "v0.8.0";

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// A repository whose workspace is `root/<workspace>` and which commits a copy at `vendor/up`
/// and its pin `vendor/up.pin`, as `tools/pin` then `git add -f` leave them, with `target`
/// ignored. Returns the folder, the workspace root and the copy's tree.
pub(crate) fn pinned_repo(
    tag: &str,
    workspace: &str,
) -> (aterm_tempfile::TempDir, PathBuf, String) {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-forge-tree-pin-{tag}-"))
        .tempdir()
        .unwrap();
    git(dir.path(), &["init", "-q"]);
    let root = dir.path().join(workspace);
    write(&root, ".gitignore", "target\n");
    write(
        &root,
        "vendor/up/Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(&root, "vendor/up/crates/a/src/lib.rs", "pub fn a() {}\n");
    write(&root, "vendor/up/tools/pin", "#!/bin/sh\n");
    git(&root, &["add", "-f", ".gitignore", DIR]);
    let prefix = git(&root, &["rev-parse", "--show-prefix"]);
    let tree = git(&root, &["write-tree", &format!("--prefix={prefix}{DIR}/")]);
    write(&root, PIN, &format!("tag={TAG}\ntree={tree}\n"));
    git(&root, &["add", "-f", PIN]);
    git(&root, &["commit", "-q", "--no-verify", "-m", "pin up"]);
    (dir, root, tree)
}

fn fail(result: Result<Pin, TreePinError>) -> String {
    match result {
        Err(TreePinError::Fail(why)) => why,
        other => panic!("expected a FAIL, got {other:?}"),
    }
}

#[test]
fn a_copy_that_is_its_pinned_tree_holds() {
    for workspace in ["", "ws"] {
        let (_dir, root, tree) = pinned_repo("match", workspace);
        assert_eq!(
            hold(&root, DIR, PIN),
            Ok(Pin {
                tag: TAG.into(),
                tree: tree.clone()
            }),
            "workspace {workspace:?}"
        );
    }
}

#[test]
fn the_check_writes_a_private_index_so_a_held_lock_does_not_refuse_it() {
    let (_dir, root, tree) = pinned_repo("locked", "");
    let lock = root.join(".git/index.lock");
    std::fs::write(&lock, "").unwrap();
    let held = hold(&root, DIR, PIN);
    std::fs::remove_file(&lock).unwrap();
    assert_eq!(held.map(|p| p.tree), Ok(tree));
}

#[test]
fn a_staged_repin_holds_before_it_is_committed() {
    let (_dir, root, old) = pinned_repo("staged-repin", "");
    write(
        &root,
        "vendor/up/crates/a/src/lib.rs",
        "pub fn a() -> u8 { 1 }\n",
    );
    git(&root, &["add", "-f", DIR]);
    let tree = git(&root, &["write-tree", &format!("--prefix={DIR}/")]);
    assert_ne!(tree, old);
    write(&root, PIN, &format!("tag=v0.8.1\ntree={tree}\n"));
    git(&root, &["add", "-f", PIN]);
    assert_eq!(
        hold(&root, DIR, PIN),
        Ok(Pin {
            tag: "v0.8.1".into(),
            tree
        })
    );
}

#[test]
fn a_copy_that_is_another_tree_fails_with_the_repin_repair() {
    let (_dir, root, tree) = pinned_repo("wrong-tree", "");
    // An edit made here and staged: the index's copy is no longer the pinned tag.
    write(
        &root,
        "vendor/up/crates/a/src/lib.rs",
        "pub fn a() { edited() }\n",
    );
    git(&root, &["add", "-f", DIR]);
    let have = git(&root, &["write-tree", &format!("--prefix={DIR}/")]);
    let why = fail(hold(&root, DIR, PIN));
    assert!(
        why.contains(&format!("the index holds `{DIR}` as tree {have}"))
            && why.contains(&format!("names tree {tree} ({TAG})"))
            && why.contains(&format!(
                "~/alab-harness/tools/pin {} {TAG}",
                root.display()
            )),
        "{why}"
    );
    // A pin that names some other tree fails the same way.
    git(&root, &["reset", "-q", "--hard"]);
    let other = "0123456789abcdef0123456789abcdef01234567";
    write(&root, PIN, &format!("tag={TAG}\ntree={other}\n"));
    git(&root, &["add", "-f", PIN]);
    let why = fail(hold(&root, DIR, PIN));
    assert!(
        why.contains(&format!("as tree {tree}")) && why.contains(&format!("names tree {other}")),
        "{why}"
    );
}

#[test]
fn an_unstaged_change_to_the_copy_or_its_pin_is_a_dirty_copy() {
    let (_dir, root, _) = pinned_repo("dirty", "");
    write(
        &root,
        "vendor/up/crates/a/src/lib.rs",
        "pub fn a() { unstaged() }\n",
    );
    let why = fail(hold(&root, DIR, PIN));
    assert!(
        why.contains("the working copy changes")
            && why.contains("vendor/up/crates/a/src/lib.rs")
            && why.contains("~/alab-harness/tools/pin"),
        "{why}"
    );
    git(&root, &["checkout", "-q", "--", DIR]);
    std::fs::remove_file(root.join("vendor/up/tools/pin")).unwrap();
    let why = fail(hold(&root, DIR, PIN));
    assert!(why.contains("vendor/up/tools/pin"), "{why}");
    git(&root, &["checkout", "-q", "--", DIR]);
    assert!(hold(&root, DIR, PIN).is_ok());
    // The pin edited but not staged: the index still names the old tree, the file another.
    let text = std::fs::read_to_string(root.join(PIN)).unwrap();
    write(&root, PIN, &text.replace(TAG, "v0.8.9"));
    let why = fail(hold(&root, DIR, PIN));
    assert!(why.contains(PIN) && why.contains("unstaged"), "{why}");
}

#[test]
fn an_untracked_or_ignored_path_in_the_copy_fails() {
    let (_dir, root, _) = pinned_repo("untracked", "");
    write(
        &root,
        "vendor/up/crates/a/src/extra.rs",
        "pub fn extra() {}\n",
    );
    let why = fail(hold(&root, DIR, PIN));
    assert!(why.contains("vendor/up/crates/a/src/extra.rs"), "{why}");
    std::fs::remove_file(root.join("vendor/up/crates/a/src/extra.rs")).unwrap();
    assert!(hold(&root, DIR, PIN).is_ok());
    // A build inside the copy: `target` is ignored here, and still refused there.
    write(&root, "vendor/up/target/debug/out", "");
    let why = fail(hold(&root, DIR, PIN));
    assert!(
        why.contains("vendor/up/target/") && why.contains("git clean -dfx -- vendor/up"),
        "{why}"
    );
}

#[test]
fn a_copy_the_index_does_not_track_fails() {
    let (_dir, root, _) = pinned_repo("untracked-copy", "");
    git(&root, &["rm", "-q", "-r", "--cached", DIR]);
    let why = fail(hold(&root, DIR, PIN));
    assert!(
        why.contains(&format!("the index tracks nothing at `{DIR}`"))
            && why.contains("~/alab-harness/tools/pin"),
        "{why}"
    );
}

/// Commit `path` as `base`, then `ours` on top of it and `theirs` beside it, and leave the
/// checkout at `ours` with git's three-way merge of the three in the index, unresolved where
/// both sides changed `path` (`read-tree -m` leaves each side at its stage, as a conflicted
/// `git merge` does, and writes no working file).
fn conflict(root: &Path, path: &str, base: &str, ours: &str, theirs: &str) {
    write(root, path, base);
    git(root, &["add", "-f", path]);
    git(
        root,
        &["commit", "-q", "--no-verify", "--allow-empty", "-m", "base"],
    );
    let base = git(root, &["rev-parse", "HEAD"]);
    write(root, path, theirs);
    git(root, &["commit", "-q", "--no-verify", "-a", "-m", "theirs"]);
    let theirs = git(root, &["rev-parse", "HEAD"]);
    git(root, &["checkout", "-q", "--detach", &base]);
    write(root, path, ours);
    git(root, &["commit", "-q", "--no-verify", "-a", "-m", "ours"]);
    git(root, &["read-tree", "-m", &base, "HEAD", &theirs]);
}

/// Whether git refuses to write a tree of the whole index at `root`, as it does while any entry
/// is unmerged.
fn whole_index_refused(root: &Path) -> bool {
    !command(aterm_uds::os_path!("git"))
        .args(["write-tree"])
        .current_dir(root)
        .output_owned()
        .unwrap()
        .status
        .success()
}

#[test]
fn a_merge_left_unresolved_elsewhere_does_not_touch_the_copy() {
    let (_dir, root, tree) = pinned_repo("conflict-elsewhere", "");
    conflict(&root, "other", "base\n", "ours\n", "theirs\n");
    assert!(
        git(&root, &["ls-files", "-u", "--", "other"])
            .lines()
            .count()
            == 3
            && whole_index_refused(&root),
        "the checkout holds no unresolved merge, so this tests nothing"
    );
    assert_eq!(
        hold(&root, DIR, PIN),
        Ok(Pin {
            tag: TAG.into(),
            tree
        })
    );
}

#[test]
fn a_merge_left_unresolved_inside_the_copy_fails() {
    let (_dir, root, _) = pinned_repo("conflict-inside", "");
    let path = "vendor/up/crates/a/src/lib.rs";
    conflict(
        &root,
        path,
        "pub fn a() {}\n",
        "pub fn a() -> u8 { 1 }\n",
        "pub fn a() -> u8 { 2 }\n",
    );
    assert!(whole_index_refused(&root));
    let why = fail(hold(&root, DIR, PIN));
    assert!(
        why.contains(&format!("unresolved merge inside `{DIR}` ({path})"))
            && why.contains("~/alab-harness/tools/pin"),
        "{why}"
    );
}

#[test]
fn unmerged_entries_are_those_at_a_stage_other_than_zero_each_path_once() {
    let entries = "100644 1111111111111111111111111111111111111111 0\tvendor/up/a\0\
                   100644 2222222222222222222222222222222222222222 1\tvendor/up/b c\0\
                   100644 3333333333333333333333333333333333333333 2\tvendor/up/b c\0\
                   100644 4444444444444444444444444444444444444444 3\tvendor/up/b c\0\
                   100644 5555555555555555555555555555555555555555 0\tvendor/up/d\0\
                   100644 6666666666666666666666666666666666666666 3\tvendor/up/e\0\
                   garbled\0";
    assert_eq!(
        unmerged(entries),
        ["vendor/up/b c", "vendor/up/e", "garbled"]
    );
    assert!(unmerged("").is_empty());
}

#[test]
fn a_missing_or_garbled_pin_fails() {
    let (_dir, root, tree) = pinned_repo("pin-file", "");
    std::fs::remove_file(root.join(PIN)).unwrap();
    let why = fail(hold(&root, DIR, PIN));
    assert!(
        why.contains(&format!("`{PIN}` does not exist"))
            && why.contains("~/alab-harness/tools/pin"),
        "{why}"
    );
    for garbled in [
        String::new(),
        format!("tag={TAG}\n"),
        format!("tag={TAG}\ntree={tree}"),
        format!("tree={tree}\ntag={TAG}\n"),
        format!("tag={TAG}\ntree={}\n", tree.to_uppercase()),
        format!("tag=main\ntree={tree}\n"),
    ] {
        write(&root, PIN, &garbled);
        let why = fail(hold(&root, DIR, PIN));
        assert!(
            why.contains(&format!("`{PIN}` is not the two lines")),
            "{garbled:?}: {why}"
        );
    }
}

#[test]
fn the_pin_is_exactly_the_bytes_tools_pin_writes() {
    let tree = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(
        parse_pin(&format!("tag=v0.8.0\ntree={tree}\n")),
        Ok(Pin {
            tag: "v0.8.0".into(),
            tree: tree.into()
        })
    );
    for (text, why) in [
        (String::new(), "has no `tag=` line"),
        ("tag=v0.8.0\n".to_owned(), "has no `tree=` line"),
        (
            format!("tag=v0.8.0\ntree={tree}"),
            "does not end in a newline",
        ),
        (
            format!("tag=v0.8.0\r\ntree={tree}\r\n"),
            "not 40 lowercase hex",
        ),
        (
            format!("tree={tree}\ntag=v0.8.0\n"),
            "is not the `tag=` line",
        ),
        (
            format!(" tag=v0.8.0\ntree={tree}\n"),
            "is not the `tag=` line",
        ),
        (
            format!("tag=v0.8.0\ntree={tree}\n\n"),
            "goes on past the tree line",
        ),
        (
            format!("tag=v0.8.0\ntree={tree}\ntree={tree}\n"),
            "goes on past",
        ),
        (
            format!("tag=v0.8.0\ntree={}\n", &tree[..39]),
            "not 40 lowercase hex",
        ),
        (
            format!("tag=v0.8.0\ntree={tree}0\n"),
            "not 40 lowercase hex",
        ),
        (
            format!("tag=v0.8.0\ntree={}\n", tree.to_uppercase()),
            "not 40 lowercase hex",
        ),
        (
            format!("tag=v0.8.0\ntree={}g\n", &tree[..39]),
            "not 40 lowercase hex",
        ),
        (format!("tag=0.8.0\ntree={tree}\n"), "no release tag"),
        (format!("tag=\ntree={tree}\n"), "no release tag"),
    ] {
        let got = parse_pin(&text);
        assert!(
            got.as_ref().is_err_and(|e| e.contains(why)),
            "{text:?}: {got:?}"
        );
    }
}

#[test]
fn a_release_tag_is_what_tools_pin_accepts() {
    // tools/pin: `case $tag in v[0-9]*.[0-9]*.[0-9]*) ;; *) refuse`.
    for tag in ["v0.8.0", "v10.20.30", "v1.2.3-rc1", "v1..2.3", "v1.2.3.4"] {
        assert!(release_tag(tag), "{tag}");
    }
    for tag in [
        "", "v", "0.8.0", "v.1.2.3", "v1.2", "v1.2.", "vx.2.3", "v1.x.3", "V1.2.3", "main",
    ] {
        assert!(!release_tag(tag), "{tag}");
    }
}

#[test]
fn status_lines_count_only_what_the_index_does_not_hold() {
    let status = "M  staged.rs\0 M unstaged.rs\0MM both.rs\0A  added.rs\0D  gone.rs\0 D deleted.rs\0\
                  R  new.rs\0old.rs\0?? untracked.rs\0!! target/\0UU conflicted.rs\0";
    assert_eq!(
        unstaged(status),
        [
            "unstaged.rs",
            "both.rs",
            "deleted.rs",
            "untracked.rs",
            "target/",
            "conflicted.rs"
        ]
    );
    assert!(unstaged("").is_empty());
}

#[test]
fn a_root_in_no_checkout_could_not_run() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-forge-tree-pin-no-git-")
        .tempdir()
        .unwrap();
    let root = dir.path();
    // A `.git` file naming no repository stops git's search here, whatever folder holds this one.
    std::fs::write(
        root.join(".git"),
        "gitdir: /nonexistent/aterm-forge-tree-pin\n",
    )
    .unwrap();
    write(root, PIN, &format!("tag={TAG}\ntree={}\n", "a".repeat(40)));
    match hold(root, DIR, PIN) {
        Err(TreePinError::CouldNotRun(why)) => {
            assert!(why.contains("needs the git checkout"), "{why}");
        }
        other => panic!("expected COULD NOT RUN, got {other:?}"),
    }
}

#[test]
fn the_roster_applies_only_to_its_own_workspace() {
    let (_dir, root, tree) = pinned_repo("roster", "");
    assert_eq!(tree_pins(&root), Ok(Vec::new()));
    assert_eq!(
        tree_pin(&root, DIR, PIN),
        Ok(TreePin {
            dir: DIR,
            pin: PIN,
            tag: TAG.into(),
            tree,
        })
    );
    write(&root, "vendor/up/crates/a/src/lib.rs", "//! Changed.\n");
    assert!(matches!(
        tree_pin(&root, DIR, PIN),
        Err(TreePinError::Fail(_))
    ));
}

#[test]
fn every_pinned_roster_row_keeps_its_pin_beside_its_directory() {
    for row in crate::provenance::FIRST_PARTY_VENDORED {
        if let Some(pin) = row.pin {
            assert_eq!(pin, format!("{}.pin", row.dir), "{}", row.dir);
        }
    }
}
