// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! POSIX console passthrough driver: raw mode, `poll(2)` I/O loop, `SIGWINCH` resize, and `SIGCHLD`
//! reap.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

static GOT_WINCH: AtomicBool = AtomicBool::new(false);
static GOT_CHLD: AtomicBool = AtomicBool::new(false);
static WAKE_WR: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(sig: libc::c_int) {
    let errno = errno_slot();
    // SAFETY: `errno` points to the calling thread's errno slot.
    let saved = unsafe { *errno };
    match sig {
        libc::SIGWINCH => GOT_WINCH.store(true, Ordering::Release),
        libc::SIGCHLD => GOT_CHLD.store(true, Ordering::Release),
        _ => {}
    }
    let wr = WAKE_WR.load(Ordering::Acquire);
    if wr >= 0 {
        let byte = 1u8;
        // SAFETY: 1-byte write from a local onto the non-blocking wake pipe.
        unsafe { libc::write(wr, (&raw const byte).cast(), 1) };
    }
    // SAFETY: `errno` points to the calling thread's errno slot.
    unsafe { *errno = saved };
}

fn errno_slot() -> *mut libc::c_int {
    // SAFETY: `libc::__error` returns the calling thread's errno pointer.
    #[cfg(target_os = "macos")]
    let slot = unsafe { libc::__error() };
    // SAFETY: `libc::__errno_location` returns the calling thread's errno pointer.
    #[cfg(not(target_os = "macos"))]
    let slot = unsafe { libc::__errno_location() };
    slot
}

fn wake_pipe() -> Option<libc::c_int> {
    static PIPE: std::sync::OnceLock<Option<(libc::c_int, libc::c_int)>> =
        std::sync::OnceLock::new();
    let (rd, wr) = (*PIPE.get_or_init(aterm_pty::make_wake_pipe))?;
    WAKE_WR.store(wr, Ordering::Release);
    Some(rd)
}

fn catch(sig: libc::c_int, flags: libc::c_int) {
    // SAFETY: zeroed `sigaction` is valid and initialized before `sigaction`.
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: `sa.sa_mask` is a valid local `sigset_t`.
    unsafe { libc::sigemptyset(&raw mut sa.sa_mask) };
    sa.sa_flags = libc::SA_RESTART | flags;
    // SAFETY: `on_signal` is async-signal-safe and `sa` is initialized.
    unsafe { libc::sigaction(sig, &raw const sa, std::ptr::null_mut()) };
}

fn host_winsize_raw() -> libc::winsize {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    if !ok || ws.ws_row == 0 || ws.ws_col == 0 {
        ws.ws_row = 24;
        ws.ws_col = 80;
    }
    ws
}

/// Host terminal size as `(rows, cols)`, falling back to `(24, 80)`.
pub(crate) fn host_winsize() -> (u16, u16) {
    let ws = host_winsize_raw();
    (ws.ws_row, ws.ws_col)
}

/// Whether `path` is a regular file this user can execute, and if not, which cause.
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

fn set_raw(fd: libc::c_int) -> Option<libc::termios> {
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) != 0 {
            return None;
        }
        let orig = t;
        libc::cfmakeraw(&mut t);
        if libc::tcsetattr(fd, libc::TCSANOW, &t) != 0 {
            return None;
        }
        Some(orig)
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

fn retry_later() -> bool {
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EINTR | libc::EAGAIN)
    )
}

const INPUT_BACKLOG_MAX: usize = 64 * 1024;

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

const fn poll_must_read(revents: libc::c_short) -> bool {
    revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

fn reap_if_exited(pid: libc::pid_t) -> Option<Option<libc::c_int>> {
    let mut status = 0;
    loop {
        // SAFETY: non-blocking `waitpid` on `pid` with a valid local `status` pointer.
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

fn wait_for(pid: libc::pid_t) -> Option<libc::c_int> {
    let mut status = 0;
    loop {
        // SAFETY: blocking `waitpid` on `pid` with a valid local `status` pointer.
        let r = unsafe { libc::waitpid(pid, &mut status, 0) };
        if r < 0 && eintr() {
            continue;
        }
        return (r == pid).then_some(status);
    }
}

/// Own raw-mode restoration and the exact shell through normal return or maintenance unwind.
struct Passthrough {
    master: Option<libc::c_int>,
    shell: libc::pid_t,
    original: Option<libc::termios>,
    reaped: Option<Option<libc::c_int>>,
}

impl Passthrough {
    fn settle(&mut self) -> Option<libc::c_int> {
        if let Some(original) = self.original.take() {
            restore(libc::STDIN_FILENO, &original);
        }
        if let Some(master) = self.master.take() {
            // SAFETY: this owner closes the session's PTY master once, before its exact wait.
            unsafe { libc::close(master) };
        }
        *self.reaped.get_or_insert_with(|| wait_for(self.shell))
    }
}

impl Drop for Passthrough {
    fn drop(&mut self) {
        let _ = self.settle();
    }
}

const fn exit_code(status: Option<libc::c_int>) -> i32 {
    match status {
        Some(status) if libc::WIFEXITED(status) => libc::WEXITSTATUS(status),
        _ => 1,
    }
}

const EXIT_DRAIN_MAX: usize = 1024 * 1024;

fn drain_master(master: libc::c_int, buf: &mut [u8], pass: &mut impl FnMut(&[u8])) {
    let mut left = EXIT_DRAIN_MAX;
    while left > 0 {
        let mut ready = libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `ready` is a valid single-element `pollfd` with zero timeout.
        let n = unsafe { libc::poll(&mut ready, 1, 0) };
        if n < 0 && eintr() {
            continue;
        }
        if n <= 0 || !poll_must_read(ready.revents) {
            return;
        }
        let want = buf.len().min(left);
        // SAFETY: reads at most `want <= buf.len()` bytes into `buf`.
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

/// Test seam: runs each loop turn between the signal-flag checks and the `poll(2)`, the
/// point where a flag-only handler would lose a signal.
#[cfg(test)]
static BEFORE_POLL: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Run the raw-mode passthrough `poll(2)` loop until the shell exits and return its exit code.
pub(crate) fn run(
    shell: aterm_pty::SpawnedShell,
    verbose: bool,
    mut maintenance: Option<crate::SessionMaintenance>,
) -> i32 {
    let master = shell.master;
    let shell_pid = shell.pid;

    let stdin_is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    let orig = stdin_is_tty.then(|| set_raw(libc::STDIN_FILENO)).flatten();
    let mut session = Passthrough {
        master: Some(master),
        shell: shell_pid,
        original: orig,
        reaped: None,
    };
    let wake_rd = wake_pipe();
    catch(libc::SIGWINCH, 0);
    catch(libc::SIGCHLD, libc::SA_NOCLDSTOP);
    if aterm_pty::size(master) != Some(host_winsize()) {
        GOT_WINCH.store(true, Ordering::Release);
    }
    GOT_CHLD.store(true, Ordering::Release);
    let idle_wait_ms = if wake_rd.is_some() { -1 } else { 100 };

    let mut bytes_in: u64 = 0;
    let mut spoke = false;
    let mut pass = |out: &[u8]| {
        write_all(libc::STDOUT_FILENO, out);
        bytes_in = bytes_in.saturating_add(out.len() as u64);
        if !spoke {
            spoke = true;
            crate::first_shell_output();
        }
    };

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
    loop {
        // Apply a pending resize before blocking.
        if GOT_WINCH.swap(false, Ordering::AcqRel) {
            let mut nws = host_winsize_raw();
            unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &mut nws) };
        }
        if GOT_CHLD.swap(false, Ordering::AcqRel)
            && let Some(status) = reap_if_exited(shell_pid)
        {
            session.reaped = Some(status);
            drain_master(master, &mut buf, &mut pass);
            break;
        }

        let deadline = maintenance.as_mut().and_then(|keep| keep());
        let wait_ms = maintenance_wait(idle_wait_ms, deadline, std::time::Instant::now());

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
        if poll_must_read(fds[2].revents) {
            let mut sink = [0u8; 64];
            // SAFETY: reads at most `sink.len()` bytes into `sink` from the non-blocking wake pipe.
            unsafe { libc::read(fds[2].fd, sink.as_mut_ptr().cast(), sink.len()) };
        }

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

    let status = session.settle();
    // `session_main` exits the process, so release the owned callback here, after settlement.
    drop(maintenance);
    if verbose {
        eprintln!("\r\n[aterm] session ended — {bytes_in} bytes passed through.");
    }
    exit_code(status)
}

/// Bound only an owed maintenance deadline; a settled host retains its infinite wake-pipe poll.
fn maintenance_wait(
    idle: libc::c_int,
    deadline: Option<std::time::Instant>,
    now: std::time::Instant,
) -> libc::c_int {
    let Some(deadline) = deadline else {
        return idle;
    };
    let remaining = deadline.saturating_duration_since(now);
    // Round positive fractions up, so sub-millisecond deadlines cannot busy-spin.
    let millis = remaining
        .as_millis()
        .saturating_add(u128::from(remaining.subsec_nanos() % 1_000_000 != 0));
    let due = libc::c_int::try_from(millis).unwrap_or(libc::c_int::MAX);
    if idle < 0 { due } else { idle.min(due) }
}

#[cfg(test)]
#[path = "driver_unix_tests.rs"]
mod tests;
