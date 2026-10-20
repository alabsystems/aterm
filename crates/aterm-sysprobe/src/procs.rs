// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The process sweep: every process's pid, parent, CPU time, footprint and
//! short name, read in one pass for the system-awareness stats
//! (`docs/DESIGN-system-awareness-2026-10-16.md`), which turn two sweeps into
//! per-process rates and walk the parent links to tell aterm's processes, and
//! each tab's, from the rest.
//!
//! * **macOS** — `proc_listallpids`, then per pid
//!   `proc_pidinfo(PROC_PIDT_SHORTBSDINFO)` (parent and `p_comm`, readable for
//!   every process) and `proc_pid_rusage(RUSAGE_INFO_V4)` (CPU time and
//!   `ri_phys_footprint`, readable for the user's own processes; another
//!   user's answer `None`). `ri_user_time` and `ri_system_time` count Mach
//!   absolute-time ticks, not nanoseconds, on Apple Silicon (the timebase is
//!   125/3 there and 1/1 on Intel), so they are scaled by
//!   `mach_timebase_info`.
//! * **Linux** — `/proc/<pid>/stat` (parent, `comm`, `utime + stime` in clock
//!   ticks) and `/proc/<pid>/statm` (resident pages, the nearest the kernel
//!   offers to a footprint), each through [`crate::read_bounded`].
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
    /// Its physical footprint in bytes (macOS `ri_phys_footprint`, Linux
    /// resident set); `None` where refused.
    pub footprint: Option<u64>,
    /// Its short name (`p_comm`, 16 bytes at most on macOS).
    pub name: String,
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

/// `/proc/<pid>/stat`'s parent, name and `utime + stime` (clock ticks): the
/// name is the text between the first `(` and the LAST `)`, since `comm` may
/// itself hold parentheses and spaces.
#[must_use]
pub fn parse_proc_stat(text: &str) -> Option<(u32, String, u64)> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let name = text.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
    // After the name: state(0) ppid(1) … utime(11) stime(12).
    let ppid = rest.get(1)?.parse().ok()?;
    let utime: u64 = rest.get(11)?.parse().ok()?;
    let stime: u64 = rest.get(12)?.parse().ok()?;
    Some((ppid, name, utime.saturating_add(stime)))
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
mod imp {
    use std::ffi::c_int;
    use std::sync::OnceLock;

    use super::{MAX_PROCESSES, ProcessReading, Sweep, ticks_to_ns};

    /// `PROC_PIDT_SHORTBSDINFO` (`sys/proc_info.h`), as the crate's macOS arm
    /// declares it (the reference `libc` does not export it).
    const PROC_PIDT_SHORTBSDINFO: c_int = 13;

    /// `struct proc_bsdshortinfo` (64 bytes, as the macOS arm pins it).
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ShortInfo {
        pid: u32,
        ppid: u32,
        pgid: u32,
        status: u32,
        comm: [u8; 16],
        flags: u32,
        uid: u32,
        gid: u32,
        ruid: u32,
        rgid: u32,
        svuid: u32,
        svgid: u32,
        rfu: u32,
    }
    const _: () = assert!(std::mem::size_of::<ShortInfo>() == 64);
    const _: () = assert!(std::mem::size_of::<libc::rusage_info_v4>() == 296);

    fn timebase() -> (u32, u32) {
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

    fn short_info(pid: c_int) -> Option<ShortInfo> {
        // SAFETY: plain integers and bytes; all-zero is valid.
        let mut info: ShortInfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<ShortInfo>() as c_int;
        // SAFETY: `info` is `size` writable bytes of the flavor's layout.
        let n = unsafe {
            libc::proc_pidinfo(pid, PROC_PIDT_SHORTBSDINFO, 0, (&raw mut info).cast(), size)
        };
        (n == size).then_some(info)
    }

    fn rusage(pid: c_int) -> Option<libc::rusage_info_v4> {
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
                continue;
            };
            let usage = rusage(pid);
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
                name: String::from_utf8_lossy(&info.comm[..end]).into_owned(),
            });
        }
        Some(Sweep { procs, truncated })
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::PathBuf;

    use super::{MAX_PROCESSES, ProcessReading, Sweep, parse_proc_stat, parse_statm_resident};

    pub(super) fn processes() -> Option<Sweep> {
        // SAFETY: `sysconf` reads a constant.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        let hz = u64::try_from(hz).ok().filter(|h| *h > 0).unwrap_or(100);
        // SAFETY: as above.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page = u64::try_from(page).ok().filter(|p| *p > 0).unwrap_or(4096);
        let mut procs = Vec::new();
        let mut truncated = false;
        for entry in std::fs::read_dir("/proc").ok()?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            if procs.len() >= MAX_PROCESSES {
                truncated = true;
                break;
            }
            let dir = PathBuf::from("/proc").join(pid.to_string());
            let Some((ppid, name, ticks)) =
                crate::read_bounded(&dir.join("stat")).and_then(|t| parse_proc_stat(&t))
            else {
                continue;
            };
            let footprint = crate::read_bounded(&dir.join("statm"))
                .and_then(|t| parse_statm_resident(&t))
                .map(|pages| pages.saturating_mul(page));
            procs.push(ProcessReading {
                pid,
                ppid,
                cpu_ns: Some(ticks.saturating_mul(1_000_000_000) / hz),
                footprint,
                name,
            });
        }
        Some(Sweep { procs, truncated })
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    pub(super) fn processes() -> Option<super::Sweep> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proc_stat_line_with_a_parenthesised_name_parses() {
        let line = "4242 (tmux: server (1)) S 1 4242 4242 0 -1 4194560 1209 0 0 0 \
                    37 12 0 0 20 0 1 0 12345 0 0";
        let (ppid, name, ticks) = parse_proc_stat(line).unwrap();
        assert_eq!((ppid, name.as_str(), ticks), (1, "tmux: server (1)", 49));
        assert_eq!(parse_proc_stat("garbage"), None);
    }

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

    #[cfg(any(target_os = "macos", target_os = "linux"))]
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
        assert!(!mine.name.is_empty());
        assert!(sweep.procs.iter().any(|p| p.pid == mine.ppid));
    }
}
