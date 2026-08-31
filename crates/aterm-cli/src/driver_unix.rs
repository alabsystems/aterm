// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The POSIX console driver for the `aterm` passthrough binary: termios raw
//! mode, a `poll(2)` I/O loop over stdin + the PTY master, SIGWINCH resize
//! forwarding, and the `waitpid`/`WIFEXITED` exit-status epilogue. All code
//! here is moved VERBATIM from `main.rs` (the shared parser/diagnostics stay
//! there); `driver_windows.rs` is this file's ConPTY console twin behind the
//! same four-function surface.

use std::sync::atomic::{AtomicBool, Ordering};

use aterm_core::terminal::Terminal;

/// Set by the SIGWINCH handler; drained in the main loop.
static GOT_WINCH: AtomicBool = AtomicBool::new(false);

extern "C" fn on_winch(_sig: libc::c_int) {
    GOT_WINCH.store(true, Ordering::Relaxed);
}

/// Ask the controlling terminal for its size; fall back to 24x80.
fn host_winsize_raw() -> libc::winsize {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    if !ok || ws.ws_row == 0 || ws.ws_col == 0 {
        ws.ws_row = 24;
        ws.ws_col = 80;
    }
    ws
}

/// The host terminal size as `(rows, cols)` — the platform-neutral surface
/// `main.rs` consumes (the raw `winsize` stays private to this driver).
pub(crate) fn host_winsize() -> (u16, u16) {
    let ws = host_winsize_raw();
    (ws.ws_row, ws.ws_col)
}

/// Whether stdout is a terminal (`doctor`'s tty check).
pub(crate) fn stdout_is_tty() -> bool {
    unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 }
}

/// Whether `path` exists and is executable (`X_OK`). Conservative: any `access(2)`
/// error (missing, not executable, embedded NUL) → `false` — a non-runnable shell
/// is a failed check.
pub(crate) fn shell_is_executable(path: &str) -> bool {
    let Ok(c) = std::ffi::CString::new(path) else {
        return false;
    };
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

fn set_raw(fd: libc::c_int) -> libc::termios {
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        libc::tcgetattr(fd, &mut t);
        let orig = t;
        libc::cfmakeraw(&mut t);
        libc::tcsetattr(fd, libc::TCSANOW, &t);
        orig
    }
}

fn restore(fd: libc::c_int, t: &libc::termios) {
    unsafe {
        libc::tcsetattr(fd, libc::TCSANOW, t);
    }
}

fn write_all(fd: libc::c_int, mut data: &[u8]) {
    while !data.is_empty() {
        let r = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
        if r <= 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break;
        }
        data = &data[r as usize..];
    }
}

fn eintr() -> bool {
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
}

/// Whether `poll(2)` says a stream fd must be read now.
///
/// `POLLHUP` is deliberately included even without `POLLIN`: Unix variants may
/// report the buffered tail and final EOF as either `POLLIN|POLLHUP` or a bare
/// hangup. Reading every observation preserves the tail and turns the final one
/// into `read == 0`; ignoring a terminal-only event leaves the fd registered and
/// can make `poll` return immediately forever. Error/invalid-fd readiness has the
/// same terminal shape and must reach the existing read-error path instead.
const fn poll_must_read(revents: libc::c_short) -> bool {
    revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

/// Raw mode, the passthrough `poll(2)` loop, resize forwarding, restore, reap:
/// returns the shell's exit status (non-exit → 1). The body is the parent-side
/// session loop moved verbatim from `main()`.
///
/// `engine` is `None` for an ordinary session — the VT model is demand-driven
/// and off by default (`$ATERM_SESSION_MODEL`, `aterm_cli::session_model_armed`).
/// Only the two `if let` arms below depend on it; everything the SHELL can
/// observe — the stdout passthrough and the `TIOCSWINSZ` forwarded to the PTY —
/// is unconditional, so a session behaves identically with the model absent.
pub(crate) fn run(
    shell: aterm_pty::SpawnedShell,
    mut engine: Option<&mut Terminal>,
    verbose: bool,
) -> i32 {
    let master = shell.master;

    // PARENT.
    let stdin_is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    let orig = if stdin_is_tty {
        Some(set_raw(libc::STDIN_FILENO))
    } else {
        None
    };
    // Cast through a function pointer (not a direct fn-item-to-int cast) so the
    // `fn_to_numeric_cast` lint is satisfied while still yielding the address
    // libc::signal expects as its sighandler_t.
    unsafe {
        libc::signal(
            libc::SIGWINCH,
            on_winch as extern "C" fn(libc::c_int) as usize,
        )
    };

    let mut bytes_in: u64 = 0;

    let mut fds = [
        libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let mut buf = [0u8; 8192];

    loop {
        // Apply a pending resize before blocking: tell the PTY (so full-screen
        // apps reflow) and, when one is armed, the engine model. The ioctl is
        // NOT inside the `if let`: the PTY is what every full-screen app reads
        // its geometry from, and it must be told whether or not anything is
        // modelling the screen. Nothing else in this process holds a size.
        if GOT_WINCH.swap(false, Ordering::Relaxed) {
            let mut nws = host_winsize_raw();
            unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &mut nws) };
            if let Some(engine) = engine.as_deref_mut() {
                engine.resize(nws.ws_row, nws.ws_col);
            }
        }

        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if n < 0 {
            if eintr() {
                continue; // a signal (e.g. SIGWINCH) — loop and apply it
            }
            break;
        }

        // host keystrokes -> the shell.
        if poll_must_read(fds[0].revents) {
            let r = unsafe {
                libc::read(
                    libc::STDIN_FILENO,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            };
            if r < 0 && eintr() {
                // retry next iteration
            } else if r <= 0 {
                fds[0].fd = -1; // input closed; let the shell run to its own exit
            } else {
                write_all(master, &buf[..r as usize]);
            }
        }

        // shell output -> host terminal (passthrough), and the engine (model)
        // only when one is armed. The write comes FIRST either way, so the
        // bytes are out the door before anything else touches them.
        if poll_must_read(fds[1].revents) {
            let r = unsafe { libc::read(master, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if r < 0 && eintr() {
                continue;
            }
            if r <= 0 {
                break; // shell exited / PTY closed
            }
            let out = &buf[..r as usize];
            write_all(libc::STDOUT_FILENO, out);
            if let Some(engine) = engine.as_deref_mut() {
                engine.process(out);
            }
            bytes_in += out.len() as u64;
        }
    }

    if let Some(t) = orig {
        restore(libc::STDIN_FILENO, &t);
    }
    let mut status = 0;
    unsafe {
        libc::close(master);
        // The protected spawn returns only the master fd; the shell (or the
        // sandbox-exec wrapper, which exits with the shell's status) is this
        // process's sole direct child, so reap it with `-1` to recover the exit
        // code and avoid a zombie.
        libc::waitpid(-1, &mut status, 0);
    }
    if verbose {
        // Say which session this was. The old wording claimed the VT core had
        // processed every byte, which is now true only when the model is armed —
        // and a summary that overstates is worse than no summary.
        let modelled = if engine.is_some() {
            "and into the armed VT core"
        } else {
            "(session model off: nothing modelled)"
        };
        eprintln!("\r\n[aterm] session ended — {bytes_in} bytes passed through {modelled}.");
    }
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;

    #[test]
    fn every_terminal_poll_flag_reaches_the_read_path() {
        for flag in [libc::POLLHUP, libc::POLLERR, libc::POLLNVAL, libc::POLLIN] {
            assert!(poll_must_read(flag), "flag {flag:#x} must be consumed");
            assert!(
                poll_must_read(flag | libc::POLLOUT),
                "unrelated readiness must not mask {flag:#x}"
            );
        }
        assert!(!poll_must_read(0));
        assert!(!poll_must_read(libc::POLLOUT));
    }

    /// A real pipe closes with buffered bytes still readable, then becomes a
    /// terminal readiness (bare `POLLHUP` on some Unix variants,
    /// `POLLIN|POLLHUP` on others). Driving every observation through
    /// `poll_must_read` must recover the complete tail and finally observe EOF;
    /// the former POLLIN-only loop could ignore terminal-only readiness forever.
    #[test]
    fn closed_pipe_drains_buffered_bytes_then_consumes_hangup() {
        let mut raw = [-1; 2];
        assert_eq!(unsafe { libc::pipe(raw.as_mut_ptr()) }, 0);
        // SAFETY: `pipe` returned two fresh owned descriptors above; each is
        // transferred exactly once into a `File` and closed by that owner.
        let mut reader = unsafe { std::fs::File::from_raw_fd(raw[0]) };
        // SAFETY: same ownership transfer as the read descriptor above.
        let mut writer = unsafe { std::fs::File::from_raw_fd(raw[1]) };
        let expected = b"buffered-after-close";
        writer.write_all(expected).expect("seed pipe");
        drop(writer);

        let mut got = Vec::new();
        let mut saw_terminal_readiness = false;
        let mut saw_eof = false;
        for _ in 0..expected.len() + 2 {
            let mut event = libc::pollfd {
                fd: raw[0],
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(unsafe { libc::poll(&mut event, 1, 1_000) }, 1);
            assert!(
                poll_must_read(event.revents),
                "revents={:#x}",
                event.revents
            );
            saw_terminal_readiness |=
                event.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0;
            let mut chunk = [0_u8; 3];
            let n = reader.read(&mut chunk).expect("consume ready pipe");
            if n == 0 {
                saw_eof = true;
                break;
            }
            got.extend_from_slice(&chunk[..n]);
        }

        assert_eq!(got, expected);
        assert!(
            saw_terminal_readiness,
            "the closed writer must surface terminal readiness"
        );
        assert!(saw_eof, "terminal readiness must be consumed through EOF");
    }
}
