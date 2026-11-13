// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Interrupted metadata and namespace names do not weaken private Git custody.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::super::{Git as _, own_entry, pending};
use super::{Gate, GitCli, for_git, gate, git, plan, retire};

fn read(dir: &Path, args: &[&str]) -> String {
    let answer = GitCli.run(dir, args);
    assert_eq!(answer.code, Some(0), "git {args:?}: {}", answer.err);
    answer.out.trim().to_owned()
}

fn common(g: &Gate) -> PathBuf {
    PathBuf::from(read(
        &g.snapshot().join("vendor/astream"),
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ))
}

fn entry(g: &Gate) -> PathBuf {
    own_entry(
        &g.checkout(),
        &g.snapshot(),
        &std::fs::read(g.snapshot().join(".git")).unwrap(),
    )
    .unwrap()
}

fn foreign(module_named_objects: bool) -> (Gate, PathBuf, PathBuf, String) {
    let g = gate();
    let sub = g.snapshot().join("vendor/astream");
    let original = common(&g);
    let store = if module_named_objects {
        let relocated = entry(&g).join("modules/other/objects");
        std::fs::create_dir_all(relocated.parent().unwrap()).unwrap();
        std::fs::rename(&original, &relocated).unwrap();
        std::fs::write(
            sub.join(".git"),
            format!("gitdir: {}\n", for_git(&relocated)),
        )
        .unwrap();
        git(
            &g.checkout(),
            &[
                "config",
                "--file",
                &for_git(&relocated.join("config")),
                "core.worktree",
                &for_git(&sub),
            ],
        );
        relocated
    } else {
        original
    };
    let work = g.home().join("retained-foreign");
    git(
        &sub,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "retained-foreign",
            &for_git(&work),
            "HEAD",
        ],
    );
    git(
        &work,
        &["commit", "--allow-empty", "-qm", "unique foreign history"],
    );
    let unique = read(&work, &["rev-parse", "HEAD"]);
    assert_ne!(
        GitCli
            .run(&g.home().join("astream"), &["cat-file", "-e", &unique])
            .code,
        Some(0)
    );
    assert_eq!(
        read(
            &g.snapshot(),
            &["status", "--porcelain", "--ignore-submodules=none"]
        ),
        ""
    );
    (g, store, work, unique)
}

fn preserved(g: &Gate, store: &Path) {
    let planned = plan(&g.roots, g.now, &GitCli);
    let done = retire(&g.roots, g.now, &GitCli);
    assert!(
        g.snapshot().join("Cargo.toml").is_file() && g.snapshot().join(".git").is_file(),
        "source must stay: plan={planned:#?} retire={done:#?}"
    );
    assert!(
        g.cells().is_dir() && store.is_dir(),
        "cells and private metadata stay: {done:#?}"
    );
    assert!(
        planned
            .iter()
            .any(|row| row.path == g.snapshot() && !row.goes()),
        "{planned:#?}"
    );
    assert!(
        done.iter()
            .any(|(row, result)| row.path == g.snapshot() && result.is_err()),
        "{done:#?}"
    );
}

#[test]
fn a_module_named_objects_keeps_its_foreign_worktree() {
    let (g, store, work, unique) = foreign(true);
    preserved(&g, &store);
    assert_eq!(read(&work, &["rev-parse", "HEAD"]), unique);
    assert_eq!(read(&work, &["status", "--porcelain"]), "");
}

#[test]
fn an_interrupted_removal_keeps_preexisting_foreign_metadata_after_source_is_gone() {
    let (g, store, work, unique) = foreign(false);
    let snapshot = g.snapshot();
    let admin = entry(&g);
    // This foreign history predates admission, so the existing new-write fence
    // alone cannot protect it. No sleep or wall-clock race establishes the case.
    let mut stack = vec![admin.clone()];
    while let Some(path) = stack.pop() {
        let meta = std::fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            for child in std::fs::read_dir(path).unwrap() {
                stack.push(child.unwrap().path());
            }
        } else {
            g.written(&path, Duration::from_secs(30 * 24 * 3600));
        }
    }
    let head = read(&snapshot, &["rev-parse", "HEAD"]);
    let text = std::fs::read(snapshot.join(".git")).unwrap();
    let mut receipt = pending::begin(&g.checkout(), &snapshot, &admin, &head, &text).unwrap();
    assert!(crate::build_cache::remove::tree(&snapshot).whole());
    pending::tree_gone(&g.checkout(), &snapshot, &mut receipt).unwrap();
    let before = std::fs::read(work.join(".git")).unwrap();

    let planned = plan(&g.roots, g.now, &GitCli);
    let done = retire(&g.roots, g.now, &GitCli);
    assert!(
        store.is_dir() && admin.is_dir() && g.cells().is_dir(),
        "private metadata must survive resume: plan={planned:#?} retire={done:#?}"
    );
    assert!(
        planned
            .iter()
            .any(|row| row.path == snapshot && !row.goes()),
        "{planned:#?}"
    );
    assert_eq!(std::fs::read(work.join(".git")).unwrap(), before);
    assert_eq!(read(&work, &["rev-parse", "HEAD"]), unique);
    assert_eq!(read(&work, &["status", "--porcelain"]), "");
}

#[test]
fn an_empty_private_registry_still_allows_ordinary_retirement() {
    let g = gate();
    std::fs::create_dir(common(&g).join("worktrees")).unwrap();
    let planned = plan(&g.roots, g.now, &GitCli);
    let done = retire(&g.roots, g.now, &GitCli);
    assert!(planned.iter().all(|row| row.goes()), "{planned:#?}");
    assert!(done.iter().all(|(_, result)| result.is_ok()), "{done:#?}");
    assert!(!g.snapshot().exists() && !g.cells().exists());
}

#[test]
fn an_unrelated_source_directory_named_worktrees_does_not_hold_the_snapshot() {
    let g = gate();
    let ordinary = g.snapshot().join("target/worktrees/not-a-registry");
    std::fs::create_dir_all(&ordinary).unwrap();
    std::fs::write(
        ordinary.join("source.rs"),
        b"// ordinary generated source\n",
    )
    .unwrap();
    let done = retire(&g.roots, g.now, &GitCli);
    assert!(done.iter().all(|(_, result)| result.is_ok()), "{done:#?}");
    assert!(!g.snapshot().exists());
}

#[test]
fn an_ambiguous_private_registry_keeps_the_snapshot() {
    let g = gate();
    let store = common(&g);
    let bytes = b"unknown registry metadata\n";
    std::fs::write(store.join("worktrees"), bytes).unwrap();
    preserved(&g, &store);
    assert_eq!(std::fs::read(store.join("worktrees")).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn a_linked_private_registry_is_not_followed_or_retired() {
    let g = gate();
    let store = common(&g);
    let outside = g.home().join("outside-registry");
    std::fs::create_dir(&outside).unwrap();
    let bytes = b"outside source stays\n";
    std::fs::write(outside.join("retained"), bytes).unwrap();
    std::os::unix::fs::symlink(&outside, store.join("worktrees")).unwrap();
    preserved(&g, &store);
    assert!(
        store
            .join("worktrees")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(outside.join("retained")).unwrap(), bytes);
}
