// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A control connection's descriptor, read with `fcntl(2)`, which the library's
//! `forbid(unsafe_code)` keeps out of its own tests.

/// A control connection holds one descriptor, kept out of every child (`FD_CLOFORK`) from its
/// dial on: on macOS a spawned child's exec-time close of an inherited copy, while a descriptor
/// scan (an instance's job sampler) is inside a call on it, drains the socket for good, and the
/// next reply read answers `EBADF` (`spawn: Bad file descriptor` in a loaded run of
/// fabric_world's exactly_once).
#[cfg(target_vendor = "apple")]
#[test]
fn a_control_connection_is_kept_out_of_every_child() {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::net::{UnixListener, UnixStream};
    /// `FD_CLOFORK` from Apple's `<sys/fcntl.h>`.
    const FD_CLOFORK: i32 = 2;
    let marked = |s: &UnixStream| {
        // SAFETY: `F_GETFD` reads the flags of a descriptor `s` owns and touches no memory.
        let flags = unsafe { libc::fcntl(s.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0, "the connection's descriptor is open");
        flags & FD_CLOFORK != 0
    };
    let (near, _far) = UnixStream::pair().expect("a socket pair");
    let ctl = aterm_link::ctl::Ctl::from_stream(near).expect("wrap the stream");
    assert!(
        marked(ctl.get_ref()),
        "a wrapped stream a child can inherit"
    );
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-dial")
        .tempdir_in("/tmp")
        .expect("a socket dir");
    let sock = dir.path().join("c.sock");
    let _listener = UnixListener::bind(&sock).expect("bind");
    assert!(
        marked(&aterm_link::ctl::dial(&sock).expect("dial")),
        "a dialled stream a child spawned before its wrap can inherit"
    );
}
