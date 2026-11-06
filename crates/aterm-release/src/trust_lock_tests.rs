// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release's locked prover over a fake store (A14's measures): flipping `current` between
//! steps leaves it unchanged, an absent locked build is installed and held to its tree, a store
//! build of another tree refuses, and a `TRUST_STAGE2_BIN` of another tree is refused in a cut and
//! taken otherwise.

use super::*;

const COMMIT: &str = "29ebb403de7a102850d6cb10a62d5cc3baf723ed";

/// A complete store build under `prefix` whose `trustc` answers `-vV` with `commit` and `--print
/// sysroot` with its directory, whose `targo` prints `build`, and its tree's fold (the same for
/// the same build and commit under any prefix).
fn store_build(prefix: &Path, build: u64, commit: &str) -> String {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = atpkg::trust_lock::build_dir(prefix, build);
    std::fs::create_dir_all(dir.join("lib/rustlib")).unwrap();
    std::fs::write(atpkg::trust_lock::ready_marker(prefix, build), "ok\n").unwrap();
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    for (tool, body) in [
        ("targo", format!("echo {build}")),
        (
            "trustc",
            format!(
                "case \"$1\" in -vV) printf 'commit-hash: {commit}\\n' ;; \
                 *) cd \"$(dirname \"$0\")/..\" && pwd ;; esac"
            ),
        ),
    ] {
        let path = dir.join("bin").join(tool);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    atpkg::tree::tree_root(&dir).unwrap()
}

fn lock_of(build: u64, tree_root: &str, commit: &str) -> TrustLock {
    TrustLock {
        build,
        tree_root: tree_root.to_string(),
        compiler_commit: commit.to_string(),
        receipt: "SEED-MINT sha256:00".to_string(),
        stock_toolchain: "1.91.1".to_string(),
    }
}

fn point_current(prefix: &Path, build: u64) {
    let current = prefix.join("store/trust/current");
    let _ = std::fs::remove_file(&current);
    std::os::unix::fs::symlink(atpkg::trust_lock::build_dir(prefix, build), current).unwrap();
}

fn bin_of(prefix: &Path, build: u64) -> PathBuf {
    atpkg::trust_lock::build_dir(prefix, build)
        .join("bin")
        .canonicalize()
        .unwrap()
}

fn never(_: &Path, _: &TrustLock) -> Result<(), String> {
    panic!("no install was due")
}

/// FLIPPING `current` BETWEEN STEPS LEAVES THE PROVER UNCHANGED: each step's resolution of the
/// candidate's lock is its build, whichever build `current` names. NEGATIVE CONTROL: the
/// unlocked resolution a cut used before (discovery with no lock) follows the flip.
#[test]
fn flipping_current_between_steps_leaves_the_prover_unchanged() {
    atpkg::toolchain::set_probe_bound_for_test(aterm_spec::wait::MUST_HAPPEN);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let prefix = scratch.path().canonicalize().unwrap().join("pkg");
    let root = store_build(&prefix, 16387, COMMIT);
    store_build(&prefix, 16500, &"b".repeat(40));
    let lock = lock_of(16387, &root, COMMIT);
    let unlocked = || {
        atpkg::toolchain::Toolchain::discover_with_store(
            None,
            &scratch.path().join("rustup"),
            Some(&prefix),
            std::ffi::OsStr::new(""),
            Some("trust"),
            None,
        )
        .stage2_dir
    };

    point_current(&prefix, 16387);
    let first = locked_prover(&prefix, &lock, None, Run::Cut, &mut never).unwrap();
    assert_eq!(first.bin, bin_of(&prefix, 16387));
    assert_eq!(unlocked(), bin_of(&prefix, 16387));

    point_current(&prefix, 16500);
    let second = locked_prover(&prefix, &lock, None, Run::Cut, &mut never).unwrap();
    assert_eq!(
        second, first,
        "an index flip mid-cut changes no step's prover"
    );
    assert_eq!(
        unlocked(),
        bin_of(&prefix, 16500),
        "unlocked, the flip moves it"
    );
    assert!(
        atpkg::trust_lock::locked_mark(&prefix, 16387).exists(),
        "gc keeps it"
    );
}

/// AN ABSENT LOCKED BUILD TRIGGERS AN INSTALL, held to the lock's tree: an install that stages
/// the lock's tree proves; one that stages another tree, or fails, refuses.
#[test]
fn an_absent_locked_build_is_installed_and_held_to_its_tree() {
    atpkg::toolchain::set_probe_bound_for_test(aterm_spec::wait::MUST_HAPPEN);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let staging = scratch.path().canonicalize().unwrap().join("staging");
    let root = store_build(&staging, 16387, COMMIT);
    let lock = lock_of(16387, &root, COMMIT);

    let prefix = scratch.path().canonicalize().unwrap().join("pkg");
    let mut installs = 0;
    let got = locked_prover(&prefix, &lock, None, Run::Cut, &mut |_, asked| {
        installs += 1;
        assert_eq!(asked, &lock);
        assert_eq!(store_build(&prefix, asked.build, COMMIT), asked.tree_root);
        Ok(())
    })
    .unwrap();
    assert_eq!(installs, 1);
    assert_eq!(got.bin, bin_of(&prefix, 16387));
    assert!(got.source.contains("reinstalled"), "{}", got.source);

    let other = scratch.path().canonicalize().unwrap().join("other");
    let why = locked_prover(&other, &lock, None, Run::Cut, &mut |_, asked| {
        let staged = atpkg::trust_lock::build_dir(&other, asked.build);
        store_build(&other, asked.build, COMMIT);
        std::fs::write(staged.join("bin/extra"), b"another tree").unwrap();
        Ok(())
    })
    .expect_err("an install of another tree");
    assert!(why.contains("tree root"), "{why}");

    let failed = scratch.path().canonicalize().unwrap().join("failed");
    let why = locked_prover(&failed, &lock, None, Run::Cut, &mut |_, _| {
        Err("offline".into())
    })
    .expect_err("a failed install");
    assert!(why.contains("its install failed: offline"), "{why}");
}

/// A TREE ROOT MISMATCH REFUSES: the store holds the lock's build number with another tree, and
/// nothing is installed over it; a compiler of another commit refuses too.
#[test]
fn a_store_build_of_another_tree_or_compiler_refuses() {
    atpkg::toolchain::set_probe_bound_for_test(aterm_spec::wait::MUST_HAPPEN);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let prefix = scratch.path().canonicalize().unwrap().join("pkg");
    let root = store_build(&prefix, 16387, COMMIT);

    let why = locked_prover(
        &prefix,
        &lock_of(16387, &"e".repeat(64), COMMIT),
        None,
        Run::Cut,
        &mut never,
    )
    .expect_err("another tree");
    assert!(why.contains(&format!("tree root {}", &root[..12])), "{why}");

    let why = locked_prover(
        &prefix,
        &lock_of(16387, &root, &"c".repeat(40)),
        None,
        Run::Cut,
        &mut never,
    )
    .expect_err("another compiler");
    assert!(why.contains(&format!("reports commit {COMMIT}")), "{why}");
}

/// A `TRUST_STAGE2_BIN` OF ANOTHER TREE IS REFUSED IN A CUT AND ALLOWED OTHERWISE, said, its tree
/// read from the build's record when it names the build laid aside; one that is the lock's own
/// build proves in a cut.
#[test]
fn an_override_of_another_tree_is_refused_in_a_cut_and_taken_otherwise() {
    atpkg::toolchain::set_probe_bound_for_test(aterm_spec::wait::MUST_HAPPEN);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let prefix = scratch.path().canonicalize().unwrap().join("pkg");
    let root = store_build(&prefix, 16387, COMMIT);
    store_build(&prefix, 16500, COMMIT);
    let lock = lock_of(16387, &root, COMMIT);
    let foreign = bin_of(&prefix, 16500);

    let why = locked_prover(&prefix, &lock, Some(&foreign), Run::Cut, &mut never)
        .expect_err("a cut refuses another tree");
    assert!(why.contains("unset TRUST_STAGE2_BIN"), "{why}");

    let taken = locked_prover(&prefix, &lock, Some(&foreign), Run::Other, &mut never).unwrap();
    assert_eq!(taken.bin, foreign);
    assert!(
        taken.source.contains("takes the override"),
        "{}",
        taken.source
    );

    // The release passes the canonical `<build>.noindex/bin`: the build's record answers for it.
    let aside = atpkg::trust_lock::build_dir(&prefix, 16500).with_file_name("16500.noindex");
    std::fs::rename(atpkg::trust_lock::build_dir(&prefix, 16500), &aside).unwrap();
    std::os::unix::fs::symlink(
        "16500.noindex",
        atpkg::trust_lock::build_dir(&prefix, 16500),
    )
    .unwrap();
    atpkg::trust_lock::write_tree_root_record(&aside, &"f".repeat(64)).unwrap();
    let why = locked_prover(
        &prefix,
        &lock,
        Some(&aside.join("bin")),
        Run::Cut,
        &mut never,
    )
    .expect_err("a recorded other tree");
    assert!(why.contains("is tree root ffffffffffff"), "{why}");

    let own = locked_prover(
        &prefix,
        &lock,
        Some(&bin_of(&prefix, 16387)),
        Run::Cut,
        &mut never,
    )
    .unwrap();
    assert_eq!(own.bin, bin_of(&prefix, 16387));
}

/// The lock a commit carries: none, a lock, and a lock that does not parse (refused, not none).
#[test]
fn lock_at_reads_the_lock_a_commit_carries() {
    use crate::ledger::testing::{Git, git, init};
    atpkg::toolchain::set_probe_bound_for_test(aterm_spec::wait::MUST_HAPPEN);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let repo = scratch.path().canonicalize().unwrap();
    init(
        &repo,
        &[
            ("user.name", "Andrew Yates"),
            ("user.email", "a@example.invalid"),
        ],
    );
    let commit = |message: &str| {
        let ok = |args: &[&str]| {
            assert!(
                git()
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        ok(&["add", "-A"]);
        ok(&["commit", "-q", "--allow-empty", "-m", message]);
    };
    let runner = Git::new(&repo);
    commit("no lock");
    assert_eq!(lock_at(&runner, "HEAD").unwrap(), None);

    let lock = lock_of(16387, &"4".repeat(64), COMMIT);
    std::fs::write(
        repo.join(LOCK_FILE),
        format!("# a head\n{}", atpkg::trust_lock::render_body(&lock)),
    )
    .unwrap();
    commit("lock");
    assert_eq!(lock_at(&runner, "HEAD").unwrap(), Some(lock.clone()));
    assert_eq!(lock_at(&runner, "HEAD~1").unwrap(), None);
    assert_eq!(stock_toolchain(Some(&lock)), "1.91.1");
    assert_eq!(stock_toolchain(None), "stable");

    std::fs::write(repo.join(LOCK_FILE), "version = 2\n").unwrap();
    commit("broken");
    assert!(lock_at(&runner, "HEAD").is_err());
}
