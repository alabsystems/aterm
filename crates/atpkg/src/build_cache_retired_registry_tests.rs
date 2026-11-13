// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private submodule store can own another worktree even when its snapshot is clean.

use std::path::{Path, PathBuf};

use super::super::Git as _;
use super::{GitCli, for_git, gate, git, plan, retire};

fn read(dir: &Path, args: &[&str]) -> String {
    let answer = GitCli.run(dir, args);
    assert_eq!(answer.code, Some(0), "git {args:?}: {}", answer.err);
    answer.out.trim().to_owned()
}

fn foreign_registry_keeps_snapshot(legacy: bool, missing: bool) {
    let g = gate();
    let snapshot = g.snapshot();
    let sub = snapshot.join("vendor/astream");
    let original_common = PathBuf::from(read(
        &sub,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ));
    let common = if legacy {
        // Older snapshots kept the same private module metadata inside their tree.
        // Relocate only this fixture's store, then bind its original worktree to it.
        let relocated = snapshot.join(".aterm-verify/modules/vendor/astream");
        std::fs::create_dir_all(relocated.parent().unwrap()).unwrap();
        std::fs::rename(&original_common, &relocated).unwrap();
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
        assert!(original_common.starts_with(g.checkout().join(".git/worktrees")));
        original_common
    };
    assert_eq!(
        std::fs::canonicalize(read(
            &sub,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ))
        .unwrap(),
        std::fs::canonicalize(&common).unwrap(),
        "the foreign worktree must borrow the snapshot's actual private common store"
    );

    let foreign = g.home().join("kept-submodule-worktree");
    git(
        &sub,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "kept-foreign",
            &for_git(&foreign),
            "HEAD",
        ],
    );
    let unique_bytes = b"// work committed only in the foreign worktree\n";
    std::fs::write(foreign.join("retained.rs"), unique_bytes).unwrap();
    git(&foreign, &["add", "retained.rs"]);
    git(
        &foreign,
        &["commit", "-qm", "retain foreign submodule work"],
    );
    let unique = read(&foreign, &["rev-parse", "HEAD"]);
    let object = format!("{unique}^{{commit}}");
    let elsewhere = GitCli.run(&g.home().join("astream"), &["cat-file", "-e", &object]);
    assert_ne!(
        elsewhere.code,
        Some(0),
        "the commit was never pushed to the fixture origin"
    );
    let foreign_admin = PathBuf::from(read(&foreign, &["rev-parse", "--absolute-git-dir"]));
    let registration = std::fs::read(foreign_admin.join("gitdir")).unwrap();
    let gitfile = std::fs::read(foreign.join(".git")).unwrap();
    assert_eq!(
        read(
            &snapshot,
            &[
                "status",
                "--porcelain",
                "--ignore-submodules=none",
                "--untracked-files=normal"
            ],
        ),
        "",
        "fixture control: the snapshot itself is clean and still at its published submodule pin"
    );
    let away = g.home().join("foreign-worktree-away");
    if missing {
        std::fs::rename(&foreign, &away).unwrap();
        assert!(!foreign.exists(), "the registered path is genuinely absent");
    }

    let planned = plan(&g.roots, g.now, &GitCli);
    let done = retire(&g.roots, g.now, &GitCli);
    assert!(
        snapshot.join(".git").is_file()
            && snapshot.join("Cargo.toml").is_file()
            && g.cells().is_dir()
            && common.is_dir()
            && foreign_admin.join("gitdir").is_file(),
        "foreign Git custody must keep source, cells and metadata; legacy={legacy} missing={missing}; plan={planned:#?}; retire={done:#?}"
    );
    assert!(
        planned
            .iter()
            .any(|row| row.path == snapshot && !row.goes()),
        "the preview must name the retained snapshot: {planned:#?}"
    );
    assert!(
        done.iter()
            .any(|(row, result)| row.path == snapshot && result.is_err()),
        "retirement must preserve the snapshot: {done:#?}"
    );
    assert!(
        g.has_worktree(&snapshot),
        "the outer worktree remains registered"
    );
    assert_eq!(
        std::fs::read(foreign_admin.join("gitdir")).unwrap(),
        registration
    );
    let held = GitCli.run(
        &g.checkout(),
        &["--git-dir", &for_git(&common), "cat-file", "-e", &object],
    );
    assert_eq!(
        held.code,
        Some(0),
        "the unique foreign commit stays: {}",
        held.err
    );
    let source = if missing { &away } else { &foreign };
    assert_eq!(std::fs::read(source.join(".git")).unwrap(), gitfile);
    assert_eq!(
        std::fs::read(source.join("retained.rs")).unwrap(),
        unique_bytes
    );
    if missing {
        std::fs::rename(&away, &foreign).unwrap();
    }
    assert_eq!(read(&foreign, &["rev-parse", "HEAD"]), unique);
    assert_eq!(read(&foreign, &["status", "--porcelain"]), "");
}

#[test]
fn a_foreign_worktree_keeps_its_private_submodule_store() {
    foreign_registry_keeps_snapshot(false, false);
}

#[test]
fn a_missing_foreign_worktree_keeps_its_private_submodule_store() {
    foreign_registry_keeps_snapshot(false, true);
}

#[test]
fn a_foreign_worktree_keeps_its_legacy_inside_snapshot_store() {
    foreign_registry_keeps_snapshot(true, false);
}

#[test]
fn a_missing_foreign_worktree_keeps_its_legacy_inside_snapshot_store() {
    foreign_registry_keeps_snapshot(true, true);
}
