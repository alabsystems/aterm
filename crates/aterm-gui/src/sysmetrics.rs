// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dependency-free OS metric probes for the HUD framework (CPU load, memory,
//! network byte counters), behind safe `Option`-returning wrappers. ALL raw FFI
//! lives here (the "one seam for unsafe" discipline, like aterm-pty): `libc` on
//! macOS/Linux, hand-rolled Win32 declarations (the aterm-pty ffi.rs house style)
//! on Windows. macOS, Linux, and Windows are implemented targets; on any other
//! target every probe returns `None` so the panels paint "n/a" and never break
//! the build.
//!
//! Honesty: these are WHOLE-MACHINE figures. macOS exposes no public per-process
//! network counter (only the private NetworkStatistics framework), so per-app
//! traffic is reported by the process itself via the app-fed `metric` channel, not
//! here.

/// Logical CPU count (for normalizing CPU usage to a per-core fraction), default 1.
#[must_use]
pub(crate) fn ncpu() -> u32 {
    #[cfg(target_os = "macos")]
    {
        sysctl_u64("hw.logicalcpu")
            .or_else(|| sysctl_u64("hw.ncpu"))
            .unwrap_or(1) as u32
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
    }
}

/// Total physical RAM in bytes, or `None`.
#[must_use]
pub(crate) fn mem_total() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        sysctl_u64("hw.memsize")
    }
    #[cfg(target_os = "linux")]
    {
        proc_meminfo_kib("MemTotal").map(|kib| kib * 1024)
    }
    #[cfg(windows)]
    {
        win::mem_total()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        None
    }
}

/// Fraction (0..1) of RAM in active use (active + wired + compressed), a proxy for
/// memory pressure; `None` if unavailable.
#[must_use]
pub(crate) fn mem_used_frac() -> Option<f64> {
    #[cfg(target_os = "macos")]
    {
        let total = mem_total()? as f64;
        let page = sysctl_u64("hw.pagesize").unwrap_or(4096) as f64;
        let vm = vm_stats64()?;
        let used = (vm.active_count as f64
            + vm.wire_count as f64
            + u64::from(vm.compressor_page_count) as f64)
            * page;
        if total > 0.0 {
            Some((used / total).clamp(0.0, 1.0))
        } else {
            None
        }
    }
    #[cfg(target_os = "linux")]
    {
        // /proc/meminfo: used = 1 - MemAvailable/MemTotal (MemAvailable is the
        // kernel's own estimate of allocatable RAM, the right "pressure" proxy).
        let total = proc_meminfo_kib("MemTotal")? as f64;
        let avail = proc_meminfo_kib("MemAvailable")? as f64;
        if total > 0.0 {
            Some((1.0 - avail / total).clamp(0.0, 1.0))
        } else {
            None
        }
    }
    #[cfg(windows)]
    {
        win::mem_used_frac()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        None
    }
}

/// PER-INTERFACE cumulative `(name, rx, tx)` byte counters for non-loopback links,
/// or `None` if unavailable. The raw 32-bit `if_data` counters are returned untouched
/// and keyed by interface name so the caller can diff EACH interface independently —
/// summing across interfaces and then diffing the sum produces a one-tick spike
/// whenever an interface appears/disappears (VPN up/down, Wi-Fi switch) or a single
/// 32-bit counter wraps; per-interface diffing avoids both.
#[must_use]
pub(crate) fn net_ifaces() -> Option<Vec<(String, u32, u32)>> {
    #[cfg(target_os = "macos")]
    {
        net_ifaces_macos()
    }
    #[cfg(target_os = "linux")]
    {
        net_bytes_linux()
    }
    #[cfg(windows)]
    {
        win::net_ifaces()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        None
    }
}

/// Cumulative whole-machine CPU tick counters `[user, system, idle, nice]` since boot,
/// or `None` if unavailable. The caller diffs successive reads (like [`net_ifaces`]):
/// busy fraction over an interval = `Δ(user+system+nice) / Δ(all four)`. macOS reads
/// `HOST_CPU_LOAD_INFO`; Linux reads `/proc/stat`'s aggregate `cpu` line; Windows
/// reads `GetSystemTimes` (100ns units, all-processor totals; nice is always 0).
#[must_use]
pub(crate) fn cpu_ticks() -> Option<[u64; 4]> {
    #[cfg(target_os = "macos")]
    {
        cpu_ticks_macos()
    }
    #[cfg(target_os = "linux")]
    {
        cpu_ticks_linux()
    }
    #[cfg(windows)]
    {
        win::cpu_ticks()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        None
    }
}

/// This process's own pid — the root of the "terminal session" process tree (the GUI
/// process plus every shell/command it spawned in its PTYs).
#[must_use]
pub(crate) fn self_pid() -> i32 {
    // std's process id — portable (the one former libc::getpid call would not
    // compile on Windows); pids fit i32 on every supported platform.
    std::process::id() as i32
}

/// Resource usage of ONE process. `cpu_ns`/`disk_*` are CUMULATIVE since the process
/// started; `footprint` is the instantaneous physical footprint. The caller keeps
/// PER-PID prior samples and diffs each pid independently, so a process that starts or
/// exits between ticks never spikes/undercounts the session rate — the churn-correct way
/// to total a subtree whose membership changes (a compile spawning hundreds of
/// short-lived children would wreck a naive sum-then-diff).
#[derive(Clone, Copy, Default)]
pub(crate) struct ProcSample {
    pub pid: i32,
    /// User+system CPU time in nanoseconds (cumulative).
    pub cpu_ns: u64,
    /// Physical memory footprint in bytes (instantaneous). This is the figure the OS
    /// charges against the process for jetsam / memory-pressure decisions — macOS
    /// `phys_footprint` (includes compressed + IOKit/GPU-mapped pages), Windows working
    /// set. NOT bare resident-set size, which under-reports and drove the "GBs too low"
    /// memory-warning discrepancy (MEM-ACCT-1).
    pub footprint: u64,
    /// Bytes read from disk (cumulative).
    pub disk_read: u64,
    /// Bytes written to disk (cumulative).
    pub disk_write: u64,
}

/// One process's usage, or `None` if the pid is gone / not ours (macOS `proc_pid_rusage`,
/// Windows `GetProcessTimes`/`K32GetProcessMemoryInfo`/`GetProcessIoCounters`); the
/// building block for both the per-session subtree ([`session_procs`]) and aterm's own
/// engine line (`crate::hud_bar` passes `self_pid`).
#[must_use]
pub(crate) fn proc_usage(pid: i32) -> Option<ProcSample> {
    #[cfg(target_os = "macos")]
    {
        proc_usage_macos(pid)
    }
    #[cfg(windows)]
    {
        win::proc_usage(pid)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = pid;
        None
    }
}

/// Every live process in the subtree rooted at `root` (aterm + all descendants) as a
/// per-pid sample — the footprint of "this terminal session". The caller sums `rss`
/// directly and diffs `cpu_ns`/`disk_*` PER PID against its own prior table. macOS walks
/// the `proc_listchildpids` tree; Windows walks a Toolhelp32 process snapshot's
/// pid→parent edges; elsewhere there is no dependency-free per-process probe, so it
/// returns `None` and the session column paints `n/a`.
#[must_use]
pub(crate) fn session_procs(root: i32) -> Option<Vec<ProcSample>> {
    #[cfg(target_os = "macos")]
    {
        session_procs_macos(root)
    }
    #[cfg(windows)]
    {
        win::session_procs(root)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = root;
        None
    }
}

/// Whole-machine GPU utilization as a fraction `0..1`, or `None` if unavailable. macOS
/// reads the `IOAccelerator` registry's `PerformanceStatistics` → `Device Utilization
/// %` (the max across accelerators); there is no public PER-PROCESS GPU counter, so the
/// session column shows aterm's own render cost in the engine HUD instead. Windows
/// stays `None`: the only public whole-machine figure is the PDH "GPU Engine" counter
/// set (open/collect/aggregate per-engine instances via pdh.dll), which is out of scope
/// for this dependency-free seam — the panel paints "n/a" there.
#[must_use]
pub(crate) fn gpu_util() -> Option<f64> {
    #[cfg(target_os = "macos")]
    {
        iokit::gpu_util()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Whole-machine GPU VRAM currently in use (bytes), or `None`. macOS reads the
/// `IOAccelerator` registry's `PerformanceStatistics` → `In use system memory` (the max
/// across accelerators) — an ALL-PROCESS, whole-GPU counter, the honest system-wide
/// "used" for unified memory. There is no per-process GPU-memory counter here (see
/// [`gpu_util`]). Non-macOS stays `None` (the panel paints `n/a`).
#[must_use]
pub(crate) fn gpu_vram_used() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        iokit::gpu_vram_used()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// GPU VRAM budget (bytes) — the device-static working-set ceiling, or `None`. macOS
/// reads Metal's `[MTLDevice recommendedMaxWorkingSetSize]` (the only "total" available
/// on unified memory; IOAccelerator exposes no budget key). The value is cached the
/// first time it is read (the device budget does not change), so the slow-probe worker
/// touches Metal exactly once. Non-macOS / headless stays `None`.
#[must_use]
pub(crate) fn gpu_vram_budget() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        metal::vram_budget()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Cumulative whole-machine disk I/O `(bytes_read, bytes_written)` since boot, or
/// `None`. The caller diffs successive reads for a B/s rate. macOS sums every
/// `IOBlockStorageDriver`'s `Statistics`; Linux sums `/proc/diskstats` sectors×512;
/// Windows sums `IOCTL_DISK_PERFORMANCE` over the physical drives (cumulative since
/// the counters were enabled, not boot — irrelevant to a caller that only diffs).
#[must_use]
pub(crate) fn disk_io_bytes() -> Option<(u64, u64)> {
    #[cfg(target_os = "macos")]
    {
        iokit::disk_io_bytes()
    }
    #[cfg(target_os = "linux")]
    {
        disk_io_bytes_linux()
    }
    #[cfg(windows)]
    {
        win::disk_io_bytes()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        None
    }
}

// --- Linux implementations (/proc, dependency-free) -------------------------

/// Read one `/proc/meminfo` field (e.g. `MemTotal`) as its KiB value. The file is
/// `Label:   <kib> kB` per line; returns the numeric KiB (the unit is always KiB
/// despite the `kB` label). `None` if the field is absent/unparsable.
#[cfg(target_os = "linux")]
fn proc_meminfo_kib(field: &str) -> Option<u64> {
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix(field)
            && rest.starts_with(':')
        {
            return rest[1..].split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// PER-INTERFACE cumulative `(name, rx, tx)` byte counters for non-loopback links
/// from `/proc/net/dev` (matching the `net_ifaces` contract the caller diffs each
/// interface against). Each data line is `iface: rx_bytes rx_packets ... tx_bytes
/// ...` — field 0 after the colon is rx_bytes, field 8 is tx_bytes; `lo` is excluded.
/// Linux's 64-bit counters are truncated to `u32` to match the `if_data` width the
/// caller already wrap-subtracts (a per-tick delta never approaches 4 GiB).
#[cfg(target_os = "linux")]
fn net_bytes_linux() -> Option<Vec<(String, u32, u32)>> {
    let s = std::fs::read_to_string("/proc/net/dev").ok()?;
    let mut out: Vec<(String, u32, u32)> = Vec::new();
    for line in s.lines() {
        let Some((iface, rest)) = line.split_once(':') else {
            continue; // the two header lines have no colon
        };
        let iface = iface.trim();
        if iface == "lo" {
            continue;
        }
        let cols: Vec<&str> = rest.split_whitespace().collect();
        if cols.len() >= 9 {
            let rx: u64 = cols[0].parse().unwrap_or(0);
            let tx: u64 = cols[8].parse().unwrap_or(0);
            out.push((iface.to_string(), rx as u32, tx as u32));
        }
    }
    Some(out)
}

/// Cumulative `[user, system, idle, nice]` jiffies from `/proc/stat`'s aggregate `cpu`
/// line (fields: user nice system idle iowait irq softirq …). We fold `iowait`/`irq`
/// into idle/system implicitly by only reading the first four; the caller diffs.
#[cfg(target_os = "linux")]
fn cpu_ticks_linux() -> Option<[u64; 4]> {
    let s = std::fs::read_to_string("/proc/stat").ok()?;
    let line = s.lines().find(|l| l.starts_with("cpu "))?;
    let mut it = line.split_whitespace().skip(1);
    let user: u64 = it.next()?.parse().ok()?;
    let nice: u64 = it.next()?.parse().ok()?;
    let system: u64 = it.next()?.parse().ok()?;
    let idle: u64 = it.next()?.parse().ok()?;
    Some([user, system, idle, nice])
}

/// Cumulative `(bytes_read, bytes_written)` from `/proc/diskstats` (sectors × 512), over
/// whole-disk devices only (skip partitions). Fields 5 and 9 (1-based after the name)
/// are sectors read / written.
#[cfg(target_os = "linux")]
fn disk_io_bytes_linux() -> Option<(u64, u64)> {
    let s = std::fs::read_to_string("/proc/diskstats").ok()?;
    let (mut r, mut w) = (0u64, 0u64);
    for line in s.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 14 {
            continue;
        }
        let name = f[2];
        // Whole disks only: skip partitions (sdaN, nvme0n1pN) to avoid double counting.
        if name.chars().last().is_some_and(|c| c.is_ascii_digit()) && !name.starts_with("nvme") {
            continue;
        }
        r += f[5].parse::<u64>().unwrap_or(0) * 512;
        w += f[9].parse::<u64>().unwrap_or(0) * 512;
    }
    Some((r, w))
}

// --- macOS implementations --------------------------------------------------

#[cfg(target_os = "macos")]
fn sysctl_u64(name: &str) -> Option<u64> {
    use std::ffi::CString;
    let cname = CString::new(name).ok()?;
    let mut val: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: `sysctlbyname` writes up to `len` bytes into `val`; we pass a valid u64
    // out-param and its size. hw.* keys return a 64- or 32-bit integer.
    let rc = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            std::ptr::addr_of_mut!(val).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    // Some keys (hw.ncpu, hw.pagesize) are 32-bit; mask if only 4 bytes were written.
    if len == 4 {
        Some(u64::from(val as u32))
    } else {
        Some(val)
    }
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
fn sysctl_u64(_name: &str) -> Option<u64> {
    None
}

// `mach_port_deallocate` is not re-exported by `libc`; declare it. It lives in
// libSystem (always linked on macOS). Used to release the send-right reference that
// `mach_host_self()` adds to this task's IPC space on every call.
#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn mach_port_deallocate(
        task: libc::mach_port_t,
        name: libc::mach_port_t,
    ) -> libc::kern_return_t;
}

#[cfg(target_os = "macos")]
fn vm_stats64() -> Option<libc::vm_statistics64> {
    // SAFETY: zeroed POD; host_statistics64 fills it. mach_host_self() returns the
    // host port (the deprecation is cosmetic; the data fn is not deprecated).
    let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = (std::mem::size_of::<libc::vm_statistics64>()
        / std::mem::size_of::<libc::integer_t>())
        as libc::mach_msg_type_number_t;
    // SAFETY: `mach_host_self()` returns a send right to the host name port AND adds a
    // user reference to it in our IPC space on EVERY call — so it must be paired with
    // `mach_port_deallocate` below, or the reference count climbs ~3×/s (the HUD poll
    // rate) for the process lifetime. The deprecation on the symbol is cosmetic.
    #[allow(deprecated)]
    let host = unsafe { libc::mach_host_self() };
    // SAFETY: valid host port, HOST_VM_INFO64 flavor, out-buffer + its element count.
    let rc = unsafe {
        libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            std::ptr::addr_of_mut!(stats).cast(),
            &mut count,
        )
    };
    // SAFETY: release the send-right reference added by `mach_host_self()` above.
    // `mach_task_self_` is this task's own port (a `static` set up by libSystem); we
    // only read its value. Done on BOTH success and failure paths — the reference is
    // added regardless of `host_statistics64`'s result. Ignoring the return is fine:
    // a failed deallocate can't make the leak worse than not calling it. The
    // deprecation (libc suggests the `mach2` crate) is cosmetic — we keep the existing
    // dependency-free `libc`-only seam, matching `mach_host_self()` above.
    #[allow(deprecated)]
    unsafe {
        let _ = mach_port_deallocate(libc::mach_task_self_, host);
    }
    if rc == libc::KERN_SUCCESS {
        Some(stats)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn net_ifaces_macos() -> Option<Vec<(String, u32, u32)>> {
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs allocates a linked list into `ifap`; freed below.
    if unsafe { libc::getifaddrs(&mut ifap) } != 0 || ifap.is_null() {
        return None;
    }
    let mut out: Vec<(String, u32, u32)> = Vec::new();
    let mut cur = ifap;
    // SAFETY: walk the NUL-terminated `ifa_next` list; each node is valid until
    // freeifaddrs. AF_LINK nodes carry an `if_data` in `ifa_data`, and `ifa_name`
    // is a valid NUL-terminated C string for the link's lifetime in this list.
    unsafe {
        while !cur.is_null() {
            let ifa = &*cur;
            if !ifa.ifa_addr.is_null()
                && i32::from((*ifa.ifa_addr).sa_family) == libc::AF_LINK
                && (ifa.ifa_flags & libc::IFF_LOOPBACK as u32) == 0
                && !ifa.ifa_data.is_null()
                && !ifa.ifa_name.is_null()
            {
                let d = &*(ifa.ifa_data as *const libc::if_data);
                let name = std::ffi::CStr::from_ptr(ifa.ifa_name)
                    .to_string_lossy()
                    .into_owned();
                out.push((name, d.ifi_ibytes, d.ifi_obytes));
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    Some(out)
}

#[cfg(target_os = "macos")]
fn cpu_ticks_macos() -> Option<[u64; 4]> {
    // SAFETY: zeroed POD; host_statistics fills `cpu_ticks`. The buffer count starts at
    // the flavor's element count and is updated in place.
    let mut info: libc::host_cpu_load_info = unsafe { std::mem::zeroed() };
    let mut count: libc::mach_msg_type_number_t = libc::HOST_CPU_LOAD_INFO_COUNT;
    // SAFETY: `mach_host_self()` adds a send-right reference we MUST release (mirrors
    // `vm_stats64` above) or the count climbs ~3×/s for the process lifetime.
    #[allow(deprecated)]
    let host = unsafe { libc::mach_host_self() };
    // SAFETY: valid host port, HOST_CPU_LOAD_INFO flavor, out-buffer cast to the
    // `host_info_t` (`*mut integer_t`) the call expects, with its element count.
    let rc = unsafe {
        libc::host_statistics(
            host,
            libc::HOST_CPU_LOAD_INFO,
            std::ptr::addr_of_mut!(info).cast(),
            &mut count,
        )
    };
    // SAFETY: release the reference added by `mach_host_self()` on every path (see
    // `vm_stats64`). The deprecation is cosmetic; we keep the `libc`-only seam.
    #[allow(deprecated)]
    unsafe {
        let _ = mach_port_deallocate(libc::mach_task_self_, host);
    }
    if rc != libc::KERN_SUCCESS {
        return None;
    }
    let t = info.cpu_ticks;
    Some([
        u64::from(t[libc::CPU_STATE_USER as usize]),
        u64::from(t[libc::CPU_STATE_SYSTEM as usize]),
        u64::from(t[libc::CPU_STATE_IDLE as usize]),
        u64::from(t[libc::CPU_STATE_NICE as usize]),
    ])
}

/// The mach absolute-time → nanoseconds ratio `(numer, denom)`, read once. `1:1` on
/// Apple Silicon (the fields are already ns); `125:3` on Intel/Rosetta — which is why
/// `proc_pid_rusage`'s CPU fields MUST be scaled or session CPU reads ~40× too low there.
#[cfg(target_os = "macos")]
// `libc` marks the mach_timebase symbols deprecated (it suggests the `mach2` crate); we
// keep the dependency-free `libc`-only seam, exactly as `mach_host_self` above does.
#[allow(deprecated)]
fn mach_timebase() -> (u64, u64) {
    use std::sync::OnceLock;
    static TB: OnceLock<(u64, u64)> = OnceLock::new();
    *TB.get_or_init(|| {
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: out-param fill; returns KERN_SUCCESS (0). Fall back to 1:1 (treat the
        // ticks as ns) if it ever fails, so CPU is at worst unscaled, never a panic.
        let rc = unsafe { libc::mach_timebase_info(&mut tb) };
        if rc == 0 && tb.numer != 0 && tb.denom != 0 {
            (u64::from(tb.numer), u64::from(tb.denom))
        } else {
            (1, 1)
        }
    })
}

#[cfg(target_os = "macos")]
fn proc_usage_macos(pid: i32) -> Option<ProcSample> {
    // SAFETY: zeroed POD; `proc_pid_rusage` fills it for our own-uid processes. The
    // buffer is reinterpreted as the opaque `rusage_info_t` out-param the call expects.
    let mut ru: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, std::ptr::addr_of_mut!(ru).cast())
    };
    if rc != 0 {
        return None; // pid gone, or not permitted (different uid)
    }
    // ri_user_time/ri_system_time are mach absolute-time UNITS, not nanoseconds — scale
    // by the timebase (a no-op on Apple Silicon, ~41.67× on Intel/Rosetta).
    let (numer, denom) = mach_timebase();
    let raw_ticks = u128::from(ru.ri_user_time.saturating_add(ru.ri_system_time));
    let cpu_ns = (raw_ticks * u128::from(numer) / u128::from(denom)) as u64;
    Some(ProcSample {
        pid,
        cpu_ns,
        // phys_footprint, NOT ri_resident_size: the resident set excludes compressed and
        // IOKit/GPU-mapped pages, so it under-reported by GBs under memory pressure. This
        // is the ledger figure the kernel uses for memory-pressure / jetsam (MEM-ACCT-1).
        footprint: ru.ri_phys_footprint,
        disk_read: ru.ri_diskio_bytesread,
        disk_write: ru.ri_diskio_byteswritten,
    })
}

/// Direct children of `pid` into `buf` (reused across calls). A fixed buffer holds up to
/// 1024 children per process — far beyond any real shell's fan-out; an over-full node is
/// simply truncated rather than reallocated on the HUD poll path.
#[cfg(target_os = "macos")]
fn children_into(pid: i32, buf: &mut Vec<i32>) {
    const CAP: usize = 1024;
    buf.clear();
    buf.resize(CAP, 0);
    // SAFETY: `proc_listchildpids` writes up to `buffersize` bytes of `pid_t` into the
    // buffer and returns the BYTE count written (or -1). We pass our sized buffer.
    let n = unsafe {
        libc::proc_listchildpids(
            pid,
            buf.as_mut_ptr().cast(),
            (CAP * std::mem::size_of::<i32>()) as libc::c_int,
        )
    };
    if n <= 0 {
        buf.clear();
        return;
    }
    // `proc_listchildpids` returns the COUNT of child pids written (NOT a byte count
    // like `proc_listpids`), each a `pid_t` already in `buf`.
    let got = (n as usize).min(CAP);
    buf.truncate(got);
}

#[cfg(target_os = "macos")]
fn session_procs_macos(root: i32) -> Option<Vec<ProcSample>> {
    let mut out: Vec<ProcSample> = Vec::new();
    let mut stack = vec![root];
    let mut childbuf: Vec<i32> = Vec::new();
    // Bound the walk so a pathological process count can never stall the poll tick.
    let mut budget = 4096u32;
    while let Some(pid) = stack.pop() {
        if budget == 0 {
            break;
        }
        budget -= 1;
        if let Some(s) = proc_usage_macos(pid) {
            out.push(s);
        }
        children_into(pid, &mut childbuf);
        for &c in &childbuf {
            if c > 0 && c != pid {
                stack.push(c);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The IOKit/CoreFoundation seam for the two whole-machine metrics with no `libc`
/// counterpart: GPU utilization (`IOAccelerator`) and disk throughput
/// (`IOBlockStorageDriver`). All raw IOKit/CF FFI is confined here, mirroring the
/// "one seam for unsafe" discipline the rest of this module follows. Read-only registry
/// property reads — no special entitlement required.
#[cfg(target_os = "macos")]
mod iokit {
    use std::os::raw::{c_char, c_void};

    type CFTypeRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFMutableDictionaryRef = *mut c_void;
    type CFStringRef = *const c_void;
    type CFAllocatorRef = *const c_void;
    type CFTypeID = usize;
    type CFIndex = isize;
    type KernReturn = i32;
    type IoObject = u32;

    const KERN_SUCCESS: KernReturn = 0;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_CF_NUMBER_SINT64_TYPE: CFIndex = 4;

    // Two separate `#[link]` attrs is the correct way to link two frameworks; clippy's
    // duplicated-attributes lint trips on the repeated `kind = "framework"`, so allow it.
    #[allow(clippy::duplicated_attributes)]
    #[link(name = "IOKit", kind = "framework")]
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
        fn IOServiceGetMatchingServices(
            main_port: u32,
            matching: CFDictionaryRef,
            existing: *mut IoObject,
        ) -> KernReturn;
        fn IOIteratorNext(iterator: IoObject) -> IoObject;
        fn IOObjectRelease(object: IoObject) -> KernReturn;
        fn IORegistryEntryCreateCFProperties(
            entry: IoObject,
            properties: *mut CFMutableDictionaryRef,
            allocator: CFAllocatorRef,
            options: u32,
        ) -> KernReturn;
        fn CFDictionaryGetValue(dict: CFDictionaryRef, key: *const c_void) -> *const c_void;
        fn CFNumberGetValue(number: *const c_void, the_type: CFIndex, value: *mut c_void) -> u8;
        fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            c_str: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
        fn CFNumberGetTypeID() -> CFTypeID;
        fn CFDictionaryGetTypeID() -> CFTypeID;
        fn CFRelease(cf: CFTypeRef);
    }

    /// Run `f` with a transient `CFString` for `key` (UTF-8), releasing it after. Returns
    /// `None` if the string could not be created.
    fn with_cfstr<T>(key: &str, f: impl FnOnce(CFStringRef) -> Option<T>) -> Option<T> {
        let c = std::ffi::CString::new(key).ok()?;
        // SAFETY: a valid NUL-terminated C string + UTF-8 encoding; the returned string
        // is owned (Create rule) and released below.
        let s = unsafe {
            CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), K_CF_STRING_ENCODING_UTF8)
        };
        if s.is_null() {
            return None;
        }
        let out = f(s);
        // SAFETY: `s` was created by us and is non-null.
        unsafe { CFRelease(s) };
        out
    }

    /// `dict[key]` as an `i64` when it is a `CFNumber`, else `None`. Does not consume the
    /// dictionary or the returned value (a get-rule borrow).
    fn dict_i64(dict: CFDictionaryRef, key: &str) -> Option<i64> {
        with_cfstr(key, |k| {
            // SAFETY: `dict` is a live CFDictionary; `CFDictionaryGetValue` is a get-rule
            // read returning a borrowed value (or null).
            let v = unsafe { CFDictionaryGetValue(dict, k) };
            if v.is_null() {
                return None;
            }
            // SAFETY: type-check before interpreting as a CFNumber.
            if unsafe { CFGetTypeID(v) } != unsafe { CFNumberGetTypeID() } {
                return None;
            }
            let mut out: i64 = 0;
            // SAFETY: `v` is a CFNumber; we read it as SInt64 into our out-param.
            let ok = unsafe {
                CFNumberGetValue(
                    v,
                    K_CF_NUMBER_SINT64_TYPE,
                    std::ptr::addr_of_mut!(out).cast(),
                )
            };
            (ok != 0).then_some(out)
        })
    }

    /// `dict[key]` as a nested `CFDictionary` (borrowed), else `None`.
    fn dict_dict(dict: CFDictionaryRef, key: &str) -> Option<CFDictionaryRef> {
        with_cfstr(key, |k| {
            // SAFETY: get-rule read off a live dictionary.
            let v = unsafe { CFDictionaryGetValue(dict, k) };
            if v.is_null() || unsafe { CFGetTypeID(v) } != unsafe { CFDictionaryGetTypeID() } {
                return None;
            }
            Some(v as CFDictionaryRef)
        })
    }

    /// Iterate every IOKit service matching `class_name`, invoking `visit` with each
    /// service's property dictionary (released after the callback). Handles all the CF
    /// lifetime bookkeeping. Returns `false` if the match/iterator could not be created.
    fn for_each_service(class_name: &str, mut visit: impl FnMut(CFDictionaryRef)) -> bool {
        let c = match std::ffi::CString::new(class_name) {
            Ok(c) => c,
            Err(_) => return false,
        };
        // SAFETY: valid C string; returns an owned matching dict (consumed by the next
        // call, which releases it — so we must NOT release it ourselves).
        let matching = unsafe { IOServiceMatching(c.as_ptr()) };
        if matching.is_null() {
            return false;
        }
        let mut it: IoObject = 0;
        // SAFETY: main port 0 = default; `matching` is consumed; `it` is our out-param.
        let rc = unsafe { IOServiceGetMatchingServices(0, matching as CFDictionaryRef, &mut it) };
        if rc != KERN_SUCCESS || it == 0 {
            return false;
        }
        loop {
            // SAFETY: `it` is a live iterator; returns 0 when exhausted.
            let entry = unsafe { IOIteratorNext(it) };
            if entry == 0 {
                break;
            }
            let mut props: CFMutableDictionaryRef = std::ptr::null_mut();
            // SAFETY: `entry` is a live registry object; on success `props` is an owned
            // dictionary we release after `visit`.
            let pr = unsafe {
                IORegistryEntryCreateCFProperties(entry, &mut props, std::ptr::null(), 0)
            };
            if pr == KERN_SUCCESS && !props.is_null() {
                visit(props as CFDictionaryRef);
                // SAFETY: `props` was created for us and is non-null.
                unsafe { CFRelease(props as CFTypeRef) };
            }
            // SAFETY: release the iterator element.
            unsafe { IOObjectRelease(entry) };
        }
        // SAFETY: release the iterator.
        unsafe { IOObjectRelease(it) };
        true
    }

    /// Whole-machine GPU utilization `0..1`: the max `Device Utilization %` across every
    /// `IOAccelerator` (so a discrete + integrated pair reports the busier one).
    pub(super) fn gpu_util() -> Option<f64> {
        let mut best: Option<f64> = None;
        for_each_service("IOAccelerator", |props| {
            if let Some(stats) = dict_dict(props, "PerformanceStatistics")
                && let Some(pct) = dict_i64(stats, "Device Utilization %")
            {
                let f = (pct as f64 / 100.0).clamp(0.0, 1.0);
                best = Some(best.map_or(f, |b| b.max(f)));
            }
        });
        best
    }

    /// Whole-machine GPU VRAM currently IN USE (bytes): the max `PerformanceStatistics`
    /// "In use system memory" across every `IOAccelerator`. This is an ALL-PROCESS,
    /// whole-GPU figure (unlike Metal's per-process `currentAllocatedSize`, which we
    /// deliberately do NOT use here — it would be a category error to put a per-process
    /// number in the system column). Reuses the exact IOKit helpers `gpu_util` walks.
    pub(super) fn gpu_vram_used() -> Option<u64> {
        let mut best: Option<u64> = None;
        for_each_service("IOAccelerator", |props| {
            if let Some(stats) = dict_dict(props, "PerformanceStatistics")
                && let Some(b) = dict_i64(stats, "In use system memory")
            {
                let u = b.max(0) as u64;
                best = Some(best.map_or(u, |x| x.max(u)));
            }
        });
        best
    }

    /// Cumulative whole-machine disk `(bytes_read, bytes_written)`: the sum of every
    /// `IOBlockStorageDriver`'s `Statistics` `Bytes (Read)` / `Bytes (Write)`.
    pub(super) fn disk_io_bytes() -> Option<(u64, u64)> {
        let (mut r, mut w) = (0u64, 0u64);
        let mut any = false;
        for_each_service("IOBlockStorageDriver", |props| {
            if let Some(stats) = dict_dict(props, "Statistics") {
                if let Some(br) = dict_i64(stats, "Bytes (Read)") {
                    r = r.saturating_add(br.max(0) as u64);
                    any = true;
                }
                if let Some(bw) = dict_i64(stats, "Bytes (Write)") {
                    w = w.saturating_add(bw.max(0) as u64);
                    any = true;
                }
            }
        });
        any.then_some((r, w))
    }
}

/// Metal seam (macOS): the GPU VRAM BUDGET via `[MTLDevice recommendedMaxWorkingSetSize]`.
/// Hand-rolled `objc_msgSend` FFI in the iokit house style (one dependency-free seam),
/// not `objc2`, to keep this module's "raw OS probe" discipline. The budget is device-
/// static, so it is computed ONCE behind a `OnceLock` — the slow-probe worker calls it
/// every pass but only the first touches Metal (no per-tick device creation).
#[cfg(target_os = "macos")]
mod metal {
    use std::os::raw::{c_char, c_void};
    use std::sync::OnceLock;

    /// An Objective-C object pointer (here, an `id<MTLDevice>`).
    type Id = *mut c_void;

    #[link(name = "Metal", kind = "framework")]
    unsafe extern "C" {
        fn MTLCreateSystemDefaultDevice() -> Id;
    }
    #[link(name = "objc")]
    unsafe extern "C" {
        fn sel_registerName(name: *const c_char) -> *const c_void;
        // Niladic base symbol; transmuted per-selector to the concrete return signature.
        fn objc_msgSend();
        fn objc_release(obj: Id);
    }

    /// `[MTLCreateSystemDefaultDevice() recommendedMaxWorkingSetSize]` in bytes, or
    /// `None` on a headless / no-Metal host. Cached: the device budget never changes.
    pub(super) fn vram_budget() -> Option<u64> {
        static BUDGET: OnceLock<Option<u64>> = OnceLock::new();
        *BUDGET.get_or_init(compute_budget)
    }

    fn compute_budget() -> Option<u64> {
        // SAFETY: MTLCreateSystemDefaultDevice returns a retained device or null (no
        // Metal-capable GPU / headless sandbox).
        let dev = unsafe { MTLCreateSystemDefaultDevice() };
        if dev.is_null() {
            return None;
        }
        // SAFETY: a valid NUL-terminated selector name; returns a process-global SEL.
        let sel = unsafe { sel_registerName(c"recommendedMaxWorkingSetSize".as_ptr()) };
        // `recommendedMaxWorkingSetSize` is a niladic property returning NSUInteger
        // (u64 on arm64/x86_64); transmute the base `objc_msgSend` to that signature.
        type Msg = unsafe extern "C" fn(Id, *const c_void) -> u64;
        // SAFETY: the objc_msgSend ABI for a u64-returning niladic message; the base
        // symbol is the correct entry point for a scalar (non-struct) return on both
        // arm64 and x86_64. Transmute the fn pointer to the concrete selector signature.
        let send: Msg = unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
        // SAFETY: `dev` is a live MTLDevice; `sel` is a registered selector it responds to.
        let v = unsafe { send(dev, sel) };
        // SAFETY: balance the retain MTLCreateSystemDefaultDevice handed us.
        unsafe { objc_release(dev) };
        (v > 0).then_some(v)
    }
}

// =============================================================================
// Per-tab / filesystem / link-speed probes for the unified metrics service.
// Merged from feat/widget-tray (the `metrics_service` + `widgets` control verb
// consume these). Complementary to the whole-machine probes above; no name
// overlap. macOS `libc` / Win32 FFI behind safe `Option`-returning wrappers;
// other targets degrade to `None`.
// =============================================================================

/// `(free_bytes, total_bytes)` of the filesystem holding `path` (free = available
/// to a non-root user). `None` if the path can't be stat'd.
#[must_use]
pub(crate) fn disk_for(path: &str) -> Option<(u64, u64)> {
    #[cfg(target_os = "macos")]
    {
        disk_for_macos(path)
    }
    #[cfg(windows)]
    {
        win::disk_for(path)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = path;
        None
    }
}

/// `(cpu_time_ns, rss_bytes)` summed over `pid` and its direct children — a tab's
/// shell job tree (`pid` is the PTY child / process-group leader). The caller diffs
/// the CPU time across samples for a utilization fraction. `None` if `pid` is gone.
#[must_use]
pub(crate) fn proc_tree_cpu_rss(pid: i32) -> Option<(u64, u64)> {
    #[cfg(target_os = "macos")]
    {
        proc_tree_cpu_rss_macos(pid)
    }
    #[cfg(windows)]
    {
        win::proc_tree_cpu_rss(pid)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = pid;
        None
    }
}

/// Nominal link speed (bits/sec) of the fastest UP non-loopback interface, or
/// `None`. Used as the denominator for a "is the network slow?" heuristic.
#[must_use]
pub(crate) fn net_primary_baud() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        net_primary_baud_macos()
    }
    #[cfg(windows)]
    {
        win::net_primary_baud()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        None
    }
}

/// A link whose nominal baud is below this (but non-zero) is classed `Slow` — legacy /
/// cellular / degraded links. `ifi_baudrate` reports 0 (unknown) on many interfaces, so
/// the classifier only calls `Slow` on a POSITIVE-but-low reading, never on `0`.
pub(crate) const SLOW_BAUD: u64 = 10_000_000; // 10 Mbit/s

/// Classify coarse network health from the reachability probe + link facts. PURE and
/// target-independent (the macOS reachability seam feeds it; off-macOS `reachable` is
/// always `None`), so it is exhaustively conformance-tested against the
/// `net_health_model` ty machine. Honesty rules:
/// - no non-loopback link at all ⇒ `Offline` (link down is provably offline);
/// - the reachability probe couldn't decide (`None`) ⇒ `Unknown` (never a fabricated
///   `Offline`/`Online`);
/// - a POSITIVE reachable proof is required for `Online`/`Slow`; a route that is only
///   reachable *after* establishing a connection (`conn_required`) is not currently
///   online, so it is `Offline`;
/// - reachable but transient or below [`SLOW_BAUD`] ⇒ `Slow`.
pub(crate) fn net_health_classify(
    reachable: Option<bool>,
    conn_required: bool,
    transient: bool,
    has_link: bool,
    slow: bool,
) -> crate::metrics_service::NetHealth {
    use crate::metrics_service::NetHealth;
    if !has_link {
        return NetHealth::Offline;
    }
    match reachable {
        None => NetHealth::Unknown,
        Some(false) => NetHealth::Offline,
        Some(true) if conn_required => NetHealth::Offline,
        Some(true) if transient || slow => NetHealth::Slow,
        Some(true) => NetHealth::Online,
    }
}

/// Coarse link/internet health for the network widget — replaces the prior two-state
/// (Online/Unknown) heuristic. On macOS it probes `SCNetworkReachability` of the default
/// route (a fast, synchronous, LOCAL query — no network round-trip) and folds in the
/// fastest link's baud; off macOS there is no reachability seam, so `reachable` is
/// `None` and it degrades to the honest link heuristic (Offline with no link, else
/// Unknown). Runs on the metrics-service tick (never the IOKit slow-probe worker: this
/// is not a multi-millisecond registry walk).
#[must_use]
pub(crate) fn net_health(
    ifaces: &Option<Vec<(String, u32, u32)>>,
    link_baud: Option<u64>,
) -> crate::metrics_service::NetHealth {
    let has_link = ifaces.as_ref().is_some_and(|l| !l.is_empty());
    let slow = link_baud.is_some_and(|b| (1..SLOW_BAUD).contains(&b));
    #[cfg(target_os = "macos")]
    let (reachable, conn_required, transient) = match netreach::probe() {
        Some((r, c, t)) => (Some(r), c, t),
        None => (None, false, false),
    };
    #[cfg(not(target_os = "macos"))]
    let (reachable, conn_required, transient) = (None::<bool>, false, false);
    net_health_classify(reachable, conn_required, transient, has_link, slow)
}

/// SCNetworkReachability seam (macOS): probe the default route's reachability flags.
/// Hand-rolled framework FFI in the iokit house style; `None` means the probe could not
/// be created or read (an honest "couldn't determine", never a fabricated Offline).
#[cfg(target_os = "macos")]
mod netreach {
    use std::os::raw::c_void;

    // SCNetworkReachabilityFlags bits (SystemConfiguration/SCNetworkReachability.h).
    const REACHABLE: u32 = 1 << 1;
    const CONNECTION_REQUIRED: u32 = 1 << 2;
    const TRANSIENT_CONNECTION: u32 = 1 << 0;

    #[allow(clippy::duplicated_attributes)]
    #[link(name = "SystemConfiguration", kind = "framework")]
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn SCNetworkReachabilityCreateWithAddress(
            alloc: *const c_void,
            address: *const libc::sockaddr,
        ) -> *const c_void;
        fn SCNetworkReachabilityGetFlags(target: *const c_void, flags: *mut u32) -> u8;
        fn CFRelease(cf: *const c_void);
    }

    /// `(reachable, connection_required, transient)` of the default route, or `None` if
    /// the reachability target could not be created / the flags could not be read.
    pub(super) fn probe() -> Option<(bool, bool, bool)> {
        // The default route is the all-zero IPv4 address (0.0.0.0, AF_INET).
        // SAFETY: a zeroed `sockaddr_in` is a valid POD; we fill the length/family.
        let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        sa.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
        sa.sin_family = libc::AF_INET as u8;
        // SAFETY: `SCNetworkReachabilityCreateWithAddress` COPIES the sockaddr; it
        // returns an owned ref (Create rule) or null. The cast reinterprets our
        // `sockaddr_in` as the generic `sockaddr` the API takes.
        let target = unsafe {
            SCNetworkReachabilityCreateWithAddress(std::ptr::null(), std::ptr::addr_of!(sa).cast())
        };
        if target.is_null() {
            return None;
        }
        let mut flags: u32 = 0;
        // SAFETY: `target` is a live reachability ref; `flags` is our out-param.
        let ok = unsafe { SCNetworkReachabilityGetFlags(target, &mut flags) };
        // SAFETY: release the ref we created (Create rule); `target` is non-null.
        unsafe { CFRelease(target) };
        (ok != 0).then_some((
            flags & REACHABLE != 0,
            flags & CONNECTION_REQUIRED != 0,
            flags & TRANSIENT_CONNECTION != 0,
        ))
    }
}

#[cfg(target_os = "macos")]
fn disk_for_macos(path: &str) -> Option<(u64, u64)> {
    use std::ffi::CString;
    let c = CString::new(path).ok()?;
    // SAFETY: statfs writes the mounted-fs stats for `path` into the zeroed struct.
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let bsize = u64::from(s.f_bsize);
    Some((s.f_bavail * bsize, s.f_blocks * bsize))
}

#[cfg(target_os = "macos")]
fn proc_rusage(pid: i32) -> Option<(u64, u64)> {
    // SAFETY: proc_pid_rusage fills the v2 record (which carries ri_user_time /
    // ri_system_time in ns and ri_phys_footprint in bytes) for `pid`. Zeroed POD.
    let mut ri: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, std::ptr::addr_of_mut!(ri).cast())
    };
    if rc != 0 {
        return None;
    }
    // Second element is phys_footprint (the memory-pressure ledger figure), NOT
    // ri_resident_size — see ProcSample::footprint / MEM-ACCT-1. The `_rss` in the
    // caller names are retained lineage (and the aterm-ctl JSON key `tab_rss`).
    Some((ri.ri_user_time + ri.ri_system_time, ri.ri_phys_footprint))
}

/// MEM-ACCT-3 pure subtree fold: sum `(cpu, rss)` over `root` and ALL of its
/// descendants — not just direct children — via a bounded DFS mirroring
/// [`session_procs_macos`]. `children_of(pid)` yields a pid's direct children;
/// `usage_of(pid)` yields its `(cpu, rss)` or `None` when unsampleable (that node is
/// skipped, but its children are still walked). `budget` caps the visited-node count so
/// a pathological process count can never stall the poll tick. `None` iff the ROOT is
/// unsampleable (a dead tab). Pure over the two closures, so grandchild inclusion + the
/// bound are unit-tested without live pids (the platform FFI supplies the closures).
// Production caller is macOS-only (`proc_tree_cpu_rss_macos`); `test` keeps it compiled
// for the portable unit test on every host.
#[cfg(any(target_os = "macos", test))]
#[must_use]
fn subtree_cpu_rss<C, U>(
    root: i32,
    budget: u32,
    mut children_of: C,
    mut usage_of: U,
) -> Option<(u64, u64)>
where
    C: FnMut(i32) -> Vec<i32>,
    U: FnMut(i32) -> Option<(u64, u64)>,
{
    let (mut cpu, mut rss) = usage_of(root)?; // the shell itself must be sampleable
    // Seed with the root's direct children; the root is already counted above.
    let mut stack: Vec<i32> = children_of(root)
        .into_iter()
        .filter(|&c| c > 0 && c != root)
        .collect();
    let mut left = budget;
    while let Some(p) = stack.pop() {
        if left == 0 {
            break;
        }
        left -= 1;
        if let Some((c, r)) = usage_of(p) {
            cpu = cpu.saturating_add(c);
            rss = rss.saturating_add(r);
        }
        for c in children_of(p) {
            if c > 0 && c != p {
                stack.push(c);
            }
        }
    }
    Some((cpu, rss))
}

#[cfg(target_os = "macos")]
fn proc_tree_cpu_rss_macos(pid: i32) -> Option<(u64, u64)> {
    // MEM-ACCT-3: sum over the WHOLE descendant tree (shell → make → cc → …) via the
    // shared bounded DFS, not the pre-fix single `proc_listchildpids` level that missed
    // grandchildren — so a `make -j` build's compiler processes are attributed to the tab.
    let mut childbuf: Vec<i32> = Vec::new();
    subtree_cpu_rss(
        pid,
        4096, // the same pathological-count bound as session_procs_macos
        |p| {
            children_into(p, &mut childbuf);
            childbuf.clone()
        },
        proc_rusage,
    )
}

#[cfg(target_os = "macos")]
fn net_primary_baud_macos() -> Option<u64> {
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs allocates a list into `ifap`; freed below.
    if unsafe { libc::getifaddrs(&mut ifap) } != 0 || ifap.is_null() {
        return None;
    }
    let mut best = 0u64;
    let mut cur = ifap;
    // SAFETY: walk the list; AF_LINK nodes carry `if_data` with `ifi_baudrate`.
    unsafe {
        while !cur.is_null() {
            let ifa = &*cur;
            if !ifa.ifa_addr.is_null()
                && i32::from((*ifa.ifa_addr).sa_family) == libc::AF_LINK
                && (ifa.ifa_flags & libc::IFF_LOOPBACK as u32) == 0
                && (ifa.ifa_flags & libc::IFF_UP as u32) != 0
                && !ifa.ifa_data.is_null()
            {
                let d = &*(ifa.ifa_data as *const libc::if_data);
                best = best.max(u64::from(d.ifi_baudrate));
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    (best > 0).then_some(best)
}

/// MEM-ACCT-3(b): register the macOS memory-pressure notifier. `on_pressure(critical)`
/// runs on a libdispatch background thread whenever the OS raises a WARN or CRITICAL
/// memory-pressure event, so the app can SHED reclaimable memory (trim scrollback) before
/// jetsam kills the process — the live consumer the scrollback watermark plumbing lacked.
/// Keep the callback cheap + thread-safe: in practice it only posts an `EventLoopProxy`
/// event, and the actual shedding runs on the main thread. Registered once at startup.
/// A no-op on non-macOS (only macOS has this source).
#[cfg(target_os = "macos")]
pub(crate) fn install_memory_pressure_source<F>(on_pressure: F)
where
    F: Fn(bool) + Send + 'static,
{
    memory_pressure::install(on_pressure);
}

/// The libdispatch `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE` seam — all raw dispatch FFI is
/// confined here, mirroring the "one seam for unsafe" discipline of the objc/IOKit
/// modules above. `libdispatch` ships in `libSystem` (auto-linked on macOS), so the
/// symbols need no explicit `#[link]`.
#[cfg(target_os = "macos")]
mod memory_pressure {
    use std::os::raw::c_void;
    use std::ptr;

    type DispatchObject = *mut c_void;

    // DISPATCH_MEMORYPRESSURE_* mask bits (dispatch/source.h).
    const WARN: usize = 0x02;
    const CRITICAL: usize = 0x04;
    // DISPATCH_QUEUE_PRIORITY_DEFAULT.
    const QUEUE_PRIORITY_DEFAULT: isize = 0;

    unsafe extern "C" {
        // The source TYPE is a process-global struct; DISPATCH_SOURCE_TYPE_MEMORYPRESSURE
        // is its address.
        #[allow(non_upper_case_globals)]
        static _dispatch_source_type_memorypressure: c_void;
        fn dispatch_source_create(
            ty: *const c_void,
            handle: usize,
            mask: usize,
            queue: DispatchObject,
        ) -> DispatchObject;
        fn dispatch_get_global_queue(identifier: isize, flags: usize) -> DispatchObject;
        fn dispatch_source_set_event_handler_f(
            source: DispatchObject,
            handler: extern "C" fn(*mut c_void),
        );
        fn dispatch_set_context(object: DispatchObject, context: *mut c_void);
        fn dispatch_source_get_data(source: DispatchObject) -> usize;
        fn dispatch_resume(object: DispatchObject);
    }

    /// Leaked handler context: the user callback + the source (read for its pressure
    /// level in the handler). Lives for the process lifetime.
    struct Ctx {
        on_pressure: Box<dyn Fn(bool) + Send>,
        source: DispatchObject,
    }
    // SAFETY: the handler runs on a serial dispatch queue; the callback is `Send` and the
    // leaked `source` pointer is only READ (get_data), never freed — no aliasing/free race.
    unsafe impl Send for Ctx {}

    extern "C" fn handler(ctx: *mut c_void) {
        if ctx.is_null() {
            return;
        }
        // SAFETY: `ctx` is the leaked `Box<Ctx>` pointer set via `dispatch_set_context`;
        // it lives for the process lifetime, so the reference is always valid.
        let ctx = unsafe { &*(ctx as *const Ctx) };
        // SAFETY: `ctx.source` is the live source this handler is attached to.
        let level = unsafe { dispatch_source_get_data(ctx.source) };
        let critical = level & CRITICAL != 0;
        // Never unwind across the C boundary (UB): contain any panic in the callback.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (ctx.on_pressure)(critical);
        }));
    }

    pub(super) fn install<F>(on_pressure: F)
    where
        F: Fn(bool) + Send + 'static,
    {
        // SAFETY: standard libdispatch source setup. The global background queue and the
        // memorypressure source type are process-global; the source + its `Ctx` are
        // deliberately leaked (one registration for the whole process), so there is no
        // lifetime or free race — the handler's context always outlives every event.
        unsafe {
            let queue = dispatch_get_global_queue(QUEUE_PRIORITY_DEFAULT, 0);
            let source = dispatch_source_create(
                ptr::addr_of!(_dispatch_source_type_memorypressure),
                0,
                WARN | CRITICAL,
                queue,
            );
            if source.is_null() {
                return;
            }
            let ctx = Box::into_raw(Box::new(Ctx {
                on_pressure: Box::new(on_pressure),
                source,
            }));
            dispatch_set_context(source, ctx.cast());
            dispatch_source_set_event_handler_f(source, handler);
            dispatch_resume(source);
            // `source` + `*ctx` intentionally leaked — they must outlive every future
            // memory-pressure event for the process lifetime.
        }
    }
}

// --- Windows implementations (hand-rolled Win32 FFI) -------------------------

/// The ONLY unsafe-FFI seam of the Windows probes: direct `extern "system"`
/// declarations against kernel32 + iphlpapi in the aterm-pty ffi.rs house style
/// (handles as plain `isize`, struct layouts transcribed from the Windows SDK),
/// each behind a safe `Option`-returning wrapper.
#[cfg(windows)]
mod win {
    // Win32 ABI names are kept verbatim (PascalCase fields, SCREAMING struct
    // names) so they can be checked against the SDK headers line by line.
    #![allow(non_snake_case, non_camel_case_types, clippy::upper_case_acronyms)]

    use super::ProcSample;
    use std::ffi::c_void;

    /// A Win32 HANDLE as a plain pointer-sized integer (aterm-pty convention).
    type HANDLE = isize;
    const INVALID_HANDLE_VALUE: HANDLE = -1;
    /// `OpenProcess` right sufficient for times/memory/io queries (Vista+).
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    /// `CreateToolhelp32Snapshot` flag: include the system process list.
    const TH32CS_SNAPPROCESS: u32 = 0x2;
    /// `IF_TYPE_SOFTWARE_LOOPBACK` (ipifcons.h).
    const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
    /// `IfOperStatusUp` (ifdef.h).
    const IF_OPER_STATUS_UP: u32 = 1;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const OPEN_EXISTING: u32 = 3;
    /// `IOCTL_DISK_PERFORMANCE` (winioctl.h).
    const IOCTL_DISK_PERFORMANCE: u32 = 0x0007_0020;

    /// `FILETIME` — a 64-bit count of 100ns units split into two u32 halves.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct FILETIME {
        dwLowDateTime: u32,
        dwHighDateTime: u32,
    }

    impl FILETIME {
        fn as_100ns(self) -> u64 {
            (u64::from(self.dwHighDateTime) << 32) | u64::from(self.dwLowDateTime)
        }
    }

    /// `MEMORYSTATUSEX` — `dwLength` must be set before the call (version check).
    #[repr(C)]
    #[allow(dead_code)] // full SDK layout; only the physical-RAM fields are read
    struct MEMORYSTATUSEX {
        dwLength: u32,
        dwMemoryLoad: u32,
        ullTotalPhys: u64,
        ullAvailPhys: u64,
        ullTotalPageFile: u64,
        ullAvailPageFile: u64,
        ullTotalVirtual: u64,
        ullAvailVirtual: u64,
        ullAvailExtendedVirtual: u64,
    }

    /// `PROCESS_MEMORY_COUNTERS` (psapi.h).
    #[repr(C)]
    #[allow(dead_code)] // full SDK layout; only WorkingSetSize is read
    struct PROCESS_MEMORY_COUNTERS {
        cb: u32,
        PageFaultCount: u32,
        PeakWorkingSetSize: usize,
        WorkingSetSize: usize,
        QuotaPeakPagedPoolUsage: usize,
        QuotaPagedPoolUsage: usize,
        QuotaPeakNonPagedPoolUsage: usize,
        QuotaNonPagedPoolUsage: usize,
        PagefileUsage: usize,
        PeakPagefileUsage: usize,
    }

    /// `IO_COUNTERS`.
    #[repr(C)]
    #[allow(dead_code)] // full SDK layout; only the transfer counts are read
    struct IO_COUNTERS {
        ReadOperationCount: u64,
        WriteOperationCount: u64,
        OtherOperationCount: u64,
        ReadTransferCount: u64,
        WriteTransferCount: u64,
        OtherTransferCount: u64,
    }

    /// `PROCESSENTRY32W` (tlhelp32.h) — `dwSize` must be set before the call.
    #[repr(C)]
    #[allow(dead_code)] // full SDK layout; only the pid/ppid edge is read
    struct PROCESSENTRY32W {
        dwSize: u32,
        cntUsage: u32,
        th32ProcessID: u32,
        th32DefaultHeapID: usize,
        th32ModuleID: u32,
        cntThreads: u32,
        th32ParentProcessID: u32,
        pcPriClassBase: i32,
        dwFlags: u32,
        szExeFile: [u16; 260],
    }

    /// `GUID` — layout-only here (never read; carried for the row layout below).
    #[repr(C)]
    #[allow(dead_code)]
    struct GUID {
        Data1: u32,
        Data2: u16,
        Data3: u16,
        Data4: [u8; 8],
    }

    /// `MIB_IF_ROW2` (netioapi.h), transcribed field for field. The SDK's
    /// `InterfaceAndOperStatusFlags` is eight one-bit `BOOLEAN` bitfields — one
    /// byte, represented as a plain `u8` (the following `OperStatus` re-aligns
    /// to 4 exactly as MSVC lays it out; total size 1352 on x64).
    #[repr(C)]
    #[allow(dead_code)] // full SDK layout; only type/status/octets/speed are read
    struct MIB_IF_ROW2 {
        InterfaceLuid: u64,
        InterfaceIndex: u32,
        InterfaceGuid: GUID,
        Alias: [u16; 257],
        Description: [u16; 257],
        PhysicalAddressLength: u32,
        PhysicalAddress: [u8; 32],
        PermanentPhysicalAddress: [u8; 32],
        Mtu: u32,
        Type: u32,
        TunnelType: u32,
        MediaType: u32,
        PhysicalMediumType: u32,
        AccessType: u32,
        DirectionType: u32,
        InterfaceAndOperStatusFlags: u8,
        OperStatus: u32,
        AdminStatus: u32,
        MediaConnectState: u32,
        NetworkGuid: GUID,
        ConnectionType: u32,
        TransmitLinkSpeed: u64,
        ReceiveLinkSpeed: u64,
        InOctets: u64,
        InUcastPkts: u64,
        InNUcastPkts: u64,
        InDiscards: u64,
        InErrors: u64,
        InUnknownProtos: u64,
        InUcastOctets: u64,
        InMulticastOctets: u64,
        InBroadcastOctets: u64,
        OutOctets: u64,
        OutUcastPkts: u64,
        OutNUcastPkts: u64,
        OutDiscards: u64,
        OutErrors: u64,
        OutUcastOctets: u64,
        OutMulticastOctets: u64,
        OutBroadcastOctets: u64,
        OutQLen: u64,
    }

    /// `MIB_IF_TABLE2` — `NumEntries` rows follow inline (flexible array member;
    /// declared `[.. ; 1]` and indexed via raw pointer arithmetic only).
    #[repr(C)]
    struct MIB_IF_TABLE2 {
        NumEntries: u32,
        Table: [MIB_IF_ROW2; 1],
    }

    /// `DISK_PERFORMANCE` (winioctl.h).
    #[repr(C)]
    #[allow(dead_code)] // full SDK layout; only the byte counters are read
    struct DISK_PERFORMANCE {
        BytesRead: i64,
        BytesWritten: i64,
        ReadTime: i64,
        WriteTime: i64,
        IdleTime: i64,
        ReadCount: u32,
        WriteCount: u32,
        QueueDepth: u32,
        SplitCount: u32,
        QueryTime: i64,
        StorageDeviceNumber: u32,
        StorageManagerName: [u16; 8],
    }

    // Guard the hand-transcribed layouts against drift (the x64 SDK sizes; the
    // bitfield byte + tail padding in MIB_IF_ROW2 are the easy ones to get wrong).
    #[cfg(target_pointer_width = "64")]
    const _: () = {
        assert!(std::mem::size_of::<MIB_IF_ROW2>() == 1352);
        assert!(std::mem::offset_of!(MIB_IF_TABLE2, Table) == 8);
        assert!(std::mem::size_of::<MEMORYSTATUSEX>() == 64);
        assert!(std::mem::size_of::<PROCESSENTRY32W>() == 568);
        assert!(std::mem::size_of::<DISK_PERFORMANCE>() == 88);
    };

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(lpBuffer: *mut MEMORYSTATUSEX) -> i32;
        fn GetSystemTimes(
            lpIdleTime: *mut FILETIME,
            lpKernelTime: *mut FILETIME,
            lpUserTime: *mut FILETIME,
        ) -> i32;
        fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> HANDLE;
        fn GetProcessTimes(
            hProcess: HANDLE,
            lpCreationTime: *mut FILETIME,
            lpExitTime: *mut FILETIME,
            lpKernelTime: *mut FILETIME,
            lpUserTime: *mut FILETIME,
        ) -> i32;
        fn GetProcessIoCounters(hProcess: HANDLE, lpIoCounters: *mut IO_COUNTERS) -> i32;
        // The psapi entry point as exported by kernel32 since Win7 (no psapi.lib).
        fn K32GetProcessMemoryInfo(
            Process: HANDLE,
            ppsmemCounters: *mut PROCESS_MEMORY_COUNTERS,
            cb: u32,
        ) -> i32;
        fn CloseHandle(hObject: HANDLE) -> i32;
        fn CreateToolhelp32Snapshot(dwFlags: u32, th32ProcessID: u32) -> HANDLE;
        fn Process32FirstW(hSnapshot: HANDLE, lppe: *mut PROCESSENTRY32W) -> i32;
        fn Process32NextW(hSnapshot: HANDLE, lppe: *mut PROCESSENTRY32W) -> i32;
        fn GetDiskFreeSpaceExW(
            lpDirectoryName: *const u16,
            lpFreeBytesAvailableToCaller: *mut u64,
            lpTotalNumberOfBytes: *mut u64,
            lpTotalNumberOfFreeBytes: *mut u64,
        ) -> i32;
        fn CreateFileW(
            lpFileName: *const u16,
            dwDesiredAccess: u32,
            dwShareMode: u32,
            lpSecurityAttributes: *mut c_void,
            dwCreationDisposition: u32,
            dwFlagsAndAttributes: u32,
            hTemplateFile: HANDLE,
        ) -> HANDLE;
        fn DeviceIoControl(
            hDevice: HANDLE,
            dwIoControlCode: u32,
            lpInBuffer: *mut c_void,
            nInBufferSize: u32,
            lpOutBuffer: *mut c_void,
            nOutBufferSize: u32,
            lpBytesReturned: *mut u32,
            lpOverlapped: *mut c_void,
        ) -> i32;
    }

    #[link(name = "iphlpapi")]
    unsafe extern "system" {
        fn GetIfTable2(Table: *mut *mut MIB_IF_TABLE2) -> u32;
        fn FreeMibTable(Memory: *mut c_void);
    }

    /// RAII guard around a raw kernel HANDLE (closed on drop); `open` maps both
    /// failure sentinels (0 and `INVALID_HANDLE_VALUE`) to `None`.
    struct Handle(HANDLE);

    impl Handle {
        fn open(raw: HANDLE) -> Option<Self> {
            (raw != 0 && raw != INVALID_HANDLE_VALUE).then_some(Self(raw))
        }

        fn raw(&self) -> HANDLE {
            self.0
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: `open` guaranteed a live handle this guard solely owns.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// UTF-16 with a terminating NUL, for the *W entry points.
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The UTF-16 buffer up to its first NUL, lossily decoded.
    fn wide_to_string(w: &[u16]) -> String {
        let len = w.iter().position(|&c| c == 0).unwrap_or(w.len());
        String::from_utf16_lossy(&w[..len])
    }

    fn mem_status() -> Option<MEMORYSTATUSEX> {
        // SAFETY: zeroed POD; the API fills it after we set `dwLength` (its
        // version check), which we do before the call below.
        let mut ms: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        ms.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        // SAFETY: valid out-param with its `dwLength` set.
        (unsafe { GlobalMemoryStatusEx(&mut ms) } != 0).then_some(ms)
    }

    pub(super) fn mem_total() -> Option<u64> {
        mem_status().map(|ms| ms.ullTotalPhys)
    }

    pub(super) fn mem_used_frac() -> Option<f64> {
        let ms = mem_status()?;
        let total = ms.ullTotalPhys as f64;
        if total > 0.0 {
            Some((1.0 - ms.ullAvailPhys as f64 / total).clamp(0.0, 1.0))
        } else {
            None
        }
    }

    pub(super) fn cpu_ticks() -> Option<[u64; 4]> {
        let (mut idle, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: three valid FILETIME out-params (all-processor cumulative totals).
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
            return None;
        }
        let (idle, kernel, user) = (idle.as_100ns(), kernel.as_100ns(), user.as_100ns());
        // Windows kernel time INCLUDES idle; there is no nice class (slot stays 0).
        Some([user, kernel.saturating_sub(idle), idle, 0])
    }

    /// Run `visit` over every `MIB_IF_ROW2` of a freshly allocated interface
    /// table, owning the `GetIfTable2`/`FreeMibTable` lifetime. `false` if the
    /// table could not be fetched.
    fn for_each_iface(mut visit: impl FnMut(&MIB_IF_ROW2)) -> bool {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        // SAFETY: on NO_ERROR (0) the API allocates the table into our out-param;
        // released via FreeMibTable below.
        if unsafe { GetIfTable2(&mut table) } != 0 || table.is_null() {
            return false;
        }
        // SAFETY: `NumEntries` rows follow inline (flexible array member) and stay
        // valid until FreeMibTable. The row pointer is derived from `table` (whole-
        // allocation provenance), so `add(i)` past the declared `[..; 1]` is in
        // bounds of the allocation.
        unsafe {
            let n = (*table).NumEntries as usize;
            let rows: *const MIB_IF_ROW2 = std::ptr::addr_of!((*table).Table).cast();
            for i in 0..n {
                visit(&*rows.add(i));
            }
            FreeMibTable(table.cast());
        }
        true
    }

    pub(super) fn net_ifaces() -> Option<Vec<(String, u32, u32)>> {
        let mut out: Vec<(String, u32, u32)> = Vec::new();
        let ok = for_each_iface(|row| {
            if row.Type == IF_TYPE_SOFTWARE_LOOPBACK || row.OperStatus != IF_OPER_STATUS_UP {
                return;
            }
            // The 64-bit octet counters are truncated to u32 to match the `if_data`
            // width the caller already wrap-subtracts (same convention as Linux).
            out.push((
                wide_to_string(&row.Alias),
                row.InOctets as u32,
                row.OutOctets as u32,
            ));
        });
        ok.then_some(out)
    }

    pub(super) fn net_primary_baud() -> Option<u64> {
        let mut best = 0u64;
        for_each_iface(|row| {
            // u64::MAX is the SDK's "speed unknown" sentinel — never a real link.
            if row.Type != IF_TYPE_SOFTWARE_LOOPBACK
                && row.OperStatus == IF_OPER_STATUS_UP
                && row.TransmitLinkSpeed != u64::MAX
            {
                best = best.max(row.TransmitLinkSpeed);
            }
        });
        (best > 0).then_some(best)
    }

    pub(super) fn proc_usage(pid: i32) -> Option<ProcSample> {
        if pid <= 0 {
            return None;
        }
        // SAFETY: query-only open; 0 means gone or access denied.
        let h =
            Handle::open(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32) })?;
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: live process handle + four FILETIME out-params.
        if unsafe { GetProcessTimes(h.raw(), &mut created, &mut exited, &mut kernel, &mut user) }
            == 0
        {
            return None;
        }
        // Process FILETIMEs are 100ns DURATIONS (unlike the wall-clock creation
        // time, which we ignore), so ns = ticks × 100.
        let cpu_ns = kernel
            .as_100ns()
            .saturating_add(user.as_100ns())
            .saturating_mul(100);
        // The memory/io counters degrade to 0 individually rather than losing the
        // whole sample (CPU is the field every consumer diffs).
        // SAFETY: zeroed POD; `cb` is the API's size/version check.
        let mut pmc: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        // SAFETY: live handle + sized out-param.
        let rss = if unsafe { K32GetProcessMemoryInfo(h.raw(), &mut pmc, pmc.cb) } != 0 {
            pmc.WorkingSetSize as u64
        } else {
            0
        };
        // SAFETY: zeroed POD out-param for the cumulative transfer counts. Windows
        // has no public disk-only per-process figure — these include pipe/socket
        // I/O too (the same "honesty" caveat as the module doc's network note).
        let mut io: IO_COUNTERS = unsafe { std::mem::zeroed() };
        // SAFETY: live handle + out-param.
        let (disk_read, disk_write) = if unsafe { GetProcessIoCounters(h.raw(), &mut io) } != 0 {
            (io.ReadTransferCount, io.WriteTransferCount)
        } else {
            (0, 0)
        };
        Some(ProcSample {
            pid,
            cpu_ns,
            // Windows has no phys_footprint analog; WorkingSetSize is the closest
            // per-process footprint proxy (see ProcSample::footprint).
            footprint: rss,
            disk_read,
            disk_write,
        })
    }

    /// Every `(pid, parent_pid)` edge in a Toolhelp32 process snapshot, or `None`.
    fn proc_edges() -> Option<Vec<(u32, u32)>> {
        // SAFETY: returns a snapshot handle or INVALID_HANDLE_VALUE; guard-closed.
        let h = Handle::open(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })?;
        // SAFETY: zeroed POD; `dwSize` is the API's size/version check.
        let mut pe: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        pe.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        // SAFETY: live snapshot handle + sized entry out-param.
        if unsafe { Process32FirstW(h.raw(), &mut pe) } == 0 {
            return None;
        }
        let mut out = Vec::new();
        loop {
            out.push((pe.th32ProcessID, pe.th32ParentProcessID));
            // SAFETY: same handle/out-param; 0 means the walk is exhausted.
            if unsafe { Process32NextW(h.raw(), &mut pe) } == 0 {
                break;
            }
        }
        Some(out)
    }

    pub(super) fn session_procs(root: i32) -> Option<Vec<ProcSample>> {
        if root <= 0 {
            return None;
        }
        let edges = proc_edges()?;
        // BFS over the pid→parent edges. The membership check both dedupes and
        // guards against parent-pid cycles (a dead ancestor's pid can be REUSED by
        // a descendant); the cap mirrors the macOS walk's budget.
        let mut members: Vec<u32> = vec![root as u32];
        let mut i = 0;
        while i < members.len() && members.len() < 4096 {
            let cur = members[i];
            i += 1;
            for &(pid, ppid) in &edges {
                if ppid == cur && pid != cur && !members.contains(&pid) {
                    members.push(pid);
                }
            }
        }
        let out: Vec<ProcSample> = members
            .iter()
            .filter_map(|&p| proc_usage(p as i32))
            .collect();
        (!out.is_empty()).then_some(out)
    }

    pub(super) fn proc_tree_cpu_rss(pid: i32) -> Option<(u64, u64)> {
        let root = proc_usage(pid)?; // the shell itself must be sampleable
        let (mut cpu, mut rss) = (root.cpu_ns, root.footprint);
        // Direct children only, matching the macOS arm's one-level walk.
        if let Some(edges) = proc_edges() {
            for (p, pp) in edges {
                if pp == pid as u32
                    && p != pid as u32
                    && let Some(s) = proc_usage(p as i32)
                {
                    cpu = cpu.saturating_add(s.cpu_ns);
                    rss = rss.saturating_add(s.footprint);
                }
            }
        }
        Some((cpu, rss))
    }

    pub(super) fn disk_for(path: &str) -> Option<(u64, u64)> {
        let wpath = wide(path);
        let (mut avail, mut total) = (0u64, 0u64);
        // SAFETY: NUL-terminated wide path + two u64 (ULARGE_INTEGER) out-params;
        // the third out-param is documented-optional (null). `avail` is the
        // caller-visible quota-aware figure, matching the unix f_bavail
        // "free to a non-root user" contract.
        let ok = unsafe {
            GetDiskFreeSpaceExW(wpath.as_ptr(), &mut avail, &mut total, std::ptr::null_mut())
        };
        (ok != 0).then_some((avail, total))
    }

    pub(super) fn disk_io_bytes() -> Option<(u64, u64)> {
        let (mut r, mut w) = (0u64, 0u64);
        let mut any = false;
        // Physical drive numbers can be sparse after hot-unplug, so probe a fixed
        // range and skip gaps rather than stopping at the first miss.
        for n in 0..16u32 {
            let path = wide(&format!("\\\\.\\PhysicalDrive{n}"));
            // SAFETY: query-metadata open (desired access 0) — sufficient for
            // IOCTL_DISK_PERFORMANCE and allowed for non-admin callers.
            let raw = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null_mut(),
                    OPEN_EXISTING,
                    0,
                    0,
                )
            };
            let Some(h) = Handle::open(raw) else {
                continue;
            };
            // SAFETY: zeroed POD out-buffer.
            let mut perf: DISK_PERFORMANCE = unsafe { std::mem::zeroed() };
            let mut got = 0u32;
            // SAFETY: live drive handle; out-buffer + its byte size + returned-size
            // out-param; no input buffer or OVERLAPPED.
            let ok = unsafe {
                DeviceIoControl(
                    h.raw(),
                    IOCTL_DISK_PERFORMANCE,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::addr_of_mut!(perf).cast(),
                    std::mem::size_of::<DISK_PERFORMANCE>() as u32,
                    &mut got,
                    std::ptr::null_mut(),
                )
            };
            // Only the two leading LARGE_INTEGER byte counters are read, so a
            // short (>= 16 byte) return still counts.
            if ok != 0 && got >= 16 {
                r = r.saturating_add(perf.BytesRead.max(0) as u64);
                w = w.saturating_add(perf.BytesWritten.max(0) as u64);
                any = true;
            }
        }
        any.then_some((r, w))
    }
}

/// Live-system smoke tests for the Windows probe arm: each probe must return a
/// plausible figure on any Windows machine the suite runs on (the whole point of
/// the arm is that these no longer paint "n/a").
#[cfg(all(test, windows))]
mod win_tests {
    #[test]
    fn mem_probes_report_plausible_figures() {
        assert!(super::mem_total().is_some_and(|t| t > 0));
        assert!(super::mem_used_frac().is_some_and(|f| (0.0..=1.0).contains(&f)));
    }

    #[test]
    fn cpu_ticks_are_cumulative() {
        let Some(a) = super::cpu_ticks() else {
            panic!("cpu_ticks returned None on Windows");
        };
        // A machine that has booted has accrued both busy and idle time.
        assert!(a[0].saturating_add(a[1]) > 0);
        assert!(a[2] > 0);
        let Some(b) = super::cpu_ticks() else {
            panic!("cpu_ticks returned None on the second read");
        };
        for i in 0..4 {
            assert!(b[i] >= a[i], "tick {i} regressed: {} -> {}", a[i], b[i]);
        }
    }

    #[test]
    fn proc_usage_sees_self() {
        let me = super::self_pid();
        let Some(s) = super::proc_usage(me) else {
            panic!("proc_usage(self) returned None");
        };
        assert_eq!(s.pid, me);
        assert!(s.footprint > 0, "a running process has a nonzero footprint");
    }

    #[test]
    fn proc_usage_rejects_bad_pids() {
        assert!(super::proc_usage(-1).is_none());
        assert!(super::proc_usage(0).is_none());
    }

    #[test]
    fn session_procs_contains_self() {
        let me = super::self_pid();
        let Some(procs) = super::session_procs(me) else {
            panic!("session_procs(self) returned None");
        };
        assert!(procs.iter().any(|p| p.pid == me));
    }

    #[test]
    fn proc_tree_cpu_rss_of_self_reports() {
        let Some((_cpu, rss)) = super::proc_tree_cpu_rss(super::self_pid()) else {
            panic!("proc_tree_cpu_rss(self) returned None");
        };
        assert!(rss > 0);
    }

    #[test]
    fn disk_for_reports_the_system_volume() {
        let root = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()) + "\\";
        let Some((free, total)) = super::disk_for(&root) else {
            panic!("disk_for({root}) returned None");
        };
        assert!(total > 0);
        assert!(free <= total);
    }

    #[test]
    fn net_ifaces_returns_a_table() {
        // May legitimately be EMPTY (every link down) but must not be None.
        assert!(super::net_ifaces().is_some());
    }

    #[test]
    fn optional_probes_do_not_error() {
        // None is legitimate here (perf counters disabled / all links down); the
        // probes just must not fail structurally.
        let _ = super::disk_io_bytes();
        let _ = super::net_primary_baud();
    }
}

// =============================================================================
// Background slow-probe sampler. The IOKit probes ([`gpu_util`], [`disk_io_bytes`])
// materialize a full CF property dictionary per matched registry service, and the
// session walk ([`session_procs`]) is one `proc_listchildpids` + `proc_pid_rusage`
// pair per live process — multi-millisecond work during a compile. Run on the
// winit event-loop thread they block keystroke/mouse delivery every HUD tick, so
// a single detached worker samples them at the HUD cadence into a cached snapshot
// the panels read lock-briefly. The worker parks on a condvar whenever every
// panel is disabled, preserving the all-off 0%-idle property.
// =============================================================================

use std::sync::{Condvar, Mutex, OnceLock, PoisonError};
use std::time::Instant;

/// One background pass over the slow probes, stamped when it was taken. `disk` is
/// the cumulative counter pair and `procs` carries cumulative per-pid counters;
/// consumers diff successive samples over the WORKER timestamps (`at`), never the
/// UI tick, so a lagged or repeated read can never distort a rate.
#[derive(Clone)]
pub(crate) struct SlowSample {
    /// Whole-machine GPU utilization `0..1` ([`gpu_util`]).
    pub gpu: Option<f64>,
    /// Cumulative whole-machine disk `(bytes_read, bytes_written)` ([`disk_io_bytes`]).
    pub disk: Option<(u64, u64)>,
    /// This terminal session's process subtree ([`session_procs`] of [`self_pid`]).
    pub procs: Option<Vec<ProcSample>>,
    /// Whole-machine GPU VRAM in use (bytes) ([`gpu_vram_used`]).
    pub vram_used: Option<u64>,
    /// GPU VRAM budget (bytes) ([`gpu_vram_budget`]; device-static, cached).
    pub vram_budget: Option<u64>,
    /// When the worker took this sample.
    pub at: Instant,
}

struct SlowProbes {
    /// Latest sample (`None` until the worker's first pass after arming).
    cache: Mutex<Option<SlowSample>>,
    /// Armed while any HUD panel is enabled; the worker parks on `gate` while off.
    active: Mutex<bool>,
    gate: Condvar,
}

static SLOW_PROBES: OnceLock<SlowProbes> = OnceLock::new();
static SLOW_WORKER: std::sync::Once = std::sync::Once::new();

/// Arm/park the background sampler. Keyed on the SAME predicate `about_to_wait`
/// uses to arm the HUD tick (any panel ENABLED — a 0-row scene-only panel must
/// keep the readout streaming); called on every event-loop wake, so the steady
/// state is one uncontended lock + compare. A `false` before the first arm is a
/// pure no-op: no statics initialized, no thread spawned.
pub(crate) fn set_slow_probes_active(on: bool) {
    if !on && SLOW_PROBES.get().is_none() {
        return;
    }
    let p = SLOW_PROBES.get_or_init(|| SlowProbes {
        cache: Mutex::new(None),
        active: Mutex::new(false),
        gate: Condvar::new(),
    });
    {
        let mut a = p.active.lock().unwrap_or_else(PoisonError::into_inner);
        if *a == on {
            return;
        }
        *a = on;
    }
    if on {
        // First arm spawns the detached worker. It touches ONLY these statics
        // (never App state), so app exit simply abandons a parked/sleeping thread.
        SLOW_WORKER.call_once(|| {
            let _ = std::thread::Builder::new()
                .name("aterm-slow-probes".into())
                .spawn(slow_probe_worker);
        });
        p.gate.notify_one();
    }
}

/// The latest background sample, or `None` before the first pass / before the
/// sampler was ever armed. Consumers must treat an old `at` as stale (the worker
/// parked or stalled) rather than rendering it as a live figure.
pub(crate) fn slow_probes_latest() -> Option<SlowSample> {
    SLOW_PROBES
        .get()?
        .cache
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn slow_probe_worker() {
    let p = SLOW_PROBES
        .get()
        .expect("worker spawns only after first arm");
    loop {
        // Park (zero wakeups) while every panel is disabled.
        {
            let mut on = p.active.lock().unwrap_or_else(PoisonError::into_inner);
            while !*on {
                on = p.gate.wait(on).unwrap_or_else(PoisonError::into_inner);
            }
        }
        let s = SlowSample {
            gpu: gpu_util(),
            disk: disk_io_bytes(),
            procs: session_procs(self_pid()),
            vram_used: gpu_vram_used(),
            vram_budget: gpu_vram_budget(),
            at: Instant::now(),
        };
        *p.cache.lock().unwrap_or_else(PoisonError::into_inner) = Some(s);
        // Pace at the HUD cadence. A disarm mid-sleep costs at most one trailing
        // sample before the loop parks above.
        std::thread::sleep(crate::HUD_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// MEM-ACCT-3(b): the real libdispatch memory-pressure registration path runs on
    /// this host without crashing (a bad symbol / ABI fault would fault here) — it
    /// creates the source, sets the handler, and resumes; the source then parks inert
    /// until the OS raises pressure. Exercises the FFI beyond link-time symbol resolution.
    #[cfg(target_os = "macos")]
    #[test]
    fn install_memory_pressure_source_registers_without_crashing() {
        super::install_memory_pressure_source(|_critical| {});
    }

    /// MEM-ACCT-3: the subtree fold sums the WHOLE descendant tree (the pre-fix walk
    /// missed grandchildren), skips an unsampleable node while still walking its
    /// children, returns `None` for an unsampleable root, and bounds a pathological fan.
    #[test]
    fn subtree_cpu_rss_sums_grandchildren_and_bounds() {
        // shell(100) -> make(200) -> cc(300), cc(301). 300/301 are GRANDCHILDREN.
        let children = |p: i32| -> Vec<i32> {
            match p {
                100 => vec![200],
                200 => vec![300, 301],
                _ => vec![],
            }
        };
        let usage = |p: i32| Some((u64::try_from(p).unwrap(), u64::try_from(p).unwrap() * 10));
        let (cpu, rss) = subtree_cpu_rss(100, 4096, children, usage).expect("root sampleable");
        assert_eq!(cpu, 100 + 200 + 300 + 301, "grandchildren must be summed");
        assert_eq!(rss, (100 + 200 + 300 + 301) * 10);

        // Unsampleable ROOT → None (a dead tab).
        assert!(subtree_cpu_rss(1, 4096, |_| vec![], |_| None).is_none());

        // A skipped (unsampleable) MIDDLE node still has its children walked.
        let mid = |p: i32| -> Vec<i32> {
            match p {
                1 => vec![2],
                2 => vec![3],
                _ => vec![],
            }
        };
        let skip_two = |p: i32| (p != 2).then_some((u64::try_from(p).unwrap(), 0));
        let (cpu2, _) = subtree_cpu_rss(1, 4096, mid, skip_two).expect("root sampleable");
        assert_eq!(
            cpu2,
            1 + 3,
            "an unsampleable node is skipped but its subtree continues"
        );

        // Budget bounds a pathological fan: no stall / no panic, ≤ budget descendants.
        let fan = |p: i32| -> Vec<i32> {
            if p == 1 {
                (2..10_000).collect()
            } else {
                vec![]
            }
        };
        let (cpu3, _) = subtree_cpu_rss(1, 8, fan, |_| Some((1, 1))).expect("root sampleable");
        assert_eq!(
            cpu3,
            1 + 8,
            "root + exactly `budget` descendants are folded, then it stops"
        );
    }

    /// The sampler's lifecycle contract: zero state before the first arm (a `false`
    /// is a pure no-op — the all-off 0%-idle property), a published sample shortly
    /// after arming, and a continuing stream while armed. (GPU/disk/procs may all be
    /// `None` on a locked-down runner; the SAMPLE itself is still published.)
    #[test]
    fn slow_probe_sampler_parks_until_armed_then_streams() {
        set_slow_probes_active(false);
        assert!(
            slow_probes_latest().is_none(),
            "never armed → no worker, no sample"
        );

        // Warm the device-static caches OUTSIDE the deadline: in the real app the
        // GPU renderer initializes Metal long before any HUD panel arms, but a cold
        // `MTLCreateSystemDefaultDevice` in a bare test process can exceed 5s
        // (IOSurface GPU-policy negotiation) — that's init cost, not sampler latency.
        let _ = gpu_vram_budget();
        let _ = gpu_util();

        set_slow_probes_active(true);
        let deadline = Instant::now() + Duration::from_secs(5);
        let first = loop {
            if let Some(s) = slow_probes_latest() {
                break s;
            }
            assert!(
                Instant::now() < deadline,
                "armed sampler publishes a sample"
            );
            std::thread::sleep(Duration::from_millis(10));
        };

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if slow_probes_latest().is_some_and(|s| s.at > first.at) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "sampler keeps streaming while armed"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        // Park again so this test leaves the process idle-quiet.
        set_slow_probes_active(false);
    }

    /// Tier-1 conformance: the SHIPPING [`net_health_classify`] tracks the `NetHealth`
    /// ty model (`aterm_spec::derive::net_health_model`), proven at Buggy=0 and caught at
    /// Buggy=1 by the real Trust `ty` in aterm-spec's `derived_ring_ty`. Enumerated over
    /// the WHOLE finite input domain, every output honours the honesty lattice (Online
    /// implies a reachable proof; Offline implies none); the Buggy-flip control (an
    /// unreachable link reported Online) is caught by the model.
    #[test]
    fn net_health_matches_model() {
        use crate::metrics_service::NetHealth;
        use std::collections::BTreeMap;
        let m = aterm_spec::derive::net_health_model();
        let code = |h: NetHealth| -> i64 {
            match h {
                NetHealth::Unknown => 0,
                NetHealth::Offline => 1,
                NetHealth::Slow => 2,
                NetHealth::Online => 3,
            }
        };
        let mut seen = [false; 4]; // Unknown / Offline / Slow / Online
        for r in [None, Some(false), Some(true)] {
            for conn_required in [false, true] {
                for transient in [false, true] {
                    for has_link in [false, true] {
                        for slow in [false, true] {
                            let h =
                                net_health_classify(r, conn_required, transient, has_link, slow);
                            // A positive reachability PROOF = a default route we can use
                            // right now (reachable, no connection first).
                            let reach = i64::from(has_link && r == Some(true) && !conn_required);
                            let st = BTreeMap::from([("reach", reach), ("health", code(h))]);
                            assert!(
                                m.check_invariant("HonestOnline", &st),
                                "Online without a reachable proof: r={r:?} conn={conn_required} \
                                 transient={transient} link={has_link} slow={slow}"
                            );
                            assert!(
                                m.check_invariant("HonestOffline", &st),
                                "Offline with a reachable proof: r={r:?} conn={conn_required} \
                                 transient={transient} link={has_link} slow={slow}"
                            );
                            seen[code(h) as usize] = true;
                        }
                    }
                }
            }
        }
        // NON-VACUOUS: every health state is reachable across the input domain.
        assert!(
            seen.iter().all(|&x| x),
            "classifier reaches all four states: {seen:?}"
        );

        // Negative control: an unreachable link reported Online (reach=0, health=Online)
        // is exactly the model's Buggy mutant — the model catches it.
        let buggy = BTreeMap::from([("reach", 0i64), ("health", 3i64)]);
        assert!(
            !m.check_invariant("HonestOnline", &buggy),
            "an unreachable-but-Online classification must be caught by the model"
        );
    }

    /// Live smoke (macOS): the whole-machine VRAM figures are either absent (locked-down
    /// / headless runner) or plausible — positive, and `used <= budget` when both are
    /// present. Never panics; asserts only on values actually returned.
    #[cfg(target_os = "macos")]
    #[test]
    fn gpu_vram_is_none_or_plausible() {
        let (used, budget) = (gpu_vram_used(), gpu_vram_budget());
        if let Some(u) = used {
            assert!(u > 0, "VRAM used, if reported, is positive");
        }
        if let Some(b) = budget {
            assert!(b > 0, "VRAM budget, if reported, is positive");
        }
        if let (Some(u), Some(b)) = (used, budget) {
            assert!(u <= b, "VRAM used {u} exceeds budget {b}");
        }
    }

    /// Live smoke (macOS): the SCNetworkReachability seam runs end-to-end and returns
    /// one of the four states without panicking or leaking the reachability ref (the
    /// runner's connectivity determines which state).
    #[cfg(target_os = "macos")]
    #[test]
    fn net_health_live_probe_does_not_panic() {
        let _ = net_health(&net_ifaces(), net_primary_baud());
    }
}
