// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_line_is_read_without_its_newline_and_nothing_after_it() {
    let mut input: &[u8] = b"hunter2\nnext";
    let secret = Secret::read_line(&mut input).unwrap().unwrap();
    assert_eq!(secret.bytes(), b"hunter2");
    assert_eq!(
        input, b"next",
        "read one byte at a time, so nothing past the line"
    );
    let mut crlf: &[u8] = b"pw\r\n";
    assert_eq!(
        Secret::read_line(&mut crlf).unwrap().unwrap().bytes(),
        b"pw"
    );
    let mut empty: &[u8] = b"";
    assert!(Secret::read_line(&mut empty).unwrap().is_none());
    let mut blank: &[u8] = b"\n";
    assert!(Secret::read_line(&mut blank).unwrap().unwrap().is_empty());
}

#[test]
fn a_process_that_holds_a_password_writes_no_core_and_its_children_neither() {
    no_core_dumps();
    // SAFETY: All-zero is a valid `rlimit`; `getrlimit` fills it.
    let mut now: libc::rlimit = unsafe { std::mem::zeroed() };
    now.rlim_cur = libc::RLIM_INFINITY;
    // SAFETY: A live out-parameter of the right type.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_CORE, &raw mut now) },
        0
    );
    assert_eq!(
        (now.rlim_cur, now.rlim_max),
        (0, 0),
        "the hard limit too, so a child cannot raise it"
    );
}

/// A terminal pair a test types into as a person (or the window) would: the side that types
/// and reads the echo, and the side a reader reads.
#[cfg(unix)]
fn terminal() -> (std::fs::File, std::fs::File) {
    use std::os::fd::FromRawFd as _;
    let (typing, reading) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: `open_pty_pair` returned two fresh, close-on-exec descriptors owned here alone.
    unsafe {
        (
            std::fs::File::from_raw_fd(typing),
            std::fs::File::from_raw_fd(reading),
        )
    }
}

/// How many bytes wait to be read on `fd`.
#[cfg(unix)]
fn pending(fd: i32) -> libc::c_int {
    let mut n: libc::c_int = 0;
    // SAFETY: FIONREAD writes one int to a live out-parameter.
    unsafe { libc::ioctl(fd, libc::FIONREAD, &raw mut n) };
    n
}

/// `line` typed into `typing` before anything reads it, its echo read off (the window reads
/// a tab's output as it comes, so nothing is left for a flush to wait on).
#[cfg(unix)]
fn typed_ahead(typing: &mut std::fs::File, reading: &std::fs::File, line: &[u8]) {
    use std::io::{Read as _, Write as _};
    use std::os::unix::io::AsRawFd as _;
    typing.write_all(line).unwrap();
    let by = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while pending(reading.as_raw_fd()) == 0 {
        assert!(
            std::time::Instant::now() < by,
            "the line never reached the reader"
        );
        aterm_uds::deadline::poll_yield();
    }
    let fd = typing.as_raw_fd();
    // SAFETY: `fd` is open; F_GETFL and F_SETFL take and return plain flags.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let mut echo = [0_u8; 256];
    while typing.read(&mut echo).is_ok_and(|n| n > 0) {}
}

/// System State types the Mac Setup tab's go line as soon as the tab lands, often
/// before the tab's program reaches its read: the reader with echo off that keeps what was
/// typed ahead (the go line's) reads that line, where one that discarded it would wait for a
/// line the window never types again.
#[cfg(unix)]
#[test]
fn a_go_line_typed_before_the_read_survives_echo_off() {
    let (mut typing, mut reading) = terminal();
    typed_ahead(&mut typing, &reading, b"install\r");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let line = ask_on(&mut reading, "", Read::HiddenKept, &|| false);
        let _ = tx.send(line.map(|line| line.map(|line| line.bytes().to_vec())));
    });
    let line = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the go line typed before the read was discarded: the read still waits");
    assert_eq!(line.unwrap().as_deref(), Some(&b"install"[..]));
    drop(typing);
}

/// A stop at the prompt (Ctrl-C, a closed tab) ends the read with [`StopSignal`] and puts the
/// echo back, though no line came. NEGATIVE CONTROL (scratch): the `(self.2)()` check in
/// `read_tty_line` made `false` leaves the read waiting, and this test's one-minute detector
/// fails.
#[cfg(unix)]
#[test]
fn a_stop_at_the_prompt_ends_the_read_and_puts_the_echo_back() {
    use std::os::unix::io::AsRawFd as _;
    let (typing, mut reading) = terminal();
    let fd = reading.as_raw_fd();
    let echoes = || {
        // SAFETY: All-zero is a valid `termios`; `tcgetattr` fills it from the open `fd`.
        let mut now: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is the open terminal `reading` owns; `now` is a live out-parameter.
        assert_eq!(unsafe { libc::tcgetattr(fd, &raw mut now) }, 0);
        now.c_lflag & libc::ECHO != 0
    };
    assert!(echoes());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let read = ask_on(&mut reading, "", Read::Hidden, &|| true);
        let _ = tx.send((read.map(|line| line.is_some()), reading));
    });
    let (read, _reading) = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("a stop at the prompt left the read waiting for a line");
    let stopped = read.expect_err("a stop is no line");
    assert_eq!(stopped.kind(), io::ErrorKind::Interrupted);
    assert!(
        stopped
            .get_ref()
            .is_some_and(|inner| inner.downcast_ref::<StopSignal>().is_some()),
        "a person's stop, not another interruption: {stopped}"
    );
    assert!(echoes(), "the echo the prompt turned off is back");
    drop(typing);
}

/// The password prompt still discards keys typed before it, which are no answer to it.
#[cfg(unix)]
#[test]
fn a_password_prompt_discards_what_was_typed_before_it() {
    use std::os::unix::io::AsRawFd as _;
    let (mut typing, reading) = terminal();
    typed_ahead(&mut typing, &reading, b"hunter2\r");
    let fd = reading.as_raw_fd();
    assert!(pending(fd) > 0);
    let kept = EchoOff::new(fd, libc::TCSANOW).unwrap();
    assert!(pending(fd) > 0, "echo off for a typed-ahead line keeps it");
    drop(kept);
    let quiet = EchoOff::new(fd, libc::TCSAFLUSH).unwrap();
    assert_eq!(
        pending(fd),
        0,
        "echo off for a password discards what came before it"
    );
    drop(quiet);
}

#[test]
fn a_secret_never_prints_and_refuses_what_it_cannot_hold() {
    let secret = Secret::new(b"hunter2").unwrap();
    assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
    assert!(Secret::new(&[b'x'; SECRET_MAX + 1]).is_err());
    let mut long: &[u8] = &[b'x'; SECRET_MAX + 1];
    assert!(Secret::read_line(&mut long).is_err());
}

/// The byte past the longest line is scrubbed from the read buffer before the refusal, as
/// every kept byte is. NEGATIVE CONTROL: a line that fits is read whole through the same
/// buffer, which holds none of it afterwards.
#[test]
fn an_over_long_line_leaves_none_of_its_bytes_in_the_read_buffer() {
    let mut long = [b'x'; SECRET_MAX + 1];
    long[SECRET_MAX] = b'y';
    let mut byte = [0_u8; 1];
    let refused = Secret::read_line_through(&mut &long[..], &mut byte).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(byte, [0]);
    let mut fits: &[u8] = &[b'x'; SECRET_MAX];
    let secret = Secret::read_line_through(&mut fits, &mut byte)
        .unwrap()
        .unwrap();
    assert_eq!(secret.bytes(), &[b'x'; SECRET_MAX]);
    assert_eq!(byte, [0]);
}
