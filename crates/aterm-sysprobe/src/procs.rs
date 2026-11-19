// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The process sweep: every process's pid, parent, CPU time, footprint and short name, read in one
//! pass for the system-awareness stats (`docs/DESIGN-system-awareness-2026-10-16.md`).

/// The sweep stops here.
pub const MAX_PROCESSES: usize = 16_384;

/// Darwin scheduling observations. Errors retain errno; a successful zero is not unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scheduling {
    /// Unix nice value (which alone does not identify effective QoS).
    pub nice: Result<i32, i32>,
    /// The target's application flag from its successful BSD-info read.
    pub application: bool,
    /// The target's internal requested-background bit, not its effective QoS.
    pub background_internal: bool,
    /// The target's external requested-background bit, not its effective QoS.
    pub background_external: bool,
    /// Whether the target's effective suppression flag is set.
    pub suppressed: bool,
    /// Darwin application role, including zero (unspecified).
    pub role: Result<i32, i32>,
    /// Lifetime CPU nanoseconds in default, maintenance, background, utility, legacy,
    /// user-initiated and user-interactive QoS, in that order. Not core residency.
    /// `None` means the kernel timebase was unavailable; errors retain the rusage errno.
    pub qos_cpu_ns: Option<Result<[u64; 7], i32>>,
}

/// The mark that ends a process name no reader holds whole ([`whole_name`]): the name may go on
/// past it.
pub const CUT_MARK: char = '\u{2026}';

/// A process's name as a person reads it. `short` is the kernel's short name, the bytes its
/// reader gave, and `limit` the length that reader cuts names to: a name of any other length is
/// taken whole (a longer one came past that reader's cut). One exactly `limit` long may be cut, so
/// it is the first of `longer` (each another reader's name for the process and the length that
/// reader cuts to, `None` where it cuts none) that begins with `short` and is shorter than its own
/// cut. Where none is, the longest that begins with `short`, or `short` itself, ends in
/// [`CUT_MARK`] after its UTF-8 prefix. `longer` is pulled only as far as that needs, so its
/// readers cost nothing for a name below `limit`.
#[must_use]
pub fn whole_name<I>(short: &[u8], limit: usize, longer: I) -> String
where
    I: IntoIterator<Item = Option<(Vec<u8>, Option<usize>)>>,
{
    if short.len() != limit {
        return String::from_utf8_lossy(short).into_owned();
    }
    let mut cut: Option<Vec<u8>> = None;
    for (name, its_limit) in longer.into_iter().flatten() {
        if !name.starts_with(short) {
            continue;
        }
        if its_limit.is_none_or(|l| name.len() < l) {
            return String::from_utf8_lossy(&name).into_owned();
        }
        if cut.as_ref().is_none_or(|c| name.len() > c.len()) {
            cut = Some(name);
        }
    }
    let mut marked = crate::utf8_prefix(cut.as_deref().unwrap_or(short)).to_owned();
    marked.push(CUT_MARK);
    marked
}

/// `getpriority` may successfully return -1; only errno distinguishes that from failure.
pub fn priority_result(value: i32, errno: i32) -> Result<i32, i32> {
    if errno == 0 { Ok(value) } else { Err(errno) }
}

/// One process in a sweep.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessReading {
    /// Darwin scheduling state where the sweep's options ask for it ([`SweepOpts::scheduling`]);
    /// `None` where they do not, off Darwin, and for exited processes.
    pub scheduling: Option<Scheduling>,
    /// Its pid.
    pub pid: u32,
    /// Kernel start identity: Mach absolute ticks on macOS, boot nanoseconds on Linux, creation FILETIME on Windows.
    pub start: Option<u64>,
    /// Precise lifetime birth in the platform's native identity units: `/proc` start ticks on
    /// Linux, Unix microseconds on Windows. Never reconstructed from [`Self::birth_ms`] or a
    /// rounded boot-clock start. macOS leaves this unread: an upper reader may pair the wall
    /// record with this row's Mach start using a generation-bracketed native observation.
    pub birth: Option<u64>,
    /// Its parent's pid (0 when the kernel names none).
    pub ppid: u32,
    /// User plus system CPU time since it started, nanoseconds; `None` where the OS refuses (another user's process on macOS). An unreaped zombie answers its final time (macOS).
    pub cpu_ns: Option<u64>,
    /// User plus system CPU time of the children it has reaped, nanoseconds (macOS
    /// `ri_child_user_time + ri_child_system_time`): at each reap the kernel adds the child's own
    /// time and its reaped children's, so it holds every reaped descendant's work once, and an
    /// unreaped zombie answers its final figure; Linux reads `cutime + cstime`; `None` where
    /// refused and on Windows.
    pub child_cpu_ns: Option<u64>,
    /// Its memory in bytes: macOS `ri_phys_footprint`, Linux `RssAnon + VmSwap`, Windows its private commit; `None` where refused.
    pub footprint: Option<u64>,
    /// Its pages in physical memory now, bytes: macOS `ri_resident_size`, below its footprint by
    /// what the kernel compressed or swapped of it and above by the file-backed pages it maps;
    /// Linux `RssAnon`, its anonymous pages, below its footprint by what was swapped. `None`
    /// where refused, for an unreaped zombie, for a Linux process read only by `statm`, and
    /// elsewhere. Windows its working set.
    pub resident: Option<u64>,
    /// Bytes it has read from disk since it started (macOS `ri_diskio_bytesread`, Linux `io`'s
    /// `read_bytes`: storage reads, not page-cache hits); `None` where refused (another user's
    /// process) and on Windows, whose process record counts no storage bytes apart from a pipe's
    /// or a socket's ([`Self::io_read`]).
    pub disk_read: Option<u64>,
    /// Bytes it has written to disk since it started (macOS `ri_diskio_byteswritten`, Linux `io`'s
    /// `write_bytes`: what reached or went toward storage, not every `write(2)`); `None` where
    /// refused (another user's process) and on Windows ([`Self::io_written`] holds what it reads
    /// there). An unreaped zombie keeps its final count, its start, its `disk_read` and its CPU
    /// times (macOS), though not its footprint.
    pub disk_written: Option<u64>,
    /// Bytes it has read through any handle since it started (Windows `ReadTransferCount`: a
    /// file's, a pipe's, a console's and a socket's alike), which says it is doing something and
    /// nothing about storage: no disk figure is made of it. `None` off Windows, where
    /// [`Self::disk_read`] is the storage count.
    pub io_read: Option<u64>,
    /// Bytes it has written through any handle since it started (Windows `WriteTransferCount`),
    /// as [`Self::io_read`]: a program printing to its terminal writes here and not to a disk.
    /// `None` off Windows.
    pub io_written: Option<u64>,
    /// Energy its threads used since it started, nanojoules (macOS `ri_energy_nj`, read with
    /// `RUSAGE_INFO_V6`, [`crate::rusage`]); an unreaped zombie keeps its final count. `None`
    /// where refused, on a kernel before macOS 12 (which has no V6), and off macOS.
    pub energy_nj: Option<u64>,
    /// Its name, whole where any reader holds it whole ([`whole_name`]): on macOS `p_comm` (15
    /// bytes through the short BSD info, 16 through a zombie's kernel record), else `p_name` (31)
    /// or its image's basename; on Linux
    /// `comm` (15), else its `exe` link's or its `argv[0]`'s file name; on Windows the image name
    /// less `.exe`. One no reader holds whole ends in [`CUT_MARK`].
    pub name: String,
    /// Its process group (0 where unread).
    pub pgid: u32,
    /// Its user; `None` where unread. On Windows its session id, the owner the design compares
    /// there (§12): [`crate::own_session`]'s is this person's, session 0 the services'
    /// ([`crate::windows::session_class`]).
    pub uid: Option<u32>,
    /// Its state (`p_stat`; on macOS `Running` covers a process blocked in `read(2)`). On
    /// Windows from its threads at the sweep ([`crate::windows::run_state`]): `Running` with a
    /// thread on a core or ready for one, `Stopped` with every thread suspended, else `Sleeping`.
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
    /// Time its threads spent runnable (running or waiting for a core) since it started,
    /// nanoseconds (macOS `ri_runnable_time`; Linux the CPU time plus every live thread's
    /// `schedstat` wait, for the user's own processes); `None` where refused or not read (Windows).
    pub runnable_ns: Option<u64>,
    /// Which kind of core its threads ran on, on a host with more than one ([`CoreSplit`]);
    /// `None` where the host has one kind, where refused or not read, and off macOS.
    pub cores: Option<CoreSplit>,
    /// The largest footprint it has had (macOS `ri_lifetime_max_phys_footprint`, Windows its peak commit); `None` where refused or not read.
    pub peak_footprint: Option<u64>,
    /// When its counters were read (a zombie's: at its second read, after every live process's).
    pub read_at: Option<std::time::Instant>,
    /// Its resource coalition's id (macOS), read only by a sweep asked for it ([`SweepOpts::coalitions`]); `None` otherwise, for a zombie, and off macOS.
    pub coalition: Option<u64>,
    /// Its pid version (macOS `p_idversion`, [`pid_version`]), which a new process and each exec take anew: the one the macOS sweep read before and after the row's other reads, given with [`Self::coalition`], or where the sweep reads no [`Self::start`] and [`SweepOpts::versions`] asks; `None` otherwise, where the first of those reads answered none, for a zombie, and off macOS.
    pub version: Option<i32>,
    /// When it started, unix ms: on Linux its boot-clock start against `/proc/uptime` and the wall clock read once a sweep ([`birth_of_boot_start`]), on Windows its creation `FILETIME` ([`birth_of_filetime`]). `None` on macOS, whose [`Self::start`] is Mach absolute time, which stops while the Mac sleeps (the kernel record, [`crate::kproc::record`], gives the wall birth there), and where unread.
    pub birth_ms: Option<i64>,
    /// Whether the kernel reaps its children for it (macOS `P_NOCLDWAIT`: it ignores `SIGCHLD`
    /// or set `SA_NOCLDWAIT`; a fork does not inherit it and an exec clears it), which then adds
    /// each child's time to its [`Self::child_cpu_ns`] twice (measured 2026-10-25: 400.4–401.6 ms
    /// for a 200 ms child). Read, after the sweep's reads, for every process, as each reading
    /// finds it: nothing records it between two readings or after the process exits. `None`
    /// where its record named another parent (a new process at the pid), and off macOS.
    pub reaps_unwaited: Option<bool>,
    /// Its threads (Linux `stat`'s `num_threads`): on Linux the per-user limit (`RLIMIT_NPROC`)
    /// counts threads, not processes. `None` where unread (macOS, whose limit counts processes;
    /// Windows).
    pub threads: Option<u32>,
}

/// Of a process's CPU time since it started, what ran on the host's performance cores and what
/// its threads retired there (macOS `RUSAGE_INFO_V6`, read where `hw.nperflevels` is above 1:
/// Apple silicon's performance and efficiency cores). An efficiency core runs a thread's
/// instructions slower, so the time on one does less work than the same time on a performance
/// core.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreSplit {
    /// User plus system time on performance cores, nanoseconds (`ri_user_ptime +
    /// ri_system_ptime`); at most [`ProcessReading::cpu_ns`].
    pub p_cpu_ns: u64,
    /// Instructions retired on every core (`ri_instructions`).
    pub instructions: u64,
    /// Of those, the instructions retired on performance cores (`ri_pinstructions`).
    pub p_instructions: u64,
}

/// What a sweep reads beyond each process's row ([`processes_with`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepOpts {
    /// Each process's resource coalition and pid version ([`ProcessReading::coalition`], [`ProcessReading::version`]): on macOS, whose sweep reads every process's version to bracket its reads, one more `proc_pidinfo` for each process whose coalition [`processes_keeping`]'s ids do not hold under its version; nothing elsewhere. On macOS a name at the short name's cut ([`whole_name`]) is then read whole once for each pid version and short name: [`processes_keeping`]'s ids keep it for the sweeps after ([`crate::coalitions::CoalitionIds::name`]).
    pub coalitions: bool,
    /// The pid version of each process whose start the sweep cannot read (another user's, on macOS, whose usage the kernel refuses), which is what tells a new process at its pid (or an exec) from the one a reader kept something for: no more reads on macOS, whose sweep reads every process's version to bracket its reads, and nothing elsewhere. [`Self::coalitions`] gives every process's.
    pub versions: bool,
    /// Each live process's scheduling ([`ProcessReading::scheduling`]): two more `getpriority`
    /// calls a process on macOS (its nice and its Darwin role), with the QoS CPU split its usage
    /// read already holds; nothing elsewhere, where it is `None` either way. Without it no
    /// `getpriority` is made and every row's is `None`.
    pub scheduling: bool,
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

/// One process's identity and usage counters, without reading unrelated processes on macOS.
/// An owning waiter may read an unreaped zombie's final own and child counters before reaping
/// it; the returned start must still match that waiter's original child.
#[must_use]
pub fn process_usage(pid: u32) -> Option<ProcessReading> {
    #[cfg(target_os = "macos")]
    {
        let signed = i32::try_from(pid).ok().filter(|p| *p > 0)?;
        // Read state before usage: a zombie cannot run again, so a following rusage read is
        // final. Reading usage first could label a still-running counter final after exit.
        let info = crate::jobs::macos_kinfo(signed)?;
        let usage = imp::rusage(signed)?;
        let (numer, denom) = imp::timebase();
        Some(ProcessReading {
            state: crate::jobs::RunState::from_bsd(info.stat),
            ppid: info.ppid,
            uid: Some(info.uid),
            coalition: crate::coalitions::coalition_of(pid),
            version: pid_version(pid),
            reaps_unwaited: Some(info.reaps_unwaited),
            ..imp::usage_row(pid, Some(&usage), numer, denom)
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        processes()?.procs.into_iter().find(|p| p.pid == pid)
    }
}

/// Sweep every process, reading what `opts` asks for too; `None` where the platform has no reader.
#[must_use]
pub fn processes_with(opts: SweepOpts) -> Option<Sweep> {
    processes_keeping(opts, &mut crate::coalitions::CoalitionIds::default())
}

/// [`processes_with`], each process's coalition taken from `ids` where they hold it under the
/// process's pid version, else read and kept there, and where `opts` asks for coalitions on
/// macOS, each name at the short name's cut likewise, under the process's pid version and short
/// name ([`crate::coalitions::CoalitionIds::name`]). On macOS `ids` keep too, under the pid
/// version of each row the sweep kept, its session id and a refusal of its usage
/// ([`crate::coalitions::Generation`]), which the sweeps after answer without reading them again
/// where they cannot have moved. `ids` then holds the kept live rows' alone.
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
    crate::parse_uint(text.split_whitespace().nth(1)?)
}

/// ONE PROCESS'S MEMORY, bytes: its footprint, as the sweep reads it for every process
/// ([`ProcessReading::footprint`]): macOS `ri_phys_footprint` (the pages it holds, those the
/// kernel compressed or swapped out included), Linux `RssAnon + VmSwap` (its anonymous pages in
/// memory and swapped; file-backed pages it maps are not its own), Windows its private commit.
/// No process-table sweep runs on macOS or Linux. Callers bracket this observation with a stable
/// native identity of their own: this read alone proves no ownership or lifetime.
///
/// # Errors
/// `InvalidInput` for pid 0 or 1 or past `i32::MAX`; `InvalidData` for a Linux `status` past the
/// bounded reader's [`crate::linux::STATUS_CAP`]; `NotFound` where the process or its figure
/// cannot be read (it is gone, another user's, or a kernel thread with no anonymous memory); the
/// kernel's own error where it refused; `Unsupported` where this platform has no reader.
pub fn footprint(pid: u32) -> std::io::Result<u64> {
    let pid = i32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a footprint read requires a valid pid above one",
            )
        })?;
    #[cfg(target_os = "macos")]
    {
        let usage = imp::rusage_result(pid).map_err(std::io::Error::from_raw_os_error)?;
        Ok(usage.ri.ri_phys_footprint)
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux::Root::system().footprint(pid.unsigned_abs())
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        let pid = pid.unsigned_abs();
        crate::win::processes()
            .and_then(|sweep| sweep.procs.into_iter().find(|p| p.pid == pid))
            .and_then(|p| p.footprint)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "the process table holds no footprint for this pid",
                )
            })
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        all(windows, target_pointer_width = "64")
    )))]
    {
        let _ = pid;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no native footprint reader on this platform",
        ))
    }
}

/// A start on the boot clock (`start_ns` since boot, sleep included, as Linux counts a process's start) as unix ms, against the boot clock and the wall clock read together (`boot_now_ns`, `unix_now_ms`). A start after that moment (a process born while the sweep ran) is that moment; `None` before the epoch.
#[must_use]
pub fn birth_of_boot_start(start_ns: u64, boot_now_ns: u64, unix_now_ms: i64) -> Option<i64> {
    let age_ms = i64::try_from(boot_now_ns.saturating_sub(start_ns) / 1_000_000).ok()?;
    unix_now_ms.checked_sub(age_ms).filter(|ms| *ms >= 0)
}

/// 100 ns ticks between the Windows `FILETIME` epoch (1601-01-01 UTC) and the Unix epoch.
const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;

/// A Windows creation `FILETIME` (100 ns ticks since 1601-01-01 UTC) as unix ms; `None` before the epoch.
#[must_use]
pub fn birth_of_filetime(ticks: u64) -> Option<i64> {
    i64::try_from(ticks.checked_sub(UNIX_EPOCH_TICKS)? / 10_000).ok()
}

/// A Windows creation `FILETIME` as the Unix microseconds used by process lifetime evidence.
/// This preserves the kernel record's microsecond identity; display milliseconds do not.
#[must_use]
pub fn birth_micros_of_filetime(ticks: u64) -> Option<u64> {
    Some(ticks.checked_sub(UNIX_EPOCH_TICKS)? / 10)
}

/// How long a process created at `ticks` (`FILETIME`, 100 ns since 1601-01-01 UTC) had run at
/// `now_us` (µs since the Unix epoch), ms; `None` before the Unix epoch, `0` when born past `now_us`.
#[must_use]
pub(crate) const fn filetime_age_ms(ticks: u64, now_us: u64) -> Option<u64> {
    let Some(since_epoch) = ticks.checked_sub(UNIX_EPOCH_TICKS) else {
        return None;
    };
    Some(now_us.saturating_sub(since_epoch / 10) / 1000)
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

/// The sysctl that answers the kernel's Mach tick rate in hertz (24,000,000 on Apple silicon, natively and under Rosetta 2).
#[cfg(target_os = "macos")]
const TICK_FREQUENCY: &std::ffi::CStr = c"hw.tbfrequency";

/// The timebase the kernel counts in, chosen from its two readings: the tick rate `hz` (what `hw.tbfrequency` answered), reduced, when it is a frequency; otherwise `mach()`, this process's `mach_timebase_info`, which is called only then. The tick rate comes first because under Rosetta 2 `mach_timebase_info` reads 1/1 while the kernel still records 24 MHz ticks (docs/measured/rosetta-mac-2026-10-20.md §1).
#[cfg(any(target_os = "macos", test))]
fn timebase_from(hz: Option<i64>, mach: impl FnOnce() -> Option<(u32, u32)>) -> Option<(u32, u32)> {
    hz.and_then(|hz| u64::try_from(hz).ok())
        .and_then(timebase_of_frequency)
        .or_else(mach)
}

/// The kernel's own Mach timebase `(numer, denom)`: what turns the tick counts the kernel records for a task (`proc_pid_rusage`'s CPU and runnable times, `PROC_PIDTASKINFO`'s `pti_total_*`) into nanoseconds. Read once: `hw.tbfrequency`'s tick rate, reduced, or this process's `mach_timebase_info` when that sysctl answers no frequency.
#[cfg(target_os = "macos")]
#[must_use]
pub fn kernel_timebase() -> Option<(u32, u32)> {
    static TB: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    *TB.get_or_init(|| {
        timebase_from(crate::macos::sysctl_int(TICK_FREQUENCY), || {
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

    use super::{CoreSplit, MAX_PROCESSES, ProcessReading, Sweep, SweepOpts, ticks_to_ns};
    use crate::jobs::RunState;
    use crate::jobs::darwin::{Kinfo, PROC_FLAG_CONTROLT, PROC_FLAG_EXEC};
    use crate::rusage::{Flavor, RusageV6, Usage};

    /// `PROC_PIDT_SHORTBSDINFO` (`sys/proc_info.h`), which the `libc` crate does not export.
    const PROC_PIDT_SHORTBSDINFO: c_int = 13;

    /// `struct proc_bsdshortinfo` (64 bytes).
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
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

    /// The kernel's Mach timebase `(numer, denom)` ([`super::kernel_timebase`]), 1/1 when it cannot be read.
    pub(crate) fn timebase() -> (u32, u32) {
        super::kernel_timebase().unwrap_or((1, 1))
    }

    /// `pid`'s short BSD info (readable for every process), whole or `None`.
    pub(crate) fn short_info(pid: c_int) -> Option<ShortInfo> {
        crate::lane::mark("proc_pidinfo SHORTBSDINFO", u32::try_from(pid).ok());
        // SAFETY: plain integers and bytes; all-zero is valid.
        let mut info: ShortInfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<ShortInfo>() as c_int;
        // SAFETY: `info` is `size` writable bytes of the flavor's layout.
        let n = unsafe {
            libc::proc_pidinfo(pid, PROC_PIDT_SHORTBSDINFO, 0, (&raw mut info).cast(), size)
        };
        (n == size).then_some(info)
    }

    /// The bytes of `p_comm` the short BSD info keeps: 15, its 16th the kernel's NUL.
    const SHORT_COMM_CUT: usize = 15;

    /// The bytes of `p_comm` the kernel record (`kinfo_proc`) keeps: 16 (`MAXCOMLEN`).
    const KINFO_COMM_CUT: usize = 16;

    /// The bytes of `p_name` `PROC_PIDTBSDINFO` keeps: 31, its 32nd the kernel's NUL.
    const P_NAME_CUT: usize = 31;

    /// `pid`'s `p_name` (`PROC_PIDTBSDINFO`): the name of the file it last exec'd, as `p_comm` is,
    /// cut at 31 bytes rather than 15. Only the process's own user (or root) may read it.
    fn p_name(pid: c_int) -> Option<Vec<u8>> {
        crate::lane::mark("proc_pidinfo TBSDINFO", u32::try_from(pid).ok());
        // SAFETY: plain integers and bytes; all-zero is valid.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as c_int;
        // SAFETY: `info` is `size` writable bytes of the flavor's layout.
        let n = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size)
        };
        if n != size {
            return None;
        }
        let name = info.pbi_name.map(|c| c.cast_unsigned());
        let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
        Some(name[..end].to_vec())
    }

    /// `pid`'s image's basename (`proc_pidpath`, which any process may read of any other, and
    /// which cuts nothing).
    fn image_name(pid: c_int) -> Option<Vec<u8>> {
        use std::os::unix::ffi::OsStrExt as _;
        let path = crate::process_path(u32::try_from(pid).ok()?)?;
        Some(path.file_name()?.as_bytes().to_vec())
    }

    /// `pid`'s longer names for [`super::whole_name`], each read from `kernel` only once pulled:
    /// its `p_name` (0.5 to 0.9 µs each, measured 2026-11-04 over 290 names at the cut, but
    /// refused for another user's process), then its image's basename (2.5 to 6.5 µs each).
    fn longer_names(
        kernel: &impl Reads,
        pid: c_int,
    ) -> impl Iterator<Item = Option<(Vec<u8>, Option<usize>)>> {
        std::iter::once_with(move || kernel.p_name(pid).map(|n| (n, Some(P_NAME_CUT)))).chain(
            std::iter::once_with(move || kernel.image_name(pid).map(|n| (n, None))),
        )
    }

    /// Every kernel read the sweep makes ([`sweep`]): [`Native`]'s in production; the tests'
    /// scripted world answers and counts them.
    pub(super) trait Reads {
        /// Every pid `proc_listallpids` lists, its live processes newest first and then its
        /// zombies, at most `MAX_PROCESSES + 1`; `None` where it does not answer.
        fn pids(&self) -> Option<Vec<c_int>>;
        /// `pid`'s usage ([`rusage_result`]).
        fn rusage(&self, pid: c_int) -> Result<Usage, i32>;
        /// `pid`'s short BSD info ([`short_info`]).
        fn short_info(&self, pid: c_int) -> Option<ShortInfo>;
        /// `pid`'s pid version ([`pid_version`]).
        fn version(&self, pid: c_int) -> Option<i32>;
        /// `pid`'s resource coalition ([`crate::coalitions::coalition_of`]).
        fn coalition(&self, pid: u32) -> Option<u64>;
        /// `getpriority(which, pid)` ([`priority`]).
        fn priority(&self, pid: c_int, which: c_int) -> Result<i32, i32>;
        /// `getsid(pid)`.
        fn sid(&self, pid: c_int) -> c_int;
        /// `pid`'s `p_name` ([`p_name`]).
        fn p_name(&self, pid: c_int) -> Option<Vec<u8>>;
        /// `pid`'s image's basename ([`image_name`]).
        fn image_name(&self, pid: c_int) -> Option<Vec<u8>>;
        /// `pid`'s kernel record ([`crate::jobs::macos_kinfo`]).
        fn kinfo(&self, pid: c_int) -> Option<Kinfo>;
        /// The whole table's `(pid, ppid, reaps unwaited)` rows
        /// ([`crate::kproc::reaps_unwaited_all`]).
        fn unwaited(&self) -> Option<Vec<(u32, u32, bool)>>;
        /// This process's effective uid (`geteuid`), the one the kernel compares a target's with
        /// before it answers that target's usage.
        fn own_uid(&self) -> u32;
    }

    /// The kernel's own reads.
    pub(super) struct Native;

    impl Reads for Native {
        fn pids(&self) -> Option<Vec<c_int>> {
            crate::lane::mark("proc_listallpids", None);
            // SAFETY: a null buffer asks for the count only.
            let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
            let count = usize::try_from(count).ok()?;
            let mut pids = vec![0i32; (count + 64).min(MAX_PROCESSES + 1)];
            let bytes = c_int::try_from(pids.len() * std::mem::size_of::<i32>()).ok()?;
            // SAFETY: `pids` is `bytes` writable bytes.
            let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
            let n = usize::try_from(n).ok()?.min(pids.len());
            pids.truncate(n);
            Some(pids)
        }

        fn rusage(&self, pid: c_int) -> Result<Usage, i32> {
            rusage_result(pid)
        }

        fn short_info(&self, pid: c_int) -> Option<ShortInfo> {
            short_info(pid)
        }

        fn version(&self, pid: c_int) -> Option<i32> {
            pid_version(pid)
        }

        fn coalition(&self, pid: u32) -> Option<u64> {
            crate::coalitions::coalition_of(pid)
        }

        fn priority(&self, pid: c_int, which: c_int) -> Result<i32, i32> {
            priority(pid, which)
        }

        fn sid(&self, pid: c_int) -> c_int {
            crate::lane::mark("getsid", u32::try_from(pid).ok());
            // SAFETY: a plain syscall on an integer.
            unsafe { libc::getsid(pid) }
        }

        fn p_name(&self, pid: c_int) -> Option<Vec<u8>> {
            p_name(pid)
        }

        fn image_name(&self, pid: c_int) -> Option<Vec<u8>> {
            image_name(pid)
        }

        fn kinfo(&self, pid: c_int) -> Option<Kinfo> {
            crate::jobs::macos_kinfo(pid)
        }

        fn unwaited(&self) -> Option<Vec<(u32, u32, bool)>> {
            crate::kproc::reaps_unwaited_all()
        }

        fn own_uid(&self) -> u32 {
            // SAFETY: a plain syscall with no arguments, which cannot fail.
            unsafe { libc::geteuid() }
        }
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
        crate::lane::mark("proc_pidinfo UNIQIDENTIFIERINFO", u32::try_from(pid).ok());
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

    /// `pid`'s usage (the user's own processes, and any zombie's), `RUSAGE_INFO_V6` where the
    /// kernel has it ([`crate::rusage`]), or `None`.
    pub(crate) fn rusage(pid: c_int) -> Option<Usage> {
        rusage_result(pid).ok()
    }

    /// The flavor this kernel answers, learned at the first read that tells.
    static FLAVOR: Flavor = Flavor::new();

    pub(super) fn rusage_result(pid: c_int) -> Result<Usage, i32> {
        crate::lane::mark("proc_pid_rusage", u32::try_from(pid).ok());
        let mut ri = RusageV6::default();
        let v6 = FLAVOR.read(&mut ri, |flavor, ri| {
            // SAFETY: `ri` is a writable `rusage_info_v6`, which holds either flavor's record
            // (V4's is its first 296 bytes), and the kernel writes no more than the flavor's.
            let rc = unsafe {
                libc::proc_pid_rusage(pid, flavor, (&raw mut *ri).cast::<libc::rusage_info_t>())
            };
            if rc == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO))
            }
        })?;
        Ok(Usage { ri, v6 })
    }

    /// Whether this host has more than one kind of core (`hw.nperflevels` above 1), the only
    /// host whose performance cores' share says where a thread ran. Read once.
    fn split_cores() -> bool {
        static SPLIT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *SPLIT.get_or_init(|| crate::macos::sysctl_int(c"hw.nperflevels").is_some_and(|n| n > 1))
    }

    /// `u`'s performance cores' share ([`CoreSplit`]), its time in ns at the timebase
    /// `numer/denom`; `None` from a V4 read or where the host has one kind of core.
    fn cores_of(u: &Usage, numer: u32, denom: u32) -> Option<CoreSplit> {
        let (p_ticks, instructions, p_instructions) = u.performance_cores()?;
        split_cores().then(|| CoreSplit {
            p_cpu_ns: ticks_to_ns(p_ticks, numer, denom),
            instructions,
            p_instructions,
        })
    }

    fn priority(pid: c_int, which: c_int) -> Result<i32, i32> {
        crate::lane::mark("getpriority", u32::try_from(pid).ok());
        // SAFETY: errno is thread-local; getpriority only reads the named process's policy.
        let (value, errno) = unsafe {
            *libc::__error() = 0;
            let value = libc::getpriority(which, pid as libc::id_t);
            (value, *libc::__error())
        };
        super::priority_result(value, errno)
    }

    fn scheduling(kernel: &impl Reads, pid: c_int, flags: u32) -> super::Scheduling {
        // PRIO_DARWIN_ROLE is selector 6 in Apple's published resource_private.h. It is a
        // read only here; an OS that refuses it retains that errno, never an invented role.
        super::Scheduling {
            nice: kernel.priority(pid, libc::PRIO_PROCESS),
            // These target-specific bits come from proc_get_darwinbgstate in XNU.
            // getpriority(PRIO_DARWIN_PROCESS, pid) instead reads current_task().
            application: flags & 0x0100_0000 != 0,
            background_internal: flags & 0x0000_8000 != 0,
            background_external: flags & 0x0001_0000 != 0,
            suppressed: flags & 0x0080_0000 != 0,
            role: kernel.priority(pid, 6),
            qos_cpu_ns: None,
        }
    }

    fn qos_cpu(u: &RusageV6, numer: u32, denom: u32) -> [u64; 7] {
        [
            u.ri_cpu_time_qos_default,
            u.ri_cpu_time_qos_maintenance,
            u.ri_cpu_time_qos_background,
            u.ri_cpu_time_qos_utility,
            u.ri_cpu_time_qos_legacy,
            u.ri_cpu_time_qos_user_initiated,
            u.ri_cpu_time_qos_user_interactive,
        ]
        .map(|n| ticks_to_ns(n, numer, denom))
    }

    /// `u`'s own user plus system time, ns at the timebase `numer/denom`.
    fn cpu_of(u: &RusageV6, numer: u32, denom: u32) -> u64 {
        ticks_to_ns(
            u.ri_user_time.saturating_add(u.ri_system_time),
            numer,
            denom,
        )
    }

    /// `u`'s reaped children's user plus system time, ns at the timebase `numer/denom`.
    fn child_cpu_of(u: &RusageV6, numer: u32, denom: u32) -> u64 {
        ticks_to_ns(
            u.ri_child_user_time.saturating_add(u.ri_child_system_time),
            numer,
            denom,
        )
    }

    /// The figures of process `pid`'s row that its usage record `usage` gives (`None`: each
    /// unread), at the timebase `numer/denom`: its start, its own and its reaped children's CPU
    /// time, its footprint and resident size, its disk reads and writes, its energy, its runnable
    /// time and its peak footprint; every other field is the default. A sweep's live row takes
    /// them, and a group's read ([`crate::group_usage`]) reads each member as
    /// [`crate::activity::member`] of them, so a member's use has one definition.
    pub(crate) fn usage_row(
        pid: u32,
        usage: Option<&Usage>,
        numer: u32,
        denom: u32,
    ) -> ProcessReading {
        ProcessReading {
            pid,
            start: usage.map(|u| u.ri.ri_proc_start_abstime),
            cpu_ns: usage.map(|u| cpu_of(&u.ri, numer, denom)),
            child_cpu_ns: usage.map(|u| child_cpu_of(&u.ri, numer, denom)),
            footprint: usage.map(|u| u.ri.ri_phys_footprint),
            resident: usage.map(|u| u.ri.ri_resident_size),
            disk_read: usage.map(|u| u.ri.ri_diskio_bytesread),
            disk_written: usage.map(|u| u.ri.ri_diskio_byteswritten),
            energy_nj: usage.and_then(Usage::energy_nj),
            runnable_ns: usage.map(|u| ticks_to_ns(u.ri.ri_runnable_time, numer, denom)),
            peak_footprint: usage.map(|u| u.ri.ri_lifetime_max_phys_footprint),
            ..ProcessReading::default()
        }
    }

    pub(super) fn processes(
        opts: SweepOpts,
        ids: &mut crate::coalitions::CoalitionIds,
    ) -> Option<Sweep> {
        sweep(&Native, opts, ids)
    }

    /// The sweep ([`super::processes_keeping`]), each of its kernel reads made through `kernel`.
    pub(super) fn sweep(
        kernel: &impl Reads,
        opts: SweepOpts,
        ids: &mut crate::coalitions::CoalitionIds,
    ) -> Option<Sweep> {
        let mut pids = kernel.pids()?;
        let truncated = pids.len() > MAX_PROCESSES;
        pids.truncate(MAX_PROCESSES);
        let (numer, denom) = timebase();
        let own = kernel.own_uid();
        let mut procs = Vec::with_capacity(pids.len());
        ids.begin_sweep();
        // The kernel lists every live process, newest first, and then its zombies. A child is
        // always newer than the process that forked it, so reading the live list from its end
        // reads each parent before its children, and reading the zombies after every live
        // process reads each zombie after its parent: a child reaped while the sweep runs is then
        // either read (its parent's reaped-children time, read before the reap, does not hold it
        // yet) or gone (its parent reaped it after its read), never counted in both (SA §7.5's
        // reap credit). A process that answers no `proc_pidinfo` flavor (a zombie, or one
        // exiting), or whose pid version goes unanswered by the end of its row's reads, waits for
        // that second pass. The live rows keep the kernel's order, and the zombies follow them, as
        // the kernel lists them.
        let mut dead = Vec::new();
        for pid in pids.into_iter().rev() {
            if pid <= 0 {
                continue;
            }
            let key = pid.unsigned_abs();
            // The row's reads are bracketed by its pid version, read first and again last. A fork
            // and each exec take a new one, as a new process at the pid does, so where the two
            // answer one version every read between them (the usage, the short info, the
            // coalition, the scheduling, the session and the names) is of one process, and no exec
            // took a new version between them, which the start alone does not show. An exec that
            // took its version before the first may still be setting the names: xnu's takes it
            // (`exec_handle_sugid`) before it sets `p_comm` and `p_name` (`set_proc_name`), and an
            // `execve`'s new process takes the pid (`proc_exec_switch_task`) before the image path
            // names its file (`exec_resettextvp`). A row whose last version read answers another
            // version is dropped, and nothing read for it is kept. One whose last answers none
            // (it exited, or is gone) waits for the zombie pass with its usage read, as one whose
            // short info answers none does, and nothing read for it is kept. Where the first
            // answers none, the row's usage is read again after its other reads, and the row is
            // kept where both reads name one start or neither answers, and dropped otherwise.
            let version = kernel.version(pid);
            let kept = version
                .and_then(|v| ids.generation(key, v))
                .unwrap_or_default();
            // A usage the kernel refused (`EPERM`: xnu answers another user's process's usage to
            // root alone, comparing the target's effective uid with the reader's) is read once a
            // generation: the refusal is kept under its pid version with the effective uid the
            // short info named (`pbsi_uid`, the process's `p_uid`, which xnu sets from its
            // credential's effective uid under the lock that installs it), and answered again
            // while the short info names that uid. One that named this process's own uid is not
            // kept: the uid moved between the reads, or something else refused it.
            let refused = kept.refused.filter(|uid| *uid != own);
            let first = refused
                .is_none()
                .then(|| (kernel.rusage(pid), std::time::Instant::now()));
            let Some(info) = kernel.short_info(pid) else {
                let before = first.map_or_else(|| kernel.rusage(pid), |(read, _)| read);
                dead.push((pid, before.ok()));
                continue;
            };
            let (read, read_at) = match first {
                Some(first) => first,
                None if refused == Some(info.uid) => (Err(libc::EPERM), std::time::Instant::now()),
                None => (kernel.rusage(pid), std::time::Instant::now()),
            };
            let coalition = if opts.coalitions {
                ids.of(key, version, || kernel.coalition(key))
            } else {
                None
            };
            let mut scheduling = opts.scheduling.then(|| scheduling(kernel, pid, info.flags));
            // A session id moves only by `setsid` (xnu's `setsid_internal`, which `posix_spawn`'s
            // `POSIX_SPAWN_SETSID` calls too), which makes the process its new session's leader
            // and its new group's (its sid and its group its pid) and is refused to a group's
            // leader, and `setpgid` moves a process only within its session and is refused to a
            // session's leader. So a kept sid that is the pid is the pid still, and where the
            // group is not the pid no `setsid` came since the kept sid was read; only a group's
            // leader that leads no session (a shell's job) is asked again.
            let sid = kept
                .sid
                .filter(|&sid| sid == key || info.pgid != key)
                .and_then(|sid| c_int::try_from(sid).ok())
                .unwrap_or_else(|| kernel.sid(pid));
            let end = info.comm.iter().position(|b| *b == 0).unwrap_or(16);
            let comm = &info.comm[..end];
            let whole = || super::whole_name(comm, SHORT_COMM_CUT, longer_names(kernel, pid));
            // A name at the cut costs its longer readers. Each names the file the process last
            // exec'd (one an exec is still setting can name the file before it, as the bracket
            // above says), and an exec moves its pid version, so a name read under this version
            // and short name is kept for the sweeps that read both again.
            let name = match version {
                Some(version) if opts.coalitions && comm.len() == SHORT_COMM_CUT => {
                    ids.name(key, version, comm, whole)
                }
                _ => whole(),
            };
            let (read, read_at) = if let Some(version) = version {
                match kernel.version(pid) {
                    Some(last) if last == version => {}
                    Some(_) => {
                        ids.forget(key);
                        continue;
                    }
                    None => {
                        ids.forget(key);
                        dead.push((pid, read.ok()));
                        continue;
                    }
                }
                ids.keep_generation(
                    key,
                    version,
                    crate::coalitions::Generation {
                        sid: u32::try_from(sid).ok().filter(|sid| *sid > 0),
                        refused: (matches!(read, Err(libc::EPERM)) && info.uid != own)
                            .then_some(info.uid),
                    },
                );
                (read, read_at)
            } else {
                // A first read the kernel refused named no start, so it is not made again: the
                // row keeps that refusal and no counters, whatever a second read would answer.
                let after = match read {
                    Err(libc::EPERM) => read,
                    _ => kernel.rusage(pid),
                };
                match (read, after) {
                    (Ok(before), Ok(after))
                        if before.ri.ri_proc_start_abstime == after.ri.ri_proc_start_abstime =>
                    {
                        (Ok(after), std::time::Instant::now())
                    }
                    (Err(_), Err(_)) => (after, read_at),
                    _ => {
                        ids.forget(key);
                        continue;
                    }
                }
            };
            if let Some(scheduling) = scheduling.as_mut() {
                scheduling.qos_cpu_ns = match read {
                    Err(errno) => Some(Err(errno)),
                    Ok(u) => super::kernel_timebase()
                        .map(|(numer, denom)| Ok(qos_cpu(&u.ri, numer, denom))),
                };
            }
            let usage = read.ok();
            procs.push(ProcessReading {
                scheduling,
                ppid: info.ppid,
                name,
                pgid: info.pgid,
                uid: Some(info.uid),
                state: RunState::from_bsd(info.status),
                session_leader: (sid > 0).then_some(sid == pid),
                sid: u32::try_from(sid).unwrap_or(0),
                has_tty: Some(info.flags & PROC_FLAG_CONTROLT != 0),
                exec: Some(info.flags & PROC_FLAG_EXEC != 0),
                cores: usage.and_then(|u| cores_of(&u, numer, denom)),
                read_at: Some(read_at),
                coalition,
                version: version.filter(|_| opts.coalitions || (opts.versions && usage.is_none())),
                birth_ms: None,
                reaps_unwaited: None,
                ..usage_row(key, usage.as_ref(), numer, denom)
            });
        }
        procs.reverse();
        // A zombie answers no `proc_pidinfo` flavor; the sysctl does, and `proc_pid_rusage` its
        // final usage (its CPU times included: xnu reads a zombie's usage off its exit record).
        // Its usage read again now, after every live process, under the start the first read
        // named, is kept: no reap between the reads moved it to its parent, and a pid reused
        // between them lends none of it.
        for (pid, before) in dead.into_iter().rev() {
            let Some(k) = kernel.kinfo(pid) else {
                continue;
            };
            let last = before
                .zip(kernel.rusage(pid).ok())
                .filter(|(b, a)| a.ri.ri_proc_start_abstime == b.ri.ri_proc_start_abstime)
                .map(|(_, a)| a);
            procs.push(ProcessReading {
                pid: pid as u32,
                start: last.map(|u| u.ri.ri_proc_start_abstime),
                cores: last.and_then(|u| cores_of(&u, numer, denom)),
                ppid: k.ppid,
                cpu_ns: last.map(|u| cpu_of(&u.ri, numer, denom)),
                child_cpu_ns: last.map(|u| child_cpu_of(&u.ri, numer, denom)),
                footprint: None,
                disk_read: last.map(|u| u.ri.ri_diskio_bytesread),
                disk_written: last.map(|u| u.ri.ri_diskio_byteswritten),
                energy_nj: last.and_then(|u| u.energy_nj()),
                name: super::whole_name(
                    k.comm.as_bytes(),
                    KINFO_COMM_CUT,
                    longer_names(kernel, pid),
                ),
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
        ids.end_sweep();
        read_unwaited(kernel, &mut procs);
        Some(Sweep { procs, truncated })
    }

    /// Past this many processes to read [`ProcessReading::reaps_unwaited`] of, one read of the
    /// whole table is cheaper than a `kinfo_proc` each: one cost 6.7 µs of CPU and the table's
    /// 749 records 96–115 µs (m22, 2026-10-25), as for births (the host's `BIRTHS_AT_ONCE`).
    const UNWAITED_AT_ONCE: usize = 12;

    /// Reads [`ProcessReading::reaps_unwaited`] of every live process (a zombie's came with its
    /// record): of one that has reaped nothing yet too, since the kernel may reap its first
    /// children for it and it may go before a reading shows their doubled time, and of one whose
    /// usage goes unread (another user's `sudo`, which may reap a member and pass its time up
    /// later). From the whole table, or from each one's `kinfo_proc` where the sweep holds no
    /// more than [`UNWAITED_AT_ONCE`]; one whose record names another parent is a new process at
    /// the pid and stays unread. Where the table was wanted and not read, every one stays unread:
    /// a table the kernel did not answer in time ([`crate::kproc::records`]) waits on a process
    /// in the middle of an exec, and its own `kinfo_proc` would wait on it too.
    fn read_unwaited(kernel: &impl Reads, procs: &mut [ProcessReading]) {
        read_unwaited_by(
            procs,
            || kernel.unwaited(),
            |pid| {
                c_int::try_from(pid)
                    .ok()
                    .and_then(|pid| kernel.kinfo(pid))
                    .map(|k| (k.ppid, k.reaps_unwaited))
            },
        );
    }

    /// [`read_unwaited`] by `table`, the whole table's `(pid, ppid, reaps unwaited)` rows, and
    /// `one`, a pid's `(ppid, reaps unwaited)`.
    pub(super) fn read_unwaited_by(
        procs: &mut [ProcessReading],
        table: impl FnOnce() -> Option<Vec<(u32, u32, bool)>>,
        one: impl Fn(u32) -> Option<(u32, bool)>,
    ) {
        let wanted: Vec<usize> = procs
            .iter()
            .enumerate()
            .filter(|(_, p)| p.reaps_unwaited.is_none())
            .map(|(i, _)| i)
            .collect();
        let table = (wanted.len() > UNWAITED_AT_ONCE).then(|| {
            table().map(|all| {
                all.into_iter()
                    .map(|(pid, ppid, flag)| (pid, (ppid, flag)))
                    .collect::<std::collections::HashMap<u32, (u32, bool)>>()
            })
        });
        for i in wanted {
            let p = &mut procs[i];
            let row = match &table {
                Some(Some(all)) => all.get(&p.pid).copied(),
                Some(None) => None,
                None => one(p.pid),
            };
            p.reaps_unwaited = row
                .filter(|(ppid, _)| *ppid == p.ppid)
                .map(|(_, flag)| flag);
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

/// Test support (macOS and Linux): a CPU-time reader checked against the kernel's own account of
/// the same CPU, for the sweep's tests here and the job probe's (`jobs/tests.rs`).
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "procs_stopped_spinner_tests.rs"]
pub(crate) mod stopped_spinner;

#[cfg(test)]
#[path = "procs_tests.rs"]
mod tests;
