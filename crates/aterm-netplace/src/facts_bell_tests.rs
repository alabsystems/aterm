// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{ErrorKind, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd};

use super::*;

fn sense() -> Live {
    Live {
        routes: None,
        path: Arc::new(PathState {
            bell: Some(Bell::new().expect("notification pair")),
            ..PathState::default()
        }),
        _monitor: None,
    }
}

fn readable(fd: BorrowedFd<'_>) -> bool {
    let mut fd = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one live pollfd, polled without waiting.
    let ready = unsafe { libc::poll(&raw mut fd, 1, 0) };
    assert!(ready >= 0);
    ready != 0
}

fn tell(path: &PathState) {
    // The monitor holds the reading while it sets the change flag and rings.
    let _held = path.reading.lock().unwrap();
    path.tell();
}

#[test]
fn path_changes_consumed_before_poll_leave_no_stale_readiness() {
    let sense = sense();
    for _ in 0..32 {
        tell(&sense.path);
        tell(&sense.path);
        assert!(sense.wait(Duration::ZERO));
        assert!(
            !readable(sense.path.bell_fd().unwrap()),
            "acknowledged path changes must not leave a stale notification"
        );
        assert!(!sense.wait(Duration::ZERO));
    }
}

#[test]
fn a_full_bell_never_blocks_the_monitor_and_drains_before_rearming() {
    let bell = Bell::new().unwrap();
    for fd in [bell.hear.as_fd(), bell.ring.as_fd()] {
        // SAFETY: F_GETFL only reads the flags of this live descriptor.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::O_NONBLOCK, 0, "notification I/O cannot block");
    }
    let fill = |buf: &[u8]| loop {
        match (&bell.ring).write(buf) {
            Ok(n) => assert!(n > 0),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) => panic!("fill notification transport: {e}"),
        }
    };
    fill(&[1; 4096]);
    fill(&[1]);
    bell.ring();
    assert!(readable(bell.hear.as_fd()));
    bell.hush();
    assert!(!readable(bell.hear.as_fd()));
    bell.ring();
    assert!(readable(bell.hear.as_fd()));
    bell.hush();
    assert!(!readable(bell.hear.as_fd()));
}

#[test]
fn draining_the_bell_keeps_the_path_change_latched() {
    let sense = sense();
    tell(&sense.path);
    sense.path.hush();
    assert!(!readable(sense.path.bell_fd().unwrap()));
    let (answer, result) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        answer.send(sense.wait(Duration::ZERO)).unwrap();
    });
    assert!(
        result.recv_timeout(Duration::from_secs(60)).unwrap(),
        "draining readiness must not clear the separately latched path change"
    );
    waiter.join().unwrap();
}
