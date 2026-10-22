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
//!   time, `ri_phys_footprint` and its lifetime peak, runnable time and the
//!   disk bytes `ri_diskio_bytesread` and `ri_diskio_byteswritten`, readable
//!   for the user's own processes; another user's answer `None`).
//!   `ri_user_time` and `ri_system_time` count the kernel's Mach
//!   absolute-time ticks, not nanoseconds, on Apple Silicon (the timebase is
//!   125/3 there and 1/1 on Intel), so they are scaled by the kernel's
//!   timebase ([`kernel_timebase`]), which a process Rosetta translates does
//!   not see as its own. A sweep asked for coalitions
//!   ([`SweepOpts::coalitions`]) reads each process's resource coalition too
//!   (`proc_pidinfo(PROC_PIDCOALITIONINFO)`, [`crate::coalitions`]).
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
    /// Kernel start identity: Mach absolute ticks on macOS, boot nanoseconds
    /// on Linux, creation FILETIME on Windows. Compared only within one boot.
    pub start: Option<u64>,
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
    /// Time its threads spent runnable (running or waiting for a core) since it
    /// started, nanoseconds (macOS `ri_runnable_time`); `None` where refused or
    /// not read. Runnable less CPU time is time spent waiting for a core.
    pub runnable_ns: Option<u64>,
    /// The largest footprint it has had (macOS
    /// `ri_lifetime_max_phys_footprint`); `None` where refused or not read.
    pub peak_footprint: Option<u64>,
    /// When its counters were read. A sweep of hundreds of processes takes
    /// long enough (and is descheduled unevenly enough on a loaded machine)
    /// that a rate over the sweep-to-sweep window misstates a process read
    /// early in one sweep and late in the next; two of these give each
    /// process its own window.
    pub read_at: Option<std::time::Instant>,
    /// Its resource coalition's id (macOS), read only by a sweep asked for
    /// it ([`SweepOpts::coalitions`]); `None` otherwise, for a zombie, and
    /// off macOS.
    pub coalition: Option<u64>,
}

/// What a sweep reads beyond each process's row ([`processes_with`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepOpts {
    /// Each process's resource coalition ([`ProcessReading::coalition`]):
    /// one more `proc_pidinfo` a process on macOS, nothing elsewhere.
    pub coalitions: bool,
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

/// Sweep every process, reading what `opts` asks for too; `None` where the
/// platform has no reader.
#[must_use]
pub fn processes_with(opts: SweepOpts) -> Option<Sweep> {
    imp::processes(opts)
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

/// The timebase `(numer, denom)` of a Mach clock that ticks `hz` times a
/// second, reduced: `ticks * numer / denom` is nanoseconds (24 MHz is 125/3,
/// 1 GHz is 1/1). `None` for no frequency, or one whose reduced terms do not
/// fit a `u32`.
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

/// The kernel's own Mach timebase `(numer, denom)`: what turns the tick
/// counts the kernel records for a task (`proc_pid_rusage`'s CPU and runnable
/// times, `PROC_PIDTASKINFO`'s `pti_total_*`) into nanoseconds. A thread's
/// `PROC_PIDTHREADINFO` times are nanoseconds already and take no scale.
/// Read from the tick frequency the kernel publishes,
/// `hw.tbfrequency` (24 MHz on Apple silicon, so 125/3; 1 GHz on Intel, so
/// 1/1), because `mach_timebase_info` answers this process's own clock: under
/// Rosetta that is 1/1 while the kernel still counts 24 MHz ticks, and a CPU
/// time scaled by it read 125/3 too small (measured 2026-10-20,
/// `docs/measured/rosetta-mac-2026-10-20.md`). `mach_timebase_info` only
/// where the sysctl answers nothing; `None` when neither answers. Read once.
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

    /// The kernel's Mach timebase `(numer, denom)` ([`super::kernel_timebase`]),
    /// 1/1 when it cannot be read.
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

    pub(super) fn processes(opts: SweepOpts) -> Option<Sweep> {
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
            let before = rusage(pid);
            let Some(info) = short_info(pid) else {
                // A zombie answers no `proc_pidinfo` flavor; the sysctl does.
                if let Some(k) = crate::jobs::macos_kinfo(pid) {
                    procs.push(ProcessReading {
                        pid: pid as u32,
                        start: None,
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
                        ..ProcessReading::default()
                    });
                }
                continue;
            };
            // Read between the two usage reads, so where they answer, their
            // start-time check below holds it to the same process.
            let coalition = opts
                .coalitions
                .then(|| crate::coalitions::coalition_of(pid as u32))
                .flatten();
            let after = rusage(pid);
            let usage = match (before, after) {
                (Some(before), Some(after))
                    if before.ri_proc_start_abstime == after.ri_proc_start_abstime =>
                {
                    Some(after)
                }
                (None, None) => None,
                // The pid exited, was reused, or became unreadable while its
                // parent and counters were read. Never splice those records.
                _ => continue,
            };
            let read_at = Some(std::time::Instant::now());
            // The short info's flags never carry `PROC_FLAG_SLEADER` (xnu
            // sets it only in the full `PROC_PIDTBSDINFO`, which is refused
            // for another user's process): the session is asked for, and
            // `getsid` answers for any process.
            // SAFETY: a plain syscall on an integer.
            let sid = unsafe { libc::getsid(pid) };
            let end = info.comm.iter().position(|b| *b == 0).unwrap_or(16);
            procs.push(ProcessReading {
                pid: pid as u32,
                start: usage.map(|u| u.ri_proc_start_abstime),
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
                runnable_ns: usage.map(|u| ticks_to_ns(u.ri_runnable_time, numer, denom)),
                peak_footprint: usage.map(|u| u.ri_lifetime_max_phys_footprint),
                read_at,
                coalition,
            });
        }
        Some(Sweep { procs, truncated })
    }
}

#[cfg(target_os = "linux")]
mod imp {
    pub(super) fn processes(_opts: super::SweepOpts) -> Option<super::Sweep> {
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
    pub(super) fn processes(_opts: super::SweepOpts) -> Option<super::Sweep> {
        crate::win::processes()
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    all(windows, target_pointer_width = "64")
)))]
mod imp {
    pub(super) fn processes(_opts: super::SweepOpts) -> Option<super::Sweep> {
        None
    }
}

/// Test support (macOS): a CPU-time reader checked against the kernel's own
/// account of the same CPU, for the sweep's tests here and the job probe's
/// (`jobs/tests.rs`).
#[cfg(all(test, target_os = "macos"))]
pub(crate) mod stopped_spinner {
    use std::time::{Duration, Instant};

    /// `struct rusage` (`<sys/resource.h>`): the two times, then fourteen
    /// `long` counters these tests do not read; 144 bytes on Darwin.
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

    /// Keeps the spinner owned until a successful manual reap. Dropping the
    /// ordinary handle alone only gives a running child to a passive reaper.
    struct OwnedSpinner(Option<aterm_uds::spawnfd::Child>);

    impl OwnedSpinner {
        fn forget_reaped(&mut self) {
            if let Some(child) = self.0.take() {
                // The raw wait already reaped it. Do not let the handle's
                // drop ask for a PID that another child may now reuse.
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
                // Still our unreaped child, even if the read panicked while
                // it was stopped. The handle caches this reap before drop.
                let _ = child.wait();
            }
        }
    }

    /// Spins a `/bin/sh` child until `ready(pid)`, stops it, hands its pid to
    /// `read` while it is stopped, then kills it and reaps it with `wait4`.
    /// Returns what `read` returned and the reap's user plus system CPU time
    /// in nanoseconds, which counts the CPU `read` saw, at any load.
    ///
    /// `ready` decides nothing: a reader off by a tick factor ends the spin
    /// early or late, and the caller's comparison catches either. Its
    /// one-minute deadline is a hang detector for a reader that never moves,
    /// which the comparison then fails.
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
        // The guard retains cleanup through either reader, including a
        // panic after this stop.
        // SAFETY: `kill` takes no pointers; `pid` is this test's unreaped child.
        let stop = unsafe { libc::kill(pid, libc::SIGSTOP) };
        let mut stopped: libc::c_int = 0;
        // SAFETY: `stopped` is a live out-param; `pid` is this test's own
        // child, and nothing else in this process waits on it.
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
        // SAFETY: live out-params of the declared layouts; a successful
        // reap disarms the handle below before any assertion or drop.
        let reaped = unsafe { wait4(pid, &mut status, 0, usage.as_mut_ptr()) };
        if reaped == pid {
            child.forget_reaped();
        }
        assert_eq!(
            (stop, waited, kill, reaped),
            (0, pid, 0, pid),
            "stop, wait for the stop, kill, reap"
        );
        // SAFETY: zeroed, then filled by the successful `wait4` above; every
        // field is a plain integer.
        let usage = unsafe { usage.assume_init() };
        let kernel = (micros(usage.utime) + micros(usage.stime)) * 1_000;
        assert!(kernel > 0, "the child ran");
        (answer, kernel)
    }

    /// Whether `read` is within 2x of `kernel`, either way: far inside a Mach
    /// tick factor (125/3 on Apple silicon).
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
    fn a_tick_frequency_reduces_to_its_timebase() {
        assert_eq!(timebase_of_frequency(24_000_000), Some((125, 3)));
        assert_eq!(timebase_of_frequency(1_000_000_000), Some((1, 1)));
        assert_eq!(timebase_of_frequency(19_200_000), Some((625, 12)));
        assert_eq!(timebase_of_frequency(0), None);
        assert_eq!(timebase_of_frequency(u64::MAX), None, "terms past u32");
    }

    /// The sweep's CPU time is the kernel's, in nanoseconds, however this
    /// process runs: a spinning child, stopped so that both readings count the
    /// same CPU at any load, reads within 2x of what `wait4` reports for it.
    /// Under Rosetta, a sweep scaled by this process's own `mach_timebase_info`
    /// (1/1 there, [`kernel_timebase`]) read 125/3 too little, far outside the
    /// band (measured 2026-10-20 on the same check in aterm-gui's
    /// `pid_cpu_ns`).
    #[cfg(target_os = "macos")]
    #[test]
    fn the_sweep_reads_a_stopped_childs_cpu_time_as_wait4_counts_it() {
        use super::stopped_spinner::{read, within_2x};
        let swept = |pid: u32| {
            processes()
                .and_then(|s| s.procs.into_iter().find(|p| p.pid == pid))
                .and_then(|p| p.cpu_ns)
        };
        // Burn until the sweep says 50 ms.
        let (swept_ns, kernel) = read(|pid| swept(pid).is_some_and(|ns| ns >= 50_000_000), swept);
        let swept_ns = swept_ns.expect("the sweep reads its own child");
        assert!(
            within_2x(swept_ns, kernel),
            "the sweep read {swept_ns} ns; the reap's rusage for the same stopped child is \
             {kernel} ns (timebase {:?})",
            kernel_timebase()
        );
    }

    /// A sweep asked for coalitions reads each process's, as the one-process
    /// reader does, another user's too (launchd's, root's); a plain sweep
    /// reads none.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_sweep_reads_coalitions_only_when_asked() {
        let sweep = processes_with(SweepOpts { coalitions: true }).expect("a sweep");
        for pid in [std::process::id(), 1] {
            let row = sweep.procs.iter().find(|p| p.pid == pid).expect("swept");
            let own = crate::coalitions::coalition_of(pid).expect("its coalition");
            assert_eq!(row.coalition, Some(own), "{pid}");
        }
        assert!(
            processes()
                .expect("a sweep")
                .procs
                .iter()
                .all(|p| p.coalition.is_none())
        );
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
