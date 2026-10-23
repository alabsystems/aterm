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
        let got = unsafe { wait4(pid, &raw mut status, i32::from(!block), &raw mut usage) };
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
