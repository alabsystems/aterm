// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One forwarding at a time ([`super::FORWARDING_TURN`]): two waits that overlap in one process
//! leave its signal dispositions as they found them.

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};

use aterm_spec::wait::{MUST_HAPPEN, until_within};

/// Set, the test binary is [`two_overlapping_waits_leave_sigint_as_they_found_it`]'s own run of
/// its waits, alone in a process of its own.
const ALONE: &str = "ATPKG_FORWARDING_OVERLAP_ALONE";

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

/// A pipe whose read end children inherit and whose write end none does: the read end and the
/// write end.
fn pipe() -> (OwnedFd, OwnedFd) {
    let mut fds = [0; 2];
    // SAFETY: `pipe` writes two descriptors into the two-element array.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // SAFETY: `fcntl` on the write end this call just made, with plain integers.
    assert_eq!(
        unsafe { libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC) },
        0
    );
    // SAFETY: both ends are this process's own, each owned once here.
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

/// One wait standing aside for a shell that reads `fd` to its end, armed as a front arms it, on a
/// thread of its own. The shell's `cat` opens it by its `/dev/fd` name: a redirection names only
/// a one-digit number in dash (Debian's and Ubuntu's `/bin/sh`), and `fd` is any free number.
fn waiting(fd: RawFd) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let forwarding = super::arm_forwarding();
        let mut child = aterm_uds::spawnfd::transparent("/bin/sh")
            .args(["-c", &format!("cat /dev/fd/{fd} >/dev/null")])
            .spawn()
            .expect("the shell");
        let status = super::wait_standing_aside(&mut child, forwarding).expect("waited");
        assert!(status.success(), "{status:?}");
    })
}

/// Two waits that overlap, the second starting once the first stands aside (SIGINT ignored) and
/// ending after it, leave SIGINT as it was before either: the second arms only once the first
/// has put its dispositions back. They run alone in a child of this test binary, from SIGINT at
/// its default, set there ([`super::Starting`]): a runner's ignored SIGINT (a `&` job's) would
/// read as the first wait standing aside before it armed, and hide a wait that left SIGINT
/// ignored; and an admission test beside it in this process catches SIGINT while it waits
/// (`aterm_toolchain::interrupt::Watch`, outside the forwarding turn), whose handler a wait would
/// keep and put back after that watch ended. NEGATIVE CONTROL (by hand: `arm_forwarding` taking
/// no turn): the second keeps the first's ignored SIGINT as the one to restore, and leaves it.
#[test]
fn two_overlapping_waits_leave_sigint_as_they_found_it() {
    if std::env::var_os(ALONE).is_none() {
        let mut alone =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
        alone
            .args([
                "--exact",
                "platform::unix::forwarding_tests::two_overlapping_waits_leave_sigint_as_they_found_it",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(ALONE, "1");
        // Twice the hang detector: the run's own detectors say first what never came.
        let out = aterm_uds::deadline::output_within(&mut alone, MUST_HAPPEN * 2)
            .expect("hang detector: the waits' own run never ended");
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && said.contains("test result: ok. 1 passed"),
            "{}\n{said}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let _start = super::Starting::at_defaults(&[libc::SIGINT]);
    let (a_read, a_write) = pipe();
    let (b_read, b_write) = pipe();
    let a = waiting(a_read.as_raw_fd());
    until_within(MUST_HAPPEN, "the first wait standing aside", || {
        (sigint_now() == libc::SIG_IGN).then_some(())
    });
    let b = waiting(b_read.as_raw_fd());
    drop(a_write);
    crate::joined(a, "the first wait's end");
    drop(b_write);
    crate::joined(b, "the second wait's end");
    assert_eq!(sigint_now(), libc::SIG_DFL, "SIGINT as the waits found it");
}

/// A test's child spawned through [`super::spawn_outside_a_stand_aside`] is spawned inside this
/// process's forwarding turn, which every wait that stands aside holds while it ignores SIGINT.
#[test]
fn a_child_spawned_outside_a_stand_aside_is_spawned_in_the_forwarding_turn() {
    assert!(
        super::spawn_outside_a_stand_aside(|| super::FORWARDING_TURN.try_lock().is_err()),
        "a child spawned while another test's wait may be ignoring SIGINT"
    );
}
