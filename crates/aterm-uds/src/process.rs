// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process facts from the kernel: pid liveness (for the stale-socket sweep,
//! where `aterm-<pid>.sock` files of dead pids are garbage), a process's start
//! record on the wall and boot clocks, and [`kill_self_now`].

/// Whether `pid` names a live process.
///
/// * Unix: `kill(pid, 0)`; success and `EPERM` both mean alive. Pids that
///   cannot be real (0, or wider than `pid_t`) are dead.
/// * Windows: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE)`
///   plus `WaitForSingleObject(h, 0) == WAIT_TIMEOUT` means alive;
///   `ERROR_ACCESS_DENIED` also means alive (the pid exists but is not ours,
///   like `EPERM`); anything else is dead.
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

/// Whether `pid` names a live process (see the Unix twin's docs).
#[must_use]
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use crate::win::ffi;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const ERROR_ACCESS_DENIED: u32 = 5;
    const WAIT_TIMEOUT: u32 = 258;
    const STILL_ACTIVE: u32 = 259;
    // The one kernel32 import this predicate needs beyond `win::ffi`.
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
    // Unsignaled (WAIT_TIMEOUT) means still running; `GetExitCodeProcess == STILL_ACTIVE`
    // alone is not enough, since a process can exit with code 259. Windows reuses pids
    // quickly, so "alive" is advisory; aterm-gui's `socket_is_live` is the authoritative probe.
    let mut code: u32 = 0;
    let alive = unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT
        && unsafe { ffi::GetExitCodeProcess(handle, &mut code) } != 0
        && code == STILL_ACTIVE;
    let _ = unsafe { ffi::CloseHandle(handle) };
    alive
}

/// A process's kernel start time: the identity that tells a pid from the next
/// process given the same number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessBirth {
    /// Start time, unix seconds.
    pub seconds: u64,
    /// The microseconds within `seconds`.
    pub microseconds: u64,
}

/// Read the kernel's start-time record for `pid`, or `None` when there is no
/// process of this uid to name there.
///
/// `None` covers every case callers refuse alike: no such process, a zombie,
/// a process of another uid, or the kernel naming a different pid than asked.
/// `pid <= 1` is always `None`.
///
/// macOS: `proc_pidinfo(PROC_PIDTBSDINFO)`, exact-size read. Elsewhere `None`:
/// Linux keeps no wall-clock start record; its boot-clock record is
/// [`read_process_start`].
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn read_process_birth(pid: i32) -> Option<ProcessBirth> {
    read_bsdinfo(pid).map(|info| info.birth)
}

/// The pid of `pid`'s parent, or `None` on the terms of [`read_process_birth`]. `Some(1)`: its
/// launcher has exited and launchd adopted it.
///
/// macOS: `proc_pidinfo(PROC_PIDTBSDINFO)`'s `pbi_ppid`. Other platforms: `None`.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn read_process_parent(pid: i32) -> Option<u32> {
    read_bsdinfo(pid).map(|info| info.parent)
}

/// See the Apple twin. Linux: the `PPid:` line of `/proc/<pid>/status`, read between
/// two [`read_process_start`] reads that must agree, so the pid named one live process
/// of this uid throughout. `Some(1)`: re-parented to init. Under a user manager that
/// is a child subreaper (`systemd --user`), an orphan is re-parented to it instead, so
/// `1` is rarer here than launchd's adoption on macOS.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn read_process_parent(pid: i32) -> Option<u32> {
    let start = read_process_start(pid)?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let parent = procfs::parent(&status)?;
    (read_process_start(pid)? == start).then_some(parent)
}

/// See the Apple twin: no parent-pid primitive is wired here.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
#[must_use]
pub fn read_process_parent(_pid: i32) -> Option<u32> {
    None
}

/// The executable the kernel records for `pid` (`proc_pidpath`), on the terms of
/// [`read_process_birth`], and `None` when the kernel cannot name the file any more:
/// `proc_pidpath` returns 0 for a live process whose binary was deleted. So a live process
/// ([`read_process_birth`] is `Some`) with no path here runs a deleted binary. Other
/// platforms: `None`.
#[cfg(target_vendor = "apple")]
#[must_use]
// Skip: bottoms out at the `proc_pidpath` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn read_process_path(pid: i32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // PROC_PIDPATHINFO_MAXSIZE (sys/proc_info.h).
    const MAX: usize = 4096;
    unsafe extern "C" {
        fn proc_pidpath(pid: i32, buffer: *mut core::ffi::c_void, buffersize: u32) -> i32;
    }
    read_bsdinfo(pid)?;
    let mut buf = [0u8; MAX];
    // SAFETY: `buf` is MAX writable bytes; libproc writes at most that many and
    // returns the path's length, or <= 0 on failure.
    let len = unsafe { proc_pidpath(pid, buf.as_mut_ptr().cast(), MAX as u32) };
    let len = usize::try_from(len).ok().filter(|&n| n > 0 && n <= MAX)?;
    Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
        buf.get(..len)?,
    )))
}

/// See the Apple twin, whose contract this keeps: Linux reads the `/proc/<pid>/exe`
/// link between two [`read_process_start`] reads that must agree, and answers `None`
/// for a live process whose binary was deleted — the kernel then appends ` (deleted)`
/// to the name it kept ([`procfs::live_exe`]), where macOS names no path at all.
/// A link the kernel refuses to read answers `None` too: for a live process of this
/// uid, a non-dumpable one (a set-id exec, `PR_SET_DUMPABLE` 0) or a security
/// module's denial. aterm never makes itself non-dumpable, so for a live aterm
/// process `None` means deleted unless such a module refuses the read.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn read_process_path(pid: i32) -> Option<std::path::PathBuf> {
    let start = read_process_start(pid)?;
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let exe = procfs::live_exe(exe)?;
    (read_process_start(pid)? == start).then_some(exe)
}

/// See the Apple twin: no executable-path primitive is wired here.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
#[must_use]
pub fn read_process_path(_pid: i32) -> Option<std::path::PathBuf> {
    None
}

/// The `proc_bsdinfo` fields this module reads, for a live process of this uid.
#[cfg(target_vendor = "apple")]
struct BsdInfo {
    birth: ProcessBirth,
    parent: u32,
}

/// `proc_pidinfo(PROC_PIDTBSDINFO)` for `pid`, on the terms
/// [`read_process_birth`] states.
#[cfg(target_vendor = "apple")]
// Skip: bottoms out at the `proc_pidinfo`/`geteuid` FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
fn read_bsdinfo(pid: i32) -> Option<BsdInfo> {
    // `struct proc_bsdinfo` (sys/proc_info.h): 136 bytes on both Apple ABIs (the SDK's
    // `offsetof`). Read as bytes at those offsets, so this leaf crate stays dependency-free
    // and a size change fails the exact-size check below instead of misreading a field.
    const PROC_PIDTBSDINFO: i32 = 3;
    const SIZE: usize = 136;
    const STATUS: usize = 4;
    const PID: usize = 12;
    const PPID: usize = 16;
    const UID: usize = 20;
    const START_SEC: usize = 120;
    const START_USEC: usize = 128;
    const SZOMB: u32 = 5;
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut core::ffi::c_void,
            buffersize: i32,
        ) -> i32;
        fn geteuid() -> u32;
    }
    if pid <= 1 {
        return None;
    }
    let mut info = [0u8; SIZE];
    // SAFETY: `info` is SIZE writable bytes, exactly the structure this flavor
    // fills; libproc returns the number of bytes it wrote.
    let read = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            SIZE as i32,
        )
    };
    if read != SIZE as i32 {
        return None;
    }
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_ne_bytes(info.get(at..at + 4)?.try_into().ok()?))
    };
    let u64_at = |at: usize| -> Option<u64> {
        Some(u64::from_ne_bytes(info.get(at..at + 8)?.try_into().ok()?))
    };
    // SAFETY: `geteuid` is a side-effect-free getter.
    let ours = unsafe { geteuid() };
    if u32_at(PID)? != u32::try_from(pid).ok()? || u32_at(UID)? != ours {
        return None;
    }
    if u32_at(STATUS)? == SZOMB {
        return None;
    }
    Some(BsdInfo {
        birth: ProcessBirth {
            seconds: u64_at(START_SEC)?,
            microseconds: u64_at(START_USEC)?,
        },
        parent: u32_at(PPID)?,
    })
}

/// See the Apple twin: no birth-record primitive is wired here.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn read_process_birth(_pid: i32) -> Option<ProcessBirth> {
    None
}

/// A reading of the kernel's boot clock, the monotonic clock this platform
/// records a process's start on. It never steps with the wall clock, so two
/// readings order two moments.
///
/// The unit is the platform's own, so a reading compares only with another
/// from this machine since this boot:
/// * Apple: the kernel's mach absolute ticks, the unit of `proc_pid_rusage`'s
///   `ri_proc_start_abstime` and of a native process's `mach_absolute_time`.
///   It pauses in sleep; order holds across it. Under Rosetta
///   `mach_absolute_time` reads nanoseconds instead, so [`boot_clock_now`]
///   answers `None` there ([`apple_boot_clock`]).
/// * Linux: nanoseconds of `CLOCK_BOOTTIME`, the clock `/proc/<pid>/stat`
///   counts `starttime` on (in clock ticks, [`procfs::tick_start`]); both are
///   read through the reader's time namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BootClock(pub u64);

/// Whether this process runs translated (Rosetta), as the kernel says
/// (`sysctl.proc_translated`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Translation {
    /// Native: `0`, or no such sysctl (an Intel Mac before macOS 11).
    Native,
    /// Translated: `1`.
    Translated,
    /// The sysctl answered something else, or failed another way.
    Unknown,
}

/// This process's [`Translation`] (Apple; `Native` elsewhere).
#[cfg(target_vendor = "apple")]
#[must_use]
// Skip: bottoms out at the `sysctlbyname` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn translation() -> Translation {
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const core::ffi::c_char,
            oldp: *mut core::ffi::c_void,
            oldlenp: *mut usize,
            newp: *mut core::ffi::c_void,
            newlen: usize,
        ) -> i32;
    }
    const ENOENT: i32 = 2;
    let mut value: i32 = 0;
    let mut len = core::mem::size_of::<i32>();
    // SAFETY: a NUL-terminated name, one writable `int` and its length, all
    // live for the call; nothing is written to the kernel (`newp` null).
    let rc = unsafe {
        sysctlbyname(
            c"sysctl.proc_translated".as_ptr(),
            (&raw mut value).cast(),
            &raw mut len,
            core::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return if std::io::Error::last_os_error().raw_os_error() == Some(ENOENT) {
            Translation::Native
        } else {
            Translation::Unknown
        };
    }
    match (len == core::mem::size_of::<i32>(), value) {
        (true, 0) => Translation::Native,
        (true, 1) => Translation::Translated,
        _ => Translation::Unknown,
    }
}

/// See the Apple twin: nothing translates a process here.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn translation() -> Translation {
    Translation::Native
}

/// The Apple boot clock for a process whose [`Translation`] is `translation`,
/// read by `ticks` (`mach_absolute_time`). `None` when translated: the reading
/// is in nanoseconds, not the kernel's ticks, so callers order start records on
/// the wall clock instead. `Unknown` reads the clock: if it were translated, its
/// nanoseconds would only read later than every start in ticks, which ends a
/// stream rather than moving it.
#[must_use]
pub fn apple_boot_clock(
    translation: Translation,
    ticks: impl FnOnce() -> u64,
) -> Option<BootClock> {
    match translation {
        Translation::Translated => None,
        Translation::Native | Translation::Unknown => Some(BootClock(ticks())),
    }
}

/// The boot clock now ([`BootClock`]), or `None` where this process cannot
/// read it in the kernel's unit: a platform with none wired (Windows, the BSDs,
/// 32-bit Linux), or an Apple process Rosetta translates ([`apple_boot_clock`]).
#[cfg(target_vendor = "apple")]
#[must_use]
// Skip: bottoms out at the `mach_absolute_time` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn boot_clock_now() -> Option<BootClock> {
    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
    }
    // SAFETY: a side-effect-free getter with no arguments.
    apple_boot_clock(translation(), || unsafe { mach_absolute_time() })
}

/// See the Apple twin.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    target_pointer_width = "64"
))]
#[must_use]
// Skip: bottoms out at the `clock_gettime` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn boot_clock_now() -> Option<BootClock> {
    // `struct timespec` on every 64-bit Linux ABI: `time_t` and `long` are both
    // 64 bits (32-bit ABIs vary, so they are not wired).
    #[repr(C)]
    struct Timespec {
        tv_sec: core::ffi::c_long,
        tv_nsec: core::ffi::c_long,
    }
    unsafe extern "C" {
        fn clock_gettime(clock: i32, tp: *mut Timespec) -> i32;
    }
    const CLOCK_BOOTTIME: i32 = 7;
    let mut now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is one writable timespec that outlives the call.
    if unsafe { clock_gettime(CLOCK_BOOTTIME, &mut now) } != 0 {
        return None;
    }
    u64::try_from(now.tv_sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u64::try_from(now.tv_nsec).ok()?)
        .map(BootClock)
}

/// See the Apple twin: no boot clock wired here.
#[cfg(not(any(
    target_vendor = "apple",
    all(
        any(target_os = "linux", target_os = "android"),
        target_pointer_width = "64"
    )
)))]
#[must_use]
pub fn boot_clock_now() -> Option<BootClock> {
    None
}

/// When a process started, as the kernel recorded it ([`read_process_start`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessStart {
    /// On the boot clock in the kernel's unit, and never later than the real
    /// start: exact on Apple; on Linux the first instant of the clock tick
    /// `starttime` names. It orders against a [`boot_clock_now`] reading only
    /// where that answers (not under Rosetta, see [`BootClock`]).
    pub boot: BootClock,
    /// The wall-clock record where the kernel keeps one ([`ProcessBirth`],
    /// Apple); `None` on Linux, whose only wall conversion is `/proc/stat`'s
    /// `btime` in whole seconds.
    pub wall: Option<ProcessBirth>,
}

/// Read the kernel's start record for `pid` on the boot clock, or `None` for
/// the same cases as [`read_process_birth`] (no such process, a zombie,
/// another uid, a record naming another pid, `pid <= 1`).
///
/// Apple: `proc_pid_rusage(RUSAGE_INFO_V0)`'s `ri_proc_start_abstime`, between
/// two [`read_process_birth`] reads that must agree, so the pid named one
/// process across both and the uid and zombie checks hold for it.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn read_process_start(pid: i32) -> Option<ProcessStart> {
    let wall = read_process_birth(pid)?;
    let boot = start_abstime(pid)?;
    (read_process_birth(pid)? == wall).then_some(ProcessStart {
        boot,
        wall: Some(wall),
    })
}

/// `ri_proc_start_abstime` of `pid` (`struct rusage_info_v0`, sys/resource.h:
/// 96 bytes, the field at offset 80).
#[cfg(target_vendor = "apple")]
// Skip: bottoms out at the `proc_pid_rusage` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
fn start_abstime(pid: i32) -> Option<BootClock> {
    const RUSAGE_INFO_V0: i32 = 0;
    const START_WORD: usize = 80 / 8;
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut core::ffi::c_void) -> i32;
    }
    // 96 bytes, 8-aligned: exactly the v0 structure.
    let mut info = [0u64; 12];
    // SAFETY: `info` is 96 writable, aligned bytes — the size of the structure
    // this flavor fills — and outlives the call.
    if unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V0, info.as_mut_ptr().cast()) } != 0 {
        return None;
    }
    Some(BootClock(*info.get(START_WORD)?))
}

/// See the Apple twin. Linux: `starttime` (field 22) of `/proc/<pid>/stat`,
/// the state and uid from `/proc/<pid>/stat` and `/proc/<pid>/status`, and
/// `stat` read again after `status`, which must name the same start
/// ([`procfs::start`]).
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn read_process_start(pid: i32) -> Option<ProcessStart> {
    if pid <= 1 {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let again = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (euid, per_second) = linux_reader()?;
    let boot = procfs::start(&stat, &status, &again, pid, euid, per_second)?;
    Some(ProcessStart { boot, wall: None })
}

/// This process's effective uid and the kernel's clock ticks per second
/// (`USER_HZ`, the unit of `starttime`), for [`procfs::start`].
///
/// The tick rate comes from `getauxval(AT_CLKTCK)` (`AT_CLKTCK` is 17 in the
/// kernel ABI whatever the C library), not `sysconf(_SC_CLK_TCK)`, whose
/// number differs between C libraries. `0` (no entry) reads as no rate, so no
/// start.
#[cfg(any(target_os = "linux", target_os = "android"))]
// Skip: bottoms out at the `geteuid`/`getauxval` FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
#[cfg_attr(
    target_pointer_width = "64",
    expect(
        clippy::useless_conversion,
        reason = "c_ulong is u64 on LP64 and u32 on 32-bit"
    )
)]
fn linux_reader() -> Option<(u32, u64)> {
    unsafe extern "C" {
        fn geteuid() -> u32;
        fn getauxval(kind: core::ffi::c_ulong) -> core::ffi::c_ulong;
    }
    const AT_CLKTCK: core::ffi::c_ulong = 17;
    // SAFETY: two side-effect-free getters.
    let (euid, ticks) = unsafe { (geteuid(), getauxval(AT_CLKTCK)) };
    (ticks > 0).then_some((euid, u64::from(ticks)))
}

/// See the Apple twin: no start record wired here.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
#[must_use]
pub fn read_process_start(_pid: i32) -> Option<ProcessStart> {
    None
}

/// Linux's record of a process's start, judged from the text of its `/proc`
/// files. Pure and public on every platform, so the parse is tested on any
/// host; [`read_process_start`] feeds it the files.
pub mod procfs {
    use super::BootClock;

    /// `(state, starttime)` from a `/proc/<pid>/stat` line (fields 3 and 22,
    /// the latter in clock ticks after boot), or `None` when the line does not
    /// parse or names another pid. The command name (field 2) may hold spaces
    /// and parentheses, so fields are counted from its last `)`.
    #[must_use]
    pub fn stat_start(stat: &str, pid: i32) -> Option<(char, u64)> {
        let open = stat.find(" (")?;
        if stat[..open].trim().parse::<i32>().ok()? != pid {
            return None;
        }
        let close = stat.rfind(')')?;
        if close < open {
            return None;
        }
        let mut fields = stat[close + 1..].split_ascii_whitespace();
        let state = fields.next()?.chars().next()?;
        // Field 3 was the state; fields 4 through 21 are skipped.
        let start = fields.nth(18)?.parse().ok()?;
        Some((state, start))
    }

    /// The parent pid in a `/proc/<pid>/status` text: its `PPid:` line.
    #[must_use]
    pub fn parent(status: &str) -> Option<u32> {
        status
            .lines()
            .find_map(|l| l.strip_prefix("PPid:"))?
            .trim()
            .parse()
            .ok()
    }

    /// The executable a `/proc/<pid>/exe` link names, or `None` when the kernel
    /// marks it ` (deleted)`: the process runs a binary no longer at that path.
    #[must_use]
    pub fn live_exe(link: std::path::PathBuf) -> Option<std::path::PathBuf> {
        (!link.as_os_str().as_encoded_bytes().ends_with(b" (deleted)")).then_some(link)
    }

    /// The effective uid in a `/proc/<pid>/status` text: the second of the four
    /// ids on its `Uid:` line (real, effective, saved, filesystem).
    #[must_use]
    pub fn effective_uid(status: &str) -> Option<u32> {
        let line = status.lines().find(|l| l.starts_with("Uid:"))?;
        line["Uid:".len()..]
            .split_ascii_whitespace()
            .nth(1)?
            .parse()
            .ok()
    }

    /// The first instant of start tick `ticks` on the boot clock, at
    /// `per_second` ticks a second: the kernel truncates a start to its tick,
    /// so the real start is at or after this, never before.
    #[must_use]
    pub fn tick_start(ticks: u64, per_second: u64) -> Option<BootClock> {
        if per_second == 0 {
            return None;
        }
        let nanos = u128::from(ticks) * 1_000_000_000 / u128::from(per_second);
        u64::try_from(nanos).ok().map(BootClock)
    }

    /// The start of `pid` from its `stat`, its `status` and `stat` read again
    /// after `status`, for a reader whose effective uid is `euid`. `None` when
    /// either `stat` names another pid or does not parse, the two disagree, the
    /// process has ended (`Z`, `X`/`x`), or its effective uid is not `euid`.
    #[must_use]
    pub fn start(
        stat: &str,
        status: &str,
        again: &str,
        pid: i32,
        euid: u32,
        per_second: u64,
    ) -> Option<BootClock> {
        let (state, ticks) = stat_start(stat, pid)?;
        if stat_start(again, pid)?.1 != ticks || matches!(state, 'Z' | 'X' | 'x') {
            return None;
        }
        if effective_uid(status)? != euid {
            return None;
        }
        tick_start(ticks, per_second)
    }
}

/// Read the argument words of one live process of this uid, between two
/// matching [`ProcessStart`] records. No display-text splitting, environment
/// return, or lossy conversion. Failure, a changed/unavailable start record,
/// unsupported platform or a resource ceiling means unknown, never a prefix.
///
/// This is an observation, not an exec lock: a process can replace its image
/// or rewrite its argument memory without changing its start record. Darwin's
/// KERN_PROCARGS2 layout uses NUL padding before a normal nonempty argv[0];
/// it cannot distinguish a deliberately empty argv[0] from that padding.
#[must_use]
pub fn read_process_argv(pid: i32) -> Option<Vec<std::ffi::OsString>> {
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
    {
        argv_of_same_process(pid, read_process_start, read_process_argv_inner)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    all(test, unix)
))]
fn argv_of_same_process(
    pid: i32,
    mut start: impl FnMut(i32) -> Option<ProcessStart>,
    argv: impl FnOnce(i32) -> Option<Vec<std::ffi::OsString>>,
) -> Option<Vec<std::ffi::OsString>> {
    if pid <= 1 {
        return None;
    }
    let before = start(pid)?;
    let words = argv(pid)?;
    (start(pid)? == before).then_some(words)
}

// Diagnostic ceilings, not kernel ARG_MAX laws. Valid larger vectors become
// unknown. Both raw bytes and the decoded word-container allocation are bounded.
#[cfg(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    all(test, unix)
))]
const PROCESS_ARGV_MAX: usize = 8 * 1024 * 1024;
#[cfg(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    all(test, unix)
))]
const PROCESS_ARGV_WORDS: usize = 65_536;

#[cfg(target_os = "macos")]
#[cfg_attr(trust_verify, trust::skip)]
fn read_process_argv_inner(pid: i32) -> Option<Vec<std::ffi::OsString>> {
    // Local SDK sys/sysctl.h: CTL_KERN=1, KERN_ARGMAX=8, KERN_PROCARGS2=49.
    unsafe extern "C" {
        fn sysctl(
            name: *mut i32,
            namelen: u32,
            oldp: *mut core::ffi::c_void,
            oldlenp: *mut usize,
            newp: *mut core::ffi::c_void,
            newlen: usize,
        ) -> i32;
    }
    let mut argmax = 0i32;
    let mut len = std::mem::size_of::<i32>();
    let mut key = [1, 8];
    // SAFETY: owned exact-size buffers; null newp requests a read only.
    let rc = unsafe {
        sysctl(
            key.as_mut_ptr(),
            2,
            (&raw mut argmax).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len != std::mem::size_of::<i32>() {
        return None;
    }
    let cap = usize::try_from(argmax)
        .ok()
        .filter(|&n| n > std::mem::size_of::<i32>() && n <= PROCESS_ARGV_MAX)?;
    let mut bytes = vec![0u8; cap];
    let mut len = cap;
    let mut key = [1, 49, pid];
    // SAFETY: bytes owns cap writable bytes; only an in-bounds successful
    // returned length reaches the parser. No environment bytes are returned.
    let rc = unsafe {
        sysctl(
            key.as_mut_ptr(),
            3,
            bytes.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len > cap {
        return None;
    }
    argv_from_procargs2(bytes.get(..len)?)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_process_argv_inner(pid: i32) -> Option<Vec<std::ffi::OsString>> {
    use std::io::Read as _;
    let file = std::fs::File::open(format!("/proc/{pid}/cmdline")).ok()?;
    let mut bytes = Vec::new();
    file.take((PROCESS_ARGV_MAX + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    argv_from_cmdline(&bytes)
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn argv_from_procargs2(bytes: &[u8]) -> Option<Vec<std::ffi::OsString>> {
    use std::os::unix::ffi::OsStringExt as _;
    if bytes.len() > PROCESS_ARGV_MAX {
        return None;
    }
    let argc = i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?);
    let argc = usize::try_from(argc)
        .ok()
        .filter(|&n| n > 0 && n <= PROCESS_ARGV_WORDS)?;
    let rest = bytes.get(4..)?;
    let path_end = rest.iter().position(|&b| b == 0)?;
    if path_end == 0 {
        return None;
    }
    let mut at = path_end;
    while rest.get(at) == Some(&0) {
        at += 1;
    }
    if argc > rest.len().checked_sub(at)? {
        return None;
    }
    let mut argv = Vec::with_capacity(argc);
    for _ in 0..argc {
        let tail = rest.get(at..)?;
        let end = tail.iter().position(|&b| b == 0)?;
        argv.push(std::ffi::OsString::from_vec(tail.get(..end)?.to_vec()));
        at = at.checked_add(end)?.checked_add(1)?;
    }
    // The kernel's argc separates argv from its environment trailer.
    Some(argv)
}

#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn argv_from_cmdline(bytes: &[u8]) -> Option<Vec<std::ffi::OsString>> {
    use std::os::unix::ffi::OsStringExt as _;
    if bytes.is_empty() || bytes.len() > PROCESS_ARGV_MAX {
        return None;
    }
    let body = bytes.strip_suffix(b"\0")?;
    let count = body.split(|&b| b == 0).count();
    if count > PROCESS_ARGV_WORDS {
        return None;
    }
    Some(
        body.split(|&b| b == 0)
            .map(|word| std::ffi::OsString::from_vec(word.to_vec()))
            .collect(),
    )
}

/// End this process by `SIGKILL`, here: no destructor, exit handler or buffered flush runs.
/// For fault injection that must die at one exact point; a `SIGKILL` death leaves no macOS
/// crash report.
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
        // Delivery is asynchronous: in a multithreaded process this thread can return from
        // `kill` before the kernel ends the process, so it waits here, running nothing.
        loop {
            std::thread::park();
        }
    }
    // Only if the kernel refused our own pid, which it does not; keeps the `!`.
    #[expect(
        clippy::disallowed_methods,
        reason = "unreachable: the kernel never refuses SIGKILL to this process's own pid"
    )]
    std::process::abort()
}

/// End this process here, running nothing (see the Unix twin); Windows has no `SIGKILL`.
#[cfg(windows)]
pub fn kill_self_now() -> ! {
    #[expect(
        clippy::disallowed_methods,
        reason = "Windows has no SIGKILL: abort ends the process at once, and no macOS crash report exists here"
    )]
    std::process::abort()
}

/// `sigset_t`: a 32-bit mask on Darwin, 1024 bits on Linux.
#[cfg(target_vendor = "apple")]
type SigSet = u32;
#[cfg(all(unix, not(target_vendor = "apple")))]
type SigSet = [u64; 16];

/// SIGTERM, SIGINT and SIGHUP, the signals a supervisor or a terminal stops a process with
/// (launchd at logout, `launchctl bootout`, `systemctl stop`, ^C, a terminal gone away). Their
/// numbers are the same on every supported Unix.
#[cfg(unix)]
// Skip: bottoms out at the sigset FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
fn stop_set() -> SigSet {
    unsafe extern "C" {
        fn sigemptyset(set: *mut SigSet) -> i32;
        fn sigaddset(set: *mut SigSet, sig: i32) -> i32;
    }
    let mut set = SigSet::default();
    // SAFETY: `sigemptyset` initialises the set it is handed; `sigaddset` then adds valid
    // signal numbers to that initialised set.
    unsafe {
        sigemptyset(&mut set);
        for sig in [15, 2, 1] {
            sigaddset(&mut set, sig);
        }
    }
    set
}

/// TAKE THE STOP SIGNALS (SIGTERM, SIGINT, SIGHUP) so a process can stop cleanly instead of
/// dying of their default action. They are blocked in the calling thread, which must be the
/// process's only one, since every thread started after inherits the mask. One thread of
/// their own, named `name`, then takes the first with `sigwait` and hands its number to
/// `on_stop`. `false` when that thread cannot start: the mask is restored and the signals keep
/// their default action. A second signal is left pending.
#[cfg(unix)]
// wasm32 never links aterm-uds; the census's wasm closure scans it anyway (OB-12).
#[cfg(not(target_arch = "wasm32"))]
// Skip: bottoms out at the signal-mask FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
pub fn take_stop_signals(name: &str, on_stop: impl FnOnce(i32) + Send + 'static) -> bool {
    unsafe extern "C" {
        fn pthread_sigmask(how: i32, set: *const SigSet, old: *mut SigSet) -> i32;
        fn sigwait(set: *const SigSet, sig: *mut i32) -> i32;
    }
    let (block, unblock) = if cfg!(target_os = "linux") || cfg!(target_os = "android") {
        (0, 1)
    } else {
        (1, 2)
    };
    let set = stop_set();
    // SAFETY: changes only the calling thread's mask, from a live, initialised set.
    unsafe { pthread_sigmask(block, &set, std::ptr::null_mut()) };
    let spawned = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let set = stop_set();
            let mut sig = 0;
            // SAFETY: waits on a live, initialised set and writes one int.
            while unsafe { sigwait(&set, &mut sig) } != 0 {}
            on_stop(sig);
        });
    if spawned.is_err() {
        // SAFETY: as above.
        unsafe { pthread_sigmask(unblock, &set, std::ptr::null_mut()) };
        return false;
    }
    true
}

#[cfg(test)]
mod start_tests {
    use super::{BootClock, procfs};

    /// Real `/proc` text (`USER_HZ` 100): a command name holding a space and both parentheses,
    /// running and as a zombie, and a `cat` read at uptime 518318.71 s.
    const TRICKY: &str = "7 (a) b (c) S 1 1 1 0 -1 4194368 63 0 0 0 0 0 0 0 20 0 1 0 51832760 \
         7032832 463 18446744073709551615 205458244435968 205458248030472 281474793899808 0 0 0 \
         0 128 0 1 0 0 17 0 0 0 0 0 0 205458248044264 205458248115556 205458772447232 \
         281474793901609 281474793901860 281474793901860 281474793902058 0\n";
    const ZOMBIE: &str = "7 (a) b (c) Z 1 1 1 0 -1 4227148 140 0 0 0 0 0 0 0 20 0 1 0 51832760 0 \
         0 18446744073709551615 0 0 0 0 0 0 0 128 0 1 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";
    const CAT: &str = "13 (cat) R 1 1 1 0 -1 4194304 43 0 0 0 0 0 0 0 20 0 1 0 51831871 2424832 \
         192 18446744073709551615 218051104473088 218051104506384 281474237451360 0 0 0 0 0 0 0 \
         0 0 17 1 0 0 0 0 0 218051104603072 218051104604896 218051165171712 281474237452043 \
         281474237452063 281474237452063 281474237452267 0\n";
    const STATUS_501: &str = "Name:\tcat\nState:\tR (running)\nTgid:\t13\nPid:\t13\n\
         Uid:\t501\t502\t501\t502\nGid:\t20\t20\t20\t20\n";

    /// Field 22 counted from the command name's last `)`, the effective uid as the second
    /// `Uid:` id, a tick as its first instant; another pid's line, a zombie, another euid, two
    /// disagreeing `stat` reads, a short line and a zero tick rate answer `None`.
    #[test]
    fn a_linux_start_is_read_off_proc_as_linux_writes_it() {
        assert_eq!(procfs::stat_start(TRICKY, 7), Some(('S', 51_832_760)));
        assert_eq!(procfs::stat_start(ZOMBIE, 7), Some(('Z', 51_832_760)));
        assert_eq!(procfs::stat_start(CAT, 13), Some(('R', 51_831_871)));
        assert_eq!(procfs::stat_start(CAT, 14), None, "another pid's line");
        assert_eq!(
            procfs::stat_start("13 (cat) R 1 1 1 0 -1 4194304 43 0 0", 13),
            None,
            "cut short"
        );
        assert_eq!(procfs::effective_uid(STATUS_501), Some(502));

        let at = |ticks: u64| Some(BootClock(ticks * 10_000_000));
        assert_eq!(
            procfs::start(CAT, STATUS_501, CAT, 13, 502, 100),
            at(51_831_871),
            "518318.71 s after boot"
        );
        let tricky_status = STATUS_501.replace("Pid:\t13", "Pid:\t7");
        assert_eq!(
            procfs::start(TRICKY, &tricky_status, TRICKY, 7, 502, 100),
            at(51_832_760)
        );
        for (case, stat, again, euid, per_second) in [
            ("a zombie", ZOMBIE, ZOMBIE, 502, 100),
            ("dead", &*TRICKY.replace(") S ", ") X "), TRICKY, 502, 100),
            ("another effective uid", TRICKY, TRICKY, 501, 100),
            (
                "reused between the reads",
                TRICKY,
                &*CAT.replacen("13 ", "7 ", 1),
                502,
                100,
            ),
            ("no tick rate", TRICKY, TRICKY, 502, 0),
        ] {
            assert_eq!(
                procfs::start(stat, &tricky_status, again, 7, euid, per_second),
                None,
                "{case}"
            );
        }
        // The first instant of a tick, never a later one: at 100 Hz tick
        // 51831871 covers [518318.71 s, 518318.72 s).
        assert_eq!(
            procfs::tick_start(51_831_871, 100),
            Some(BootClock(518_318_710_000_000))
        );
        assert_eq!(procfs::tick_start(7, 3), Some(BootClock(2_333_333_333)));
    }

    /// A child spawned after a boot-clock reading starts later, and no later than a reading
    /// taken once it runs; this process started earlier; an ended process has no record. The
    /// bracket pins the unit (a wrong `USER_HZ` lands outside it); the 20 ms gap covers two
    /// Linux ticks of truncation. On Apple the wall record matches `read_process_birth`; under
    /// Rosetta the child is ordered on its wall record instead.
    #[cfg(unix)]
    #[test]
    fn a_child_started_after_a_boot_clock_reading_reads_later() {
        use std::time::Duration;
        let own = i32::try_from(std::process::id()).unwrap();
        let Some(moment) = super::boot_clock_now() else {
            if super::translation() == super::Translation::Translated {
                a_child_started_after_a_wall_reading_reads_later();
                return;
            }
            let wired = cfg!(any(target_vendor = "apple", target_os = "linux"));
            assert!(!wired, "macOS and Linux have a boot clock");
            assert_eq!(super::read_process_start(own), None);
            return;
        };
        std::thread::sleep(Duration::from_millis(20));
        let mut child = crate::spawnfd::command("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn /bin/sleep");
        let pid = i32::try_from(child.id()).unwrap();
        let started = super::read_process_start(pid);
        let after = super::boot_clock_now().expect("the boot clock again");
        let wall = super::read_process_birth(pid);
        let _ = child.kill();
        let _ = child.wait();
        let started = started.expect("a live child's start");
        assert!(started.boot > moment, "{started:?} after {moment:?}");
        assert!(started.boot <= after, "{started:?} by {after:?}");
        assert_eq!(started.wall, wall, "the wall record beside it");
        assert_eq!(started.wall.is_some(), cfg!(target_vendor = "apple"));
        let mine = super::read_process_start(own).expect("this process's start");
        assert!(mine.boot < moment, "{mine:?} before {moment:?}");
        assert_eq!(super::read_process_start(pid), None, "an ended process");
        assert_eq!(super::read_process_start(1), None, "never pid 1");
    }

    /// The translated half of [`a_child_started_after_a_boot_clock_reading_reads_later`]: the
    /// wall record orders a child spawned after a wall-clock reading as later.
    #[cfg(unix)]
    fn a_child_started_after_a_wall_reading_reads_later() {
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        let moment = SystemTime::now();
        std::thread::sleep(Duration::from_millis(20));
        let mut child = crate::spawnfd::command("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn /bin/sleep");
        let started = super::read_process_start(i32::try_from(child.id()).unwrap());
        let _ = child.kill();
        let _ = child.wait();
        let birth = started
            .and_then(|s| s.wall)
            .expect("a translated reader still reads the start record");
        let wall = UNIX_EPOCH
            + Duration::from_secs(birth.seconds)
            + Duration::from_micros(birth.microseconds);
        assert!(wall > moment, "{wall:?} after {moment:?}");
    }

    /// With the translation injected, `Translated` answers `None` without reading the clock,
    /// and `Native` and `Unknown` answer the reading as is.
    #[test]
    fn a_translated_process_reads_no_boot_clock() {
        let native_ticks = 16_941_646_465_128;
        assert_eq!(
            super::apple_boot_clock(super::Translation::Native, || native_ticks),
            Some(BootClock(native_ticks))
        );
        assert_eq!(
            super::apple_boot_clock(super::Translation::Unknown, || native_ticks),
            Some(BootClock(native_ticks))
        );
        assert_eq!(
            super::apple_boot_clock(super::Translation::Translated, || {
                panic!("a translated process's reading is never taken")
            }),
            None
        );
        // This host's own answer agrees with its translation.
        if cfg!(target_vendor = "apple") {
            assert_eq!(
                super::boot_clock_now().is_none(),
                super::translation() == super::Translation::Translated
            );
        } else {
            assert_eq!(super::translation(), super::Translation::Native);
        }
    }
}

#[cfg(all(test, unix))]
mod kill_self_tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};

    const CHILD_ENV: &str = "ATERM_UDS_TEST_KILL_SELF_CHILD";

    /// `kill_self_now` ends the process by SIGKILL at the call, which leaves no
    /// macOS crash report.
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
    /// A process that exits with code 259 (== STILL_ACTIVE) still reads dead.
    /// The `Child` is kept un-dropped so its open handle pins the pid.
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

#[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
mod parent_and_path_tests {
    use super::{read_process_parent, read_process_path};

    /// This process: its parent is the one `std` reports, and its executable
    /// is the test binary running now.
    #[test]
    fn this_process_reads_its_parent_and_its_executable() {
        let me = i32::try_from(std::process::id()).expect("pid");
        assert_eq!(
            read_process_parent(me),
            Some(std::os::unix::process::parent_id())
        );
        let path = read_process_path(me).expect("own path");
        let exe = std::env::current_exe().expect("current_exe");
        assert_eq!(
            path.canonicalize().expect("path exists"),
            exe.canonicalize().expect("exe exists")
        );
    }

    /// Linux's `/proc` text: the `PPid:` line, and an exe link the kernel marks
    /// ` (deleted)` names no live binary.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_reads_the_parent_line_and_refuses_a_deleted_exe_link() {
        use super::procfs;
        let status = "Name:\tcat\nUmask:\t0002\nState:\tR (running)\nTgid:\t13\nNgid:\t0\n\
                      Pid:\t13\nPPid:\t4417\nTracerPid:\t0\n";
        assert_eq!(procfs::parent(status), Some(4417));
        assert_eq!(procfs::parent("Name:\tcat\n"), None);
        let live = std::path::PathBuf::from("/opt/aterm/aterm");
        assert_eq!(procfs::live_exe(live.clone()), Some(live));
        assert_eq!(
            procfs::live_exe(std::path::PathBuf::from("/opt/aterm/aterm (deleted)")),
            None
        );
    }

    /// A live process whose binary was deleted under it reads no path — the macOS
    /// contract — while its start record still answers. The control is the same
    /// process before the delete.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_process_on_a_deleted_binary_reads_no_path() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-uds-deleted-exe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        // Copied by `cp`, never by this process: a binary this process held open
        // for writing could be inherited by another test thread's fork, and its
        // exec would fail ETXTBSY.
        let copy = dir.join("sleep-copy");
        let copied = crate::spawnfd::command("/bin/cp")
            .arg("/bin/sleep")
            .arg(&copy)
            .status()
            .expect("run cp");
        assert!(copied.success(), "cp /bin/sleep: {copied}");
        let mut child = crate::spawnfd::command(&copy)
            .arg("120")
            .spawn()
            .expect("spawn the copy");
        let pid = i32::try_from(child.id()).expect("pid");
        assert_eq!(read_process_path(pid), Some(copy.clone()), "the control");
        std::fs::remove_file(&copy).expect("delete the binary");
        let path = read_process_path(pid);
        let start = super::read_process_start(pid);
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(path, None, "a deleted binary names no path");
        assert!(start.is_some(), "the process itself still reads");
    }
}

#[cfg(all(test, unix))]
mod argv_tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    fn mac_frame(argc: i32, tail: &[u8]) -> Vec<u8> {
        let mut bytes = argc.to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"/bin/aterm\0\0");
        bytes.extend_from_slice(tail);
        bytes
    }

    #[test]
    fn a_kernel_argv_buffer_keeps_words_and_stops_before_the_environment() {
        let expected = vec![
            OsString::from("/bin/aterm"),
            OsString::from("link"),
            OsString::from_vec(b"name\xff with --durability relaxed".to_vec()),
            OsString::new(),
        ];
        let bytes = mac_frame(4, b"/bin/aterm\0link\0name\xff with --durability relaxed\0\0--durability=relaxed\0SECRET=not-argv\0\0");
        assert_eq!(argv_from_procargs2(&bytes), Some(expected));
        let ordinary = mac_frame(1, b"/bin/aterm\0SECRET=not-argv\0");
        assert_eq!(
            argv_from_procargs2(&ordinary),
            Some(vec![OsString::from("/bin/aterm")])
        );
        let mut broken = Vec::new();
        for argc in [-1, 0, PROCESS_ARGV_WORDS as i32 + 1] {
            broken.push(mac_frame(argc, b"/bin/aterm\0"));
        }
        broken.push(mac_frame(2, b"/bin/aterm\0"));
        broken.push(mac_frame(1, b"/bin/aterm"));
        broken.push(vec![1, 0, 0]);
        let got: Vec<_> = broken
            .iter()
            .map(|bytes| argv_from_procargs2(bytes))
            .collect();
        assert!(got.iter().all(Option::is_none));
    }

    #[test]
    fn a_proc_cmdline_keeps_empty_and_non_utf8_words_but_refuses_a_prefix() {
        assert_eq!(
            argv_from_cmdline(b"aterm\0\0name\xff with --durability relaxed\0\0"),
            Some(vec![
                OsString::from("aterm"),
                OsString::new(),
                OsString::from_vec(b"name\xff with --durability relaxed".to_vec()),
                OsString::new()
            ])
        );
        assert_eq!(
            argv_from_cmdline(b"aterm\0"),
            Some(vec![OsString::from("aterm")])
        );
        assert_eq!(argv_from_cmdline(b""), None);
        assert_eq!(argv_from_cmdline(b"aterm\0truncated"), None);
        let at_limit = vec![0u8; PROCESS_ARGV_WORDS];
        assert_eq!(
            argv_from_cmdline(&at_limit).unwrap().len(),
            PROCESS_ARGV_WORDS
        );
        let over_limit = vec![0u8; PROCESS_ARGV_WORDS + 1];
        assert_eq!(argv_from_cmdline(&over_limit), None);
    }

    #[test]
    fn an_argv_observation_needs_the_same_known_process_start_on_both_sides() {
        let old = ProcessStart {
            boot: BootClock(17),
            wall: None,
        };
        let new = ProcessStart {
            boot: BootClock(18),
            wall: None,
        };
        let words = || Some(vec![OsString::from("aterm"), OsString::new()]);
        let mut count = 0;
        assert_eq!(
            argv_of_same_process(
                71,
                |_| {
                    count += 1;
                    Some(old)
                },
                |_| words()
            ),
            words()
        );
        assert_eq!(count, 2);
        for after in [None, Some(new)] {
            let mut n = 0;
            assert_eq!(
                argv_of_same_process(
                    71,
                    |_| {
                        n += 1;
                        if n == 1 { Some(old) } else { after }
                    },
                    |_| words()
                ),
                None
            );
            assert_eq!(n, 2);
        }
        assert_eq!(
            argv_of_same_process(
                71,
                |_| None,
                |_| panic!("unknown identity must not read argv")
            ),
            None
        );
        for pid in [-1, 0, 1] {
            assert_eq!(
                argv_of_same_process(pid, |_| panic!("invalid pid"), |_| panic!("invalid pid")),
                None
            );
        }
    }
}
