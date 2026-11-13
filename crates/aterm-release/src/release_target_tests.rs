// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::symlink;

fn retire_with(
    clone: &Path,
    drain: &mut dyn FnMut(&Path) -> Result<(), String>,
) -> Result<usize, String> {
    super::retire_using(
        clone,
        |physical| {
            atpkg::build_cache::retirement::stage_with_processes(physical, &|_, _| {
                Ok(Default::default())
            })
        },
        drain,
    )
}

fn legacy(clone: &Path, target: &str) {
    let deps = clone.join("target.noindex/debug/deps");
    std::fs::create_dir_all(&deps).unwrap();
    std::fs::write(deps.join("one"), b"one").unwrap();
    std::fs::write(deps.join("two"), b"two").unwrap();
    std::fs::write(clone.join("target.noindex/debug/.cargo-lock"), b"").unwrap();
    symlink(target, clone.join("target")).unwrap();
    symlink("retired-gate.noindex", clone.join("target-gate")).unwrap();
}

fn retire(clone: &Path) -> Result<usize, String> {
    retire_with(clone, &mut |physical| {
        atpkg::build_cache::retirement::finish_with_processes(physical, &|_, _| {
            Ok(Default::default())
        })
        .map_err(|e| e.to_string())
    })
}

/// NEGATIVE CONTROL: synchronous removal would erase the payload before the drain is
/// scheduled. A second call must schedule the interrupted aside with the original absent.
#[test]
fn links_retire_before_bulk_deletion_and_a_later_call_resumes() {
    let clone = aterm_tempfile::tempdir().unwrap();
    legacy(clone.path(), "target.noindex");
    let mut calls = 0;
    let mut deferred = |_: &Path| {
        calls += 1;
        Ok(())
    };
    assert_eq!(retire_with(clone.path(), &mut deferred), Ok(0));
    let aside = clone.path().join(".ubd/target.noindex.removing");
    assert!(aside.join("debug/deps/one").is_file());
    assert!(!stands(&clone.path().join("target")).unwrap());
    assert!(!stands(&clone.path().join("target-gate")).unwrap());
    assert_eq!(retire_with(clone.path(), &mut deferred), Ok(0));
    assert_eq!(calls, 2);
    assert_eq!(retire(clone.path()), Ok(0));
    assert!(!aside.exists());
}

/// The physical cache goes first, both admitted spellings of its link go, and the retired
/// gate link goes. Mutating the pass to unlink first strands the measured cache population.
#[test]
fn legacy_physical_target_and_links_retire_with_no_deps_entries() {
    for target in ["target.noindex", "./target.noindex"] {
        let clone = aterm_tempfile::tempdir().unwrap();
        legacy(clone.path(), target);

        assert_eq!(retire(clone.path()), Ok(0), "link spelling {target}");
        for name in ["target.noindex", "target", "target-gate"] {
            assert!(
                std::fs::symlink_metadata(clone.path().join(name)).is_err(),
                "{name} retired for link spelling {target}"
            );
        }
    }
}

/// NEGATIVE CONTROL: a held Cargo lock refuses physical retirement, and neither link is
/// removed early. Once released, the same state retires in one pass.
#[test]
fn a_held_build_refuses_retirement_before_either_link_is_removed() {
    let clone = aterm_tempfile::tempdir().unwrap();
    legacy(clone.path(), "target.noindex");
    let lock_path = clone.path().join("target.noindex/debug/.cargo-lock");
    // The suite forks children in parallel. Closing this descriptor alone may leave a
    // child's copy holding the lock on kernels without effective close-on-fork. The
    // standard guard explicitly unlocks before closing, including while unwinding.
    let lock = aterm_uds::file_lock::LockFile::take_existing(
        &lock_path,
        std::time::Instant::now(),
        "the fixture's Cargo lock was unexpectedly held",
    )
    .unwrap();

    let said = retire(clone.path()).expect_err("the live build must refuse retirement");
    assert!(said.contains("could not safely retire"), "{said}");
    assert!(said.contains("2 deps entries remain"), "{said}");
    assert!(clone.path().join("target.noindex/debug/deps/one").is_file());
    assert_eq!(
        std::fs::read_link(clone.path().join("target")).unwrap(),
        Path::new("target.noindex")
    );
    assert!(
        std::fs::symlink_metadata(clone.path().join("target-gate"))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    drop(lock);
    assert_eq!(retire(clone.path()), Ok(0));
}

/// NEGATIVE CONTROL: only the two legacy link shapes belong to this pass. Plain entries and
/// a `target` symlink to somewhere else remain byte-for-byte available.
#[test]
fn plain_entries_and_an_unrelated_target_link_are_never_touched() {
    let plain = aterm_tempfile::tempdir().unwrap();
    for name in ["target", "target-gate"] {
        let dir = plain.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("keep"), name).unwrap();
    }
    assert_eq!(retire(plain.path()), Ok(0));
    for name in ["target", "target-gate"] {
        assert_eq!(
            std::fs::read_to_string(plain.path().join(name).join("keep")).unwrap(),
            name
        );
    }

    let unrelated = aterm_tempfile::tempdir().unwrap();
    symlink("other.noindex", unrelated.path().join("target")).unwrap();
    std::fs::write(unrelated.path().join("target-gate"), b"plain gate").unwrap();
    assert_eq!(retire(unrelated.path()), Ok(0));
    assert_eq!(
        std::fs::read_link(unrelated.path().join("target")).unwrap(),
        Path::new("other.noindex")
    );
    assert_eq!(
        std::fs::read(unrelated.path().join("target-gate")).unwrap(),
        b"plain gate"
    );
}
