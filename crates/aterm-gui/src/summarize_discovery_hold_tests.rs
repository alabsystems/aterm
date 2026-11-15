// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn timeout_never_writes_a_passed_marker() {
    let root = aterm_tempfile::tempdir().unwrap();
    let directory = PrivateDirectory::open(root.path()).unwrap();
    assert_eq!(
        wait_at(&directory, "claude", Duration::ZERO).unwrap(),
        Outcome::TimedOut
    );
    assert!(root.path().join("claude.held").is_file());
    assert!(root.path().join("claude.timed-out").is_file());
    assert!(!root.path().join("claude.passed").exists());
}

#[test]
fn only_the_requested_readers_release_ends_its_hold() {
    let root = aterm_tempfile::tempdir().unwrap();
    let directory = PrivateDirectory::open(root.path()).unwrap();
    {
        let view = directory.view().unwrap();
        view.write("fleet.release", b"release\n").unwrap();
        assert!(released(&view, "fleet").unwrap());
        assert!(!released(&view, "claude").unwrap());
    }
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| wait_at(&directory, "claude", HANG));
        let until = Instant::now() + HANG;
        while !root.path().join("claude.held").is_file() {
            assert!(Instant::now() < until, "the discovery worker did not enter");
            std::thread::sleep(POLL);
        }
        directory
            .view()
            .unwrap()
            .write("claude.release", b"release\n")
            .unwrap();
        assert_eq!(worker.join().unwrap().unwrap(), Outcome::Released);
    });
    assert!(root.path().join("claude.passed").is_file());
    assert!(!root.path().join("claude.timed-out").exists());
}

#[test]
fn preexisting_release_is_refused_without_a_passed_marker() {
    let root = aterm_tempfile::tempdir().unwrap();
    let directory = PrivateDirectory::open(root.path()).unwrap();
    directory
        .view()
        .unwrap()
        .write("claude.release", b"release\n")
        .unwrap();
    assert!(wait_at(&directory, "claude", HANG).is_err());
    assert!(!root.path().join("claude.held").exists());
    assert!(!root.path().join("claude.passed").exists());
}

#[test]
fn gate_must_be_private_and_beneath_the_lab_home() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = aterm_tempfile::tempdir().unwrap();
    let gate = home.path().join("gate");
    std::fs::create_dir(&gate).unwrap();
    std::fs::set_permissions(&gate, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(open_gate(&gate, home.path()).is_ok());
    assert!(open_gate(home.path(), home.path()).is_err());
    let outside = aterm_tempfile::tempdir().unwrap();
    assert!(open_gate(outside.path(), home.path()).is_err());
    std::fs::set_permissions(&gate, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(open_gate(&gate, home.path()).is_err());
}
