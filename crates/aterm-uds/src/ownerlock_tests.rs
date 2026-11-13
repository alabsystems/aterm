// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::fork_window::Parked;

fn release(file: std::fs::File) {
    file.unlock().expect("LOCK_UN");
    drop(file);
}

#[test]
fn the_probe_reads_the_owner_lock() {
    let dir = std::env::temp_dir().join(format!("ownerlock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("m.log");
    let file = std::fs::File::create(&path).expect("create");
    #[expect(
        clippy::disallowed_methods,
        reason = "the test's stand-in owner lets go by `release`: LOCK_UN, then the close"
    )]
    file.try_lock().expect("LOCK_EX|LOCK_NB");
    assert_eq!(probe(&path), Owner::Live, "held through another open");
    assert_eq!(probe_any(&path), Owner::Live, "held through another open");

    let parked = Parked::park();
    release(file);
    assert_eq!(probe(&path), Owner::Dead, "the holder released it");
    let status = parked.release();
    assert!(status.success(), "{status}");

    assert_eq!(probe(&path), Owner::Dead, "the probe released its own lock");
    assert_eq!(probe(&dir.join("absent.log")), Owner::Unknown);
    assert_eq!(probe(&dir), Owner::Unknown, "a directory is no owner lock");
    let link = dir.join("link.log");
    std::os::unix::fs::symlink(&path, &link).expect("symlink");
    assert_eq!(probe(&link), Owner::Unknown, "a symlink is never followed");
    let fifo = dir.join("fifo.log");
    let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).expect("path");
    // SAFETY: a NUL-terminated path that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0, "mkfifo");
    let answer = probe(&fifo);
    if cfg!(target_vendor = "apple") {
        assert_eq!(answer, Owner::Unknown, "Darwin's flock refuses a FIFO");
    } else {
        assert_ne!(answer, Owner::Live, "nobody holds a FIFO's lock");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_child_forked_mid_probe_does_not_keep_the_probes_lock() {
    let dir = std::env::temp_dir().join(format!("ownerlock-mid-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("m.log");
    drop(std::fs::File::create(&path).expect("create"));

    for take in [Take::Shared, Take::Exclusive] {
        let mut parked = None;
        let first = probe_while_locked(&path, take, || parked = Some(Parked::park()));
        assert_eq!(first, Owner::Dead, "nobody holds it");
        let parked = parked.expect("the probe took the free lock and forked holding it");
        assert_eq!(
            probe_any(&path),
            Owner::Dead,
            "the child forked mid-probe kept the probe's lock: the probe closed without \
             LOCK_UN"
        );
        let status = parked.release();
        assert!(status.success(), "{status}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_probe_under_way_is_no_owner_to_another_probe() {
    let dir = std::env::temp_dir().join(format!("ownerlock-two-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("m.log");
    drop(std::fs::File::create(&path).expect("create"));

    let mut second = None;
    let first = probe_while_locked(&path, Take::Shared, || second = Some(probe(&path)));
    assert_eq!(first, Owner::Dead, "nobody holds it");
    assert_eq!(
        second,
        Some(Owner::Dead),
        "the first probe read as an owner"
    );

    let mut any = None;
    probe_while_locked(&path, Take::Shared, || any = Some(probe_any(&path)));
    assert_eq!(any, Some(Owner::Live), "probe_any reads any lock at all");
    let _ = std::fs::remove_dir_all(&dir);
}
