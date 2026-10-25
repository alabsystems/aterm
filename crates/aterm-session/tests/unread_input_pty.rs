// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `SinkWriter::unread_input`, the one reader of a program's unread keys, against a real pty on
//! each platform that counts them.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::time::Duration;

use aterm_session::sink::SinkWriter;
use aterm_spec::wait::{MUST_HAPPEN, within};

/// The program's unread input is read through the master on macOS and through a descriptor of
/// the slave on Linux, never the master's own count there (the program's output waiting for
/// aterm): keys written count until the program reads them, output it writes never does, and the
/// backlog's queue is that same count.
#[test]
fn unread_input_is_the_keys_the_program_has_not_read_and_the_backlog_reads_it() {
    let (master, slave) = aterm_pty::open_pty_pair().expect("a pty");
    // SAFETY: `termios` is plain integers and bytes; all-zero is valid until `tcgetattr` fills it.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `slave` is this test's live slave and `t` a writable `termios`.
    assert_eq!(unsafe { libc::tcgetattr(slave, &raw mut t) }, 0);
    // SAFETY: `t` is a whole `termios`.
    unsafe { libc::cfmakeraw(&raw mut t) };
    // SAFETY: as above; `t` is the slave's own mode, made raw.
    assert_eq!(
        unsafe { libc::tcsetattr(slave, libc::TCSANOW, &raw const t) },
        0
    );
    let sink = SinkWriter::new(master);
    // Linux hands a write to the slave from a worker: a hang detector, never a budget.
    let reads = |n: usize| {
        within(MUST_HAPPEN, Duration::from_millis(1), || {
            (sink.unread_input() == Some(n)).then_some(())
        })
        .is_some()
    };
    assert_eq!(sink.unread_input(), Some(0), "nothing typed");
    // SAFETY: `slave` is live and the five bytes live for the call.
    let wrote = unsafe { libc::write(slave, b"hello".as_ptr().cast(), 5) };
    assert_eq!(wrote, 5, "the program's output");
    assert_eq!(sink.unread_input(), Some(0), "output is not input");
    assert_eq!(sink.write_frame(b"abc").expect("keys"), 3);
    assert!(
        reads(3),
        "the keys the program has not read: {:?}",
        sink.unread_input()
    );
    assert_eq!(
        sink.input_backlog().map(|b| b.queued),
        Some(3),
        "the backlog's queue is that count"
    );
    let mut got = [0_u8; 3];
    let mut n = 0;
    while n < got.len() {
        // SAFETY: a bounded read into the unfilled tail of `got`.
        let r = unsafe { libc::read(slave, got[n..].as_mut_ptr().cast(), 1) };
        assert_eq!(r, 1, "the program reads a key");
        n += 1;
        assert!(
            reads(got.len() - n),
            "the program read {n}: {:?}",
            sink.unread_input()
        );
    }
    assert_eq!(&got, b"abc", "the keys in order");
    drop(sink);
    // SAFETY: both fds are this test's own, closed once, no sink left over them.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}
