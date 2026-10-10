// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The POSIX console driver for the `aterm` passthrough binary: termios raw
//! mode, a `poll(2)` I/O loop over stdin + the PTY master, SIGWINCH resize
//! forwarding, and the `waitpid`/`WIFEXITED` exit-status epilogue.
//! `driver_windows.rs` is its ConPTY twin behind the same surface.

use std::sync::atomic::{AtomicBool, Ordering};

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

/// The host terminal size as `(rows, cols)` (the raw `winsize` stays private).
pub(crate) fn host_winsize() -> (u16, u16) {
    let ws = host_winsize_raw();
    (ws.ws_row, ws.ws_col)
}

/// Whether `path` names a shell this session can start: a regular file that
/// `access(2)` grants `X_OK`. A path that does not exist is
/// `Missing`; anything else that cannot run is `NotExecutable`.
pub(crate) fn shell_check(path: &str) -> crate::ShellCheck {
    use crate::ShellCheck;
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => {}
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return ShellCheck::Missing;
        }
        // A directory, or a path this user cannot reach.
        _ => return ShellCheck::NotExecutable,
    }
    let Ok(c) = std::ffi::CString::new(path) else {
        return ShellCheck::Missing;
    };
    if unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0 {
        ShellCheck::Runs
    } else {
        ShellCheck::NotExecutable
    }
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

/// Whether the failed `read(2)` just made is one to try again: a signal
/// (`EINTR`), or a non-blocking descriptor with nothing ready (`EAGAIN`) — the
/// master is non-blocking, and a stdin that came in non-blocking is the same.
fn retry_later() -> bool {
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EINTR | libc::EAGAIN)
    )
}

/// The most input the session holds for a program that is not reading it
/// before it stops reading its own stdin (see [`run`]): far past any tty input
/// queue, so typing and pasting into a program that reads never touch it.
const INPUT_BACKLOG_MAX: usize = 64 * 1024;

/// Hand the program as much of `pending` as its tty input queue has room for,
/// without ever parking: a full queue leaves the rest for the next turn of the
/// loop. A master that refuses input for good (the program side is gone) drops
/// it — the read of the master then ends the session.
fn feed_input(master: libc::c_int, pending: &mut std::collections::VecDeque<u8>) {
    while !pending.is_empty() {
        let (front, _) = pending.as_slices();
        match aterm_pty::write_some_nonparking(master, front) {
            aterm_pty::NonParkWrite::Wrote(n) => {
                pending.drain(..n.min(front.len()));
            }
            aterm_pty::NonParkWrite::WouldBlock => return,
            aterm_pty::NonParkWrite::Closed | aterm_pty::NonParkWrite::Fatal(_) => {
                pending.clear();
                return;
            }
        }
    }
}

/// Whether `poll(2)` says a stream fd must be read now.
///
/// Includes `POLLHUP` without `POLLIN`: platforms report the buffered tail and
/// final EOF as either `POLLIN|POLLHUP` or a bare hangup, and ignoring a
/// terminal-only event would make `poll` return immediately forever.
/// Error/invalid-fd readiness goes to the read-error path the same way.
const fn poll_must_read(revents: libc::c_short) -> bool {
    revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

/// Raw mode, the passthrough `poll(2)` loop, resize forwarding, restore, reap:
/// returns the shell's exit status (non-exit or a failed wait → 1). Nothing
/// here models the screen; the PTY is the one thing told the geometry.
pub(crate) fn run(shell: aterm_pty::SpawnedShell, verbose: bool) -> i32 {
    let master = shell.master;
    let shell_pid = shell.pid;

    let stdin_is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    let orig = if stdin_is_tty {
        Some(set_raw(libc::STDIN_FILENO))
    } else {
        None
    };
    // Cast through a function pointer to satisfy `fn_to_numeric_cast` while
    // yielding the `sighandler_t` address.
    unsafe {
        libc::signal(
            libc::SIGWINCH,
            on_winch as extern "C" fn(libc::c_int) as usize,
        )
    };
    // Until the line above, `SIGWINCH` was at its default — ignored — so a
    // resize since the spawn read the host's size (containment, the PATH dirs,
    // the fork and exec all ran in between) was dropped. Now that the handler
    // sees every later one, catch up once: when the host's grid is no longer
    // the PTY's, the first turn of the loop applies it. An unmoved grid sends
    // the program nothing, as before.
    if aterm_pty::size(master) != Some(host_winsize()) {
        GOT_WINCH.store(true, Ordering::Relaxed);
    }

    let mut bytes_in: u64 = 0;
    // The first output written through marks a healthy start (`on_first_shell_output`).
    let mut spoke = false;

    // Input waits HERE, never in `write(2)`. A program that is not reading
    // (busy printing, `sleep`, a stopped job) leaves its tty input queue full,
    // and a write into a full queue parks until it reads; a loop parked there
    // stops reading the program's OUTPUT, so a program that is printing parks
    // too and neither ever moves again: no output, no resize, no keystroke
    // read. So the master is non-blocking, the input it has no room for is
    // held in `pending` while the output keeps flowing, and stdin is not read
    // while `pending` holds [`INPUT_BACKLOG_MAX`]: the rest waits in the host's
    // own terminal, never in this process's memory. Should the fcntl fail (it
    // cannot on a live master), writes park as they always did.
    let _ = aterm_pty::set_nonblocking(master, true);
    let mut pending: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
    let mut stdin_open = true;

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
        // Apply a pending resize before blocking.
        if GOT_WINCH.swap(false, Ordering::Relaxed) {
            let mut nws = host_winsize_raw();
            unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &mut nws) };
        }

        fds[0].fd = if stdin_open && pending.len() < INPUT_BACKLOG_MAX {
            libc::STDIN_FILENO
        } else {
            -1
        };
        fds[1].events = if pending.is_empty() {
            libc::POLLIN
        } else {
            libc::POLLIN | libc::POLLOUT
        };
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
            if r < 0 && retry_later() {
                // retry next iteration
            } else if r <= 0 {
                stdin_open = false; // input closed; let the shell run to its own exit
            } else {
                pending.extend(&buf[..r as usize]);
            }
        }
        feed_input(master, &mut pending);

        // shell output -> host terminal (passthrough).
        if poll_must_read(fds[1].revents) {
            let r = unsafe { libc::read(master, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if r < 0 && retry_later() {
                continue;
            }
            if r <= 0 {
                break; // shell exited / PTY closed
            }
            let out = &buf[..r as usize];
            write_all(libc::STDOUT_FILENO, out);
            bytes_in += out.len() as u64;
            if !spoke {
                spoke = true;
                crate::first_shell_output();
            }
        }
    }

    if let Some(t) = orig {
        restore(libc::STDIN_FILENO, &t);
    }
    let mut status = 0;
    // Reap the shell by its pid, never `waitpid(-1)`: this process may have
    // other children (a detached `aterm pkg update`, the update checker's
    // codesign/curl) whose status would be mistaken for the shell's. The pid
    // is the session leader whether or not it execs the sandbox-exec wrapper.
    let reaped = unsafe {
        libc::close(master);
        loop {
            let r = libc::waitpid(shell_pid, &mut status, 0);
            if r < 0 && eintr() {
                continue;
            }
            break r == shell_pid;
        }
    };
    if verbose {
        eprintln!("\r\n[aterm] session ended — {bytes_in} bytes passed through.");
    }
    // A failed wait has no status to report: treat it like a non-exit.
    if reaped && libc::WIFEXITED(status) {
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

    /// The session's exit status is the shell's, even when another child of
    /// this process has already exited. Runs in a re-exec of this test binary
    /// with stdin at /dev/null, isolated from real terminals and other tests'
    /// children.
    #[test]
    fn run_returns_the_shells_status_not_an_older_zombie_childs() {
        const CHILD: &str = "ATERM_TEST_DRIVER_REAPS_THE_SHELL";
        if std::env::var_os(CHILD).is_none() {
            let name = format!(
                "{}::run_returns_the_shells_status_not_an_older_zombie_childs",
                module_path!().split_once("::").map_or("", |(_, rest)| rest)
            );
            let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args(["--exact", &name, "--nocapture", "--test-threads=1"])
                .env(CHILD, "1")
                .stdin(std::process::Stdio::null())
                .output()
                .expect("re-exec the test binary");
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(out.status.success(), "the re-exec failed:\n{text}");
            assert!(
                text.contains("1 passed"),
                "the re-exec ran no test:\n{text}"
            );
            return;
        }
        // A decoy child that exits non-zero and is a zombie before the shell
        // is spawned: `waitid(WNOWAIT)` blocks until it has exited and leaves
        // it unreaped.
        let decoy = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .spawn()
            .expect("decoy");
        let decoy_pid = libc::pid_t::try_from(decoy.id()).expect("pid");
        std::mem::forget(decoy);
        let decoy_id = libc::id_t::try_from(decoy_pid).expect("id");
        loop {
            // SAFETY: `info` is a zeroed out-parameter owned by this frame.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: waits on this process's own child; `info` is valid.
            let r = unsafe {
                libc::waitid(
                    libc::P_PID,
                    decoy_id,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if r == 0 {
                break;
            }
            let e = std::io::Error::last_os_error();
            assert_eq!(
                e.kind(),
                std::io::ErrorKind::Interrupted,
                "waitid on the decoy: {e}"
            );
        }

        // The same protected spawn seam as the session. The shell closes the
        // slave so the master sees EOF while it lives, then exits 3; the
        // hangup from the driver closing the master is ignored so that status
        // survives on Linux. The older zombie distinguishes `waitpid(-1)` from
        // waiting for this shell.
        // SAFETY: this is the isolated test child's trusted launch point.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let command: [std::ffi::OsString; 3] = [
            "/bin/sh".into(),
            "-c".into(),
            "trap '' HUP; exec 0<&- 1>&- 2>&-; sleep 0.3; exit 3".into(),
        ];
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&command),
            None,
            None,
            aterm_sandbox::Limits::shell_default(),
        )
        .expect("protected shell spawn");
        let code = run(shell, false);
        let mut status = 0;
        unsafe { libc::waitpid(decoy_pid, &mut status, libc::WNOHANG) };
        assert_eq!(code, 3, "the session took another child's exit status");
    }

    /// The first-shell-output hook ([`crate::on_first_shell_output`]) runs
    /// once, after the first bytes reach the terminal. A re-exec like the test
    /// above, since the hook is process-wide.
    #[test]
    fn the_shells_first_output_runs_the_hook_once() {
        use std::sync::atomic::AtomicUsize;
        const CHILD: &str = "ATERM_TEST_DRIVER_FIRST_OUTPUT_HOOK";
        static RAN: AtomicUsize = AtomicUsize::new(0);
        if std::env::var_os(CHILD).is_none() {
            let name = format!(
                "{}::the_shells_first_output_runs_the_hook_once",
                module_path!().split_once("::").map_or("", |(_, rest)| rest)
            );
            let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args(["--exact", &name, "--nocapture", "--test-threads=1"])
                .env(CHILD, "1")
                .stdin(std::process::Stdio::null())
                .output()
                .expect("re-exec the test binary");
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(out.status.success(), "the re-exec failed:\n{text}");
            assert!(
                text.contains("1 passed"),
                "the re-exec ran no test:\n{text}"
            );
            return;
        }
        assert!(crate::on_first_shell_output(|| {
            RAN.fetch_add(1, Ordering::SeqCst);
        }));
        assert!(
            !crate::on_first_shell_output(|| {}),
            "the first registration wins"
        );
        // SAFETY: this is the isolated test child's trusted launch point.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let command: [std::ffi::OsString; 3] = [
            "/bin/sh".into(),
            "-c".into(),
            "printf one; sleep 0.2; printf two; sleep 0.2; printf three".into(),
        ];
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&command),
            None,
            None,
            aterm_sandbox::Limits::shell_default(),
        )
        .expect("protected shell spawn");
        assert_eq!(
            RAN.load(Ordering::SeqCst),
            0,
            "nothing before the shell speaks"
        );
        assert_eq!(run(shell, false), 0);
        assert_eq!(RAN.load(Ordering::SeqCst), 1, "once, however much it said");
    }

    /// Input the program is not reading never stops its output. The session
    /// is handed far more typed-ahead input than a tty input queue holds while
    /// the program it runs writes thousands of lines and never reads: the
    /// driver must keep passing the output through and return the program's
    /// own exit status. A driver that parks writing input into the full queue
    /// stops reading the output, so the program parks writing it and neither
    /// ever moves. Runs in a re-exec of this test binary whose stdin is a pipe
    /// holding the input; a minute is the hang detector.
    #[test]
    fn input_the_program_never_reads_does_not_stop_its_output() {
        const CHILD: &str = "ATERM_TEST_DRIVER_UNREAD_INPUT";
        const LINES: usize = 4000;
        if std::env::var_os(CHILD).is_none() {
            let name = format!(
                "{}::input_the_program_never_reads_does_not_stop_its_output",
                module_path!().split_once("::").map_or("", |(_, rest)| rest)
            );
            let mut child =
                aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
                    .args(["--exact", &name, "--nocapture", "--test-threads=1"])
                    .env(CHILD, "1")
                    .stdin(aterm_uds::spawnfd::Stdio::piped())
                    .stdout(aterm_uds::spawnfd::Stdio::piped())
                    .stderr(aterm_uds::spawnfd::Stdio::piped())
                    .spawn()
                    .expect("re-exec the test binary");
            // 256 KiB of complete lines: far past any tty input queue.
            let mut stdin = child.stdin.take().expect("stdin");
            let feeder = std::thread::spawn(move || {
                let line = [b"typed-ahead input nobody reads".as_slice(), b"\n"].concat();
                for _ in 0..(256 * 1024 / line.len()) {
                    if stdin.write_all(&line).is_err() {
                        return;
                    }
                }
            });
            let drain = |mut from: Box<dyn Read + Send>| {
                std::thread::spawn(move || {
                    let mut all = Vec::new();
                    let _ = from.read_to_end(&mut all);
                    all
                })
            };
            let stdout = drain(Box::new(child.stdout.take().expect("stdout")));
            let stderr = drain(Box::new(child.stderr.take().expect("stderr")));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            let status = loop {
                if let Some(status) = child.try_wait().expect("try_wait") {
                    break Some(status);
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            };
            let _ = feeder.join();
            let out = stdout.join().expect("stdout drain");
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out),
                String::from_utf8_lossy(&stderr.join().expect("stderr drain"))
            );
            let status = status.unwrap_or_else(|| {
                panic!(
                    "the session hung: the program's output stopped behind input it never \
                     reads ({} bytes passed through)",
                    out.len()
                )
            });
            assert!(status.success(), "the re-exec failed:\n{text}");
            assert!(
                text.contains("1 passed"),
                "the re-exec ran no test:\n{text}"
            );
            let last = format!("out{}", LINES - 1);
            assert!(
                String::from_utf8_lossy(&out).contains(&last),
                "the program's last line never passed through"
            );
            return;
        }
        // SAFETY: this is the isolated test child's trusted launch point.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let command: [std::ffi::OsString; 3] = [
            "/bin/sh".into(),
            "-c".into(),
            format!("i=0; while [ $i -lt {LINES} ]; do echo out$i; i=$((i+1)); done; exit 5")
                .into(),
        ];
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&command),
            None,
            None,
            aterm_sandbox::Limits::shell_default(),
        )
        .expect("protected shell spawn");
        assert_eq!(
            run(shell, false),
            5,
            "the session returns the program's status"
        );
    }

    /// A resize of the host terminal that lands before the driver is
    /// listening still reaches the program. The session reads the host size
    /// for the spawn, then sets up for tens of milliseconds (containment, the
    /// PATH dirs, the fork and exec) with `SIGWINCH` at its default, ignored —
    /// so a window manager laying out a just-opened terminal resizes it inside
    /// that window, the signal is dropped, and the program keeps the stale size
    /// until the next resize. Here the host terminal is a PTY this re-exec puts
    /// on its stdout, resized after the program is up and before [`run`]; the
    /// program waits for a `SIGWINCH` and prints the size it then reads. Its
    /// wait is bounded at a minute, the hang detector.
    #[test]
    fn a_resize_before_the_driver_listens_reaches_the_program() {
        const CHILD: &str = "ATERM_TEST_DRIVER_EARLY_RESIZE";
        if std::env::var_os(CHILD).is_none() {
            let name = format!(
                "{}::a_resize_before_the_driver_listens_reaches_the_program",
                module_path!().split_once("::").map_or("", |(_, rest)| rest)
            );
            let out = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
                .args(["--exact", &name, "--nocapture", "--test-threads=1"])
                .env(CHILD, "1")
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .output()
                .expect("re-exec the test binary");
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(out.status.success(), "the re-exec failed:\n{text}");
            assert!(
                text.contains("1 passed"),
                "the re-exec ran no test:\n{text}"
            );
            return;
        }
        // The host terminal: a PTY whose slave is this process's stdout, so
        // `host_winsize` reads it, and whose master this test resizes and
        // drains like the window around a real session would.
        let (host, host_slave) = aterm_pty::open_pty_pair().expect("host pty");
        aterm_pty::resize(host, 30, 100);
        // SAFETY: duplicating descriptors this test owns: the harness's stdout
        // is kept aside for its result line, and the host slave takes its place.
        let harness_stdout = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
        assert!(harness_stdout >= 0, "keep the harness's stdout");
        // SAFETY: as above.
        assert_eq!(
            unsafe { libc::dup2(host_slave, libc::STDOUT_FILENO) },
            libc::STDOUT_FILENO
        );
        aterm_pty::close_fd(host_slave);
        let drained = std::thread::spawn(move || {
            let mut all = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = aterm_pty::read(host, &mut buf);
                if n <= 0 {
                    return all;
                }
                all.extend_from_slice(&buf[..n as usize]);
                if String::from_utf8_lossy(&all).contains("SIZE ") {
                    return all;
                }
            }
        });

        // SAFETY: this is the isolated test child's trusted launch point.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let command: [std::ffi::OsString; 3] = [
            "/bin/sh".into(),
            "-c".into(),
            "trap 'echo SIZE $(stty size); exit 0' WINCH; echo READY; i=0; \
             while [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done; \
             echo SIZE $(stty size) unchanged; exit 3"
                .into(),
        ];
        let (rows, cols) = host_winsize();
        assert_eq!((rows, cols), (30, 100), "the spawn reads the host's size");
        let shell = aterm_pty::spawn_shell_with_pid(
            rows,
            cols,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&command),
            None,
            None,
            aterm_sandbox::Limits::shell_default(),
        )
        .expect("protected shell spawn");
        // The program is up, with its WINCH trap set.
        let mut seen = Vec::new();
        let mut buf = [0u8; 256];
        while !String::from_utf8_lossy(&seen).contains("READY") {
            let n = aterm_pty::read(shell.master, &mut buf);
            assert!(n > 0, "the program ended before it was up");
            seen.extend_from_slice(&buf[..n as usize]);
        }
        // The host terminal is resized before the driver listens.
        aterm_pty::resize(host, 40, 120);
        let code = run(shell, false);
        // The harness's own stdout back for its result line; that also closes
        // the host's last slave, so the drain ends even if nothing matched.
        // SAFETY: duplicating a descriptor this test owns onto stdout.
        unsafe { libc::dup2(harness_stdout, libc::STDOUT_FILENO) };
        let out = drained.join().expect("host drain");
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("SIZE 40 120"),
            "the program never learned the host's new size: {text:?}"
        );
        assert_eq!(code, 0, "the program saw the resize: {text:?}");
    }

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

    /// A pipe closed with buffered bytes becomes terminal readiness (bare
    /// `POLLHUP` or `POLLIN|POLLHUP`); driving every observation through
    /// `poll_must_read` recovers the full tail and then EOF.
    #[test]
    fn closed_pipe_drains_buffered_bytes_then_consumes_hangup() {
        let mut raw = [-1; 2];
        assert_eq!(unsafe { libc::pipe(raw.as_mut_ptr()) }, 0);
        // Mark both ends close-on-exec so another test's child cannot hold the
        // write end open.
        for fd in raw {
            // SAFETY: `fd` is a live descriptor `pipe` just returned.
            assert_eq!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
                0
            );
        }
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
            // 10 s: the hang-up needs every copy of the write end closed, and
            // a concurrently forking test may hold one until it execs.
            assert_eq!(unsafe { libc::poll(&mut event, 1, 10_000) }, 1);
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
