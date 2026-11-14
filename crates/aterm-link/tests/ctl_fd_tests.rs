// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A control connection's descriptor, read with `fcntl(2)`, which the library's
//! `forbid(unsafe_code)` keeps out of its own tests.

/// Wrapped and dialed control connections retain close-on-exec and the close-on-fork
/// capability observed on an independent socket. An unsupported fork flag must not
/// bypass either connection path's checks.
#[cfg(target_vendor = "apple")]
#[test]
fn a_control_connection_retains_the_supported_child_inheritance_flags() {
    use std::os::fd::{AsFd as _, AsRawFd as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    /// `FD_CLOFORK` from Apple's `<sys/fcntl.h>`.
    const FD_CLOFORK: i32 = 2;
    let flags = |s: &UnixStream| {
        // SAFETY: `F_GETFD` reads the flags of a descriptor `s` owns and touches no memory.
        let flags = unsafe { libc::fcntl(s.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0, "the connection's descriptor is open");
        flags
    };
    let (near, far) = UnixStream::pair().expect("a socket pair");
    let supported = astream_sys::close_on_fork(far.as_fd())
        .expect("an independent socket's observed close-on-fork support");
    let expected = libc::FD_CLOEXEC | if supported { FD_CLOFORK } else { 0 };
    let check = |stream: &UnixStream, operation: &str| {
        assert_eq!(
            flags(stream) & (libc::FD_CLOEXEC | FD_CLOFORK),
            expected,
            "{operation}: close-on-exec and the observed close-on-fork capability"
        );
    };
    let ctl = aterm_link::ctl::Ctl::from_stream(near).expect("wrap the stream");
    check(ctl.get_ref(), "a wrapped control stream");
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-dial")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("a socket dir");
    let sock = dir.path().join("c.sock");
    let _listener = UnixListener::bind(&sock).expect("bind");
    check(
        &aterm_link::ctl::dial(&sock).expect("dial"),
        "a dialed control stream",
    );
}
