// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One forwarding at a time ([`super::FORWARDING_TURN`]): two waits that overlap in one process
//! leave its signal dispositions as they found them.

use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};

/// SIGINT's disposition now (`sigaction` with no new action).
fn sigint_now() -> libc::sighandler_t {
    let mut old = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: a null new action only reads the disposition into `old`, zeroed and live across
    // the call.
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGINT, std::ptr::null(), old.as_mut_ptr()) },
        0
    );
    // SAFETY: written whole by the call that succeeded.
    unsafe { old.assume_init() }.sa_sigaction
}

/// A pipe whose read end children inherit and whose write end none does: the read end's number
/// and the write end.
fn pipe() -> (RawFd, OwnedFd) {
    let mut fds = [0; 2];
    // SAFETY: `pipe` writes two descriptors into the two-element array.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // SAFETY: `fcntl` on the write end this call just made, with plain integers.
    assert_eq!(
        unsafe { libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC) },
        0
    );
    // SAFETY: the write end is this process's own, owned once here.
    (fds[0], unsafe { OwnedFd::from_raw_fd(fds[1]) })
}

/// One wait standing aside for a shell that reads `fd` to its end, armed as a front arms it, on a
/// thread of its own.
fn waiting(fd: RawFd) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let forwarding = super::arm_forwarding();
        let mut child = aterm_uds::spawnfd::transparent("/bin/sh")
            .args(["-c", &format!("cat <&{fd} >/dev/null")])
            .spawn()
            .expect("the shell");
        let status = super::wait_standing_aside(&mut child, forwarding).expect("waited");
        assert!(status.success(), "{status:?}");
    })
}

/// Two waits that overlap, the second starting once the first stands aside (SIGINT ignored) and
/// ending after it, leave SIGINT as it was before either: the second arms only once the first
/// has put its dispositions back. NEGATIVE CONTROL (by hand: `arm_forwarding` taking no turn):
/// the second keeps the first's ignored SIGINT as the one to restore, and leaves it.
#[test]
fn two_overlapping_waits_leave_sigint_as_they_found_it() {
    let before = {
        let _turn = super::FORWARDING_TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sigint_now()
    };
    let (a_read, a_write) = pipe();
    let (b_read, b_write) = pipe();
    let a = waiting(a_read);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the first wait standing aside",
        || (sigint_now() == libc::SIG_IGN).then_some(()),
    );
    let b = waiting(b_read);
    drop(a_write);
    a.join().expect("the first wait");
    drop(b_write);
    b.join().expect("the second wait");
    let _turn = super::FORWARDING_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: closes the two read ends this test made and handed to its shells.
    unsafe {
        libc::close(a_read);
        libc::close(b_read);
    }
    assert_eq!(sigint_now(), before, "SIGINT as the waits found it");
}
