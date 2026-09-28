// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pid liveness for the stale-socket sweep (`aterm-<pid>.sock` files of dead
//! pids are garbage; live ones — including our own — are untouchable).
//!
//! The shipping Unix caller keeps its own `kill(pid, 0)` code verbatim
//! (`control_auth`); the Unix implementation here mirrors it so this crate's
//! tests exercise one portable surface, and the Windows implementation is
//! what `control_auth`'s Windows half dispatches to.

/// Whether `pid` names a live process.
///
/// * Unix: `kill(pid, 0)` — delivery permission is checked without sending,
///   so success and `EPERM` both mean "alive". Pids that cannot be real
///   (0, or wider than `pid_t`) are dead.
/// * Windows: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE)`
///   — success plus `WaitForSingleObject(h, 0) == WAIT_TIMEOUT` (the process
///   object is unsignaled, i.e. still running) means alive;
///   `ERROR_ACCESS_DENIED` also means alive (the pid exists but is not ours —
///   mirrors `EPERM`); anything else is dead.
#[must_use]
#[cfg(unix)]
// Skip: liveness probes the OS via a `kill(pid, 0)` syscall (FFI, unverifiable body).
#[cfg_attr(trust_verify, trust::skip)]
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    if unsafe { kill(pid as i32, 0) } == 0 {
        return true;
    }
    // EPERM == 1 on every supported Unix.
    std::io::Error::last_os_error().raw_os_error() == Some(1)
}

/// Whether `pid` names a live process (see the Unix twin's docs for the
/// shared contract).
#[must_use]
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use crate::win::ffi;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const ERROR_ACCESS_DENIED: u32 = 5;
    const WAIT_TIMEOUT: u32 = 258;
    const STILL_ACTIVE: u32 = 259;
    // Inline like the Unix twin's `kill`: the one extra kernel32 import this
    // predicate needs beyond win::ffi.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn WaitForSingleObject(hHandle: ffi::RawHandle, dwMilliseconds: u32) -> u32;
    }
    if pid == 0 {
        return false;
    }
    let handle =
        unsafe { ffi::OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) };
    if handle == 0 {
        // Can't even query it: only "exists but is not ours" counts as alive.
        return unsafe { ffi::GetLastError() } == ERROR_ACCESS_DENIED;
    }
    // Unsignaled (WAIT_TIMEOUT) == still running — the primary check, NOT
    // `GetExitCodeProcess == STILL_ACTIVE` alone: a process that exited WITH
    // code 259 collides with that sentinel and would read alive forever,
    // while its object is signaled. The exit-code conjunct is a belt-and-
    // braces confirmation (a running process always reports STILL_ACTIVE).
    // (Windows' fast pid reuse still applies — an unrelated process on a
    // recycled pid reads alive — so callers must treat "alive" as advisory,
    // never as proof the socket's owner survives; `socket_is_live` is the
    // authoritative probe.)
    let mut code: u32 = 0;
    let alive = unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT
        && unsafe { ffi::GetExitCodeProcess(handle, &mut code) } != 0
        && code == STILL_ACTIVE;
    let _ = unsafe { ffi::CloseHandle(handle) };
    alive
}

/// End this process by `SIGKILL`, here: no destructor, exit handler or buffered
/// flush runs. For fault injection that must die at one exact point (aterm-link's
/// `ATERM_LINK_FAULT` and `ATERM_LINK_NOTIFY_FAULT`), which used
/// `std::process::abort` — but abort dies by `SIGABRT`, a core-dump signal, so
/// macOS wrote a crash report for every fault a test fired (26 `aterm-link`
/// reports on the owner's machine by 2026-09-27). `SIGKILL` writes none.
#[cfg(unix)]
// Skip: signals this process via the `getpid`/`kill` syscalls (FFI, unverifiable body).
#[cfg_attr(trust_verify, trust::skip)]
pub fn kill_self_now() -> ! {
    unsafe extern "C" {
        fn getpid() -> i32;
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SIGKILL is 9 on every supported Unix.
    const SIGKILL: i32 = 9;
    // SAFETY: neither call takes a pointer, and `getpid` cannot fail.
    let sent = unsafe { kill(getpid(), SIGKILL) };
    if sent == 0 {
        // SIGKILL cannot be caught, blocked or ignored, but its delivery is
        // asynchronous: in a multithreaded process this thread can return from
        // `kill` before the kernel ends the process. Falling through to `abort`
        // let SIGABRT win that race in 2 of 5 runs of the test below (measured
        // 2026-09-27), so wait here, running nothing, until the kernel is done.
        loop {
            std::thread::park();
        }
    }
    // Only if the kernel refused our own pid, which it does not; keeps the `!`.
    std::process::abort()
}

/// End this process here, running nothing (see the Unix twin): Windows has no
/// `SIGKILL`, and its `abort` writes no macOS crash report.
#[cfg(windows)]
pub fn kill_self_now() -> ! {
    std::process::abort()
}

#[cfg(all(test, unix))]
mod kill_self_tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};

    const CHILD_ENV: &str = "ATERM_UDS_TEST_KILL_SELF_CHILD";

    /// `kill_self_now` ends the process by SIGKILL: what the fault-injection
    /// callers rely on (the process stops at the call, nothing of it runs), and a
    /// signal that, unlike abort's SIGABRT, leaves no macOS crash report.
    #[test]
    fn kill_self_now_dies_by_sigkill() {
        if std::env::var_os(CHILD_ENV).is_some() {
            super::kill_self_now();
        }
        let status = Command::new(std::env::current_exe().expect("the test binary"))
            .arg("process::kill_self_tests::kill_self_now_dies_by_sigkill")
            .arg("--exact")
            .arg("--test-threads=1")
            .env(CHILD_ENV, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("re-exec this test binary as the child");
        assert_eq!(status.signal(), Some(9), "the child's end: {status}");
    }
}

#[cfg(all(test, windows))]
mod tests {
    /// The documented Win32 footgun this predicate must dodge: a process that
    /// exits WITH code 259 (== STILL_ACTIVE) must still read dead. The
    /// `Child` is kept un-dropped so its open handle pins the pid — the exact
    /// window where `GetExitCodeProcess == STILL_ACTIVE` misreads alive.
    #[test]
    fn exit_code_259_is_not_alive() {
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "exit 259"])
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let status = child.wait().expect("wait");
        assert_eq!(status.code(), Some(259), "child exited with the sentinel");
        assert!(
            !super::pid_alive(pid),
            "exit code 259 must not be misread as STILL_ACTIVE"
        );
        drop(child);
    }
}
