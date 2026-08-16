// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `SCM_RIGHTS` descriptor passing against a real kernel — the transport the
//! seamless handoff needs once the successor is a LaunchServices instance with
//! its own launchd job instead of a fork child, and therefore inherits no
//! descriptor table to find PTY masters in.
//!
//! Every assertion here is one of the measured facts `aterm_uds::fdpass` is
//! built on, run rather than cited:
//!
//! * descriptors arrive at DIFFERENT numbers naming the SAME open file
//!   descriptions — asserted via `fstat` identity AND a shared file offset,
//!   because the offset is a property of the open file description and a
//!   fresh `open` of the same path would not share it. This is exactly why
//!   the handoff's adoption proof may not name a descriptor number once the
//!   masters travel here;
//! * `FD_CLOEXEC` is FALSE on arrival (Darwin has no `MSG_CMSG_CLOEXEC`), so
//!   the receiver must set it itself, immediately;
//! * an over-budget receive is REFUSED and leaks nothing — the descriptors
//!   the kernel installed before we could refuse are all closed again.
//!
//! Descriptor-count assertions are process-wide, so every test in this file
//! takes [`serial`]: cargo runs these in parallel threads of ONE process, and
//! another test opening a file mid-count would make the leak assertion read
//! like a leak.
#![cfg(any(
    target_vendor = "apple",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Seek, SeekFrom, Write};
use std::mem::ManuallyDrop;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use aterm_uds::fdpass::{self, MAX_FDS};
use aterm_uds::{CtlListener, CtlStream};

unsafe extern "C" {
    fn fcntl(fd: RawFd, cmd: i32, ...) -> i32;
}
const F_GETFD: i32 = 1;
const F_SETFD: i32 = 2;
const FD_CLOEXEC: i32 = 1;

/// Descriptor numbers the probes below sweep. Received descriptors land at the
/// lowest free numbers, so a leak always lands well inside this range.
const PROBE_LIMIT: RawFd = 1024;

/// Serialize this file's tests: the descriptor-count probes are process-wide.
/// Poisoning is deliberately ignored — one failing test must not turn every
/// other test in the file into a cascade of lock panics.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A fresh per-test directory (same shape as `roundtrip.rs`).
fn test_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aterm-uds-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

/// A regular file with 16 bytes in it, seeked to `offset`.
fn seeded_file(dir: &std::path::Path, name: &str, offset: u64) -> File {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(dir.join(name))
        .expect("open");
    f.write_all(b"0123456789abcdef").expect("write");
    f.seek(SeekFrom::Start(offset)).expect("seek");
    f
}

/// The `fstat`-derived identity of an open file: what survives a descriptor
/// transfer, unlike the number. (`st_rdev` is the PTY twin of this — every
/// `/dev/ptmx` open takes its own minor.)
fn identity(f: &File) -> (u64, u64) {
    let m = f.metadata().expect("fstat");
    (m.dev(), m.ino())
}

/// This process's open-descriptor count.
fn open_descriptor_count() -> usize {
    (0..PROBE_LIMIT)
        // SAFETY: `F_GETFD` only reads the descriptor flags; an unopened
        // number answers -1/EBADF, which is exactly what is being counted.
        .filter(|&fd| unsafe { fcntl(fd, F_GETFD) } != -1)
        .count()
}

/// How many of this process's descriptors name `(dev, ino)`. Sharper than the
/// bare count for a leak assertion — it cannot be perturbed by an unrelated
/// descriptor, only by another copy of the very file that was passed.
fn descriptors_naming(want: (u64, u64)) -> usize {
    (0..PROBE_LIMIT)
        .filter(|&fd| {
            // SAFETY: the `File` is wrapped in `ManuallyDrop` and never
            // dropped, so this borrows the descriptor for one `fstat` and
            // never closes a descriptor it does not own. An invalid number
            // fails `metadata()` (EBADF) and counts as no match.
            let f = ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
            f.metadata().is_ok_and(|m| (m.dev(), m.ino()) == want)
        })
        .count()
}

/// Descriptor flags of `fd`, or -1.
fn descriptor_flags(fd: RawFd) -> i32 {
    // SAFETY: `F_GETFD` only reads the descriptor flags of a descriptor this
    // process owns.
    unsafe { fcntl(fd, F_GETFD) }
}

/// THE property the handoff buys with this transport: three descriptors ride
/// one message, and each arrives as the SAME OPEN FILE DESCRIPTION at a
/// DIFFERENT number. Identity is `fstat`-derived (never the number), and the
/// shared file offset is the part a re-`open` of the same path could not fake.
#[test]
fn three_descriptors_arrive_as_the_same_open_file_descriptions() {
    let _guard = serial();
    let dir = test_dir("fdpass-identity");
    let files: Vec<(File, u64)> = [3u64, 5, 7]
        .into_iter()
        .enumerate()
        .map(|(i, offset)| (seeded_file(&dir, &format!("m{i}"), offset), offset))
        .collect();

    let (a, b) = CtlStream::pair().expect("pair");
    let borrowed: Vec<BorrowedFd<'_>> = files.iter().map(|(f, _)| f.as_fd()).collect();
    assert_eq!(
        fdpass::send_with_fds(&a, b"PTY", &borrowed).expect("send"),
        3,
        "the whole header rode one sendmsg"
    );

    let mut buf = [0u8; 16];
    let got = fdpass::recv_with_fds(&b, &mut buf, 3).expect("receive");
    assert_eq!(
        &buf[..got.bytes],
        b"PTY",
        "payload rides with the descriptors"
    );
    assert_eq!(got.fds.len(), 3, "three descriptors on one message");

    for ((original, offset), received) in files.iter().zip(got.fds) {
        assert_ne!(
            received.as_raw_fd(),
            original.as_raw_fd(),
            "the kernel picks the receiver's numbers — this is why the \
             adoption proof cannot name one"
        );
        let mut received = File::from(received);
        assert_eq!(
            identity(&received),
            identity(original),
            "same file, proved by fstat rather than by a number"
        );
        assert_eq!(
            received.stream_position().expect("tell"),
            *offset,
            "same OPEN FILE DESCRIPTION: the offset is shared, which a fresh \
             open of the same path would not be"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// `FD_CLOEXEC` is false on arrival and Darwin has no `MSG_CMSG_CLOEXEC`, so
/// the receiver sets it itself. The sender's own flag is CLEARED first, which
/// kills the "it was just inherited" reading of a passing test as well.
#[test]
fn received_descriptors_are_close_on_exec_on_arrival() {
    let _guard = serial();
    let dir = test_dir("fdpass-cloexec");
    let f = seeded_file(&dir, "m", 0);
    // SAFETY: clearing the descriptor flags of a descriptor this process owns.
    assert_ne!(unsafe { fcntl(f.as_raw_fd(), F_SETFD, 0) }, -1, "clear");
    assert_eq!(descriptor_flags(f.as_raw_fd()), 0, "sender is NOT cloexec");

    let (a, b) = CtlStream::pair().expect("pair");
    fdpass::send_with_fds(&a, b"1", &[f.as_fd()]).expect("send");
    let mut buf = [0u8; 4];
    let got = fdpass::recv_with_fds(&b, &mut buf, 1).expect("receive");
    let received = got.fds.first().expect("one descriptor");
    assert_eq!(
        descriptor_flags(received.as_raw_fd()),
        FD_CLOEXEC,
        "a received descriptor must be close-on-exec before anything can exec"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The measured trap: a receiver that budgets for fewer descriptors than were
/// sent still ends up with the extras in its table. So an over-count is
/// refused, and the refusal closes everything — the process is left exactly as
/// it was, both in total descriptors and in descriptors naming the passed
/// file.
#[test]
fn an_over_budget_receive_is_refused_without_leaking_descriptors() {
    let _guard = serial();
    let dir = test_dir("fdpass-overbudget");
    let f = seeded_file(&dir, "m", 0);
    let want = identity(&f);

    let (a, b) = CtlStream::pair().expect("pair");
    let fd = f.as_fd();
    fdpass::send_with_fds(&a, b"1", &[fd, fd, fd]).expect("send three");

    let before_total = open_descriptor_count();
    let before_named = descriptors_naming(want);
    assert_eq!(before_named, 1, "only the sender's own open, so far");

    let mut buf = [0u8; 4];
    let err = fdpass::recv_with_fds(&b, &mut buf, 2).expect_err("three > two");
    assert_eq!(
        err.kind(),
        ErrorKind::InvalidData,
        "a peer over-send is peer misbehavior, not caller misuse: {err}"
    );
    assert_eq!(
        open_descriptor_count(),
        before_total,
        "the refusal closed every descriptor it received"
    );
    assert_eq!(
        descriptors_naming(want),
        before_named,
        "no copy of the passed file survived the refusal"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The two degenerate shapes the handoff's read loop still has to survive: a
/// message carrying no descriptors at all (an ordinary write, which must not
/// read as a truncated control message), and a hung-up peer, which must read
/// as EOF rather than as an error or an empty success that loops forever.
#[test]
fn a_plain_message_and_a_hangup_are_ordinary_receives() {
    let _guard = serial();
    let (a, b) = CtlStream::pair().expect("pair");

    fdpass::send_with_fds(&a, b"bare", &[]).expect("send with no descriptors");
    let mut buf = [0u8; 8];
    let got = fdpass::recv_with_fds(&b, &mut buf, 3).expect("receive");
    assert_eq!(&buf[..got.bytes], b"bare");
    assert!(got.fds.is_empty(), "no control message, no descriptors");

    drop(a);
    let eof = fdpass::recv_with_fds(&b, &mut buf, 3).expect("hangup is not an error");
    assert_eq!(eof.bytes, 0, "a hung-up peer reads as EOF");
    assert!(eof.fds.is_empty());
}

/// The kernel-attested peer pid over a real bind/connect/accept, on both ends.
/// Unforgeable identity is the point: it is what lets the handoff trust a
/// process it did not fork, paired with the birth record that rules out a
/// recycled pid.
#[test]
fn peer_pid_is_the_kernel_attested_peer() {
    let _guard = serial();
    let dir = test_dir("fdpass-peerpid");
    let path = dir.join("aterm-9.sock");
    let listener = CtlListener::bind(&path).expect("bind");
    let client = CtlStream::connect(&path).expect("connect");
    let (accepted, _) = listener.accept().expect("accept");

    let us = std::process::id();
    assert_eq!(fdpass::peer_pid(&client).expect("client side"), us);
    assert_eq!(fdpass::peer_pid(&accepted).expect("server side"), us);

    drop(listener);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Caller misuse is refused BEFORE the kernel is asked — proved by the peer
/// seeing nothing at all afterwards. A zero-length `sendmsg` would otherwise
/// transfer nothing on Linux while still consuming the descriptors.
#[test]
fn misuse_is_refused_before_the_kernel_is_asked() {
    let _guard = serial();
    let dir = test_dir("fdpass-misuse");
    let f = seeded_file(&dir, "m", 0);
    let (a, b) = CtlStream::pair().expect("pair");

    let empty_payload = fdpass::send_with_fds(&a, b"", &[f.as_fd()]).expect_err("empty payload");
    assert_eq!(empty_payload.kind(), ErrorKind::InvalidInput);

    let too_many = [f.as_fd(); MAX_FDS + 1];
    let over_cap = fdpass::send_with_fds(&a, b"1", &too_many).expect_err("over the cap");
    assert_eq!(over_cap.kind(), ErrorKind::InvalidInput);

    let mut buf = [0u8; 4];
    let no_room = fdpass::recv_with_fds(&b, &mut [], 1).expect_err("empty buffer");
    assert_eq!(no_room.kind(), ErrorKind::InvalidInput);
    let over_max = fdpass::recv_with_fds(&b, &mut buf, MAX_FDS + 1).expect_err("over the cap");
    assert_eq!(over_max.kind(), ErrorKind::InvalidInput);

    // Nothing was sent: a timed receive finds an empty socket rather than a
    // message whose descriptors were quietly consumed.
    b.set_read_timeout(Some(Duration::from_millis(150)))
        .expect("timeout");
    let idle = fdpass::recv_with_fds(&b, &mut buf, 1).expect_err("nothing queued");
    assert!(
        matches!(idle.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
        "a refused send must not have reached the peer, got {:?}",
        idle.kind()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
