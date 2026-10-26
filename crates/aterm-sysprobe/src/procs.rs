// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The process sweep: every process's pid, parent, CPU time, footprint and short name, read in one
//! pass for the system-awareness stats (`docs/DESIGN-system-awareness-2026-10-16.md`).

/// The sweep stops here.
pub const MAX_PROCESSES: usize = 16_384;

/// One process in a sweep.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessReading {
    /// Its pid.
    pub pid: u32,
    /// Kernel start identity: Mach absolute ticks on macOS, boot nanoseconds on Linux, creation FILETIME on Windows.
    pub start: Option<u64>,
    /// Its parent's pid (0 when the kernel names none).
    pub ppid: u32,
    /// User plus system CPU time since it started, nanoseconds; `None` where the OS refuses (another user's process on macOS). An unreaped zombie answers its final time (macOS).
    pub cpu_ns: Option<u64>,
    /// User plus system CPU time of the children it has reaped, nanoseconds (macOS
    /// `ri_child_user_time + ri_child_system_time`): at each reap the kernel adds the child's own
    /// time and its reaped children's, so it holds every reaped descendant's work once, and an
    /// unreaped zombie answers its final figure; `None` where refused, and off macOS.
    pub child_cpu_ns: Option<u64>,
    /// Its memory in bytes: macOS `ri_phys_footprint`, Linux `RssAnon + VmSwap`, Windows its private commit; `None` where refused.
    pub footprint: Option<u64>,
    /// Bytes it has read from disk since it started (macOS `ri_diskio_bytesread`: storage reads,
    /// not page-cache hits); `None` where refused, and off macOS.
    pub disk_read: Option<u64>,
    /// Bytes it has written to disk since it started (macOS `ri_diskio_byteswritten`: what reached storage, not every `write(2)`); `None` where refused, and on Linux. An unreaped zombie keeps its final count, its start, its `disk_read` and its CPU times (macOS), though not its footprint.
    pub disk_written: Option<u64>,
    /// Its short name (`p_comm`, 16 bytes at most on macOS; Linux `comm`; Windows the image name less `.exe`).
    pub name: String,
    /// Its process group (0 where unread).
    pub pgid: u32,
    /// Its user; `None` where unread.
    pub uid: Option<u32>,
    /// Its state (`p_stat`; on macOS `Running` covers a process blocked in `read(2)`).
    pub state: crate::jobs::RunState,
    /// Whether it leads its session; `None` where unread.
    pub session_leader: Option<bool>,
    /// Its session's id, its leader's pid (`getsid`; Linux `session`); 0 where unread, for a
    /// zombie on macOS, and on Windows.
    pub sid: u32,
    /// Whether it has a controlling terminal; `None` where unread.
    pub has_tty: Option<bool>,
    /// Whether it ran a program since its fork (macOS `P_EXEC`, Linux `PF_FORKNOEXEC` clear): a
    /// fork that kept its parent's program did not; `None` where unread, and on Windows.
    pub exec: Option<bool>,
    /// Time its threads spent runnable (running or waiting for a core) since it started, nanoseconds (macOS `ri_runnable_time`); `None` where refused or not read.
    pub runnable_ns: Option<u64>,
    /// The largest footprint it has had (macOS `ri_lifetime_max_phys_footprint`); `None` where refused or not read.
    pub peak_footprint: Option<u64>,
    /// When its counters were read (a zombie's: at its second read, after every live process's).
    pub read_at: Option<std::time::Instant>,
    /// Its resource coalition's id (macOS), read only by a sweep asked for it ([`SweepOpts::coalitions`]); `None` otherwise, for a zombie, and off macOS.
    pub coalition: Option<u64>,
    /// Its pid version (macOS `p_idversion`, [`pid_version`]), which a new process and each exec take anew, read with [`Self::coalition`], or where the sweep reads no [`Self::start`] and [`SweepOpts::versions`] asks; `None` otherwise, for a zombie, and off macOS.
    pub version: Option<i32>,
    /// When it started, unix ms: on Linux its boot-clock start against `/proc/uptime` and the wall clock read once a sweep ([`birth_of_boot_start`]), on Windows its creation `FILETIME` ([`birth_of_filetime`]). `None` on macOS, whose [`Self::start`] is Mach absolute time, which stops while the Mac sleeps (the kernel record, [`crate::kproc::record`], gives the wall birth there), and where unread.
    pub birth_ms: Option<i64>,
    /// Whether the kernel reaps its children for it (macOS `P_NOCLDWAIT`: it ignores `SIGCHLD`
    /// or set `SA_NOCLDWAIT`; a fork does not inherit it and an exec clears it), which then adds
    /// each child's time to its [`Self::child_cpu_ns`] twice (measured 2026-10-25: 400.4–401.6 ms
    /// for a 200 ms child). Read, after the sweep's reads, for each process with reaped time and
    /// for each whose usage goes unread that is the parent of one whose usage is read; `None`
    /// otherwise, and off macOS.
    pub reaps_unwaited: Option<bool>,
}

/// What a sweep reads beyond each process's row ([`processes_with`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepOpts {
    /// Each process's resource coalition and pid version ([`ProcessReading::coalition`], [`ProcessReading::version`]): one more `proc_pidinfo` a process on macOS, two more for one whose coalition [`processes_keeping`]'s ids do not hold under that version, nothing elsewhere.
    pub coalitions: bool,
    /// The pid version of each process whose start the sweep cannot read (another user's, on macOS, whose usage the kernel refuses), which is what tells a new process at its pid (or an exec) from the one a reader kept something for: one more `proc_pidinfo` for each such process on macOS, nothing elsewhere. [`Self::coalitions`] reads every process's.
    pub versions: bool,
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
    processes_with(SweepOpts::default())
}

/// Sweep every process, reading what `opts` asks for too; `None` where the platform has no reader.
#[must_use]
pub fn processes_with(opts: SweepOpts) -> Option<Sweep> {
    processes_keeping(opts, &mut crate::coalitions::CoalitionIds::default())
}

/// [`processes_with`], each process's coalition taken from `ids` where they hold it under the
/// process's pid version, else read and kept there; `ids` then holds the swept processes' alone.
#[must_use]
pub fn processes_keeping(
    opts: SweepOpts,
    ids: &mut crate::coalitions::CoalitionIds,
) -> Option<Sweep> {
    imp::processes(opts, ids)
}

/// `pid`'s pid version (`p_idversion`, `PROC_PIDUNIQIDENTIFIERINFO`, which any process may read
/// of any other): a fork and each exec take a new one, so it moves where the pid, the kernel
/// start and often the short name stay. `None` when the kernel does not say, and off macOS.
#[must_use]
pub fn pid_version(pid: u32) -> Option<i32> {
    #[cfg(target_os = "macos")]
    {
        imp::pid_version(std::ffi::c_int::try_from(pid).ok().filter(|p| *p > 0)?)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

/// `/proc/<pid>/statm`'s resident pages (its second field).
#[must_use]
pub fn parse_statm_resident(text: &str) -> Option<u64> {
    text.split_whitespace().nth(1)?.parse().ok()
}

/// A start on the boot clock (`start_ns` since boot, sleep included, as Linux counts a process's start) as unix ms, against the boot clock and the wall clock read together (`boot_now_ns`, `unix_now_ms`). A start after that moment (a process born while the sweep ran) is that moment; `None` before the epoch.
#[must_use]
pub fn birth_of_boot_start(start_ns: u64, boot_now_ns: u64, unix_now_ms: i64) -> Option<i64> {
    let age_ms = i64::try_from(boot_now_ns.saturating_sub(start_ns) / 1_000_000).ok()?;
    unix_now_ms.checked_sub(age_ms).filter(|ms| *ms >= 0)
}

/// A Windows creation `FILETIME` (100 ns ticks since 1601-01-01 UTC) as unix ms; `None` before the epoch.
#[must_use]
pub fn birth_of_filetime(ticks: u64) -> Option<i64> {
    const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
    i64::try_from(ticks.checked_sub(UNIX_EPOCH_TICKS)? / 10_000).ok()
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

/// The timebase `(numer, denom)` of a Mach clock that ticks `hz` times a second, reduced: `ticks * numer / denom` is nanoseconds (24 MHz is 125/3, 1 GHz is 1/1).
#[must_use]
pub fn timebase_of_frequency(hz: u64) -> Option<(u32, u32)> {
    const NS_PER_S: u64 = 1_000_000_000;
    if hz == 0 {
        return None;
    }
    let (mut a, mut b) = (NS_PER_S, hz);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    Some((
        u32::try_from(NS_PER_S / a).ok()?,
        u32::try_from(hz / a).ok()?,
    ))
}

/// The kernel's own Mach timebase `(numer, denom)`: what turns the tick counts the kernel records for a task (`proc_pid_rusage`'s CPU and runnable times, `PROC_PIDTASKINFO`'s `pti_total_*`) into nanoseconds.
#[cfg(target_os = "macos")]
#[must_use]
pub fn kernel_timebase() -> Option<(u32, u32)> {
    static TB: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    *TB.get_or_init(|| {
        crate::macos::sysctl_int(c"hw.tbfrequency")
            .and_then(|hz| u64::try_from(hz).ok())
            .and_then(timebase_of_frequency)
            .or_else(|| {
                let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
                // SAFETY: `info` is a writable `mach_timebase_info`.
                let rc = unsafe { libc::mach_timebase_info(&raw mut info) };
                (rc == 0 && info.denom != 0).then_some((info.numer, info.denom))
            })
    })
}

#[cfg(target_os = "macos")]
pub(crate) mod imp {
    use std::ffi::c_int;

    use super::{MAX_PROCESSES, ProcessReading, Sweep, SweepOpts, ticks_to_ns};
    use crate::jobs::RunState;
    use crate::jobs::darwin::{PROC_FLAG_CONTROLT, PROC_FLAG_EXEC};

    /// `PROC_PIDT_SHORTBSDINFO` (`sys/proc_info.h`), which the `libc` crate does not export.
    const PROC_PIDT_SHORTBSDINFO: c_int = 13;

    /// `struct proc_bsdshortinfo` (64 bytes).
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

    /// The kernel's Mach timebase `(numer, denom)` ([`super::kernel_timebase`]), 1/1 when it cannot be read.
    pub(crate) fn timebase() -> (u32, u32) {
        super::kernel_timebase().unwrap_or((1, 1))
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

    /// `PROC_PIDUNIQIDENTIFIERINFO` (`sys/proc_info.h`).
    const PROC_PIDUNIQIDENTIFIERINFO: c_int = 17;

    /// `struct proc_uniqidentifierinfo` (56 bytes).
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct UniqIdentifierInfo {
        uuid: [u8; 16],
        uniqueid: u64,
        puniqueid: u64,
        idversion: i32,
        orig_ppidversion: i32,
        reserve2: u64,
        reserve3: u64,
    }
    const _: () = assert!(std::mem::size_of::<UniqIdentifierInfo>() == 56);

    /// `pid`'s `p_idversion` ([`super::pid_version`]).
    pub(crate) fn pid_version(pid: c_int) -> Option<i32> {
        // SAFETY: plain integers and bytes; all-zero is valid.
        let mut info: UniqIdentifierInfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<UniqIdentifierInfo>() as c_int;
        // SAFETY: `info` is `size` writable bytes of the flavor's layout.
        let n = unsafe {
            libc::proc_pidinfo(
                pid,
                PROC_PIDUNIQIDENTIFIERINFO,
                0,
                (&raw mut info).cast(),
                size,
            )
        };
        (n == size).then_some(info.idversion)
    }

    /// `pid`'s `RUSAGE_INFO_V4` (the user's own processes), or `None`.
    pub(crate) fn rusage(pid: c_int) -> Option<libc::rusage_info_v4> {
        // SAFETY: `rusage_info_v4` is plain integers; all-zero is valid.
        let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a writable `rusage_info_v4` and the flavor names exactly that layout.
        let rc = unsafe {
            libc::proc_pid_rusage(
                pid,
                libc::RUSAGE_INFO_V4,
                (&raw mut info).cast::<libc::rusage_info_t>(),
            )
        };
        (rc == 0).then_some(info)
    }

    /// `u`'s own user plus system time, ns at the timebase `numer/denom`.
    fn cpu_of(u: libc::rusage_info_v4, numer: u32, denom: u32) -> u64 {
        ticks_to_ns(
            u.ri_user_time.saturating_add(u.ri_system_time),
            numer,
            denom,
        )
    }

    /// `u`'s reaped children's user plus system time, ns at the timebase `numer/denom`.
    fn child_cpu_of(u: libc::rusage_info_v4, numer: u32, denom: u32) -> u64 {
        ticks_to_ns(
            u.ri_child_user_time.saturating_add(u.ri_child_system_time),
            numer,
            denom,
        )
    }

    pub(super) fn processes(
        opts: SweepOpts,
        ids: &mut crate::coalitions::CoalitionIds,
    ) -> Option<Sweep> {
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
        if opts.coalitions {
            ids.begin_sweep();
        }
        // The kernel lists every live process, newest first, and then its zombies. A child is
        // always newer than the process that forked it, so reading the live list from its end
        // reads each parent before its children, and reading the zombies after every live
        // process reads each zombie after its parent: a child reaped while the sweep runs is then
        // either read (its parent's reaped-children time, read before the reap, does not hold it
        // yet) or gone (its parent reaped it after its read), never counted in both (SA §7.5's
        // reap credit). A process that answers no `proc_pidinfo` flavor (a zombie, or one
        // exiting) waits for that second pass. The live rows keep the kernel's order, and the
        // zombies follow them, as the kernel lists them.
        let mut dead = Vec::new();
        for pid in pids.into_iter().rev() {
            if pid <= 0 {
                continue;
            }
            let before = rusage(pid);
            let Some(info) = short_info(pid) else {
                dead.push((pid, before));
                continue;
            };
            let (coalition, version) = if opts.coalitions {
                ids.read(
                    pid as u32,
                    || pid_version(pid),
                    || crate::coalitions::coalition_of(pid as u32),
                )
            } else if opts.versions && before.is_none() {
                (None, pid_version(pid))
            } else {
                (None, None)
            };
            let after = rusage(pid);
            let usage = match (before, after) {
                (Some(before), Some(after))
                    if before.ri_proc_start_abstime == after.ri_proc_start_abstime =>
                {
                    Some(after)
                }
                (None, None) => None,
                _ => continue,
            };
            let read_at = Some(std::time::Instant::now());
            // SAFETY: a plain syscall on an integer.
            let sid = unsafe { libc::getsid(pid) };
            let end = info.comm.iter().position(|b| *b == 0).unwrap_or(16);
            procs.push(ProcessReading {
                pid: pid as u32,
                start: usage.map(|u| u.ri_proc_start_abstime),
                ppid: info.ppid,
                cpu_ns: usage.map(|u| cpu_of(u, numer, denom)),
                child_cpu_ns: usage.map(|u| child_cpu_of(u, numer, denom)),
                footprint: usage.map(|u| u.ri_phys_footprint),
                disk_read: usage.map(|u| u.ri_diskio_bytesread),
                disk_written: usage.map(|u| u.ri_diskio_byteswritten),
                name: String::from_utf8_lossy(&info.comm[..end]).into_owned(),
                pgid: info.pgid,
                uid: Some(info.uid),
                state: RunState::from_bsd(info.status),
                session_leader: (sid > 0).then_some(sid == pid),
                sid: u32::try_from(sid).unwrap_or(0),
                has_tty: Some(info.flags & PROC_FLAG_CONTROLT != 0),
                exec: Some(info.flags & PROC_FLAG_EXEC != 0),
                runnable_ns: usage.map(|u| ticks_to_ns(u.ri_runnable_time, numer, denom)),
                peak_footprint: usage.map(|u| u.ri_lifetime_max_phys_footprint),
                read_at,
                coalition,
                version,
                birth_ms: None,
                reaps_unwaited: None,
            });
        }
        procs.reverse();
        // A zombie answers no `proc_pidinfo` flavor; the sysctl does, and `proc_pid_rusage` its
        // final usage (its CPU times included: xnu reads a zombie's usage off its exit record).
        // Its usage read again now, after every live process, under the start the first read
        // named, is kept: no reap between the reads moved it to its parent, and a pid reused
        // between them lends none of it.
        for (pid, before) in dead.into_iter().rev() {
            let Some(k) = crate::jobs::macos_kinfo(pid) else {
                continue;
            };
            let last = before
                .zip(rusage(pid))
                .filter(|(b, a)| a.ri_proc_start_abstime == b.ri_proc_start_abstime)
                .map(|(_, a)| a);
            procs.push(ProcessReading {
                pid: pid as u32,
                start: last.map(|u| u.ri_proc_start_abstime),
                ppid: k.ppid,
                cpu_ns: last.map(|u| cpu_of(u, numer, denom)),
                child_cpu_ns: last.map(|u| child_cpu_of(u, numer, denom)),
                footprint: None,
                disk_read: last.map(|u| u.ri_diskio_bytesread),
                disk_written: last.map(|u| u.ri_diskio_byteswritten),
                name: k.comm,
                pgid: k.pgid,
                uid: Some(k.uid),
                state: RunState::from_bsd(k.stat),
                session_leader: Some(k.session_leader),
                has_tty: Some(k.tty.is_some()),
                exec: Some(k.exec),
                read_at: last.map(|_| std::time::Instant::now()),
                reaps_unwaited: Some(k.reaps_unwaited),
                ..ProcessReading::default()
            });
        }
        if opts.coalitions {
            ids.end_sweep();
        }
        read_unwaited(&mut procs);
        Some(Sweep { procs, truncated })
    }

    /// Past this many processes to read [`ProcessReading::reaps_unwaited`] of, one read of the
    /// whole table is cheaper than a `kinfo_proc` each: one cost 6.7 µs of CPU and the table's
    /// 749 records 96–115 µs (m22, 2026-10-25), as for births (the host's `BIRTHS_AT_ONCE`).
    const UNWAITED_AT_ONCE: usize = 12;

    /// Reads [`ProcessReading::reaps_unwaited`] of each process whose reaped time is above 0
    /// (only a reap moves it, and only one that ran with the flag set counts twice) and of each
    /// whose usage goes unread that parents one whose usage is read (another user's `sudo`,
    /// which may reap a member and pass its time up later): from its `kinfo_proc`, or from the
    /// whole table past [`UNWAITED_AT_ONCE`]; one whose record names another parent is a new
    /// process at the pid and stays unread. A zombie's came with its record.
    fn read_unwaited(procs: &mut [ProcessReading]) {
        let parents: std::collections::HashSet<u32> = procs
            .iter()
            .filter(|p| p.cpu_ns.is_some())
            .map(|p| p.ppid)
            .collect();
        let wanted: Vec<usize> = procs
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.reaps_unwaited.is_none()
                    && (p.child_cpu_ns.is_some_and(|c| c > 0)
                        || (p.cpu_ns.is_none() && p.pid > 1 && parents.contains(&p.pid)))
            })
            .map(|(i, _)| i)
            .collect();
        let table = (wanted.len() > UNWAITED_AT_ONCE)
            .then(crate::kproc::reaps_unwaited_all)
            .flatten()
            .map(|all| {
                all.into_iter()
                    .map(|(pid, ppid, flag)| (pid, (ppid, flag)))
                    .collect::<std::collections::HashMap<u32, (u32, bool)>>()
            });
        for i in wanted {
            let p = &mut procs[i];
            p.reaps_unwaited = match &table {
                Some(all) => all
                    .get(&p.pid)
                    .filter(|(ppid, _)| *ppid == p.ppid)
                    .map(|(_, flag)| *flag),
                None => c_int::try_from(p.pid)
                    .ok()
                    .and_then(crate::jobs::macos_kinfo)
                    .filter(|k| k.ppid == p.ppid)
                    .map(|k| k.reaps_unwaited),
            };
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    pub(super) fn processes(
        _opts: super::SweepOpts,
        _ids: &mut crate::coalitions::CoalitionIds,
    ) -> Option<super::Sweep> {
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
    pub(super) fn processes(
        _opts: super::SweepOpts,
        _ids: &mut crate::coalitions::CoalitionIds,
    ) -> Option<super::Sweep> {
        crate::win::processes()
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    all(windows, target_pointer_width = "64")
)))]
mod imp {
    pub(super) fn processes(
        _opts: super::SweepOpts,
        _ids: &mut crate::coalitions::CoalitionIds,
    ) -> Option<super::Sweep> {
        None
    }
}

/// Test support (macOS): a CPU-time reader checked against the kernel's own account of the same CPU, for the sweep's tests here and the job probe's (`jobs/tests.rs`).
#[cfg(all(test, target_os = "macos"))]
pub(crate) mod stopped_spinner {
    use std::time::{Duration, Instant};

    /// `struct rusage` (`<sys/resource.h>`): the two times, then fourteen `long` counters these tests do not read; 144 bytes on Darwin.
    #[repr(C)]
    struct Rusage {
        utime: libc::timeval,
        stime: libc::timeval,
        _counters: [libc::c_long; 14],
    }
    const _: () = assert!(std::mem::size_of::<Rusage>() == 144);
    unsafe extern "C" {
        /// Declared here: aterm-libc does not export it.
        fn wait4(
            pid: libc::pid_t,
            status: *mut libc::c_int,
            options: libc::c_int,
            usage: *mut Rusage,
        ) -> libc::pid_t;
    }

    /// Keeps the spinner owned until a successful manual reap.
    struct OwnedSpinner(Option<aterm_uds::spawnfd::Child>);

    impl OwnedSpinner {
        fn forget_reaped(&mut self) {
            if let Some(child) = self.0.take() {
                std::mem::forget(child);
            }
        }
    }

    impl Drop for OwnedSpinner {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut()
                && matches!(child.try_wait(), Ok(None))
                && child.kill().is_ok()
            {
                let _ = child.wait();
            }
        }
    }

    /// Spins a `/bin/sh` child until `ready(pid)`, stops it, hands its pid to `read` while it is stopped, then kills it and reaps it with `wait4`.
    pub(crate) fn read<T>(
        mut ready: impl FnMut(u32) -> bool,
        read: impl FnOnce(u32) -> T,
    ) -> (T, u64) {
        let micros = |t: libc::timeval| {
            u64::try_from(t.tv_sec).expect("non-negative") * 1_000_000
                + u64::try_from(t.tv_usec).expect("non-negative")
        };
        let child = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", "while :; do :; done"])
            .spawn()
            .expect("spawn a spinning shell");
        let id = child.id();
        let mut child = OwnedSpinner(Some(child));
        let pid = libc::pid_t::try_from(id).expect("pid");
        let deadline = Instant::now() + Duration::from_secs(60);
        while !ready(id) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        // SAFETY: `kill` takes no pointers; `pid` is this test's unreaped child.
        let stop = unsafe { libc::kill(pid, libc::SIGSTOP) };
        let mut stopped: libc::c_int = 0;
        // SAFETY: `stopped` is a live out-param; `pid` is this test's own child, and nothing else in this process waits on it.
        let waited = unsafe { libc::waitpid(pid, &mut stopped, libc::WUNTRACED) };
        if waited == pid && (libc::WIFEXITED(stopped) || libc::WIFSIGNALED(stopped)) {
            child.forget_reaped();
        }
        assert!(
            libc::WIFSTOPPED(stopped),
            "read while stopped: {stopped:#x}"
        );
        let answer = read(id);
        // SAFETY: as above; a stopped child is still this test's to kill.
        let kill = unsafe { libc::kill(pid, libc::SIGKILL) };
        let mut status: libc::c_int = 0;
        let mut usage = std::mem::MaybeUninit::<Rusage>::zeroed();
        // SAFETY: live out-params of the declared layouts; a successful reap disarms the handle below before any assertion or drop.
        let reaped = unsafe { wait4(pid, &mut status, 0, usage.as_mut_ptr()) };
        if reaped == pid {
            child.forget_reaped();
        }
        assert_eq!(
            (stop, waited, kill, reaped),
            (0, pid, 0, pid),
            "stop, wait for the stop, kill, reap"
        );
        // SAFETY: zeroed, then filled by the successful `wait4` above; every field is a plain integer.
        let usage = unsafe { usage.assume_init() };
        let kernel = (micros(usage.utime) + micros(usage.stime)) * 1_000;
        assert!(kernel > 0, "the child ran");
        (answer, kernel)
    }

    /// Whether `read` is within 2x of `kernel`, either way: far inside a Mach tick factor (125/3 on Apple silicon).
    pub(crate) fn within_2x(read: u64, kernel: u64) -> bool {
        read <= kernel.saturating_mul(2) && read.saturating_mul(2) >= kernel
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

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn sweep_start_identity_matches_the_control_peers_kernel_record() {
        let pid = std::process::id();
        let expected = aterm_uds::process::read_process_start(i32::try_from(pid).unwrap())
            .expect("own process has a kernel start record");
        let sweep = processes().expect("native process sweep");
        let me = sweep
            .procs
            .iter()
            .find(|process| process.pid == pid)
            .unwrap();
        assert_eq!(me.start, Some(expected.boot.0));
    }

    #[test]
    fn a_boot_clock_start_is_its_wall_birth() {
        const NOW_MS: i64 = 1_790_000_000_000;
        let boot = 3_600 * 1_000_000_000;
        assert_eq!(
            birth_of_boot_start(boot - 47 * 60 * 1_000_000_000, boot, NOW_MS),
            Some(NOW_MS - 47 * 60_000)
        );
        assert_eq!(
            birth_of_boot_start(boot + 5_000_000, boot, NOW_MS),
            Some(NOW_MS),
            "born while the sweep ran"
        );
        assert_eq!(
            birth_of_boot_start(0, u64::MAX, NOW_MS),
            None,
            "before the epoch"
        );
        assert_eq!(
            birth_of_boot_start(0, boot, i64::MIN),
            None,
            "past the range"
        );
    }

    #[test]
    fn a_filetime_is_its_unix_birth() {
        assert_eq!(birth_of_filetime(116_444_736_000_000_000), Some(0));
        assert_eq!(
            birth_of_filetime(116_444_736_000_000_000 + 17_900_000_000_000_000 + 12_345),
            Some(1_790_000_000_001)
        );
        assert_eq!(birth_of_filetime(116_444_735_999_999_999), None);
        assert_eq!(birth_of_filetime(u64::MAX), Some(1_833_029_933_770_955));
    }

    /// Live on Linux: this process's swept birth is `ps`'s reckoning of it (`/proc/stat`'s
    /// `btime` plus its start in clock ticks), within `btime`'s whole second.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_linux_sweep_reads_this_processs_birth_as_ps_reckons_it() {
        let me = std::process::id();
        let swept = processes()
            .expect("a sweep")
            .procs
            .into_iter()
            .find(|p| p.pid == me)
            .and_then(|p| p.birth_ms)
            .expect("this process's birth");
        let stat = std::fs::read_to_string("/proc/stat").expect("/proc/stat");
        let btime: i64 = stat
            .lines()
            .find_map(|l| l.strip_prefix("btime "))
            .and_then(|b| b.trim().parse().ok())
            .expect("btime");
        let own = std::fs::read_to_string(format!("/proc/{me}/stat")).expect("its stat");
        let start = crate::jobs::linux::parse_stat(&own)
            .expect("its start")
            .start;
        // SAFETY: `sysconf` reads a constant.
        let hz = i64::from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).max(1);
        let ps_ms = btime * 1000 + i64::try_from(start).expect("ticks") * 1000 / hz;
        // `btime` is a whole second and `/proc/uptime` a centisecond, each cut short, so the
        // two reckonings part by up to 1.01 s plus the milliseconds each division drops.
        assert!(
            (swept - ps_ms).abs() <= 1_100,
            "swept {swept}, ps reckons {ps_ms}"
        );
    }

    /// The macOS sweep reads no wall birth: its start is Mach absolute time, which stops while
    /// the Mac sleeps, so an age from it would read hours young after a night's sleep.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_sweep_leaves_the_birth_to_the_kernel_record() {
        let sweep = processes().expect("a sweep");
        assert!(sweep.procs.iter().all(|p| p.birth_ms.is_none()));
    }

    #[test]
    fn a_tick_frequency_reduces_to_its_timebase() {
        assert_eq!(timebase_of_frequency(24_000_000), Some((125, 3)));
        assert_eq!(timebase_of_frequency(1_000_000_000), Some((1, 1)));
        assert_eq!(timebase_of_frequency(19_200_000), Some((625, 12)));
        assert_eq!(timebase_of_frequency(0), None);
        assert_eq!(timebase_of_frequency(u64::MAX), None, "terms past u32");
    }

    /// The sweep's CPU time is the kernel's, in nanoseconds, natively or under Rosetta: a stopped
    /// spinning child reads within 2x of what `wait4` reports for it.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_sweep_reads_a_stopped_childs_cpu_time_as_wait4_counts_it() {
        use super::stopped_spinner::{read, within_2x};
        let swept = |pid: u32| {
            processes()
                .and_then(|s| s.procs.into_iter().find(|p| p.pid == pid))
                .and_then(|p| p.cpu_ns)
        };
        let (swept_ns, kernel) = read(|pid| swept(pid).is_some_and(|ns| ns >= 50_000_000), swept);
        let swept_ns = swept_ns.expect("the sweep reads its own child");
        assert!(
            within_2x(swept_ns, kernel),
            "the sweep read {swept_ns} ns; the reap's rusage for the same stopped child is \
             {kernel} ns (timebase {:?})",
            kernel_timebase()
        );
    }

    /// A sweep asked for coalitions reads each process's, and its pid version, as the one-process readers do, another user's too (launchd's, root's); a plain sweep reads neither.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_sweep_reads_coalitions_only_when_asked() {
        let sweep = processes_with(SweepOpts {
            coalitions: true,
            ..SweepOpts::default()
        })
        .expect("a sweep");
        for pid in [std::process::id(), 1] {
            let row = sweep.procs.iter().find(|p| p.pid == pid).expect("swept");
            let own = crate::coalitions::coalition_of(pid).expect("its coalition");
            assert_eq!(row.coalition, Some(own), "{pid}");
            let version = pid_version(pid).expect("its pid version");
            assert_eq!(row.version, Some(version), "{pid}");
            // The same field as aterm-uds's flavor-17 reader reads it (no SDK header declares it).
            let uds = aterm_uds::process::uniq_identifier(i32::try_from(pid).expect("a pid"))
                .map(|i| i.p_idversion);
            assert_eq!(uds, Some(version), "{pid}");
        }
        assert!(
            processes()
                .expect("a sweep")
                .procs
                .iter()
                .all(|p| p.coalition.is_none() && p.version.is_none())
        );
    }

    /// A sweep asked for versions reads the pid version of each process whose start it cannot
    /// read (launchd's, root's, to a user that is not root) and of no other (this process's,
    /// whose start it reads), and no coalition. NEGATIVE CONTROL: a plain sweep reads no version.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_sweep_asked_for_versions_reads_them_where_it_reads_no_start() {
        let sweep = processes_with(SweepOpts {
            versions: true,
            ..SweepOpts::default()
        })
        .expect("a sweep");
        let row = |pid: u32| sweep.procs.iter().find(|p| p.pid == pid).expect("swept");
        let me = row(std::process::id());
        assert!(me.start.is_some(), "this process's start: {me:?}");
        assert_eq!(me.version, None, "read where the start is not");
        let launchd = row(1);
        let version = pid_version(1).expect("launchd's pid version");
        assert_eq!(
            launchd.version,
            launchd.start.is_none().then_some(version),
            "{launchd:?}"
        );
        assert!(sweep.procs.iter().all(|p| p.coalition.is_none()));
        assert!(
            processes()
                .expect("a sweep")
                .procs
                .iter()
                .all(|p| p.version.is_none()),
            "NEGATIVE CONTROL: a plain sweep"
        );
    }

    /// The exec stand-in's stage: `before` execs this test binary again as `after` on a byte.
    #[cfg(target_os = "macos")]
    const EXEC_STAGE: &str = "ATERM_SYSPROBE_EXEC_STAGE";

    /// [`exec_stand_in`], as `--exact` names it.
    #[cfg(target_os = "macos")]
    const EXEC_STAND_IN: &str = "procs::tests::exec_stand_in";

    /// The hang detector for what must happen (a child's line).
    #[cfg(target_os = "macos")]
    const HANG: std::time::Duration = std::time::Duration::from_secs(60);

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "the child the exec test starts; alone it returns"]
    fn exec_stand_in() {
        use std::io::{Read as _, Write as _};
        let Ok(stage) = std::env::var(EXEC_STAGE) else {
            return;
        };
        println!("\n{stage}");
        std::io::stdout().flush().expect("the stage's proof");
        let mut byte = [0; 1];
        match std::io::stdin().read(&mut byte) {
            Ok(1) if stage == "before" => {
                let failed =
                    aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
                        .args([
                            "--exact",
                            EXEC_STAND_IN,
                            "--ignored",
                            "--nocapture",
                            "--test-threads=1",
                        ])
                        .env(EXEC_STAGE, "after")
                        .exec();
                panic!("the exec: {failed}");
            }
            _ => aterm_uds::process::kill_self_now(),
        }
    }

    /// Start this test binary as the stand-in `test` (as `--exact` names it) with `var` set and
    /// its stdin and stdout piped; the second half waits for one of its lines (the hang detector).
    #[cfg(target_os = "macos")]
    fn stand_in(
        test: &str,
        var: (&str, &std::ffi::OsStr),
    ) -> (aterm_uds::spawnfd::Child, impl Fn(&str)) {
        use aterm_uds::spawnfd::Stdio;
        use std::io::BufRead as _;
        let mut child =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
                .args([
                    "--exact",
                    test,
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(var.0, var.1)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("the stand-in starts");
        let out = child.stdout.take().expect("its stdout");
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let saw = move |word: &str| loop {
            match lines.recv_timeout(HANG) {
                Ok(line) if line == word => break,
                Ok(_) => {}
                Err(e) => panic!("hang detector: the stand-in never wrote {word:?} ({e})"),
            }
        };
        (child, saw)
    }

    /// AN EXEC MOVES THE PID VERSION, SO A KEPT COALITION IS READ AGAIN (macOS, live): a child
    /// that execs keeps its pid, its kernel start and its coalition (its new task inherits the
    /// old one's) while its pid version moves, and the version is what [`processes_keeping`]
    /// keeps a coalition under. NEGATIVE CONTROL: across two sweeps with no exec between, the
    /// version stays and the coalition is kept.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_exec_moves_the_pid_version_and_keeps_the_coalition() {
        use std::io::Write as _;
        let (mut child, saw) = stand_in(EXEC_STAND_IN, (EXEC_STAGE, "before".as_ref()));
        saw("before");
        let pid = child.id();
        let mut ids = crate::coalitions::CoalitionIds::default();
        let mut row = || {
            processes_keeping(
                SweepOpts {
                    coalitions: true,
                    ..SweepOpts::default()
                },
                &mut ids,
            )
            .expect("a sweep")
            .procs
            .into_iter()
            .find(|p| p.pid == pid)
            .expect("the stand-in swept")
        };
        let first = row();
        assert!(first.version.is_some(), "{first:?}");
        assert_eq!(first.coalition, crate::coalitions::coalition_of(pid));
        let again = row();
        assert_eq!(
            (again.version, again.coalition),
            (first.version, first.coalition),
            "no exec between"
        );
        let mut stdin = child.stdin.take().expect("its stdin");
        stdin.write_all(b"x").expect("the byte that execs it");
        saw("after");
        let after = row();
        assert_eq!(
            (after.pid, after.start),
            (first.pid, first.start),
            "the same process"
        );
        assert_ne!(after.version, first.version, "an exec moved the version");
        assert_eq!(
            after.coalition, first.coalition,
            "the new task's, inherited"
        );
        drop(stdin);
        aterm_uds::deadline::kill_within(&mut child, HANG);
    }

    /// The write stand-in's scratch directory: [`write_stand_in`] writes [`WRITE_BYTES`] there on a byte.
    #[cfg(target_os = "macos")]
    const WRITE_DIR: &str = "ATERM_SYSPROBE_WRITE_DIR";

    /// [`write_stand_in`], as `--exact` names it.
    #[cfg(target_os = "macos")]
    const WRITE_STAND_IN: &str = "procs::tests::write_stand_in";

    /// What [`write_stand_in`] writes and syncs: 8 MiB.
    #[cfg(target_os = "macos")]
    const WRITE_BYTES: u64 = 8 << 20;

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "the child the zombie-write test starts; alone it returns"]
    fn write_stand_in() {
        use std::io::{Read as _, Write as _};
        let Some(dir) = std::env::var_os(WRITE_DIR) else {
            return;
        };
        println!("\nready");
        std::io::stdout().flush().expect("the ready proof");
        let mut byte = [0; 1];
        if !matches!(std::io::stdin().read(&mut byte), Ok(1)) {
            aterm_uds::process::kill_self_now();
        }
        let mut file = std::fs::File::create(std::path::Path::new(&dir).join("written"))
            .expect("the scratch file");
        let block: Vec<u8> = (0..1usize << 20).map(|i| (i * 31 % 251) as u8).collect();
        for _ in 0..WRITE_BYTES >> 20 {
            file.write_all(&block).expect("a block");
        }
        aterm_durable::durable(&file).expect("to storage");
        println!("wrote");
        std::io::stdout().flush().expect("the wrote proof");
    }

    /// A ZOMBIE KEEPS WHAT IT WROTE (macOS, live): a child that writes 8 MiB to storage and exits
    /// unreaped reads, as a zombie, at least those bytes under the start it had alive, where the
    /// sweep once gave a zombie no counters, so the writes after its last live sweep were lost.
    /// NEGATIVE CONTROL: its row read before it wrote reads under 8 MiB.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_zombie_keeps_the_bytes_it_wrote_and_its_start() {
        use std::io::Write as _;
        let dir = aterm_tempfile::tempdir().expect("a scratch directory");
        let (mut child, saw) = stand_in(WRITE_STAND_IN, (WRITE_DIR, dir.path().as_os_str()));
        saw("ready");
        let pid = child.id();
        let row = || {
            processes()
                .expect("a sweep")
                .procs
                .into_iter()
                .find(|p| p.pid == pid)
        };
        let alive = row().expect("the stand-in swept");
        assert!(
            alive.disk_written.is_some_and(|w| w < WRITE_BYTES) && alive.start.is_some(),
            "{alive:?}"
        );
        let mut stdin = child.stdin.take().expect("its stdin");
        stdin
            .write_all(b"x")
            .expect("the byte that starts the write");
        saw("wrote");
        let mut exit = aterm_uds::deadline::ExitWait::new(&child);
        let by = std::time::Instant::now() + HANG;
        while !child.exited().expect("whether it exited") {
            let left = by.saturating_duration_since(std::time::Instant::now());
            assert!(!left.is_zero(), "hang detector: the stand-in never exited");
            exit.wait(left);
        }
        let zombie = row().expect("the zombie swept");
        assert_eq!(zombie.state, crate::jobs::RunState::Zombie, "{zombie:?}");
        assert_eq!(zombie.start, alive.start, "the same process");
        assert!(
            zombie.disk_written.is_some_and(|w| w >= WRITE_BYTES),
            "{zombie:?}"
        );
        child.wait().expect("reaped");
    }

    /// The spin stand-in's switch: [`spin_stand_in`] spins until it has run [`SPIN_NS`] of CPU;
    /// at `nested` it first runs and reaps one more spinner of its own, then says `ready` and
    /// spins only on a byte.
    #[cfg(target_os = "macos")]
    const SPIN_VAR: &str = "ATERM_SYSPROBE_SPIN";

    /// [`spin_stand_in`], as `--exact` names it.
    #[cfg(target_os = "macos")]
    const SPIN_STAND_IN: &str = "procs::tests::spin_stand_in";

    /// The CPU [`spin_stand_in`] runs before it exits, by its own account: 200 ms.
    #[cfg(target_os = "macos")]
    const SPIN_NS: u64 = 200_000_000;

    /// This process's own and reaped children's CPU, ns, by the kernel's account.
    #[cfg(target_os = "macos")]
    fn own_and_reaped_ns(pid: u32) -> (u64, u64) {
        let (numer, denom) = imp::timebase();
        let u = imp::rusage(i32::try_from(pid).expect("a pid")).expect("its usage");
        (
            ticks_to_ns(
                u.ri_user_time.saturating_add(u.ri_system_time),
                numer,
                denom,
            ),
            ticks_to_ns(
                u.ri_child_user_time.saturating_add(u.ri_child_system_time),
                numer,
                denom,
            ),
        )
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "the child the zombie-CPU test starts; alone it returns"]
    fn spin_stand_in() {
        use std::io::{Read as _, Write as _};
        let Some(how) = std::env::var_os(SPIN_VAR) else {
            return;
        };
        if how == "nocld" {
            nocld_stand_in();
        }
        if how == "nested" {
            let status =
                aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
                    .args([
                        "--exact",
                        SPIN_STAND_IN,
                        "--ignored",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env(SPIN_VAR, "1")
                    .stdout(aterm_uds::spawnfd::Stdio::null())
                    .status()
                    .expect("the nested spinner runs");
            assert!(status.success(), "the nested spinner: {status:?}");
            println!("\nready");
            std::io::stdout().flush().expect("the ready proof");
            let mut byte = [0; 1];
            if !matches!(std::io::stdin().read(&mut byte), Ok(1)) {
                aterm_uds::process::kill_self_now();
            }
        }
        let me = std::process::id();
        let mut x = 0u64;
        while own_and_reaped_ns(me).0 < SPIN_NS {
            for i in 0..100_000u64 {
                x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(i));
            }
        }
        std::hint::black_box(x);
        println!("\nspun");
        std::io::stdout().flush().expect("the spun proof");
        std::process::exit(0);
    }

    /// [`spin_stand_in`] at `nocld`: ignores `SIGCHLD`, runs one spinner the kernel reaps for it,
    /// and once that spinner's pid answers no signal (reaped) says `ready` and waits for a byte.
    #[cfg(target_os = "macos")]
    fn nocld_stand_in() -> ! {
        use std::io::{Read as _, Write as _};
        // SAFETY: sets one signal's disposition; this process waits on no child but by its pid.
        let before = unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
        assert_ne!(before, libc::SIG_ERR, "SIGCHLD ignored");
        let mut child =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
                .args([
                    "--exact",
                    SPIN_STAND_IN,
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(SPIN_VAR, "1")
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .spawn()
                .expect("the spinner starts");
        let mut out = String::new();
        child
            .stdout
            .take()
            .expect("its stdout")
            .read_to_string(&mut out)
            .expect("its output");
        assert!(out.lines().any(|l| l == "spun"), "the spinner ran: {out:?}");
        let pid = child.id();
        let alive = || {
            // SAFETY: signal 0 only asks whether the pid names a process.
            unsafe { libc::kill(i32::try_from(pid).expect("a pid"), 0) == 0 }
        };
        let mut exit = aterm_uds::deadline::ExitWait::on_pid(pid);
        let by = std::time::Instant::now() + HANG;
        while alive() {
            let left = by.saturating_duration_since(std::time::Instant::now());
            assert!(
                !left.is_zero(),
                "hang detector: the spinner was never reaped"
            );
            exit.wait(left);
        }
        drop(child);
        println!("\nready");
        std::io::stdout().flush().expect("the ready proof");
        let mut byte = [0; 1];
        let _ = std::io::stdin().read(&mut byte);
        std::process::exit(0);
    }

    /// A PARENT THAT IGNORES `SIGCHLD` HAS ITS CHILD'S TIME ADDED TO ITS REAPED TIME TWICE, AND
    /// THE SWEEP SAYS SO (macOS, live): a stand-in that ignores `SIGCHLD` runs a spinner of 200 ms
    /// by its own account, which the kernel reaps for it; its `kinfo_proc` and its sweep row read
    /// `reaps_unwaited`, and its reaped-children time is at least twice the spinner's 200 ms.
    /// NEGATIVE CONTROL: the zombie test's stand-in, which waits for its spinner, reads
    /// `Some(false)`.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_parent_ignoring_sigchld_reads_its_reaped_time_doubled() {
        use std::io::Write as _;
        let (mut child, saw) = stand_in(SPIN_STAND_IN, (SPIN_VAR, "nocld".as_ref()));
        let pid = child.id();
        saw("ready");
        let kinfo = crate::jobs::macos_kinfo(std::ffi::c_int::try_from(pid).expect("a pid"));
        let row = processes()
            .expect("a sweep")
            .procs
            .into_iter()
            .find(|p| p.pid == pid)
            .expect("the stand-in swept");
        let (_, reaped) = own_and_reaped_ns(pid);
        child
            .stdin
            .take()
            .expect("its stdin")
            .write_all(b"x")
            .expect("the byte that ends it");
        child.wait().expect("reaped");
        eprintln!("ignoring SIGCHLD: reaped {reaped} ns for a {SPIN_NS} ns spinner, {row:?}");
        assert!(kinfo.is_some_and(|k| k.reaps_unwaited), "its kinfo_proc");
        assert_eq!(row.reaps_unwaited, Some(true), "its row");
        assert!(reaped >= 2 * SPIN_NS, "doubled: {reaped}");
        assert!(
            row.child_cpu_ns.is_some_and(|ns| ns >= 2 * SPIN_NS),
            "{row:?}"
        );
    }

    /// AN UNREAPED ZOMBIE ANSWERS ITS FINAL CPU, AND ITS PARENT'S REAPED-CHILDREN TIME TAKES IT AT
    /// THE REAP (macOS, live): a child that ran and reaped a 200 ms spinner of its own, then ran
    /// 200 ms of CPU by its own account and exited, reads, unreaped, at least that CPU and that
    /// reaped time under the start it had alive, from both `proc_pid_rusage` and the sweep's
    /// zombie row; once reaped, this process's reaped-children time has grown by at least both
    /// (the kernel adds a child's own time and its reaped children's at its reap). NEGATIVE
    /// CONTROL: the row read before the spin, which waits for a byte, reads under 200 ms.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_zombie_answers_its_final_cpu_and_the_reap_moves_it_to_its_parent() {
        use std::io::Write as _;
        let (mut child, saw) = stand_in(SPIN_STAND_IN, (SPIN_VAR, "nested".as_ref()));
        let pid = child.id();
        let row = || {
            processes()
                .expect("a sweep")
                .procs
                .into_iter()
                .find(|p| p.pid == pid)
        };
        saw("ready");
        let alive = row().expect("the stand-in swept");
        let mut stdin = child.stdin.take().expect("its stdin");
        stdin
            .write_all(b"x")
            .expect("the byte that starts the spin");
        saw("spun");
        let mut exit = aterm_uds::deadline::ExitWait::new(&child);
        let by = std::time::Instant::now() + HANG;
        while !child.exited().expect("whether it exited") {
            let left = by.saturating_duration_since(std::time::Instant::now());
            assert!(!left.is_zero(), "hang detector: the stand-in never exited");
            exit.wait(left);
        }
        let (zombie_own, zombie_reaped) = own_and_reaped_ns(pid);
        let zombie = row().expect("the zombie swept");
        let me = std::process::id();
        let (_, reaped_before) = own_and_reaped_ns(me);
        child.wait().expect("reaped");
        let (_, reaped_after) = own_and_reaped_ns(me);
        eprintln!(
            "zombie: own {zombie_own} ns, reaped {zombie_reaped} ns; swept {:?} / {:?}; this \
             process's reaped-children time {reaped_before} -> {reaped_after} ns (+{})",
            zombie.cpu_ns,
            zombie.child_cpu_ns,
            reaped_after - reaped_before
        );
        assert!(
            alive.cpu_ns.is_some_and(|ns| ns < SPIN_NS),
            "NEGATIVE CONTROL: before the spin {alive:?}"
        );
        assert_eq!(
            alive.reaps_unwaited,
            Some(false),
            "it waits for its children: {alive:?}"
        );
        assert_eq!(zombie.state, crate::jobs::RunState::Zombie, "{zombie:?}");
        assert_eq!(zombie.start, alive.start, "the same process");
        assert!(zombie_own >= SPIN_NS, "the zombie's own CPU: {zombie_own}");
        assert!(
            zombie_reaped >= SPIN_NS,
            "the zombie's reaped child's CPU: {zombie_reaped}"
        );
        assert_eq!(
            (zombie.cpu_ns, zombie.child_cpu_ns),
            (Some(zombie_own), Some(zombie_reaped)),
            "the sweep's zombie row"
        );
        assert!(
            reaped_after - reaped_before >= zombie_own + zombie_reaped,
            "the reap moved the zombie's final CPU to its parent"
        );
    }

    /// THE SWEEP READS A PARENT BEFORE ITS CHILD (macOS, live), so a child reaped while it runs
    /// is never counted both in its own row and in its parent's reaped-children time: this
    /// process's counters are read before its child's, while the rows keep the kernel's order,
    /// the child (the newer) first. NEGATIVE CONTROL: reading the list in the kernel's order
    /// reads the child first.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_sweep_reads_a_parent_before_its_child_and_keeps_the_kernels_order() {
        let (mut child, saw) = stand_in(EXEC_STAND_IN, (EXEC_STAGE, "parent-first".as_ref()));
        saw("parent-first");
        let sweep = processes().expect("a sweep");
        let at = |pid: u32| {
            sweep
                .procs
                .iter()
                .position(|p| p.pid == pid)
                .map(|k| (k, sweep.procs[k].read_at.expect("its counters' read")))
                .expect("swept")
        };
        let (me, read_me) = at(std::process::id());
        let (kid, read_kid) = at(child.id());
        assert!(read_me < read_kid, "the parent's counters read first");
        assert!(kid < me, "the rows in the kernel's order, newest first");
        aterm_uds::deadline::kill_within(&mut child, HANG);
    }

    /// THE SWEEP READS A ZOMBIE AFTER ITS PARENT (macOS, live): the kernel lists its zombies after
    /// every live process, so read from the list's end a zombie read before its parent, and a
    /// parent that reaped it between the two reads held it twice, in the zombie's final row and
    /// in its own reaped-children time. The sweep reads the zombies in a second pass, after
    /// every live process: this process's counters before its unreaped child's, whose row
    /// follows the live ones. NEGATIVE CONTROL: the zombie read in the live pass reads first.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_sweep_reads_a_zombie_after_its_parent() {
        let (mut child, saw) = stand_in(EXEC_STAND_IN, (EXEC_STAGE, "parent-first".as_ref()));
        saw("parent-first");
        drop(child.stdin.take());
        let mut exit = aterm_uds::deadline::ExitWait::new(&child);
        let by = std::time::Instant::now() + HANG;
        while !child.exited().expect("whether it exited") {
            let left = by.saturating_duration_since(std::time::Instant::now());
            assert!(!left.is_zero(), "hang detector: the stand-in never exited");
            exit.wait(left);
        }
        let sweep = processes().expect("a sweep");
        let at = |pid: u32| {
            sweep
                .procs
                .iter()
                .position(|p| p.pid == pid)
                .map(|k| {
                    let p = &sweep.procs[k];
                    (k, p.read_at.expect("its counters' read"), p.state)
                })
                .expect("swept")
        };
        let (me, read_me, _) = at(std::process::id());
        let (zombie, read_zombie, state) = at(child.id());
        assert_eq!(state, crate::jobs::RunState::Zombie, "unreaped");
        assert!(read_me < read_zombie, "the parent's counters read first");
        assert!(me < zombie, "the zombie's row after the live ones");
        child.wait().expect("reaped");
    }

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
        if cfg!(unix) {
            assert!(mine.uid.is_some(), "{mine:?}");
        }
        assert!(mine.read_at.is_some(), "{mine:?}");
        if cfg!(target_os = "macos") {
            assert!(mine.runnable_ns.is_some_and(|r| r > 0), "{mine:?}");
            assert!(mine.peak_footprint.is_some_and(|p| p > 0), "{mine:?}");
            assert!(mine.disk_written.is_some(), "{mine:?}");
        }
        assert!(sweep.procs.iter().any(|p| p.pid == mine.ppid));
    }
}
