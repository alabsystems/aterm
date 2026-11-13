// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The store's side of a tree's `trust.lock`: folding an unrecorded build, the shim hop's locked
//! target, and what a shim says about locks.

use super::*;

const COMMIT: &str = "29ebb403de7a102850d6cb10a62d5cc3baf723ed";

/// A complete store build `build` under `prefix` whose `bin` holds runnable `targo` and
/// `targo-tippy` stand-ins that print `build` (named with the platform's executable suffix, as
/// [`locked_build`] looks for them), and its tree's fold. Off Unix the stand-ins' modes are
/// declared beside the build, as an install records them ([`crate::store::declared_modes`]).
fn store_build(prefix: &Path, build: u64) -> String {
    let dir = build_dir(prefix, build);
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(ready_marker(prefix, build), "ok\n").unwrap();
    let mut modes = crate::tree::DeclaredModes::new();
    for tool in ["targo", "targo-tippy"] {
        let file = aterm_toolchain::tool_file(tool);
        let path = bin.join(&file);
        std::fs::write(&path, format!("#!/bin/sh\necho {build}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        modes.insert(format!("bin/{file}").into_bytes(), 0o755);
    }
    crate::store::write_declared_modes(&dir, &modes).unwrap();
    let modes = crate::store::declared_modes(&dir).unwrap();
    crate::tree::tree_root_declared(&dir, &modes).unwrap()
}

fn lock_of(build: u64, tree_root: &str) -> TrustLock {
    TrustLock {
        build,
        tree_root: tree_root.to_string(),
        compiler_commit: COMMIT.to_string(),
        receipt: "SEED-MINT sha256:00".to_string(),
        stock_toolchain: "1.99.1".to_string(),
    }
}

fn write_lock(tree: &Path, lock: &TrustLock) {
    std::fs::create_dir_all(tree).unwrap();
    std::fs::write(tree.join(LOCK_FILE), render_body(lock)).unwrap();
}

#[test]
fn an_unrecorded_build_is_folded_once_and_the_fold_recorded() {
    let scratch = crate::testdir::dir("atpkg-lock-fold-");
    let prefix = scratch.join("pkg");
    let root = store_build(&prefix, 16387);
    let lock = lock_of(16387, &root);
    assert_eq!(locked_build(&prefix, &lock), LockedBuild::Unrecorded);
    let bin = build_dir(&prefix, 16387).join("bin");
    assert_eq!(
        standing(&prefix, &lock),
        LockedBuild::Present { bin: bin.clone() }
    );
    assert_eq!(
        locked_build(&prefix, &lock),
        LockedBuild::Present { bin },
        "the fold is recorded: the next look is a read"
    );
    let other = lock_of(16387, &"a".repeat(64));
    assert_eq!(
        standing(&prefix, &other),
        LockedBuild::Mismatch { recorded: root },
        "a lock naming another tree under the same number is refused"
    );
}

#[cfg(unix)]
#[test]
fn a_scheduled_reclaim_does_not_make_a_complete_build_unrecorded() {
    use std::time::{Duration, Instant};

    let scratch = crate::testdir::dir("atpkg-lock-fold-reclaim-");
    let prefix = scratch.join("pkg");
    let root = store_build(&prefix, 16387);
    let lock = lock_of(16387, &root);
    let present = LockedBuild::Present {
        bin: build_dir(&prefix, lock.build).join("bin"),
    };
    assert_eq!(
        standing_started(&prefix, &lock, Instant::now() - Duration::from_secs(6)),
        present
    );
    assert_eq!(locked_build(&prefix, &lock), present);
    assert_eq!(
        standing_started(&prefix, &lock, Instant::now() - Duration::from_secs(61)),
        present,
        "an already recorded complete build needs no fold lease"
    );
    assert_eq!(
        standing(&prefix, &lock_of(lock.build, &"a".repeat(64))),
        LockedBuild::Mismatch { recorded: root }
    );

    let root = store_build(&prefix, 16388);
    let lock = lock_of(16388, &root);
    let subject = crate::lease::Subject::build(PROGRAM, lock.build).unwrap();
    let crate::lease::Reclaim::Clear(held) = crate::lease::reclaim(&prefix, &subject) else {
        panic!("the complete build has no active lease");
    };
    assert_eq!(
        standing_started(&prefix, &lock, Instant::now() - Duration::from_secs(61)),
        LockedBuild::Unrecorded
    );
    assert!(!tree_root_record(&prefix, lock.build).exists());
    held.check_subject(&prefix, &subject).unwrap();
    drop(held);
    let result = standing(&prefix, &lock);
    assert_eq!(
        result,
        LockedBuild::Present {
            bin: build_dir(&prefix, lock.build).join("bin")
        }
    );
    assert_eq!(locked_build(&prefix, &lock), result);
}

/// A TREE THE STORE DOES NOT READ COMPLETE IS NO LOCKED BUILD, and is never folded: one with no
/// ready marker (a stage cut short, or a build its resolve check refused while a first-exec hold
/// kept the tree) and one whose marker records another slice both stand absent, recorded or not.
/// NEGATIVE CONTROL: the same tree, marked ready for this slice, folds to the lock's root.
#[test]
fn a_build_the_store_does_not_read_complete_stands_absent() {
    let scratch = crate::testdir::dir("atpkg-lock-incomplete-");
    let prefix = scratch.join("pkg");
    let root = store_build(&prefix, 16387);
    let lock = lock_of(16387, &root);
    let marker = ready_marker(&prefix, 16387);
    let record = tree_root_record(&prefix, 16387);

    std::fs::remove_file(&marker).unwrap();
    assert_eq!(standing(&prefix, &lock), LockedBuild::Absent);
    assert!(!record.exists(), "an incomplete tree is not folded");

    std::fs::write(&marker, "ok\nplatform=no-such-slice\n").unwrap();
    assert_eq!(
        locked_build(&prefix, &lock),
        LockedBuild::Unrecorded,
        "discovery's cheap read sees a marker"
    );
    assert_eq!(standing(&prefix, &lock), LockedBuild::Absent);
    assert!(!record.exists(), "a tree of another slice is not folded");
    write_tree_root_record(&build_dir(&prefix, 16387), &root).unwrap();
    assert_eq!(
        standing(&prefix, &lock),
        LockedBuild::Absent,
        "nor stands present by its record"
    );

    std::fs::write(&marker, "ok\n").unwrap();
    std::fs::remove_file(&record).unwrap();
    assert_eq!(
        standing(&prefix, &lock),
        LockedBuild::Present {
            bin: build_dir(&prefix, 16387).join("bin")
        }
    );
}

#[test]
fn a_fold_that_is_not_the_locks_tree_is_a_mismatch() {
    let scratch = crate::testdir::dir("atpkg-lock-fold-other-");
    let prefix = scratch.join("pkg");
    let root = store_build(&prefix, 9);
    let lock = lock_of(9, &"b".repeat(64));
    assert_eq!(
        standing(&prefix, &lock),
        LockedBuild::Mismatch { recorded: root }
    );
}

#[cfg(unix)]
#[test]
fn the_shim_hop_runs_the_locked_build_whatever_current_is() {
    let scratch = crate::testdir::dir("atpkg-lock-hop-");
    let prefix = scratch.join("pkg");
    let locked_root = store_build(&prefix, 16387);
    store_build(&prefix, 16500);
    let tree = scratch.join("checkout");
    let cwd = tree.join("crates/x");
    std::fs::create_dir_all(&cwd).unwrap();
    let laid = |build: u64, tool: &str| build_dir(&prefix, build).join("bin").join(tool);

    // No lock: the shim runs what it was laid for.
    assert_eq!(
        shim_target(&laid(16500, "targo"), &cwd, &mut no_install),
        Ok(None)
    );

    write_lock(&tree, &lock_of(16387, &locked_root));
    // Current flipped to 16500 (the shim was laid for it): the hop still runs 16387, for each tool.
    assert_eq!(
        shim_target(&laid(16500, "targo"), &cwd, &mut no_install),
        Ok(Some(laid(16387, "targo")))
    );
    assert_eq!(
        shim_target(&laid(16500, "targo-tippy"), &cwd, &mut no_install),
        Ok(Some(laid(16387, "targo-tippy")))
    );
    // Laid for the locked build itself: run as laid.
    assert_eq!(
        shim_target(&laid(16387, "targo"), &cwd, &mut no_install),
        Ok(None)
    );
    assert!(
        locked_within_keep(&build_dir(&prefix, 16387), std::time::SystemTime::now()),
        "a resolution marks the build for gc to keep"
    );
    // A dev-linked checkout's target is not a store build: the link takes the lock's place.
    let linked = scratch.join("trust-checkout/bin/targo");
    assert_eq!(shim_target(&linked, &cwd, &mut no_install), Ok(None));
}

#[cfg(unix)]
fn no_install(_: &Path, _: &TrustLock) -> Result<(), String> {
    panic!("no install was due")
}

/// A LOCKED TREE NEVER FALLS BACK TO THE LAID BUILD: a locked build the store lacks is installed
/// first and run once it stands as the lock names it; an install that fails, or stages another
/// tree, refuses, as do a store build of another tree and a lock that does not read.
#[cfg(unix)]
#[test]
fn a_locked_tree_never_falls_back_to_the_laid_build() {
    let scratch = crate::testdir::dir("atpkg-lock-refuse-");
    let prefix = scratch.join("pkg");
    store_build(&prefix, 16500);
    let staging = scratch.join("staging");
    let root = store_build(&staging, 16387);
    let tree = scratch.join("checkout");
    let laid = build_dir(&prefix, 16500).join("bin/targo");
    write_lock(&tree, &lock_of(16387, &root));

    let failed = shim_target(&laid, &tree, &mut |_, _| Err("offline".to_string())).unwrap_err();
    assert!(failed.contains("its install failed: offline"), "{failed}");
    assert!(!build_dir(&prefix, 16387).exists());

    let mut installs = 0;
    let ran = shim_target(&laid, &tree, &mut |_, asked| {
        installs += 1;
        assert_eq!(store_build(&prefix, asked.build), asked.tree_root);
        Ok(())
    });
    assert_eq!(ran, Ok(Some(build_dir(&prefix, 16387).join("bin/targo"))));
    assert_eq!(installs, 1);

    write_lock(&tree, &lock_of(16387, &"c".repeat(64)));
    let mismatch = shim_target(&laid, &tree, &mut no_install).unwrap_err();
    assert!(mismatch.contains("another build"), "{mismatch}");

    let other = scratch.join("other");
    let other_laid = {
        store_build(&other, 16500);
        build_dir(&other, 16500).join("bin/targo")
    };
    write_lock(&tree, &lock_of(16387, &root));
    let staged_other = shim_target(&other_laid, &tree, &mut |_, asked| {
        store_build(&other, asked.build);
        std::fs::write(build_dir(&other, asked.build).join("bin/extra"), b"x").unwrap();
        Ok(())
    })
    .unwrap_err();
    assert!(staged_other.contains("another build"), "{staged_other}");

    std::fs::write(tree.join(LOCK_FILE), "version = 1\n").unwrap();
    let unreadable = shim_target(&laid, &tree, &mut no_install).unwrap_err();
    assert!(
        unreadable.contains("trust.lock does not read"),
        "{unreadable}"
    );
}

#[test]
fn a_laid_shim_disagreeing_with_the_lock_is_said_and_agreeing_is_not() {
    let found = Found {
        path: PathBuf::from("/c/trust.lock"),
        lock: Ok(lock_of(83, &"d".repeat(64))),
    };
    let lock = found.lock.clone().unwrap();
    let laid = |build| ShimAnswer::Laid {
        target: PathBuf::from(format!("/p/store/trust/{build}/bin/targo")),
        build: Some(build),
    };
    let resolved = PathBuf::from("/p/store/trust/83/bin");
    let said = shim_mismatch(&laid(16387), &found, &lock, &resolved).expect("a mismatch");
    assert!(said.starts_with("plain `targo` resolves 16387"), "{said}");
    assert!(said.contains("/c/trust.lock names 83"), "{said}");
    assert_eq!(shim_mismatch(&laid(83), &found, &lock, &resolved), None);
    assert_eq!(
        shim_mismatch(&ShimAnswer::Locked, &found, &lock, &resolved),
        None
    );
    // A dev-linked checkout's shim runs what discovery chose in the lock's place: no mismatch.
    let linked = ShimAnswer::Laid {
        target: PathBuf::from("/dev/trust/bin/targo"),
        build: None,
    };
    assert_eq!(
        shim_mismatch(&linked, &found, &lock, Path::new("/dev/trust/bin")),
        None
    );
    assert!(shim_mismatch(&linked, &found, &lock, &resolved).is_some());
}

#[cfg(unix)]
#[test]
fn a_shim_without_the_hop_runs_its_laid_build() {
    let scratch = crate::testdir::dir("atpkg-lock-answer-");
    let prefix = scratch.join("pkg");
    assert_eq!(shim_answer(&prefix), ShimAnswer::NoShim);
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    let target = build_dir(&prefix, 16387).join("bin/targo");
    std::fs::write(
        prefix.join("bin/targo"),
        format!(
            "#!/bin/sh\n# atpkg shim\nexec '{}' \"$@\"\n",
            target.display()
        ),
    )
    .unwrap();
    assert_eq!(
        shim_answer(&prefix),
        ShimAnswer::Laid {
            target,
            build: Some(16387)
        }
    );
}

/// What discovery is handed for a locked tree stands ready for its pure read: an unrecorded build
/// is folded and marked, so discovery resolves it where `find` alone leaves it unrecorded (the
/// negative control), and a tree with no lock hands nothing.
#[test]
fn found_for_folds_and_marks_what_discovery_reads() {
    let scratch = crate::testdir::dir("atpkg-lock-found-");
    let prefix = scratch.join("pkg");
    let root = store_build(&prefix, 16387);
    let tree = scratch.join("checkout");
    write_lock(&tree, &lock_of(16387, &root));
    let deep = tree.join("crates/one");
    std::fs::create_dir_all(&deep).unwrap();

    let bare = find(&deep).unwrap();
    let lock = bare.lock.clone().unwrap();
    assert_eq!(locked_build(&prefix, &lock), LockedBuild::Unrecorded);
    assert!(!locked_mark(&prefix, 16387).exists());

    let found = found_for(&deep, Some(&prefix)).unwrap();
    assert_eq!(found.path, tree.join(LOCK_FILE));
    assert!(matches!(
        locked_build(&prefix, &lock),
        LockedBuild::Present { .. }
    ));
    assert!(locked_mark(&prefix, 16387).exists());

    assert!(found_for(&scratch, Some(&prefix)).is_none());
}
