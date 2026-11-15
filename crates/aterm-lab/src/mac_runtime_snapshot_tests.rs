// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn commit(repo: &Path, message: &str) -> String {
    git(repo, ["add", "."]).unwrap();
    git(
        repo,
        [
            "-c",
            "user.name=Andrew Yates",
            "-c",
            "user.email=lab@localhost",
            "commit",
            "-qm",
            message,
        ],
    )
    .unwrap();
    git(repo, ["rev-parse", "HEAD"]).unwrap()
}

fn fixture(root: &Path) -> (std::path::PathBuf, String, String, String) {
    let submodule = root.join("astream");
    let repo = root.join("repo");
    for dir in [&submodule, &repo] {
        std::fs::create_dir(dir).unwrap();
        git(dir, ["init", "-q", "--initial-branch=main"]).unwrap();
    }
    std::fs::write(submodule.join("old"), "parent-only submodule bytes").unwrap();
    commit(&submodule, "old submodule");
    std::fs::remove_file(submodule.join("old")).unwrap();
    std::fs::write(submodule.join("current"), "pinned submodule bytes").unwrap();
    let pin = commit(&submodule, "pinned submodule");
    std::fs::create_dir(repo.join("vendor")).unwrap();
    clone(&submodule, &repo.join("vendor/astream"), &pin).unwrap();
    std::fs::write(
        repo.join(".gitmodules"),
        "[submodule \"vendor/astream\"]\npath = vendor/astream\nurl = ../astream\n",
    )
    .unwrap();
    std::fs::write(repo.join("parent-only"), "absent from selected tree").unwrap();
    let parent = commit(&repo, "parent");
    git(&repo, ["checkout", "-qb", "side"]).unwrap();
    std::fs::write(repo.join("side"), "merged bytes").unwrap();
    commit(&repo, "side");
    git(&repo, ["checkout", "-q", "main"]).unwrap();
    std::fs::remove_file(repo.join("parent-only")).unwrap();
    std::fs::write(repo.join("kept"), "selected source bytes").unwrap();
    std::fs::write(repo.join(".gitattributes"), "kept export-ignore\n").unwrap();
    commit(&repo, "main");
    git(
        &repo,
        [
            "-c",
            "user.name=Andrew Yates",
            "-c",
            "user.email=lab@localhost",
            "merge",
            "--no-ff",
            "-qm",
            "selected merge",
            "side",
        ],
    )
    .unwrap();
    let head = git(&repo, ["rev-parse", "HEAD"]).unwrap();
    (repo, head, pin, parent)
}

#[test]
fn portable_snapshot_keeps_original_merge_and_gitlink_without_host_objects_or_history() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (repo, head, pin, parent) = fixture(scratch.path());
    let linked = scratch.path().join("linked");
    for (origin, target, oid) in [
        (repo.clone(), linked.clone(), &head),
        (
            repo.join("vendor/astream"),
            linked.join("vendor/astream"),
            &pin,
        ),
    ] {
        git(
            &origin,
            [
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("--quiet"),
                OsStr::new("--detach"),
                target.as_os_str(),
                OsStr::new(oid),
            ],
        )
        .unwrap();
    }
    assert!(linked.join(".git").is_file());
    assert!(linked.join("vendor/astream/.git").is_file());
    assert_eq!(astream_pin(&linked, &head).unwrap(), pin);
    let tree = git(&repo, ["rev-parse", "HEAD^{tree}"]).unwrap();
    let share = scratch.path().join("share");
    std::fs::create_dir(&share).unwrap();
    archive(&linked, &head, &pin, &share).unwrap();
    std::fs::remove_dir_all(&linked).unwrap();
    std::fs::remove_dir_all(&repo).unwrap();
    std::fs::remove_dir_all(scratch.path().join("astream")).unwrap();
    std::fs::remove_dir_all(share.join("source")).unwrap();
    let restored = scratch.path().join("restored");
    std::fs::create_dir(&restored).unwrap();
    completed(
        command("/usr/bin/tar")
            .arg("-xf")
            .arg(share.join("source.tar"))
            .arg("-C")
            .arg(&restored),
        "restore snapshot fixture",
    )
    .unwrap();
    assert_eq!(git(&restored, ["rev-parse", "HEAD"]).unwrap(), head);
    assert_eq!(git(&restored, ["rev-parse", "HEAD^{tree}"]).unwrap(), tree);
    assert_eq!(astream_pin(&restored, &head).unwrap(), pin);
    assert_eq!(
        std::fs::read_to_string(restored.join("kept")).unwrap(),
        "selected source bytes"
    );
    for dir in [&restored, &restored.join("vendor/astream")] {
        assert_eq!(git(dir, ["rev-list", "--count", "HEAD"]).unwrap(), "1");
        assert!(git(dir, ["remote"]).unwrap().is_empty());
        assert!(!dir.join(".git/objects/info/alternates").exists());
        git(dir, ["fsck", "--full", "--strict"]).unwrap();
    }
    assert!(git(&restored, ["cat-file", "-e", &parent]).is_err());
    assert!(
        git(&restored, ["status", "--porcelain"])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn changed_source_or_submodule_cannot_publish_a_snapshot_with_the_captured_identity() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (repo, head, pin, _) = fixture(scratch.path());
    let share = scratch.path().join("share");
    std::fs::create_dir(&share).unwrap();
    std::fs::write(repo.join("later"), "later source").unwrap();
    let later = commit(&repo, "later source");
    assert!(archive(&repo, &head, &pin, &share).is_err());
    assert!(!share.join("source.tar").exists());
    std::fs::remove_dir_all(share.join("source")).unwrap();
    std::fs::write(repo.join("vendor/astream/current"), "dirty tracked bytes").unwrap();
    assert!(astream_pin(&repo, &later).is_err());
    assert!(archive(&repo, &later, &pin, &share).is_err());
    assert!(!share.join("source.tar").exists());
    std::fs::remove_dir_all(share.join("source")).unwrap();
    let moved = commit(&repo.join("vendor/astream"), "moved submodule");
    assert_ne!(moved, pin);
    assert!(astream_pin(&repo, &later).is_err());
    assert!(archive(&repo, &later, &pin, &share).is_err());
    assert!(!share.join("source.tar").exists());
}
