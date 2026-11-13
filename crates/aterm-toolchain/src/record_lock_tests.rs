// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::fs::{Metadata, OpenOptions, TryLockError};
use std::os::fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _, OwnedFd, RawFd};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;

use aterm_spec::wait::{MUST_HAPPEN, until_within};

fn create(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap()
}

#[test]
fn a_received_copy_requires_the_retained_description_not_just_its_inode() {
    use std::os::fd::AsFd as _;
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("original.lock");
    let held = RecordLock::try_lock(create(&path), 0)
        .unwrap()
        .into_close_only();
    let copy = held.try_clone_fd().unwrap();
    held.verify_copy(copy.as_fd()).unwrap();
    let independent = open(&path);
    assert!(held.verify_copy(independent.as_fd()).is_err());
    let other_path = world.path().join("other.lock");
    let other = RecordLock::try_lock(create(&other_path), 0)
        .unwrap()
        .into_close_only();
    let other_copy = other.try_clone_fd().unwrap();
    assert!(held.verify_copy(other_copy.as_fd()).is_err());
    drop(copy);
    assert_blocked(&path);
    assert_blocked(&other_path);
    drop(held);
    drop(reacquire_after_final_close(&path));
}

fn open(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

fn identity(metadata: Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn assert_blocked(path: &Path) {
    assert!(
        matches!(
            RecordLock::try_lock(open(path), 0),
            Err(TryLockError::WouldBlock)
        ),
        "an independent open must observe the actual original lock at {}",
        path.display()
    );
}

fn reacquire_after_final_close(path: &Path) -> RecordLock {
    // A sibling test's concurrent fork can briefly inherit a CLOEXEC descriptor until
    // its exec. Require actual independent reacquisition, with only a hang detector.
    until_within(
        MUST_HAPPEN,
        "the original lock's final close",
        || match RecordLock::try_lock(open(path), 0) {
            Ok(guard) => Some(guard),
            Err(TryLockError::WouldBlock) => None,
            Err(TryLockError::Error(error)) => panic!("reacquire {}: {error}", path.display()),
        },
    )
}

fn flags(fd: RawFd) -> i32 {
    // SAFETY: F_GETFD only queries this number, returning EBADF when it is not open.
    unsafe { libc::fcntl(fd, libc::F_GETFD) }
}

fn inherited_offer(fd: OwnedFd) -> RawFd {
    let previous = flags(fd.as_raw_fd());
    assert!(previous >= 0);
    // SAFETY: this fixture alone owns fd. Clearing its descriptor flag models the actual
    // spawn inheritance offer, without changing stdio or any other descriptor.
    assert_eq!(
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, previous & !libc::FD_CLOEXEC) },
        0
    );
    fd.into_raw_fd()
}

fn reclaim_refused(fd: RawFd, expected: (u64, u64)) -> OwnedFd {
    assert_eq!(
        flags(fd) & libc::FD_CLOEXEC,
        0,
        "refusal must not close or mark the original"
    );
    // SAFETY: this fixture offered its sole owner and adoption refused without taking it.
    // Nothing else closes or replaces the still-open, exclusively offered number.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let file = File::from(fd);
    assert_eq!(identity(file.metadata().unwrap()), expected);
    file.into()
}

/// A child parked between fork and exec holds a copy of every descriptor: a close-only lock
/// never offered out is let go by `LOCK_UN` at its last guard's drop, so it is free at once.
#[test]
fn an_undelegated_close_only_lock_is_free_at_its_last_drop_beside_a_forked_child() {
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("own.lock");
    let held = RecordLock::try_lock(create(&path), 0)
        .unwrap()
        .into_sole_close_only();
    let twin = held.try_clone().unwrap();
    let parked = aterm_uds::fork_window::Parked::park();
    drop(held);
    assert_blocked(&path);
    drop(twin);
    assert!(
        RecordLock::try_lock(open(&path), 0).is_ok(),
        "the last guard of a description never offered out unlocks"
    );
    assert!(parked.release().success());
}

/// A description offered out keeps its lock in the copy: no guard's drop unlocks it.
#[test]
fn an_offered_close_only_lock_stays_held_by_its_copy() {
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("offered-copy.lock");
    let held = RecordLock::try_lock(create(&path), 0)
        .unwrap()
        .into_sole_close_only();
    let copy = held.try_clone_fd().unwrap();
    drop(held);
    assert_blocked(&path);
    drop(copy);
    drop(reacquire_after_final_close(&path));
}

#[test]
fn inherited_actual_guard_adopts_cloexec_and_keeps_lock_after_sender_closes() {
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("offered.lock");
    let sender = RecordLock::try_lock(create(&path), 0)
        .unwrap()
        .into_close_only();
    let original = identity(sender.metadata().unwrap());
    let offered = inherited_offer(sender.try_clone_fd().unwrap());
    // SAFETY: this test exclusively offers the actual sender's shared description; the
    // sender remains held throughout adoption and the subsequent acknowledgement checks.
    let adopted = unsafe { CloseOnlyLock::adopt_inherited(offered, &path) }.unwrap();
    assert_eq!(identity(adopted.metadata().unwrap()), original);
    assert_ne!(adopted.file.as_raw_fd(), offered);
    assert_ne!(flags(adopted.file.as_raw_fd()) & libc::FD_CLOEXEC, 0);
    let exported = adopted.try_clone_fd().unwrap();
    assert_ne!(flags(exported.as_raw_fd()) & libc::FD_CLOEXEC, 0);
    assert_blocked(&path);
    drop(sender);
    assert_blocked(&path);
    drop(adopted);
    assert_blocked(&path);
    drop(exported);
    assert_eq!(
        identity(reacquire_after_final_close(&path).metadata().unwrap()),
        original
    );
}

#[test]
fn inherited_adoption_refuses_wrong_inode_unheld_and_independent_descriptions() {
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("original.lock");
    let sender = RecordLock::try_lock(create(&path), 0)
        .unwrap()
        .into_close_only();
    let original = identity(sender.metadata().unwrap());
    let other_path = world.path().join("other.lock");
    let other = create(&other_path);
    let other_identity = identity(other.metadata().unwrap());
    let offered = inherited_offer(other.into());
    // SAFETY: this fixture exclusively offers a stable raw descriptor. The independently
    // held original prevents an unrelated OFD from being mistaken for shared custody.
    assert!(unsafe { CloseOnlyLock::adopt_inherited(offered, &path) }.is_err());
    drop(reclaim_refused(offered, other_identity));
    assert_blocked(&path);

    let independent = inherited_offer(open(&path).into());
    // SAFETY: the same stable, exclusive offer, but this description is deliberately not
    // the sender's. The genuine sender remains continuously held through the refusal.
    assert!(unsafe { CloseOnlyLock::adopt_inherited(independent, &path) }.is_err());
    drop(reclaim_refused(independent, original));
    assert_blocked(&path);

    let unlocked = inherited_offer(open(&other_path).into());
    // SAFETY: the offered original file is unheld, intentionally failing the lock proof.
    assert!(unsafe { CloseOnlyLock::adopt_inherited(unlocked, &other_path) }.is_err());
    drop(reclaim_refused(unlocked, other_identity));
    let unheld = RecordLock::try_lock(open(&other_path), 0).unwrap();
    drop(unheld);

    let moved = world.path().join("moved.lock");
    let offered = inherited_offer(sender.try_clone_fd().unwrap());
    std::fs::rename(&path, &moved).unwrap();
    drop(create(&path));
    // SAFETY: actual shared offer remains continuously held; the named inode was replaced.
    assert!(unsafe { CloseOnlyLock::adopt_inherited(offered, &path) }.is_err());
    drop(reclaim_refused(offered, original));
    assert_blocked(&moved);
}

#[test]
fn inherited_adoption_leaves_stdio_cloexec_and_nonregular_numbers_untouched() {
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("original.lock");
    let sender = RecordLock::try_lock(create(&path), 0)
        .unwrap()
        .into_close_only();
    for fd in [0, 1, 2, -1] {
        let before = flags(fd);
        // SAFETY: ineligible standard/negative numbers are never owned or changed by this
        // API; this call makes no offer of the test process's standard streams.
        assert!(unsafe { CloseOnlyLock::adopt_inherited(fd, &path) }.is_err());
        assert_eq!(flags(fd), before);
    }
    let cloexec = sender.try_clone_fd().unwrap();
    let before = flags(cloexec.as_raw_fd());
    assert_ne!(before & libc::FD_CLOEXEC, 0);
    // SAFETY: this known ineligible CLOEXEC descriptor remains owned by this fixture.
    assert!(unsafe { CloseOnlyLock::adopt_inherited(cloexec.as_raw_fd(), &path) }.is_err());
    assert_eq!(flags(cloexec.as_raw_fd()), before);
    let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let socket: OwnedFd = socket.into();
    let socket_identity = identity(File::from(socket.try_clone().unwrap()).metadata().unwrap());
    let offered = inherited_offer(socket);
    // SAFETY: exclusive stable offer of a socket, intentionally ineligible as a file guard.
    assert!(unsafe { CloseOnlyLock::adopt_inherited(offered, &path) }.is_err());
    drop(reclaim_refused(offered, socket_identity));
    assert_blocked(&path);
}

#[test]
fn ordinary_guard_drop_still_unlocks_an_independent_file_duplicate() {
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("ordinary.lock");
    let ordinary = RecordLock::try_lock(create(&path), 0).unwrap();
    let duplicate = ordinary.try_clone().unwrap();
    let original = identity(ordinary.metadata().unwrap());
    assert_eq!(identity(duplicate.metadata().unwrap()), original);
    assert_blocked(&path);
    drop(ordinary);
    // A plain File copy does not change the ordinary guard's explicit-unlock contract.
    let reacquired = RecordLock::try_lock(open(&path), 0).unwrap();
    assert_eq!(identity(reacquired.metadata().unwrap()), original);
    drop(reacquired);
    drop(duplicate);
}

#[test]
fn consumed_transfer_and_actual_duplicates_hold_until_the_final_close() {
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("transferred.lock");
    let ordinary = RecordLock::try_lock(create(&path), 0).unwrap();
    let original = identity(ordinary.metadata().unwrap());
    let held = ordinary.into_close_only();
    assert_eq!(identity(held.metadata().unwrap()), original);
    assert_blocked(&path);
    let duplicate = held.try_clone().unwrap();
    assert_eq!(identity(duplicate.metadata().unwrap()), original);
    let descriptor = held.try_clone_fd().unwrap();
    drop(held);
    assert_blocked(&path);
    drop(duplicate);
    assert_blocked(&path);
    drop(descriptor);
    let reacquired = reacquire_after_final_close(&path);
    assert_eq!(identity(reacquired.metadata().unwrap()), original);
}

#[test]
fn close_only_copies_preserve_the_original_inode_when_its_name_is_replaced() {
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("named.lock");
    let original_path = world.path().join("original.lock");
    let held = RecordLock::try_lock(create(&path), 0)
        .unwrap()
        .into_close_only();
    let original = identity(held.metadata().unwrap());
    std::fs::rename(&path, &original_path).unwrap();
    let replacement = create(&path);
    assert_ne!(identity(replacement.metadata().unwrap()), original);
    assert_eq!(identity(held.metadata().unwrap()), original);
    let duplicate = held.try_clone().unwrap();
    assert_eq!(identity(duplicate.metadata().unwrap()), original);
    // The changed pathname is an independent inode: it supplies no custody of the old one.
    let unrelated = RecordLock::try_lock(open(&path), 0).unwrap();
    assert_blocked(&original_path);
    drop(held);
    assert_blocked(&original_path);
    drop(duplicate);
    let reacquired = reacquire_after_final_close(&original_path);
    assert_eq!(identity(reacquired.metadata().unwrap()), original);
    drop(unrelated);
}
