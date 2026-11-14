// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`crate::pipe`]'s tests: a child another thread forks holds no end of one where the kernel
//! keeps `FD_CLOFORK`, so its reader sees EOF once the writer drops.

use std::io::{ErrorKind, Read as _};
use std::os::fd::AsRawFd;

/// `FD_CLOFORK`, which Apple's libc crate does not name.
#[cfg(target_vendor = "apple")]
const FD_CLOFORK: i32 = 2;

/// Whether this kernel keeps a requested `FD_CLOFORK` (never off Apple, where nothing asks).
fn kernel_keeps_clofork() -> bool {
    #[cfg(target_vendor = "apple")]
    {
        crate::spawn_lock::tests::clofork_supported()
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        false
    }
}

fn fd_flags(fd: &impl AsRawFd) -> i32 {
    // SAFETY: `F_GETFD` reads one live test descriptor's flags and touches no memory.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0, "F_GETFD: {}", std::io::Error::last_os_error());
    flags
}

/// Whether a child forked beside this pipe holds its write end: fork a child that waits for its
/// kill (as a parked child waits for its exec), drop the parent's write end and read the
/// nonblocking read end, which answers EOF once no write end is open anywhere and `WouldBlock`
/// while the child's copy is. `fork` has copied the table when it returns, so the read needs no
/// wait. The fork holds [`crate::spawn_lock::spawning`], as the PTY spawn does.
fn a_forked_child_holds(read: std::io::PipeReader, write: std::io::PipeWriter) -> bool {
    // SAFETY: `F_GETFL`/`F_SETFL` read and set one live test descriptor's status flags.
    let made_nonblocking = unsafe {
        let status = libc::fcntl(read.as_raw_fd(), libc::F_GETFL);
        status >= 0 && libc::fcntl(read.as_raw_fd(), libc::F_SETFL, status | libc::O_NONBLOCK) == 0
    };
    assert!(
        made_nonblocking,
        "O_NONBLOCK: {}",
        std::io::Error::last_os_error()
    );
    let spawning = crate::spawn_lock::spawning();
    // SAFETY: `fork` takes no arguments; the child calls only `pause` until its kill.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        loop {
            // SAFETY: async-signal-safe; the child waits here until the parent kills it.
            unsafe { libc::pause() };
        }
    }
    drop(spawning);
    assert!(pid > 0, "fork: {}", std::io::Error::last_os_error());
    drop(write);
    let mut byte = [0];
    let answer = (&read).read(&mut byte);
    // SAFETY: `kill` and `waitpid` name the child this function forked, and `waitpid` writes one
    // stack integer.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        let mut status = 0;
        libc::waitpid(pid, &mut status, 0);
    }
    match answer {
        Ok(0) => false,
        Err(error) if error.kind() == ErrorKind::WouldBlock => true,
        other => panic!("the read answers EOF or WouldBlock: {other:?}"),
    }
}

/// Both ends of [`crate::pipe`] are close-on-exec and carry the mark the kernel keeps, and a
/// child forked beside it holds its write end exactly when the kernel keeps no `FD_CLOFORK`.
/// NEGATIVE CONTROL: a child forked beside a bare `std::io::pipe` holds its write end, so the
/// reader sees no EOF after the writer drops.
#[test]
fn a_forked_child_holds_no_end_of_a_kept_out_pipe() {
    #[expect(
        clippy::disallowed_methods,
        reason = "the negative control: a bare pipe, whose ends a forked child holds"
    )]
    let (read, write) = std::io::pipe().expect("a bare pipe");
    assert!(
        a_forked_child_holds(read, write),
        "the negative control: a child forked beside a bare pipe holds its write end"
    );

    let (read, write) = crate::pipe().expect("a kept-out pipe");
    for flags in [fd_flags(&read), fd_flags(&write)] {
        assert_ne!(flags & libc::FD_CLOEXEC, 0, "each end is close-on-exec");
        #[cfg(target_vendor = "apple")]
        assert_eq!(
            flags & FD_CLOFORK,
            if kernel_keeps_clofork() {
                FD_CLOFORK
            } else {
                0
            },
            "each end agrees with the kernel's supported flags"
        );
    }
    assert_eq!(
        a_forked_child_holds(read, write),
        !kernel_keeps_clofork(),
        "a forked child holds the write end exactly when the kernel keeps no FD_CLOFORK"
    );
}
