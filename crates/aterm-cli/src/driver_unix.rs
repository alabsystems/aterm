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

/// Raw mode, the passthrough `poll(2)` loop, resize forwarding, restore, reap:
/// returns the shell's exit status (non-exit → 1). The body is the parent-side
/// session loop moved verbatim from `main()`.
pub(crate) fn run(shell: aterm_pty::SpawnedShell, engine: &mut Terminal, verbose: bool) -> i32 {
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
        // apps reflow) and the engine model.
        if GOT_WINCH.swap(false, Ordering::Relaxed) {
            let mut nws = host_winsize_raw();
            unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &mut nws) };
            engine.resize(nws.ws_row, nws.ws_col);
        }

        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if n < 0 {
            if eintr() {
                continue; // a signal (e.g. SIGWINCH) — loop and apply it
            }
            break;
        }

        // host keystrokes -> the shell.
        if fds[0].revents & libc::POLLIN != 0 {
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

        // shell output -> host terminal (passthrough) AND the engine (model).
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            let r = unsafe { libc::read(master, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if r < 0 && eintr() {
                continue;
            }
            if r <= 0 {
                break; // shell exited / PTY closed
            }
            let out = &buf[..r as usize];
            write_all(libc::STDOUT_FILENO, out);
            engine.process(out);
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
        eprintln!("\r\n[aterm] session ended — engine processed {bytes_in} bytes via the VT core.");
    }
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        1
    }
}
