// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{SignalWake, Signals};

/// Sending `SIGWINCH` to this process wakes the pipe and is taken once.
#[test]
fn a_resize_wakes_the_pipe() {
    let _alone = crate::raw::TEST_TERMINAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let wake = SignalWake::install().expect("the wake installs");
    assert!(
        SignalWake::install().is_err(),
        "one wake per process at a time"
    );
    // SAFETY: SIGWINCH to this process, which the wake now catches.
    assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGWINCH) }, 0);
    let mut pfd = libc::pollfd {
        fd: wake.fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let budget = aterm_spec::wait::MUST_HAPPEN;
    let ms = libc::c_int::try_from(budget.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: one live pollfd.
    let ready = unsafe { libc::poll(&raw mut pfd, 1, ms) };
    assert_eq!(ready, 1, "the pipe woke");
    assert_eq!(wake.take(), Signals::WINCH);
    assert_eq!(wake.take(), Signals::NONE, "taken once");
    // SAFETY: as above.
    assert_eq!(
        unsafe { libc::poll(&raw mut pfd, 1, 0) },
        0,
        "the pipe is empty"
    );
    drop(wake);
    let again = SignalWake::install().expect("installs again once dropped");
    drop(again);
}
