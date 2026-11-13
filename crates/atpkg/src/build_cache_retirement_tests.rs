// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// The bounded survey is the negative control: its small budget refuses exactly the
/// fixture retirement must handle, including a Cargo lock beyond the usual profile depth.
#[test]
fn oversized_retirement_keeps_every_lock_and_resumes_its_aside() {
    inventory::with_entry_budget(8, oversized_retirement);
}

fn oversized_retirement() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dir = scratch.path().join("target.noindex");
    let profile = dir.join("debug/tmp/nested/target/debug");
    std::fs::create_dir_all(&profile).unwrap();
    let deps = dir.join("debug/deps");
    std::fs::create_dir(&deps).unwrap();
    for n in 0..32 {
        std::fs::write(deps.join(format!("artifact-{n}")), b"artifact").unwrap();
    }
    let lock_path = profile.join(".cargo-lock");
    std::fs::write(&lock_path, b"").unwrap();
    assert!(inventory::scan(&dir).is_err());
    let fence = Fence::new(&|_, _| Ok(Default::default()));
    let held = super::super::hold(&[lock_path]).unwrap();
    assert!(
        stage_on(&dir, &fence).is_err(),
        "even a deep held lock keeps this oversized tree"
    );
    assert!(dir.is_dir());
    drop(held);

    let stages = stage_on(&dir, &fence).unwrap();
    assert!(!dir.exists());
    assert_eq!(stages.len(), 1);
    assert_eq!(stages[0].parent().unwrap(), scratch.path().join(".ubd"));
    assert!(
        stages[0].join("debug/deps/artifact-0").is_file(),
        "staging does not delete synchronously"
    );
    assert_eq!(
        stage_on(&dir, &fence).unwrap(),
        stages,
        "an interrupted drain is rediscovered"
    );
    finish_on(&dir, &fence).unwrap();
    assert!(!stages[0].exists());
    assert!(stage_on(&dir, &fence).unwrap().is_empty());
    println!(
        "retirement fixture: bounded inventory=refused, deps artifacts before=32 after=0; aside drained"
    );
}

#[test]
fn resumed_asides_recheck_locks_processes_and_custody() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dir = scratch.path().join("target.noindex");
    std::fs::create_dir_all(dir.join("debug")).unwrap();
    std::fs::write(dir.join("debug/.cargo-lock"), b"").unwrap();
    let clear = Fence::new(&|_, _| Ok(Default::default()));
    let aside = stage_on(&dir, &clear).unwrap().remove(0);
    let held = super::super::hold(&[aside.join("debug/.cargo-lock")]).unwrap();
    assert!(finish_on(&dir, &clear).is_err());
    assert!(aside.is_dir());
    drop(held);
    let unavailable = Fence::new(&|_, _| Err(std::io::Error::other("unavailable")));
    assert!(finish_on(&dir, &unavailable).is_err());
    std::fs::write(aside.join(super::super::registered::MARKER), b"kept").unwrap();
    assert!(finish_on(&dir, &clear).is_err());
    assert!(aside.join("debug/.cargo-lock").is_file());
    std::fs::remove_file(aside.join(super::super::registered::MARKER)).unwrap();
    // A fresh target belongs to the next build and must survive finishing the old aside.
    std::fs::create_dir(&dir).unwrap();
    finish_on(&dir, &clear).unwrap();
    assert!(dir.is_dir());
    assert!(!aside.exists());
}

/// Old adjacent asides remain recoverable; a linked holder never extends deletion
/// authority into an unrelated folder.
#[test]
fn old_asides_resume_but_a_linked_holder_is_never_used() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dir = scratch.path().join("target.noindex");
    let old = scratch.path().join("target.noindex.removing-2");
    std::fs::create_dir(&old).unwrap();
    std::fs::write(old.join("old-artifact"), b"old").unwrap();
    let external = scratch.path().join("external");
    std::fs::create_dir(&external).unwrap();
    let foreign = external.join("target.noindex.removing");
    std::fs::create_dir(&foreign).unwrap();
    std::os::unix::fs::symlink(&external, scratch.path().join(".ubd")).unwrap();
    let clear = Fence::new(&|_, _| Ok(Default::default()));
    assert_eq!(
        stage_on(&dir, &clear).unwrap().as_slice(),
        std::slice::from_ref(&old)
    );
    finish_on(&dir, &clear).unwrap();
    assert!(!old.exists());
    assert!(foreign.is_dir());
    std::fs::create_dir(&dir).unwrap();
    assert!(stage_on(&dir, &clear).is_err());
    assert!(dir.is_dir());
    assert!(foreign.is_dir());
}
