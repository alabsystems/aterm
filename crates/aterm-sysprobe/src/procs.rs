// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The process sweep: every process's pid, parent, CPU time, footprint and
//! short name, read in one pass for the system-awareness stats
//! (`docs/DESIGN-system-awareness-2026-10-16.md`), which turn two sweeps into
//! per-process rates and walk the parent links to tell aterm's processes, and
//! each tab's, from the rest.
//!
//! * **macOS** — `proc_listallpids`, then per pid
//!   `proc_pidinfo(PROC_PIDT_SHORTBSDINFO)` (parent, group, `p_stat`, uid,
//!   the session-leader and terminal flags and `p_comm`, readable for every
//!   process; a zombie answers none, so its row comes from
//!   `sysctl(KERN_PROC_PID)`) and `proc_pid_rusage(RUSAGE_INFO_V4)` (CPU
//!   time, `ri_phys_footprint` and the disk bytes `ri_diskio_bytesread` and
//!   `ri_diskio_byteswritten`, readable for the user's own processes;
//!   another user's answer `None`). `ri_user_time` and `ri_system_time`
//!   count Mach absolute-time ticks, not nanoseconds, on Apple Silicon (the
//!   timebase is 125/3 there and 1/1 on Intel), so they are scaled by
//!   `mach_timebase_info`.
//! * **Linux** — `/proc/<pid>/stat` (parent, `comm`, state, group, session,
//!   terminal, `utime + stime` in clock ticks), `/proc/<pid>/status`
//!   (`RssAnon + VmSwap`: anonymous memory, resident or swapped, the nearest
//!   Linux has to a footprint; `statm`'s resident pages where `RssAnon` is
//!   missing) and the owner of `/proc/<pid>`, each through
//!   [`crate::with_bounded`] ([`crate::linux::Root::processes`]). Every
//!   user's processes answer unless `/proc` is mounted `hidepid`.
//! * **Windows** — one `NtQuerySystemInformation(SystemProcessInformation)`
//!   pass ([`crate::windows::processes`]): every process's parent, image
//!   name, user and kernel time and private commit (`PrivatePageCount`),
//!   readable unprivileged for every user. The System Idle Process (pid 0),
//!   whose "CPU time" is the cores' idle time, is left out, and a parent
//!   link whose pid was reused by a process born after the child is cut.
//!   Windows has no process groups, sessions in the Unix sense or
//!   controlling terminals, so those read 0 and `None`, and the state
//!   `Unknown`.
//!   No disk bytes are read on Linux or Windows. The Linux job probe
//!   reads `/proc/<pid>/io` only for processes it is asked about.
//!
//! The group, state and the two flags are what the stuck-process engine
//! (`aterm_vitals::jobs`) forms job units from before it asks the job probe
//! ([`crate::jobs`]) about the members.
//! * **Elsewhere** — `None`.
//!
//! Bounded: at most [`MAX_PROCESSES`] processes; a machine with more is
//! swept up to the cap and says so ([`Sweep::truncated`]).

/// The sweep stops here.
pub const MAX_PROCESSES: usize = 16_384;

/// One process in a sweep.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessReading {
    /// Its pid.
    pub pid: u32,
    /// Its parent's pid (0 when the kernel names none).
    pub ppid: u32,
    /// User plus system CPU time since it started, nanoseconds; `None` where
    /// the OS refuses (another user's process on macOS).
    pub cpu_ns: Option<u64>,
    /// Its memory in bytes: macOS `ri_phys_footprint`, Linux `RssAnon +
    /// VmSwap`, Windows its private commit; `None` where refused.
    pub footprint: Option<u64>,
    /// Bytes it has read from disk since it started (macOS
    /// `ri_diskio_bytesread`: storage reads, not page-cache hits); `None`
    /// where refused, and on Linux.
    pub disk_read: Option<u64>,
    /// Bytes it has written to disk since it started (macOS
    /// `ri_diskio_byteswritten`: what reached storage, not every
    /// `write(2)`); `None` where refused, and on Linux.
    pub disk_written: Option<u64>,
    /// Its short name (`p_comm`, 16 bytes at most on macOS; Linux `comm`;
    /// Windows the image name less `.exe`).
    pub name: String,
    /// Its process group (0 where unread).
    pub pgid: u32,
    /// Its user; `None` where unread.
    pub uid: Option<u32>,
    /// Its state (`p_stat`; on macOS `Running` covers a process blocked in
    /// `read(2)`).
    pub state: crate::jobs::RunState,
    /// Whether it leads its session; `None` where unread.
    pub session_leader: Option<bool>,
    /// Whether it has a controlling terminal; `None` where unread.
    pub has_tty: Option<bool>,
}

/// One sweep of the process table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sweep {
    /// Every process read.
    pub procs: Vec<ProcessReading>,
    /// Whether the table held more than [`MAX_PROCESSES`].
    pub truncated: bool,
}

/// Sweep every process; `None` where the platform has no reader.
#[must_use]
pub fn processes() -> Option<Sweep> {
    imp::processes()
}

/// `/proc/<pid>/statm`'s resident pages (its second field).
#[must_use]
pub fn parse_statm_resident(text: &str) -> Option<u64> {
    text.split_whitespace().nth(1)?.parse().ok()
}

/// Mach ticks to nanoseconds at the timebase `numer/denom`.
#[must_use]
pub fn ticks_to_ns(ticks: u64, numer: u32, denom: u32) -> u64 {
    if denom == 0 {
        return ticks;
    }
    let ns = u128::from(ticks) * u128::from(numer) / u128::from(denom);
    u64::try_from(ns).unwrap_or(u64::MAX)
}

#[cfg(target_os = "macos")]
pub(crate) mod imp {
    use std::ffi::c_int;
    use std::sync::OnceLock;

    use super::{MAX_PROCESSES, ProcessReading, Sweep, ticks_to_ns};
    use crate::jobs::RunState;
    use crate::jobs::darwin::PROC_FLAG_CONTROLT;

    /// `PROC_PIDT_SHORTBSDINFO` (`sys/proc_info.h`), as the crate's macOS arm
    /// declares it (the reference `libc` does not export it).
    const PROC_PIDT_SHORTBSDINFO: c_int = 13;

    /// `struct proc_bsdshortinfo` (64 bytes, as the macOS arm pins it).
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(crate) struct ShortInfo {
        pub(crate) pid: u32,
        pub(crate) ppid: u32,
        pub(crate) pgid: u32,
        pub(crate) status: u32,
        pub(crate) comm: [u8; 16],
        pub(crate) flags: u32,
        pub(crate) uid: u32,
        gid: u32,
        ruid: u32,
        rgid: u32,
        svuid: u32,
        svgid: u32,
        rfu: u32,
    }
    const _: () = assert!(std::mem::size_of::<ShortInfo>() == 64);
    const _: () = assert!(std::mem::size_of::<libc::rusage_info_v4>() == 296);

    /// The Mach timebase `(numer, denom)`, read once.
    pub(crate) fn timebase() -> (u32, u32) {
        static TB: OnceLock<(u32, u32)> = OnceLock::new();
        *TB.get_or_init(|| {
            let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
            // SAFETY: `info` is a writable `mach_timebase_info`.
            let rc = unsafe { libc::mach_timebase_info(&raw mut info) };
            if rc == 0 && info.denom != 0 {
                (info.numer, info.denom)
            } else {
                (1, 1)
            }
        })
    }

    /// `pid`'s short BSD info (readable for every process), whole or `None`.
    pub(crate) fn short_info(pid: c_int) -> Option<ShortInfo> {
        // SAFETY: plain integers and bytes; all-zero is valid.
        let mut info: ShortInfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<ShortInfo>() as c_int;
        // SAFETY: `info` is `size` writable bytes of the flavor's layout.
        let n = unsafe {
            libc::proc_pidinfo(pid, PROC_PIDT_SHORTBSDINFO, 0, (&raw mut info).cast(), size)
        };
        (n == size).then_some(info)
    }

    /// `pid`'s `RUSAGE_INFO_V4` (the user's own processes), or `None`.
    pub(crate) fn rusage(pid: c_int) -> Option<libc::rusage_info_v4> {
        // SAFETY: `rusage_info_v4` is plain integers; all-zero is valid.
        let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a writable `rusage_info_v4` and the flavor names
        // exactly that layout.
        let rc = unsafe {
            libc::proc_pid_rusage(
                pid,
                libc::RUSAGE_INFO_V4,
                (&raw mut info).cast::<libc::rusage_info_t>(),
            )
        };
        (rc == 0).then_some(info)
    }

    pub(super) fn processes() -> Option<Sweep> {
        // SAFETY: a null buffer asks for the count only.
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        let count = usize::try_from(count).ok()?;
        let mut pids = vec![0i32; (count + 64).min(MAX_PROCESSES + 1)];
        let bytes = c_int::try_from(pids.len() * std::mem::size_of::<i32>()).ok()?;
        // SAFETY: `pids` is `bytes` writable bytes.
        let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        let n = usize::try_from(n).ok()?.min(pids.len());
        pids.truncate(n);
        let truncated = pids.len() > MAX_PROCESSES;
        pids.truncate(MAX_PROCESSES);
        let (numer, denom) = timebase();
        let mut procs = Vec::with_capacity(pids.len());
        for pid in pids {
            if pid <= 0 {
                continue;
            }
            let Some(info) = short_info(pid) else {
                // A zombie answers no `proc_pidinfo` flavor; the sysctl does.
                if let Some(k) = crate::jobs::macos_kinfo(pid) {
                    procs.push(ProcessReading {
                        pid: pid as u32,
                        ppid: k.ppid,
                        cpu_ns: None,
                        footprint: None,
                        disk_read: None,
                        disk_written: None,
                        name: k.comm,
                        pgid: k.pgid,
                        uid: Some(k.uid),
                        state: RunState::from_bsd(k.stat),
                        session_leader: Some(k.session_leader),
                        has_tty: Some(k.tty.is_some()),
                    });
                }
                continue;
            };
            let usage = rusage(pid);
            // The short info's flags never carry `PROC_FLAG_SLEADER` (xnu
            // sets it only in the full `PROC_PIDTBSDINFO`, which is refused
            // for another user's process): the session is asked for, and
            // `getsid` answers for any process.
            // SAFETY: a plain syscall on an integer.
            let sid = unsafe { libc::getsid(pid) };
            let end = info.comm.iter().position(|b| *b == 0).unwrap_or(16);
            procs.push(ProcessReading {
                pid: pid as u32,
                ppid: info.ppid,
                cpu_ns: usage.map(|u| {
                    ticks_to_ns(
                        u.ri_user_time.saturating_add(u.ri_system_time),
                        numer,
                        denom,
                    )
                }),
                footprint: usage.map(|u| u.ri_phys_footprint),
                disk_read: usage.map(|u| u.ri_diskio_bytesread),
                disk_written: usage.map(|u| u.ri_diskio_byteswritten),
                name: String::from_utf8_lossy(&info.comm[..end]).into_owned(),
                pgid: info.pgid,
                uid: Some(info.uid),
                state: RunState::from_bsd(info.status),
                session_leader: (sid > 0).then_some(sid == pid),
                has_tty: Some(info.flags & PROC_FLAG_CONTROLT != 0),
            });
        }
        Some(Sweep { procs, truncated })
    }
}

#[cfg(target_os = "linux")]
mod imp {
    pub(super) fn processes() -> Option<super::Sweep> {
        // SAFETY: `sysconf` reads a constant.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        let hz = u64::try_from(hz).ok().filter(|h| *h > 0).unwrap_or(100);
        // SAFETY: as above.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page = u64::try_from(page).ok().filter(|p| *p > 0).unwrap_or(4096);
        crate::linux::Root::system().processes(hz, page)
    }
}

#[cfg(all(windows, target_pointer_width = "64"))]
mod imp {
    pub(super) fn processes() -> Option<super::Sweep> {
        crate::win::processes()
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    all(windows, target_pointer_width = "64")
)))]
mod imp {
    pub(super) fn processes() -> Option<super::Sweep> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statm_gives_the_resident_pages() {
        assert_eq!(parse_statm_resident("2048 512 100 1 0 300 0"), Some(512));
        assert_eq!(parse_statm_resident("2048"), None);
    }

    #[test]
    fn mach_ticks_scale_by_the_timebase() {
        assert_eq!(ticks_to_ns(3, 125, 3), 125);
        assert_eq!(ticks_to_ns(7, 1, 1), 7);
        assert_eq!(ticks_to_ns(7, 1, 0), 7);
        assert_eq!(ticks_to_ns(u64::MAX, 125, 3), u64::MAX);
    }

    // The Windows sweep exists on 64-bit Windows only (32-bit answers
    // `None`), so the test runs where a sweep is promised.
    #[cfg(any(
        target_os = "macos",
        target_os = "linux",
        all(windows, target_pointer_width = "64")
    ))]
    #[test]
    fn the_sweep_finds_this_process_and_its_parent() {
        let sweep = processes().expect("the platform has a sweep");
        let me = std::process::id();
        let mine = sweep
            .procs
            .iter()
            .find(|p| p.pid == me)
            .expect("this process is in the sweep");
        assert!(mine.ppid > 0, "{mine:?}");
        assert!(mine.cpu_ns.is_some_and(|ns| ns > 0), "{mine:?}");
        assert!(mine.footprint.is_some_and(|b| b > 0), "{mine:?}");
        if cfg!(target_os = "macos") {
            assert!(
                mine.disk_read.is_some() && mine.disk_written.is_some(),
                "{mine:?}"
            );
        }
        assert!(!mine.name.is_empty());
        assert!(sweep.procs.iter().any(|p| p.pid == mine.ppid));
    }
}
