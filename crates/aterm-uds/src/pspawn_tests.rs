// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

#[test]
fn the_child_inherits_nothing_but_stdio() {
    unsafe extern "C" {
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
        fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    }
    const F_DUPFD: i32 = 0;
    const F_GETFD: i32 = 1;
    let (held, _other) = crate::CtlStream::pair().expect("socketpair");
    // SAFETY: F_DUPFD on a descriptor we own; the result is ours to own.
    let raw = unsafe { fcntl(held.as_raw_fd(), F_DUPFD, 200) };
    assert!(raw >= 200, "a high duplicate: {raw}");
    // SAFETY: a fresh descriptor this test exclusively owns.
    let leak = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: flag read on a descriptor we own.
    assert_eq!(
        unsafe { fcntl(leak.as_raw_fd(), F_GETFD) } & 1,
        0,
        "inheritable here"
    );
    let probe = crate::spawnfd::test_support::held_probe(raw);

    let control = crate::spawnfd::test_support::leaky_status("/bin/sh", &["-c", &probe])
        .expect("control spawn");
    assert_eq!(control.code(), Some(3), "a leaky spawn carries fd {raw}");

    let pid = super::spawn_cloexec_default("/bin/sh", &["-c", &probe]).expect("spawn");
    let mut status = -1;
    // SAFETY: our own child; blocking reap.
    let reaped = unsafe { waitpid(pid as i32, &mut status, 0) };
    assert_eq!(reaped, pid as i32);
    assert_eq!(
        status, 0,
        "fd {raw} did not reach the child (wait status {status:#x})"
    );
    drop(leak);
}
