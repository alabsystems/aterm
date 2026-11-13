// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Independent retained history admits ordinary refs, not borrowed or unrooted objects.

use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_uds::holders::PathScan;

use super::super::super::fence::Fence;
use super::super::{Git as _, own_entry, pending, retire_on};
use super::{Gate, GitCli, for_git, gate, git, plan};

fn read(dir: &Path, args: &[&str]) -> String {
    let answer = GitCli.run(dir, args);
    assert_eq!(answer.code, Some(0), "git {args:?}: {}", answer.err);
    answer.out.trim().to_owned()
}

struct Module {
    sub: PathBuf,
    common: PathBuf,
    witness: PathBuf,
    pin: String,
}

fn module(g: &Gate) -> Module {
    let sub = g.snapshot().join("vendor/astream");
    let common = PathBuf::from(read(
        &sub,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ));
    let witness = g.checkout().join("vendor/astream");
    let pin = read(&sub, &["rev-parse", "HEAD"]);
    assert!(common.starts_with(g.checkout().join(".git/worktrees")));
    assert_eq!(read(&witness, &["rev-parse", "HEAD"]), pin);
    Module {
        sub,
        common,
        witness,
        pin,
    }
}

fn clean(g: &Gate, m: &Module) {
    assert_eq!(read(&m.sub, &["rev-parse", "HEAD"]), m.pin);
    assert_eq!(
        read(
            &g.snapshot(),
            &["status", "--porcelain", "--ignore-submodules=none"]
        ),
        "",
    );
}

fn retained_private_commit(g: &Gate, m: &Module) -> (String, String) {
    git(&m.sub, &["checkout", "-qb", "retained-private-edge"]);
    std::fs::write(m.sub.join("lib.rs"), "// sole private edge source\n").unwrap();
    git(&m.sub, &["add", "lib.rs"]);
    git(&m.sub, &["commit", "-qm", "retained private edge source"]);
    let commit = read(&m.sub, &["rev-parse", "HEAD"]);
    let blob = read(&m.sub, &["rev-parse", "HEAD:lib.rs"]);
    git(&m.sub, &["checkout", "-q", "--detach", &m.pin]);
    clean(g, m);
    (commit, blob)
}

fn copy_private_objects_to_witness(m: &Module, commit: &str, blob: &str) -> PathBuf {
    let tree = read(&m.sub, &["rev-parse", &format!("{commit}^{{tree}}")]);
    let witness_common = PathBuf::from(read(
        &m.witness,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ));
    // Copy actual loose Git objects without creating a canonical ref or reflog.
    // Mere cat-file success must not be confused with retained reachable history.
    for oid in [commit, tree.as_str(), blob] {
        let relative = PathBuf::from(&oid[..2]).join(&oid[2..]);
        let target = witness_common.join("objects").join(&relative);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(m.common.join("objects").join(&relative), &target).unwrap();
        assert!(target.is_file());
        assert_eq!(read(&m.witness, &["cat-file", "-e", oid]), "");
    }
    witness_common
}

fn retired(g: &Gate, m: &Module) {
    let planned = plan(&g.roots, g.now, &GitCli);
    let clear = |_: &[PathBuf], _: &[u32]| Ok(PathScan::default());
    let done = retire_on(&g.roots, g.now, &GitCli, &Fence::new(&clear));
    assert!(planned.iter().all(|row| row.goes()), "{planned:#?}");
    assert!(done.iter().all(|(_, result)| result.is_ok()), "{done:#?}");
    assert!(!g.snapshot().exists() && !g.cells().exists() && !m.common.exists());
    assert!(!g.has_worktree(&g.snapshot()));
}

fn kept(g: &Gate, m: &Module, source_present: bool) {
    let planned = plan(&g.roots, g.now, &GitCli);
    let clear = |_: &[PathBuf], _: &[u32]| Ok(PathScan::default());
    let done = retire_on(&g.roots, g.now, &GitCli, &Fence::new(&clear));
    assert!(
        planned
            .iter()
            .any(|row| row.path == g.snapshot() && !row.goes()),
        "{planned:#?}",
    );
    assert!(
        done.iter()
            .any(|(row, result)| row.path == g.snapshot() && result.is_err()),
        "{done:#?}",
    );
    assert!(g.cells().is_dir() && m.common.is_dir(), "{done:#?}");
    if source_present {
        assert!(g.snapshot().join("Cargo.toml").is_file(), "{done:#?}");
        clean(g, m);
    } else {
        assert!(!g.snapshot().exists(), "{done:#?}");
    }
}

#[test]
fn a_private_branch_different_from_the_pin_goes_when_canonically_rooted() {
    let g = gate();
    let m = module(&g);
    std::fs::write(
        m.witness.join("lib.rs"),
        "// retained canonical branch source\n",
    )
    .unwrap();
    git(&m.witness, &["add", "lib.rs"]);
    git(&m.witness, &["commit", "-qm", "retained canonical branch"]);
    let retained = read(&m.witness, &["rev-parse", "HEAD"]);
    assert_ne!(retained, m.pin);
    git(
        &m.sub,
        &[
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=always",
            "fetch",
            "-q",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            &for_git(&m.witness),
            "refs/heads/main:refs/heads/main",
        ],
    );
    assert_eq!(read(&m.sub, &["rev-parse", "refs/heads/main"]), retained);
    clean(&g, &m);

    retired(&g, &m);
    assert_eq!(
        read(&m.witness, &["rev-parse", "refs/heads/main"]),
        retained
    );
    assert_eq!(
        read(&m.witness, &["show", "HEAD:lib.rs"]),
        "// retained canonical branch source"
    );
}

#[test]
fn a_shared_annotated_tag_object_allows_private_retirement() {
    let g = gate();
    let m = module(&g);
    git(
        &m.witness,
        &[
            "-c",
            "tag.gpgsign=false",
            "tag",
            "-a",
            "retained-annotation",
            "-m",
            "retained annotation",
        ],
    );
    let tag = read(&m.witness, &["rev-parse", "refs/tags/retained-annotation"]);
    assert_ne!(tag, m.pin);
    assert_eq!(read(&m.witness, &["cat-file", "-t", &tag]), "tag");
    git(
        &m.sub,
        &[
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=always",
            "fetch",
            "-q",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            &for_git(&m.witness),
            "refs/tags/retained-annotation:refs/tags/retained-annotation",
        ],
    );
    assert_eq!(
        read(&m.sub, &["rev-parse", "refs/tags/retained-annotation"]),
        tag
    );
    clean(&g, &m);

    retired(&g, &m);
    assert_eq!(
        read(&m.witness, &["rev-parse", "refs/tags/retained-annotation"]),
        tag
    );
    assert_eq!(read(&m.witness, &["cat-file", "-t", &tag]), "tag");
}

#[test]
fn a_canonical_ref_borrowing_private_objects_does_not_authorize_retirement() {
    let g = gate();
    let m = module(&g);
    let (unique, blob) = retained_private_commit(&g, &m);
    let witness_common = PathBuf::from(read(
        &m.witness,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ));
    let alternate = witness_common.join("objects/info/alternates");
    let bytes = format!("{}\n", for_git(&m.common.join("objects"))).into_bytes();
    std::fs::write(&alternate, &bytes).unwrap();
    git(
        &m.witness,
        &["update-ref", "refs/heads/apparently-retained", &unique],
    );
    assert_eq!(read(&m.witness, &["cat-file", "-t", &unique]), "commit");

    kept(&g, &m, true);
    assert_eq!(std::fs::read(alternate).unwrap(), bytes);
    assert_eq!(
        read(&m.sub, &["rev-parse", "refs/heads/retained-private-edge"]),
        unique
    );
    assert_eq!(read(&m.sub, &["cat-file", "-t", &blob]), "blob");
    assert_eq!(
        read(&m.sub, &["show", &format!("{unique}:lib.rs")]),
        "// sole private edge source"
    );
    assert_eq!(
        read(&m.witness, &["rev-parse", "refs/heads/apparently-retained"]),
        unique
    );
}

#[test]
fn unrooted_canonical_loose_objects_do_not_authorize_private_retirement() {
    let g = gate();
    let m = module(&g);
    let (unique, blob) = retained_private_commit(&g, &m);
    copy_private_objects_to_witness(&m, &unique, &blob);
    assert!(
        !read(&m.witness, &["rev-list", "--all"])
            .lines()
            .any(|oid| oid == unique)
    );

    kept(&g, &m, true);
    assert_eq!(
        read(&m.sub, &["rev-parse", "refs/heads/retained-private-edge"]),
        unique
    );
    assert_eq!(
        read(&m.sub, &["show", &format!("{unique}:lib.rs")]),
        "// sole private edge source"
    );
    assert_eq!(read(&m.witness, &["cat-file", "-t", &blob]), "blob");
}

#[test]
fn a_canonical_orig_head_alone_does_not_retain_private_history() {
    let g = gate();
    let m = module(&g);
    let (unique, blob) = retained_private_commit(&g, &m);
    let witness_common = copy_private_objects_to_witness(&m, &unique, &blob);
    let orig_head = witness_common.join("ORIG_HEAD");
    let before = format!("{unique}\n").into_bytes();
    std::fs::write(&orig_head, &before).unwrap();
    assert_eq!(
        read(&m.witness, &["rev-parse", "--verify", "ORIG_HEAD"]),
        unique
    );
    assert_eq!(read(&m.witness, &["rev-parse", "HEAD"]), m.pin);
    assert!(
        !read(&m.witness, &["rev-list", "--all", "--reflog"])
            .lines()
            .any(|oid| oid == unique)
    );
    clean(&g, &m);

    kept(&g, &m, true);
    assert_eq!(std::fs::read(orig_head).unwrap(), before);
    assert_eq!(
        read(&m.sub, &["rev-parse", "refs/heads/retained-private-edge"]),
        unique
    );
    assert_eq!(
        read(&m.sub, &["show", &format!("{unique}:lib.rs")]),
        "// sole private edge source"
    );
    assert_eq!(read(&m.sub, &["cat-file", "-t", &blob]), "blob");
}

/// A pseudoref (`FETCH_HEAD`, `MERGE_HEAD`) or a root ref (`ORIG_HEAD`; `AUTO_MERGE`, one git
/// names outside the `_HEAD` pattern) alone keeps the private history it names, whether git
/// lists root refs itself (`for-each-ref --include-root-refs`) or the store's own files name
/// them (a git without that option).
#[test]
fn private_pseudo_and_root_refs_keep_otherwise_unreferenced_history() {
    for name in ["FETCH_HEAD", "MERGE_HEAD", "ORIG_HEAD", "AUTO_MERGE"] {
        let g = gate();
        let m = module(&g);
        let refs_before = read(
            &m.sub,
            &["for-each-ref", "--format=%(refname)%00%(objectname)"],
        );
        let payload = format!("// source retained only by private {name}\n");
        std::fs::write(m.sub.join("lib.rs"), &payload).unwrap();
        git(&m.sub, &["add", "lib.rs"]);
        let tree = read(&m.sub, &["write-tree"]);
        let blob = read(&m.sub, &["rev-parse", ":lib.rs"]);
        // commit-tree writes immutable objects only. No branch, HEAD or
        // reflog ever refers to the resulting commit.
        let unique = read(
            &m.sub,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                &tree,
                "-p",
                &m.pin,
                "-m",
                "private pseudo-ref source",
            ],
        );
        git(
            &m.sub,
            &[
                "restore",
                "--source=HEAD",
                "--staged",
                "--worktree",
                "--",
                "lib.rs",
            ],
        );
        clean(&g, &m);
        assert_eq!(
            read(
                &m.sub,
                &["for-each-ref", "--format=%(refname)%00%(objectname)"]
            ),
            refs_before,
        );
        assert!(
            !read(&m.sub, &["rev-list", "--all", "--reflog"])
                .lines()
                .any(|oid| oid == unique)
        );
        for oid in [&unique, &blob] {
            let absent = GitCli.run(&m.witness, &["cat-file", "-e", oid]);
            assert!(absent.code.is_some(), "Git must run: {}", absent.err);
            assert_ne!(
                absent.code,
                Some(0),
                "canonical witness unexpectedly has {oid}"
            );
        }
        let bytes = if name == "FETCH_HEAD" {
            format!("{unique}\t\tbranch 'private-only' of fixture\n").into_bytes()
        } else {
            format!("{unique}\n").into_bytes()
        };
        let pseudoref = m.common.join(name);
        std::fs::write(&pseudoref, &bytes).unwrap();
        assert_eq!(read(&m.sub, &["rev-parse", "--verify", name]), unique);
        assert_eq!(
            read(
                &m.sub,
                &["for-each-ref", "--format=%(refname)%00%(objectname)"]
            ),
            refs_before
        );
        clean(&g, &m);

        kept(&g, &m, true);
        assert_eq!(std::fs::read(pseudoref).unwrap(), bytes);
        assert_eq!(read(&m.sub, &["rev-parse", "--verify", name]), unique);
        let saved = GitCli.run(&m.sub, &["show", &format!("{unique}:lib.rs")]);
        assert_eq!(saved.code, Some(0), "{}", saved.err);
        assert_eq!(saved.out, payload);
        assert_eq!(read(&m.sub, &["cat-file", "-t", &blob]), "blob");
    }
}

#[test]
fn old_private_history_keeps_tree_gone_metadata_and_cells() {
    let g = gate();
    let m = module(&g);
    let (unique, blob) = retained_private_commit(&g, &m);
    let snapshot = g.snapshot();
    let text = std::fs::read(snapshot.join(".git")).unwrap();
    let admin = own_entry(&g.checkout(), &snapshot, &text).unwrap();
    let mut stack = vec![admin.clone()];
    while let Some(path) = stack.pop() {
        if std::fs::symlink_metadata(&path).unwrap().is_dir() {
            for child in std::fs::read_dir(path).unwrap() {
                stack.push(child.unwrap().path());
            }
        } else {
            g.written(&path, Duration::from_secs(30 * 24 * 3600));
        }
    }
    let head = read(&snapshot, &["rev-parse", "HEAD"]);
    let mut receipt = pending::begin(&g.checkout(), &snapshot, &admin, &head, &text).unwrap();
    assert!(crate::build_cache::remove::tree(&snapshot).whole());
    pending::tree_gone(&g.checkout(), &snapshot, &mut receipt).unwrap();
    let branch = m.common.join("refs/heads/retained-private-edge");
    let before = std::fs::read(&branch).unwrap();

    kept(&g, &m, false);
    assert!(admin.is_dir());
    assert_eq!(std::fs::read(branch).unwrap(), before);
    let git_dir = for_git(&m.common);
    let worktree = for_git(&g.checkout());
    assert_eq!(
        read(
            &g.checkout(),
            &[
                "--git-dir",
                &git_dir,
                "--work-tree",
                &worktree,
                "rev-parse",
                "refs/heads/retained-private-edge"
            ]
        ),
        unique
    );
    assert_eq!(
        read(
            &g.checkout(),
            &[
                "--git-dir",
                &git_dir,
                "--work-tree",
                &worktree,
                "cat-file",
                "-t",
                &blob
            ]
        ),
        "blob"
    );
}
