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

#[cfg(unix)]
#[test]
fn an_explicit_store_holds_its_source_compiler_independently_of_home_policy() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().canonicalize().unwrap();
    let prefix = home.join("pkg");
    let root = store_build(&prefix, 16387, COMMIT);
    let bin = bin_of(&prefix, 16387);
    let lock = lock_of(16387, &root, COMMIT);
    let store = ManagedStoreContext::declared(&home, &prefix).unwrap();
    let mut hold = None;
    let selected = super::locked_prover(
        store.prefix(),
        &lock,
        None,
        Run::Cut,
        &mut never,
        &mut |bin| {
            hold = Some(store.offer(bin, lock.build)?);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(selected.bin, bin);
    assert!(hold.is_some());
    assert!(store.offer(&bin, lock.build + 1).is_err());
    let atpkg::lease::Holders::Held(holders) = atpkg::lease::holders(
        &prefix,
        &atpkg::lease::Subject::build("trust", lock.build).unwrap(),
    ) else {
        panic!("the source compiler is protected by a real holder");
    };
    assert_eq!(holders.len(), 1);
}

/// These disposable stores have no managed lease registry. Production supplies the hold
/// before any compiler probe; the ordering control below exercises that boundary directly.
fn locked_prover(
    prefix: &Path,
    lock: &TrustLock,
    explicit: Option<&Path>,
    run: Run,
    install: &mut dyn FnMut(&Path, &TrustLock) -> Result<(), String>,
) -> Result<LockedProver, String> {
    super::locked_prover(prefix, lock, explicit, run, install, &mut |_| Ok(()))
}

#[test]
fn a_compiler_probe_requires_its_lease_first() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let prefix = scratch.path().canonicalize().unwrap().join("pkg");
    store_build(&prefix, 16387, COMMIT);
    let bin = bin_of(&prefix, 16387);
    let invoked = scratch.path().join("compiler-ran");
    let leased = scratch.path().join("lease-held");
    let quoted = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    std::fs::write(
        bin.join("trustc"),
        format!(
            "#!/bin/sh\nprintf invoked > {}\ntest -f {} || exit 73\nprintf 'commit-hash: {COMMIT}\\n'\n",
            quoted(&invoked),
            quoted(&leased),
        ),
    )
    .unwrap();
    let root = atpkg::tree::tree_root(bin.parent().unwrap()).unwrap();
    let lock = lock_of(16387, &root, COMMIT);
    let why = super::locked_prover(&prefix, &lock, None, Run::Cut, &mut never, &mut |_| {
        Err("lease refused".into())
    })
    .unwrap_err();
    assert_eq!(why, "lease refused");
    assert!(
        !invoked.exists(),
        "a refused lease must execute no compiler"
    );

    let selected = super::locked_prover(&prefix, &lock, None, Run::Cut, &mut never, &mut |asked| {
        assert_eq!(asked, bin);
        std::fs::write(&leased, b"held").map_err(|error| error.to_string())
    })
    .unwrap();
    assert_eq!(selected.bin, bin);
    assert!(invoked.exists());
}

#[test]
fn a_held_replacement_with_the_same_compiler_commit_is_not_the_selected_tree() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let prefix = scratch.path().canonicalize().unwrap().join("pkg");
    let root = store_build(&prefix, 16387, COMMIT);
    let bin = bin_of(&prefix, 16387);
    let lock = lock_of(16387, &root, COMMIT);
    // Populate the old identity's standing record as a prior release would have done.
    assert!(matches!(
        atpkg::trust_lock::standing(&prefix, &lock),
        LockedBuild::Present { .. }
    ));
    let invoked = scratch.path().join("replacement-ran");
    let why = super::locked_prover(&prefix, &lock, None, Run::Cut, &mut never, &mut |_| {
        // Change bytes in place too: a directory-identity record alone cannot authorize
        // the compiler that will execute after acquisition.
        std::fs::write(
            bin.join("trustc"),
            format!(
                "#!/bin/sh\nprintf invoked > '{}'\nprintf 'commit-hash: {COMMIT}\\n'\n",
                invoked.to_string_lossy().replace('\'', "'\\''"),
            ),
        )
        .map_err(|error| error.to_string())
    })
    .unwrap_err();
    assert!(why.contains("held compiler tree changed"), "{why}");
    assert!(!invoked.exists(), "replacement must not execute");
}

/// FLIPPING `current` BETWEEN STEPS LEAVES THE PROVER UNCHANGED: each step's resolution of the
/// candidate's lock is its build, whichever build `current` names. NEGATIVE CONTROL: the
/// unlocked resolution a cut used before (discovery with no lock) follows the flip.
#[test]
fn flipping_current_between_steps_leaves_the_prover_unchanged() {
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
    use crate::ledger::GitCli;
    use crate::ledger::testing::{git, init};
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
    let runner = GitCli::new(&repo);
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
    let selected = crate::ledger::git_ok(&runner, &["rev-parse", "HEAD"])
        .unwrap()
        .stdout_utf8()
        .trim()
        .to_owned();
    assert_eq!(lock_at(&runner, "HEAD~1").unwrap(), None);
    assert_eq!(stock_toolchain_at(&runner, "HEAD").as_deref(), Ok("1.91.1"));

    std::fs::write(repo.join(LOCK_FILE), "version = 2\n").unwrap();
    commit("broken");
    assert!(lock_at(&runner, "HEAD").is_err());
    assert_eq!(
        CompilerPin::at(&runner, &selected).unwrap(),
        CompilerPin::of(&lock)
    );
    assert!(CompilerPin::at(&runner, "HEAD").is_err());
    let tree = crate::ledger::git_ok(&runner, &["rev-parse", &format!("{selected}^{{tree}}")])
        .unwrap()
        .stdout_utf8()
        .trim()
        .to_owned();
    assert!(
        CompilerPin::at(&runner, &tree).is_err(),
        "a lock-bearing tree is not a selected commit"
    );

    // A local replacement makes an ordinary exact-commit read name the broken checkout's
    // lock. The release must still read the original selected source, not that rewritten view.
    assert!(
        git()
            .arg("-C")
            .arg(&repo)
            .args(["replace", &selected, "HEAD"])
            .status()
            .unwrap()
            .success()
    );
    let replaced = git()
        .arg("-C")
        .arg(&repo)
        .args(["show", &format!("{selected}:{LOCK_FILE}")])
        .output()
        .unwrap();
    assert!(replaced.status.success());
    assert_eq!(replaced.stdout, b"version = 2\n");
    assert_eq!(
        CompilerPin::at(&runner, &selected).unwrap(),
        CompilerPin::of(&lock)
    );
}

#[test]
fn unavailable_original_lock_evidence_never_becomes_an_unlocked_candidate() {
    struct Unread;
    impl GitRunner for Unread {
        fn git(&self, _: &[&str]) -> crate::ledger::Result<crate::ledger::RunOut> {
            panic!("candidate compiler authority may not use ordinary Git reads")
        }
        fn git_original(&self, _: &[&str]) -> crate::ledger::Result<crate::ledger::RunOut> {
            Err(crate::ledger::Error::new("original objects unavailable"))
        }
    }
    let why = CompilerPin::at(&Unread, &"1".repeat(40)).unwrap_err();
    assert!(why.contains("original objects unavailable"));
}

#[test]
fn original_candidate_b_selects_its_build_while_checkout_and_current_hold_a() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let prefix = scratch.path().canonicalize().unwrap().join("pkg");
    let a = lock_of(16387, &store_build(&prefix, 16387, COMMIT), COMMIT);
    let b = lock_of(16500, &store_build(&prefix, 16500, COMMIT), COMMIT);
    let repo = scratch.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let git = crate::ledger::GitCli::new(&repo);
    crate::ledger::git_ok(&git, &["init", "-q"]).unwrap();
    let save = |lock: &TrustLock, name: &str| {
        std::fs::write(repo.join(LOCK_FILE), atpkg::trust_lock::render_body(lock)).unwrap();
        crate::ledger::git_ok(&git, &["add", LOCK_FILE]).unwrap();
        crate::ledger::git_ok(
            &git,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                name,
            ],
        )
        .unwrap();
        crate::ledger::git_ok(&git, &["rev-parse", "HEAD"])
            .unwrap()
            .stdout_utf8()
            .trim()
            .to_owned()
    };
    let source_a = save(&a, "compiler A");
    let source_b = save(&b, "compiler B");
    crate::ledger::git_ok(&git, &["checkout", "-q", "--detach", &source_a]).unwrap();
    point_current(&prefix, a.build);
    let selected = CompilerPin::at(&git, &source_b).unwrap();
    assert_eq!(selected, CompilerPin::of(&b));
    assert_eq!(lock_at(&git, "HEAD").unwrap(), Some(a.clone()));
    let prover = locked_prover(&prefix, &selected.lock(), None, Run::Cut, &mut never).unwrap();
    assert_eq!(prover.bin, bin_of(&prefix, b.build));
    assert_ne!(prover.bin, bin_of(&prefix, a.build));
    // The binaries report the same compiler revision. Neither that revision nor a physical
    // path from the other checkout can replace the exact build and tree declaration.
    assert!(
        locked_prover(
            &prefix,
            &selected.lock(),
            Some(&bin_of(&prefix, a.build)),
            Run::Cut,
            &mut never
        )
        .is_err()
    );
}

/// THE STOCK SLICES' RELEASE IS THE LOCK'S, AND NOTHING STANDS IN FOR A MISSING ONE: a commit or
/// checkout whose lock names `[stock] toolchain` drives that release; one that carries no lock,
/// or a lock with no `[stock]` table, is refused with what is missing named. NEGATIVE CONTROL: a
/// resolver that falls back to rustup's `stable` for the lockless commit answers `Ok("stable")`
/// and the first refusal assertion fails.
#[test]
fn the_stock_release_is_the_locks_and_a_lockless_tree_is_refused() {
    use crate::ledger::GitCli;
    use crate::ledger::testing::{git, init};
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
        for args in [
            &["add", "-A"][..],
            &["commit", "-q", "--allow-empty", "-m", message],
        ] {
            assert!(
                git()
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    };
    let runner = GitCli::new(&repo);
    commit("no lock");
    let refused = stock_toolchain_at(&runner, "HEAD").unwrap_err();
    assert!(
        refused.contains("commit HEAD carries no trust.lock")
            && refused.contains("[stock] toolchain")
            && refused.contains("never falls back"),
        "{refused}"
    );
    let refused = stock_toolchain_in(&repo).unwrap_err();
    assert!(refused.contains("carries no trust.lock"), "{refused}");

    let lock = lock_of(16387, &"4".repeat(64), COMMIT);
    let body = atpkg::trust_lock::render_body(&lock);
    std::fs::write(repo.join(LOCK_FILE), &body).unwrap();
    commit("lock");
    assert_eq!(stock_toolchain_at(&runner, "HEAD").as_deref(), Ok("1.91.1"));
    let below = repo.join("crates/some-crate");
    std::fs::create_dir_all(&below).unwrap();
    assert_eq!(
        stock_toolchain_in(&below).as_deref(),
        Ok("1.91.1"),
        "a checkout's lock is found on the way up, as Trust's own is"
    );

    let without_stock = body
        .split("\n[stock]")
        .next()
        .expect("a rendered lock carries [stock]")
        .to_string();
    assert_ne!(without_stock, body);
    std::fs::write(repo.join(LOCK_FILE), format!("{without_stock}\n")).unwrap();
    commit("lock without [stock]");
    for refused in [
        stock_toolchain_at(&runner, "HEAD").unwrap_err(),
        stock_toolchain_in(&repo).unwrap_err(),
    ] {
        assert!(refused.contains("no `stock.toolchain`"), "{refused}");
    }
}
