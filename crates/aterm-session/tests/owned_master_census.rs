// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OWNED-MASTER CENSUS: `aterm_session::sink::owned_masters_open`
//! counts the masters this process's sinks own and have not closed, the number
//! the GUI's `metrics` prints as `pty_masters=` beside its live sessions.
//!
//! One test in its own binary, so no other test's sinks move the count while it
//! reads it. It pins the census to the fd itself: the count falls exactly when
//! the master closes (the LAST clone's drop), never at an earlier clone's drop,
//! and a borrowed sink is not counted at all.

#![cfg(unix)]

use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use aterm_session::sink::{SinkWriter, owned_masters_open};

/// Whether `fd` names an open descriptor in this process.
fn open(fd: i32) -> bool {
    // SAFETY: F_GETFD only reads the descriptor flags of `fd`.
    unsafe { libc::fcntl(fd, libc::F_GETFD) != -1 }
}

#[test]
fn the_census_counts_an_owned_master_until_its_last_clone_closes_it() {
    let base = owned_masters_open();
    let (ours, _peer) = UnixStream::pair().expect("socket pair");
    let owned = OwnedFd::from(ours);
    let fd = owned.as_raw_fd();

    // A BORROWED sink over the same number owns nothing and counts nothing.
    let borrowed = SinkWriter::new(fd);
    assert_eq!(owned_masters_open(), base, "a borrowed sink is not counted");
    drop(borrowed);
    assert!(open(fd), "a borrowed sink closes nothing");

    let sink = Arc::new(SinkWriter::new_owned(owned));
    assert_eq!(
        owned_masters_open(),
        base + 1,
        "the owned master is counted"
    );
    let holder = Arc::clone(&sink);
    drop(sink);
    assert!(open(fd), "an earlier clone's drop leaves the master open");
    assert_eq!(
        owned_masters_open(),
        base + 1,
        "and it stays counted while it is open"
    );
    drop(holder);
    assert!(!open(fd), "the last clone closes the master");
    assert_eq!(
        owned_masters_open(),
        base,
        "the census falls with the close"
    );
}
