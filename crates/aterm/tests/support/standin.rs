// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! STAND-IN PROGRAMS IN RUST: a live lane runs a terminal program of its own
//! (a TUI, a job that arms modes and dies) by re-executing THIS test binary.
//!
//! Each lane uses one of its real tests as the child entry. Before that test's
//! ordinary body, it checks [`ENV`] and plays the named program when set,
//! leaving through [`exit`] without returning to libtest. [`argv`] selects
//! exactly that test; an ordinary run executes its full parent assertions.
//!
//! libtest prints `running 1 test` before the entry starts; every program
//! here paints over that line or prints past it, and no lane reads it.
//!
//! The terminal calls are `libc`'s on the session's own descriptors, and the
//! ones a forked child makes ([`answered`]) are async-signal-safe: no
//! allocation, no lock.

#![allow(dead_code)]

use std::path::PathBuf;

/// The variable that names the program the entry plays.
pub const ENV: &str = "LIVE_STANDIN";

/// This test binary: the program every stand-in runs as.
pub fn exe() -> PathBuf {
    std::env::current_exe().expect("the test binary's own path")
}

/// The arguments after [`exe`] that run the entry `test` (its full path in
/// this binary, `<module>::<fn>`) and nothing else.
pub fn argv(test: &str) -> [&str; 4] {
    ["--exact", test, "--nocapture", "--quiet"]
}

/// A shell command line that runs the entry `test` as the program `spec`
/// (`env` sets [`ENV`], so every shell reads the line alike).
pub fn shell_line(test: &str, spec: &str) -> String {
    let exe = exe();
    let exe = exe.to_str().expect("a UTF-8 test binary path");
    assert!(
        !exe.contains('\'') && !spec.contains('\''),
        "quoted below: {exe} {spec}"
    );
    format!("env {ENV}='{spec}' '{exe}' {}", argv(test).join(" "))
}

/// The program the entry was started to play; `None` in an ordinary run.
pub fn spec() -> Option<String> {
    std::env::var(ENV).ok()
}

/// Leave the program now: libtest's summary must not print after its output.
pub fn exit(code: i32) -> ! {
    // SAFETY: `_exit` ends the process at once; nothing here is left owed.
    unsafe { libc::_exit(code) }
}

/// Write all of `bytes` to the terminal (descriptor 1), unbuffered.
pub fn put(bytes: &[u8]) {
    let mut rest = bytes;
    while !rest.is_empty() {
        // SAFETY: a write from a live slice to the process's own stdout.
        let n = unsafe { libc::write(1, rest.as_ptr().cast(), rest.len()) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            exit(3);
        }
        rest = &rest[usize::try_from(n).unwrap_or(0)..];
    }
}

/// The terminal's size as the program reads it (`TIOCGWINSZ` on stdout):
/// `(rows, cols)`.
pub fn winsize() -> (u16, u16) {
    // SAFETY: an all-zero `winsize` is a valid value for the ioctl to fill.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: TIOCGWINSZ writes one `winsize` through a valid pointer.
    let rc = unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) };
    assert_eq!(rc, 0, "TIOCGWINSZ: {}", std::io::Error::last_os_error());
    (ws.ws_row, ws.ws_col)
}

/// Put the terminal (descriptor 0) in raw mode.
pub fn raw() {
    // SAFETY: an all-zero `termios` is a valid buffer for tcgetattr to fill.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: both calls read/write one `termios` through a valid pointer.
    unsafe {
        assert_eq!(libc::tcgetattr(0, &mut t), 0, "tcgetattr");
        libc::cfmakeraw(&mut t);
        assert_eq!(libc::tcsetattr(0, libc::TCSANOW, &t), 0, "tcsetattr");
    }
}

/// Ask the terminal where its cursor is (`ESC[6n`) and return once it has
/// answered, the terminal (descriptor 0) as it was. The terminal answers only
/// after it has parsed every byte written before the question, so everything
/// this process wrote was read while this process's group held the terminal,
/// however late its reader runs. Async-signal-safe: a forked child calls it.
pub fn answered() {
    // SAFETY: an all-zero `termios` is a valid buffer for tcgetattr to fill;
    // every call below takes valid pointers to this frame's locals.
    unsafe {
        let mut was: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut was) != 0 {
            libc::_exit(3);
        }
        let mut cbreak = was;
        cbreak.c_lflag &= !(libc::ICANON | libc::ECHO);
        cbreak.c_cc[libc::VMIN] = 1;
        cbreak.c_cc[libc::VTIME] = 0;
        libc::tcsetattr(0, libc::TCSANOW, &cbreak);
        let ask = b"\x1b[6n";
        libc::write(1, ask.as_ptr().cast(), ask.len());
        let mut byte = 0u8;
        loop {
            match libc::read(0, (&raw mut byte).cast(), 1) {
                1 if byte == b'R' => break,
                1 => {}
                n if n < 0
                    && std::io::Error::last_os_error().kind()
                        == std::io::ErrorKind::Interrupted => {}
                _ => libc::_exit(3),
            }
        }
        libc::tcsetattr(0, libc::TCSANOW, &was);
    }
}
