// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native resource accounting for the owning `spawnfd::Child`, not a free pid reaper.

use super::Rusage;
use std::io;

#[cfg(all(
    target_pointer_width = "64",
    any(target_vendor = "apple", target_os = "linux")
))]
pub(super) fn wait(pid: u32, block: bool) -> io::Result<Option<Rusage>> {
    collect(pid, i32::from(!block))
}

#[cfg(all(
    target_pointer_width = "64",
    any(target_vendor = "apple", target_os = "linux")
))]
pub(super) fn take_job(pid: u32) -> io::Result<Option<Rusage>> {
    collect(pid, libc::WUNTRACED | libc::WNOHANG)
}

#[cfg(all(
    target_pointer_width = "64",
    any(target_vendor = "apple", target_os = "linux")
))]
fn collect(pid: u32, options: i32) -> io::Result<Option<Rusage>> {
    use std::os::unix::process::ExitStatusExt as _;
    use std::time::Duration;

    #[repr(C)]
    #[derive(Default)]
    struct Timeval {
        sec: i64,
        #[cfg(target_vendor = "apple")]
        usec: i32,
        #[cfg(target_vendor = "apple")]
        pad: i32,
        #[cfg(target_os = "linux")]
        usec: i64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct NativeUsage {
        user: Timeval,
        system: Timeval,
        maxrss: i64,
        rest: [i64; 13],
    }
    unsafe extern "C" {
        fn wait4(pid: i32, status: *mut i32, options: i32, usage: *mut NativeUsage) -> i32;
    }
    let pid = i32::try_from(pid).map_err(|_| io::Error::other("pid out of range"))?;
    let span = |t: &Timeval| {
        Duration::from_secs(u64::try_from(t.sec).unwrap_or(0))
            + Duration::from_micros(u64::try_from(t.usec).unwrap_or(0))
    };
    loop {
        let mut status = 0;
        let mut usage = NativeUsage::default();
        // SAFETY: the caller owns this unreaped child; both outputs are live with the native ABI's
        // layout, and wait4 retains neither pointer.
        let got = unsafe { wait4(pid, &raw mut status, options, &raw mut usage) };
        if got == 0 {
            return Ok(None);
        }
        if got < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        crate::deaths::note(got, status);
        let rss = u64::try_from(usage.maxrss).unwrap_or(0);
        return Ok(Some(Rusage {
            status: std::process::ExitStatus::from_raw(status),
            cpu: span(&usage.user) + span(&usage.system),
            maxrss: if cfg!(target_vendor = "apple") {
                rss
            } else {
                rss.saturating_mul(1024)
            },
        }));
    }
}

/// Whether `pid`, the caller's own child, has exited, left unreaped (`waitid` with `WNOWAIT`).
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
pub(super) fn exited(pid: u32) -> io::Result<bool> {
    observe_exit(pid, false)
}

/// Observe this owned child's exit while preserving its unreaped identity.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
pub(super) fn observe_exit(pid: u32, block: bool) -> io::Result<bool> {
    loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: `info` is one writable `siginfo_t` that `waitid` does not retain; the pid is
        // the caller's own child, which `WNOWAIT` leaves unreaped.
        let got = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT | if block { 0 } else { libc::WNOHANG },
            )
        };
        if got == 0 {
            // SAFETY: zeroed plain data, which `waitid` filled or, with no exit to report under
            // `WNOHANG`, left with `si_pid` 0.
            let info = unsafe { info.assume_init() };
            if info.si_signo == 0 && !block {
                return Ok(false);
            }
            // A start-suspended Darwin child can produce CLD_STOPPED despite
            // WEXITED. A returned pid alone is not evidence of termination.
            if info.si_signo == libc::SIGCHLD
                // SAFETY: SIGCHLD selects the pid arm of this initialized record.
                && u32::try_from(unsafe { info.si_pid() }).ok() == Some(pid)
            {
                return match info.si_code {
                    libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED => Ok(true),
                    libc::CLD_STOPPED | libc::CLD_TRAPPED | libc::CLD_CONTINUED if !block => {
                        Ok(false)
                    }
                    libc::CLD_STOPPED | libc::CLD_TRAPPED | libc::CLD_CONTINUED => {
                        wait_after_nonterminal(pid)
                    }
                    _ => Err(io::Error::other(
                        "owned child reported an unknown wait event",
                    )),
                };
            }
            return Err(io::Error::other(
                "wait did not observe this owned child's exit",
            ));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Darwin can leave a suspended-start notification pending. Wait on the existing
/// kernel exit witness rather than consuming that state, reaping, or busy polling.
#[cfg(target_vendor = "apple")]
fn wait_after_nonterminal(pid: u32) -> io::Result<bool> {
    wait_after_nonterminal_with(pid, || {})
}

#[cfg(target_vendor = "apple")]
fn wait_after_nonterminal_with(pid: u32, ready: impl FnOnce()) -> io::Result<bool> {
    use std::os::fd::BorrowedFd;
    let Some(watch) = crate::exitwatch::ExitWatch::watch(pid) else {
        return if observe_exit(pid, false)? {
            Ok(true)
        } else {
            Err(io::Error::other(
                "no kernel exit watch for this owned child",
            ))
        };
    };
    ready();
    if watch.exit_status().is_some() || observe_exit(pid, false)? {
        return Ok(true);
    }
    // SAFETY: this watch owns the descriptor until the readiness wait completes.
    crate::ready::one(unsafe { BorrowedFd::borrow_raw(watch.as_raw_fd()) })?;
    if watch.exit_status().is_some() {
        Ok(true)
    } else {
        Err(io::Error::other(
            "owned child's exit watch reported no exit",
        ))
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::*;
    use crate::spawnfd::{Stdio, command};

    /// The blocking nonterminal path waits on actual kernel exit, preserves the
    /// owning wait, and needs neither a polling loop nor another process owner.
    #[test]
    fn a_stopped_notification_waits_for_real_exit_without_reaping() {
        let mut child = command("/bin/cat")
            .start_suspended(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut resumed = false;
        let seen = wait_after_nonterminal_with(pid, || {
            child.resume().unwrap();
            child.stdin.take();
            resumed = true;
        });
        if !resumed {
            let _ = child.kill();
        }
        let status = child.wait().unwrap();
        assert!(seen.unwrap());
        assert!(resumed);
        assert!(status.success(), "the original owner still reaps its child");
    }
}

#[cfg(all(not(target_vendor = "apple"), target_os = "linux"))]
fn wait_after_nonterminal(_pid: u32) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "owned child reported a nonterminal wait event; it remains unreaped",
    ))
}

/// See the Apple and Linux twin.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub(super) fn exited(_pid: u32) -> io::Result<bool> {
    observe_exit(_pid, false)
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub(super) fn observe_exit(_pid: u32, _block: bool) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "an unreaped exit is read on Darwin and Linux only",
    ))
}

#[cfg(not(all(
    target_pointer_width = "64",
    any(target_vendor = "apple", target_os = "linux")
)))]
pub(super) fn wait(_pid: u32, _block: bool) -> io::Result<Option<Rusage>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "resource accounting requires 64-bit Darwin or Linux",
    ))
}

#[cfg(not(all(
    target_pointer_width = "64",
    any(target_vendor = "apple", target_os = "linux")
)))]
pub(super) fn take_job(_pid: u32) -> io::Result<Option<Rusage>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "job resource accounting requires 64-bit Darwin or Linux",
    ))
}
