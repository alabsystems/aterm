// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::testing;

fn run(dir: &Path, args: &[&str]) {
    let out = testing::git()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn repo(at: &Path) {
    testing::init(
        at,
        &[
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
            ("commit.gpgsign", "false"),
        ],
    );
    std::fs::write(at.join(".gitignore"), "ignored/\n").unwrap();
    std::fs::write(at.join("a.rs"), "a").unwrap();
    std::fs::write(at.join("flagged.rs"), "f").unwrap();
    run(at, &["add", "."]);
    run(at, &["commit", "-q", "-m", "base"]);
}

fn keys(root: &Path) -> Vec<String> {
    dirty_against_head(root).unwrap().into_keys().collect()
}

#[test]
fn every_kind_of_dirt_is_listed_and_a_clean_tree_lists_none() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().join("w");
    repo(&root);
    assert!(keys(&root).is_empty(), "{:?}", keys(&root));

    std::fs::create_dir_all(root.join("ignored")).unwrap();
    std::fs::write(root.join("ignored/x"), "x").unwrap();
    std::fs::write(root.join(".git/info/exclude"), "local.rs\n").unwrap();
    std::fs::write(root.join("local.rs"), "only excluded by this checkout").unwrap();
    std::fs::write(root.join("a.rs"), "edited").unwrap();
    run(&root, &["update-index", "--skip-worktree", "flagged.rs"]);
    std::fs::write(root.join("flagged.rs"), "hidden edit").unwrap();
    assert_eq!(keys(&root), ["a.rs", "flagged.rs", "local.rs"]);
}

#[test]
fn the_strict_judge_reads_the_bytes_git_diff_trusts() {
    use std::time::{Duration, SystemTime};
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().join("w");
    repo(&root);
    let old = SystemTime::now() - Duration::from_secs(100);
    let set_old = |p: &Path| {
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(old)
            .unwrap();
    };
    std::fs::write(root.join("lf.txt"), "a\nb\n").unwrap();
    std::fs::write(root.join("same.rs"), "aaaa").unwrap();
    set_old(&root.join("same.rs"));
    std::fs::write(root.join("run.sh"), "x").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::os::unix::fs::symlink("a.rs", root.join("link")).unwrap();
    }
    run(&root, &["add", "."]);
    run(&root, &["commit", "-q", "-m", "more"]);
    assert_eq!(strictly_clean(&root), Some(true));

    run(&root, &["config", "core.autocrlf", "input"]);
    std::fs::write(root.join("lf.txt"), "a\r\nb\r\n").unwrap();
    assert!(keys(&root).is_empty(), "the case: {:?}", keys(&root));
    assert_eq!(raw_differences(&root).unwrap(), ["lf.txt"]);
    assert_eq!(strictly_clean(&root), Some(false));
    std::fs::write(root.join("lf.txt"), "a\nb\n").unwrap();
    assert_eq!(strictly_clean(&root), Some(true));

    run(&root, &["config", "core.checkStat", "minimal"]);
    run(&root, &["config", "core.trustctime", "false"]);
    std::fs::write(root.join("same.rs"), "bbbb").unwrap();
    set_old(&root.join("same.rs"));
    assert!(keys(&root).is_empty(), "the case: {:?}", keys(&root));
    assert_eq!(raw_differences(&root).unwrap(), ["same.rs"]);
    std::fs::write(root.join("same.rs"), "aaaa").unwrap();
    assert_eq!(strictly_clean(&root), Some(true));

    run(&root, &["update-index", "--assume-unchanged", "a.rs"]);
    assert_eq!(raw_differences(&root).unwrap(), ["a.rs"]);
    run(&root, &["update-index", "--no-assume-unchanged", "a.rs"]);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        run(&root, &["config", "core.fileMode", "false"]);
        std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert_eq!(raw_differences(&root).unwrap(), ["run.sh"]);
        std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::fs::remove_file(root.join("link")).unwrap();
        std::os::unix::fs::symlink("flagged.rs", root.join("link")).unwrap();
        assert_eq!(raw_differences(&root).unwrap(), ["link"]);
    }
}

/// The tracked files are the index's, an initialised submodule's own under its path; no
/// untracked or ignored file is one, in the checkout or in the submodule.
#[test]
fn the_tracked_files_are_the_indexes_a_submodules_included() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    repo(&sub);
    let root = dir.path().join("w");
    repo(&root);
    let url = sub.to_str().unwrap();
    let file = "protocol.file.allow=always";
    run(
        &root,
        &["-c", file, "submodule", "add", "-q", url, "vendor/sub"],
    );
    run(&root, &["commit", "-q", "-m", "sub"]);
    for loose in [
        "ignored/x.rs",
        "loose.rs",
        "vendor/sub/ignored/y.rs",
        "vendor/sub/z.rs",
    ] {
        let path = root.join(loose);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "not tracked").unwrap();
    }
    let tracked: Vec<String> = tracked_files(&root).unwrap().into_iter().collect();
    assert_eq!(
        tracked,
        [
            ".gitignore",
            ".gitmodules",
            "a.rs",
            "flagged.rs",
            "vendor/sub/.gitignore",
            "vendor/sub/a.rs",
            "vendor/sub/flagged.rs",
        ]
    );
}

#[test]
#[cfg(unix)]
fn a_gitlink_replaced_by_an_outside_repository_symlink_enumerates_no_outside_rows() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub-source");
    let outside = dir.path().join("outside");
    let root = dir.path().join("w");
    repo(&sub);
    repo(&outside);
    repo(&root);
    std::fs::write(
        outside.join("outside-only.rs"),
        "private fixture outside the index",
    )
    .unwrap();
    run(&outside, &["add", "outside-only.rs"]);
    run(&outside, &["commit", "-q", "-m", "outside row"]);
    run(
        &root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            sub.to_str().unwrap(),
            "vendor/sub",
        ],
    );
    run(&root, &["commit", "-q", "-m", "indexed gitlink"]);
    assert!(
        tracked_files(&root).unwrap().contains("vendor/sub/a.rs"),
        "a real populated gitlink still enumerates its tracked files"
    );
    assert!(
        tracked_files(&outside).unwrap().contains("outside-only.rs"),
        "the symlink target is a real repository with an independently tracked row"
    );

    let link = root.join("vendor/sub");
    std::fs::rename(&link, dir.path().join("parked-sub")).unwrap();
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    assert!(
        link.is_dir() && is_toplevel(&link),
        "the former following-directory condition admits this outside target"
    );
    let tracked = tracked_files(&root).unwrap();
    assert_eq!(
        tracked,
        [".gitignore", ".gitmodules", "a.rs", "flagged.rs"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>()
    );
    assert!(!tracked.contains("vendor/sub/outside-only.rs"));
    let dirty = dirty_against_head(&root).unwrap();
    assert_eq!(dirty.get("vendor/sub"), Some(&Dirt::Changed));
    assert!(dirty.keys().all(|path| !path.starts_with("vendor/sub/")));
}

#[test]
fn a_submodule_is_judged_by_its_own_checkout() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    repo(&sub);
    let root = dir.path().join("w");
    repo(&root);
    run(
        &root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            sub.to_str().unwrap(),
            "vendor/sub",
        ],
    );
    run(&root, &["commit", "-q", "-m", "sub"]);
    assert!(keys(&root).is_empty(), "{:?}", keys(&root));
    assert_eq!(strictly_clean(&root), Some(true));

    std::fs::write(root.join("vendor/sub/a.rs"), "edited beneath").unwrap();
    assert_eq!(keys(&root), ["vendor/sub", "vendor/sub/a.rs"]);
    assert_eq!(raw_differences(&root).unwrap(), ["vendor/sub/a.rs"]);
    std::fs::write(root.join("vendor/sub/a.rs"), "a").unwrap();
    assert_eq!(strictly_clean(&root), Some(true));

    // NEGATIVE CONTROL: moving the submodule to a different clean commit is caught both by
    // `raw_differences` (even with `submodule.<name>.ignore=all`) and by `dirty_against_head`
    // when the gitlink is marked `--skip-worktree`.
    std::fs::write(root.join("vendor/sub/extra.rs"), "second").unwrap();
    run(&root.join("vendor/sub"), &["add", "extra.rs"]);
    run(&root.join("vendor/sub"), &["commit", "-q", "-m", "second"]);
    assert_eq!(raw_differences(&root).unwrap(), ["vendor/sub"]);
    run(&root, &["update-index", "--skip-worktree", "vendor/sub"]);
    assert_eq!(keys(&root), ["vendor/sub"]);
    assert_eq!(strictly_clean(&root), Some(false));
    run(&root, &["update-index", "--no-skip-worktree", "vendor/sub"]);
    run(&root.join("vendor/sub"), &["checkout", "-q", "HEAD~1"]);
    assert_eq!(strictly_clean(&root), Some(true));

    run(&root, &["submodule", "deinit", "-q", "-f", "vendor/sub"]);
    let found = dirty_against_head(&root).unwrap();
    assert_eq!(
        found.get("vendor/sub"),
        Some(&Dirt::Unpopulated),
        "{found:?}"
    );

    let link = root.join("vendor/sub");
    std::fs::remove_dir(&link).unwrap();
    std::fs::write(&link, "a file").unwrap();
    let found = dirty_against_head(&root).unwrap();
    assert_eq!(found.get("vendor/sub"), Some(&Dirt::Changed), "{found:?}");

    #[cfg(unix)]
    {
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink("../..", &link).unwrap();
        let found = dirty_against_head(&root).unwrap();
        assert_eq!(found.get("vendor/sub"), Some(&Dirt::Changed), "{found:?}");
        assert!(
            found.keys().all(|p| !p.starts_with("vendor/sub/")),
            "{found:?}"
        );
    }
}
