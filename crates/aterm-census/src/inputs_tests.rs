// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[cfg(unix)]
#[test]
fn followed_links_cannot_hide_inputs_outside_the_package() {
    let root = aterm_tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("crates/a")).unwrap();
    let target = root.path().join("shared.rs");
    std::fs::write(&target, "shared").unwrap();
    let link = root.path().join("crates/a/link.rs");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let (_, proof) = capture(root.path(), || crate::tree::read_to_string(&link).unwrap());
    assert!(proof.is_none());
    let (_, proof) = capture(root.path(), || {
        crate::tree::read_to_string(&target).unwrap()
    });
    assert!(proof.is_some());
    std::fs::remove_file(&target).unwrap();
    let (_, proof) = capture(root.path(), || crate::tree::exists(&link));
    assert!(
        proof.is_none(),
        "a dangling link can acquire an external target"
    );
}

/// A read on a thread the census started is one of its inputs: the lazy-init, lock-order and
/// main-loop censuses read on worker threads.
/// NEGATIVE CONTROL: a thread started outside `memo::threads` records nothing, so its
/// package is missing from the evidence.
#[test]
fn a_census_threads_reads_are_its_inputs() {
    let root = aterm_tempfile::tempdir().unwrap();
    let file = root.path().join("crates/a/src/lib.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "one").unwrap();
    let packages = |inputs: Option<Vec<Input>>| -> Vec<String> {
        inputs
            .expect("bounded")
            .into_iter()
            .map(|(kind, path, _)| format!("{kind} {path}"))
            .collect()
    };
    let (_, carried) = capture(root.path(), || {
        crate::memo::threads(|s| s.spawn(|| crate::tree::read_to_string(&file)).join())
            .unwrap()
            .unwrap()
    });
    for own_worker in [false, true] {
        let read = || {
            crate::memo::threads(|scope| scope.spawn(|| crate::tree::read_to_string(&file)).join())
                .unwrap()
                .unwrap()
        };
        let (text, serial) = capture(root.path(), || {
            if own_worker {
                crate::on_one_worker(read).unwrap()
            } else {
                crate::without_parallel_workers(read)
            }
        });
        assert_eq!(text, "one");
        assert_eq!(
            serial, carried,
            "serial work retains identical input evidence"
        );
    }
    assert_eq!(packages(carried), ["tree crates/a"]);
    let (_, raw) = capture(root.path(), || {
        std::thread::scope(|s| s.spawn(|| crate::tree::read_to_string(&file)).join())
            .unwrap()
            .unwrap()
    });
    assert!(packages(raw).is_empty());
}

#[test]
fn package_changes_additions_and_mid_read_drift_invalidate_evidence() {
    let root = aterm_tempfile::tempdir().unwrap();
    let file = root.path().join("crates/a/src/lib.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "one").unwrap();
    let (_, proof) = capture(root.path(), || crate::tree::read_to_string(&file).unwrap());
    let proof = proof.unwrap();
    assert!(matches(root.path(), &proof));
    std::fs::create_dir_all(root.path().join("crates/b")).unwrap();
    std::fs::write(root.path().join("crates/b/unrelated.rs"), "unrelated").unwrap();
    assert!(
        matches(root.path(), &proof),
        "another package is not this reader's input"
    );
    std::fs::write(file.parent().unwrap().join("extra.rs"), "new").unwrap();
    assert!(!matches(root.path(), &proof));
    let (_, changed) = capture(root.path(), || {
        crate::tree::read_to_string(&file).unwrap();
        std::fs::write(&file, "changed during analysis").unwrap();
    });
    assert!(changed.is_none());
}
