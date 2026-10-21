// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The POSIX console driver for the `aterm` passthrough binary: termios raw
//! mode, a `poll(2)` I/O loop over stdin, the PTY master and the wake pipe its
//! signal handlers write, SIGWINCH resize forwarding, and the shell's reap on
//! SIGCHLD (`waitpid`/`WIFEXITED`), which ends the session with its status.
//! `driver_windows.rs` is its ConPTY twin behind the same surface.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Set by the SIGWINCH handler; drained in the main loop.
static GOT_WINCH: AtomicBool = AtomicBool::new(false);
/// Set by the SIGCHLD handler: a child of this process exited, so the loop
/// asks whether it was the shell.
static GOT_CHLD: AtomicBool = AtomicBool::new(false);
/// The write end of the loop's wake pipe ([`wake_pipe`]); -1 until it exists.
static WAKE_WR: AtomicI32 = AtomicI32::new(-1);

/// The handler of both signals the loop waits on. A flag alone is a lost
/// wakeup: one raised between the loop's check and its `poll(2)` (or on
/// another thread, where `poll` sees no `EINTR`) waits for the next I/O, a
/// resize applied late and a shell's exit not seen while a job holds the
/// slave. So it raises the flag and then writes a byte into the wake pipe the
/// loop polls — both async-signal-safe — and puts back the `errno` of the code
/// it interrupted, which may be about to read it.
extern "C" fn on_signal(sig: libc::c_int) {
    let errno = errno_slot();
    // SAFETY: the calling thread's own errno slot, valid for its lifetime.
    let saved = unsafe { *errno };
    match sig {
        libc::SIGWINCH => GOT_WINCH.store(true, Ordering::Release),
        libc::SIGCHLD => GOT_CHLD.store(true, Ordering::Release),
        _ => {}
    }
    let wr = WAKE_WR.load(Ordering::Acquire);
    if wr >= 0 {
        let byte = 1u8;
        // SAFETY: a one-byte write(2) from a local; the pipe is non-blocking
        // and never closed ([`wake_pipe`]), so a full one answers `EAGAIN`,
        // and the bytes already in it wake the loop just the same.
        unsafe { libc::write(wr, (&raw const byte).cast(), 1) };
    }
    // SAFETY: as above.
    unsafe { *errno = saved };
}

/// The calling thread's `errno`.
fn errno_slot() -> *mut libc::c_int {
    // SAFETY: returns the calling thread's errno address; takes no arguments.
    #[cfg(target_os = "macos")]
    let slot = unsafe { libc::__error() };
    // SAFETY: as above.
    #[cfg(not(target_os = "macos"))]
    let slot = unsafe { libc::__errno_location() };
    slot
}

/// The read end of the loop's wake pipe, made once per process (non-blocking
/// and close-on-exec, [`aterm_pty::make_wake_pipe`]) and never closed, so a
/// handler running on any thread at any moment writes into this pipe and never
/// into a descriptor that later took its number. `None` if it cannot be made.
fn wake_pipe() -> Option<libc::c_int> {
    static PIPE: std::sync::OnceLock<Option<(libc::c_int, libc::c_int)>> =
        std::sync::OnceLock::new();
    let (rd, wr) = (*PIPE.get_or_init(aterm_pty::make_wake_pipe))?;
    WAKE_WR.store(wr, Ordering::Release);
    Some(rd)
}

/// Route `sig` to [`on_signal`]. `SA_RESTART` keeps this process's other
/// threads' blocking calls running through it; the loop's `poll(2)` is never
/// restarted, and the wake pipe holds the byte by then anyway.
fn catch(sig: libc::c_int, flags: libc::c_int) {
    // SAFETY: an all-zero `sigaction` is a valid value; filled in below.
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    // Cast through a function pointer to satisfy `fn_to_numeric_cast` while
    // yielding the `sighandler_t` address.
    sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: `sa.sa_mask` is a valid out-parameter owned by this frame.
    unsafe { libc::sigemptyset(&raw mut sa.sa_mask) };
    sa.sa_flags = libc::SA_RESTART | flags;
    // SAFETY: installs `on_signal`, which is async-signal-safe; `sa` is fully
    // initialized and the old action is not asked for.
    unsafe { libc::sigaction(sig, &raw const sa, std::ptr::null_mut()) };
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

/// Reap the shell if it has exited, never waiting: `None` while it runs,
/// `Some(status)` once it is gone — `Some(None)` when its status is not this
/// process's to read any more (`ECHILD`: a child can only stop being one by
/// being reaped, so it has exited).
fn reap_if_exited(pid: libc::pid_t) -> Option<Option<libc::c_int>> {
    let mut status = 0;
    loop {
        // SAFETY: a non-blocking wait on this process's own child, by its
        // pid; `status` is a valid out-parameter.
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if r == pid {
            return Some(Some(status));
        }
        if r == 0 {
            return None;
        }
        if !eintr() {
            return Some(None);
        }
    }
}

/// Wait for the shell's exit and return its status (`None` when the wait
/// fails). By its pid, never `waitpid(-1)`: this process may have other
/// children (a detached `aterm pkg update`, the update checker's
/// codesign/curl) whose status would be mistaken for the shell's. The pid is
/// the session leader whether or not it execs the sandbox-exec wrapper.
fn wait_for(pid: libc::pid_t) -> Option<libc::c_int> {
    let mut status = 0;
    loop {
        // SAFETY: a blocking wait on this process's own child, by its pid;
        // `status` is a valid out-parameter.
        let r = unsafe { libc::waitpid(pid, &mut status, 0) };
        if r < 0 && eintr() {
            continue;
        }
        return (r == pid).then_some(status);
    }
}

/// The session's exit code for the shell's wait status: its own code when it
/// exited, 1 when a signal ended it or no status was to be had.
fn exit_code(status: Option<libc::c_int>) -> i32 {
    match status {
        Some(status) if libc::WIFEXITED(status) => libc::WEXITSTATUS(status),
        _ => 1,
    }
}

/// The most output the session still passes through once the shell has
/// exited: far past what a PTY buffers, which is all the shell itself can have
/// left there, so only a job that keeps writing after it ever meets the cap.
const EXIT_DRAIN_MAX: usize = 1024 * 1024;

/// Hand `pass` what the master holds right now, never waiting for more: a
/// zero-timeout `poll(2)` before every read, so this cannot park whether or
/// not the master is non-blocking and whatever still holds the slave. Ends at
/// an empty master, EOF, an error, or [`EXIT_DRAIN_MAX`] bytes.
fn drain_master(master: libc::c_int, buf: &mut [u8], pass: &mut impl FnMut(&[u8])) {
    let mut left = EXIT_DRAIN_MAX;
    while left > 0 {
        let mut ready = libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid `pollfd` owned by this frame; a zero timeout.
        let n = unsafe { libc::poll(&mut ready, 1, 0) };
        if n < 0 && eintr() {
            continue;
        }
        if n <= 0 || !poll_must_read(ready.revents) {
            return;
        }
        let want = buf.len().min(left);
        // SAFETY: reads at most `want` bytes into `buf`, which holds them.
        let r = unsafe { libc::read(master, buf.as_mut_ptr().cast(), want) };
        if r < 0 && eintr() {
            continue;
        }
        let Ok(got) = usize::try_from(r) else { return };
        if got == 0 {
            return;
        }
        pass(&buf[..got]);
        left -= got;
    }
}

/// Test seam: run at the one point of every loop turn between the signal flag
/// checks and the `poll(2)` that waits, where a flag-only signal would be lost.
#[cfg(test)]
static BEFORE_POLL: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Raw mode, the passthrough `poll(2)` loop, resize forwarding, restore, reap:
/// returns the shell's exit status (non-exit or a failed wait → 1). Nothing
/// here models the screen; the PTY is the one thing told the geometry.
///
/// The session ends with the shell, and the master's EOF is not that: it
/// comes when the last holder of the slave lets go, and a job the shell leaves
/// behind (a `nohup`ed `cmd &`, a `setsid` daemon) can hold it past the
/// shell's `exit`. Linux does not hang up a PTY session at its leader's exit
/// at all; macOS revokes the slave then, but a job that opens it again by path
/// before this loop has read the EOF holds the master open again. So the loop
/// reaps the shell on SIGCHLD, passes through what the master still holds and
/// ends; closing the master then hangs up whatever still holds the slave. An
/// EOF while the shell lives (it closed its terminal) still ends the loop, and
/// the shell is waited for after it.
pub(crate) fn run(shell: aterm_pty::SpawnedShell, verbose: bool) -> i32 {
    let master = shell.master;
    let shell_pid = shell.pid;

    let stdin_is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    let orig = if stdin_is_tty {
        Some(set_raw(libc::STDIN_FILENO))
    } else {
        None
    };
    let wake_rd = wake_pipe();
    catch(libc::SIGWINCH, 0);
    // A child that stops or continues has not exited.
    catch(libc::SIGCHLD, libc::SA_NOCLDSTOP);
    // Until the lines above, `SIGWINCH` was at its default — ignored — so a
    // resize since the spawn read the host's size (containment, the PATH dirs,
    // the fork and exec all ran in between) was dropped. Now that the handler
    // sees every later one, catch up once: when the host's grid is no longer
    // the PTY's, the first turn of the loop applies it. An unmoved grid sends
    // the program nothing.
    if aterm_pty::size(master) != Some(host_winsize()) {
        GOT_WINCH.store(true, Ordering::Release);
    }
    // Likewise the shell may have exited before SIGCHLD was caught (a command
    // that fails at once): the first turn asks.
    GOT_CHLD.store(true, Ordering::Release);
    // Without a wake pipe (none could be made) a signal still lands at most
    // this late; with one, the loop sleeps until something happens.
    let wait_ms = if wake_rd.is_some() { -1 } else { 100 };

    let mut bytes_in: u64 = 0;
    // The first output written through marks a healthy start (`on_first_shell_output`).
    let mut spoke = false;
    let mut pass = |out: &[u8]| {
        write_all(libc::STDOUT_FILENO, out);
        bytes_in += out.len() as u64;
        if !spoke {
            spoke = true;
            crate::first_shell_output();
        }
    };

    // Input waits HERE, never in `write(2)`. A program that is not reading
    // (busy printing, `sleep`, a stopped job) leaves its tty input queue full,
    // and a write into a full queue parks until it reads; a loop parked there
    // stops reading the program's OUTPUT, so a program that is printing parks
    // too and neither ever moves again: no output, no resize, no keystroke
    // read. So the master is non-blocking, the input it has no room for is
    // held in `pending` while the output keeps flowing, and stdin is not read
    // while `pending` holds [`INPUT_BACKLOG_MAX`]: the rest waits in the host's
    // own terminal, never in this process's memory. Should the fcntl fail (it
    // cannot on a live master), writes block.
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
        libc::pollfd {
            fd: wake_rd.unwrap_or(-1),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let mut buf = [0u8; 8192];
    // The shell's status once the loop has reaped it.
    let mut reaped: Option<Option<libc::c_int>> = None;

    loop {
        // Apply a pending resize before blocking.
        if GOT_WINCH.swap(false, Ordering::AcqRel) {
            let mut nws = host_winsize_raw();
            unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &mut nws) };
        }
        // A child exited: if it was the shell, the session is over.
        if GOT_CHLD.swap(false, Ordering::AcqRel)
            && let Some(status) = reap_if_exited(shell_pid)
        {
            drain_master(master, &mut buf, &mut pass);
            reaped = Some(status);
            break;
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
        #[cfg(test)]
        if let Some(hook) = BEFORE_POLL.get() {
            hook();
        }
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 3, wait_ms) };
        if n < 0 {
            if eintr() {
                continue; // a signal — loop and apply it
            }
            break;
        }
        // The wake pipe's bytes did their work by waking the poll; the flags
        // say what for. Read after the wake, so a signal from here on leaves
        // a byte for the next poll.
        if poll_must_read(fds[2].revents) {
            let mut sink = [0u8; 64];
            // SAFETY: reads at most `sink.len()` bytes into it from the
            // non-blocking wake pipe.
            unsafe { libc::read(fds[2].fd, sink.as_mut_ptr().cast(), sink.len()) };
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
                break; // the slave closed (or the PTY failed)
            }
            pass(&buf[..r as usize]);
        }
    }

    if let Some(t) = orig {
        restore(libc::STDIN_FILENO, &t);
    }
    // SAFETY: the session's own master, closed once; it hangs up whatever
    // still holds the slave.
    unsafe { libc::close(master) };
    let status = reaped.unwrap_or_else(|| wait_for(shell_pid));
    if verbose {
        eprintln!("\r\n[aterm] session ended — {bytes_in} bytes passed through.");
    }
    exit_code(status)
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

    /// Run this module's test `name` again in a fresh process of this test
    /// binary, with `child` set and `stdin` as its stdin, and check that it
    /// ran and passed. The tests that set [`BEFORE_POLL`] or own this
    /// process's stdout and signals run there, alone.
    fn reexec(name: &str, child: &str, stdin: aterm_uds::spawnfd::Stdio) {
        let name = format!(
            "{}::{name}",
            module_path!().split_once("::").map_or("", |(_, rest)| rest)
        );
        let out = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
            .args(["--exact", &name, "--nocapture", "--test-threads=1"])
            .env(child, "1")
            .stdin(stdin)
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
    }

    /// Poll every 20 ms until `done` holds; a minute is the hang detector.
    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !done() {
            assert!(
                std::time::Instant::now() < deadline,
                "a minute passed and {what}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Whether this process's child `pid` has exited, left unreaped.
    fn has_exited(pid: libc::pid_t) -> bool {
        // SAFETY: `info` is a zeroed out-parameter owned by this frame.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: a non-blocking look at this process's own child that leaves
        // it unreaped (`WNOWAIT`); `info` is valid.
        let r = unsafe {
            libc::waitid(
                libc::P_PID,
                libc::id_t::try_from(pid).expect("pid"),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        // SAFETY: `waitid` filled `info` for a child's exit, or left it zeroed.
        r == 0 && unsafe { info.si_pid() } == pid
    }

    /// A resize that lands between the loop's signal check and its `poll(2)`
    /// reaches the program with nothing else to wake the loop. The seam
    /// [`BEFORE_POLL`] resizes the host terminal and raises `SIGWINCH` there,
    /// once, while the program waits silently for the resize and stdin is a
    /// pipe that stays open and empty. A flag-only handler leaves the resize
    /// unapplied until the next I/O, here the program giving up its wait after
    /// a minute, the hang detector.
    #[test]
    fn a_resize_between_the_signal_check_and_the_poll_reaches_the_program() {
        const CHILD: &str = "ATERM_TEST_DRIVER_RESIZE_BEFORE_POLL";
        static HOST: AtomicI32 = AtomicI32::new(-1);
        static FIRED: AtomicBool = AtomicBool::new(false);
        if std::env::var_os(CHILD).is_none() {
            let (stdin, held) = std::io::pipe().expect("stdin pipe");
            reexec(
                "a_resize_between_the_signal_check_and_the_poll_reaches_the_program",
                CHILD,
                stdin.into(),
            );
            drop(held);
            return;
        }
        // The host terminal, as in the test above: a PTY whose slave is this
        // process's stdout.
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
                let text = String::from_utf8_lossy(&all);
                if text
                    .find("SIZE ")
                    .is_some_and(|at| text[at..].contains('\n'))
                {
                    return all;
                }
            }
        });

        // SAFETY: this is the isolated test child's trusted launch point.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        // `READY` with no newline, so reading it leaves the master empty; the
        // wait's last line is past the trap, so only a resize in time counts.
        let command: [std::ffi::OsString; 3] = [
            "/bin/sh".into(),
            "-c".into(),
            "trap 'echo SIZE $(stty size); exit 0' WINCH; printf READY; i=0; \
             while [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done; \
             trap '' WINCH; echo SIZE $(stty size) unchanged; exit 3"
                .into(),
        ];
        let shell = aterm_pty::spawn_shell_with_pid(
            30,
            100,
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
        let mut seen = Vec::new();
        let mut buf = [0u8; 256];
        while !String::from_utf8_lossy(&seen).contains("READY") {
            let n = aterm_pty::read(shell.master, &mut buf);
            assert!(n > 0, "the program ended before it was up");
            seen.extend_from_slice(&buf[..n as usize]);
        }
        HOST.store(host, Ordering::SeqCst);
        assert!(
            BEFORE_POLL
                .set(|| {
                    if !FIRED.swap(true, Ordering::SeqCst) {
                        aterm_pty::resize(HOST.load(Ordering::SeqCst), 40, 120);
                        // SAFETY: raises a signal at this thread; its handler
                        // runs before `raise` returns.
                        #[expect(
                            clippy::disallowed_methods,
                            reason = "test code: SIGWINCH, which dumps no core"
                        )]
                        unsafe {
                            libc::raise(libc::SIGWINCH)
                        };
                    }
                })
                .is_ok(),
            "the seam is this process's"
        );
        let code = run(shell, false);
        // SAFETY: duplicating a descriptor this test owns onto stdout.
        unsafe { libc::dup2(harness_stdout, libc::STDOUT_FILENO) };
        let out = drained.join().expect("host drain");
        let text = String::from_utf8_lossy(&out);
        assert!(FIRED.load(Ordering::SeqCst), "the seam never ran");
        assert!(
            text.contains("SIZE 40 120"),
            "the resize raised before the poll waited for other I/O: {text:?}"
        );
        assert_eq!(code, 0, "the program saw the resize: {text:?}");
    }

    /// The session ends when the shell exits, though a job it left behind
    /// still holds the terminal. The job ignores `SIGHUP` (as under `nohup`),
    /// drops its inherited copies of the terminal and, once the shell is gone,
    /// opens it again by path. On Linux any such holder keeps the master from
    /// EOF, since a PTY session is not hung up at its leader's exit; macOS
    /// revokes the slave then, so here the job must open it again before the
    /// loop reads the EOF, which the seam [`BEFORE_POLL`] arranges: once the
    /// job ignores `SIGHUP` it lets the shell exit, waits for that and for the
    /// job's open, then lets the loop poll. The job holds the terminal until told to let go, at most a
    /// minute, the hang detector; a session that ends only at the master's EOF
    /// ends only when that minute is up.
    #[test]
    fn the_session_ends_with_the_shell_while_a_job_holds_its_terminal() {
        const CHILD: &str = "ATERM_TEST_DRIVER_JOB_HOLDS_THE_SLAVE";
        static SHELL: AtomicI32 = AtomicI32::new(-1);
        static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        if std::env::var_os(CHILD).is_none() {
            reexec(
                "the_session_ends_with_the_shell_while_a_job_holds_its_terminal",
                CHILD,
                aterm_uds::spawnfd::Stdio::null(),
            );
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "aterm-driver-job-holds-slave-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        // SAFETY: this is the isolated test child's trusted launch point.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let command: [std::ffi::OsString; 3] = [
            "/bin/sh".into(),
            "-c".into(),
            r#"t=$(tty); d=$ATERM_TEST_DIR
            ( trap '' HUP; : >"$d/armed"; exec </dev/null >/dev/null 2>&1
              i=0; while [ ! -e "$d/go" ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done
              exec 3<>"$t"; : >"$d/held"
              i=0; while [ ! -e "$d/release" ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done
              [ -e "$d/release" ] || : >"$d/held-to-the-end"
              : >"$d/done" ) &
            i=0; while [ ! -e "$d/exit" ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done
            exit 7"#
                .into(),
        ];
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[("ATERM_TEST_DIR".into(), dir.to_string_lossy().into_owned())],
            None,
            None,
            None,
            Some(&command),
            None,
            None,
            aterm_sandbox::Limits::shell_default(),
        )
        .expect("protected shell spawn");
        SHELL.store(shell.pid, Ordering::SeqCst);
        assert!(
            DIR.set(dir.clone()).is_ok(),
            "the scratch dir is this process's"
        );
        assert!(
            BEFORE_POLL
                .set(|| {
                    if FIRED.swap(true, Ordering::SeqCst) {
                        return;
                    }
                    let dir = DIR.get().expect("scratch dir");
                    // The shell's exit hangs up the job unless it already
                    // ignores SIGHUP: a job forked just before the exit can
                    // be cut before its `trap` (seen on a loaded machine).
                    wait_until("the job does not ignore SIGHUP yet", || {
                        dir.join("armed").exists()
                    });
                    std::fs::write(dir.join("exit"), "").expect("let the shell exit");
                    wait_until("the shell has not exited", || {
                        has_exited(SHELL.load(Ordering::SeqCst))
                    });
                    std::fs::write(dir.join("go"), "").expect("let the job open the terminal");
                    wait_until("the job has not opened the terminal again", || {
                        dir.join("held").exists()
                    });
                })
                .is_ok(),
            "the seam is this process's"
        );
        let code = run(shell, false);
        let held_to_the_end = dir.join("held-to-the-end").exists();
        std::fs::write(dir.join("release"), "").expect("let the job go");
        wait_until("the job has not finished", || dir.join("done").exists());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(FIRED.load(Ordering::SeqCst), "the seam never ran");
        assert!(
            !held_to_the_end,
            "the session outlived its shell until the job let go of the terminal"
        );
        assert_eq!(code, 7, "the session returns the shell's status");
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
