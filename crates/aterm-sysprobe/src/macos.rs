// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The macOS arm (ruling 210).
//!
//! * **CPU** — `host_statistics(mach_host_self(), HOST_CPU_LOAD_INFO)`,
//!   all cores together (a P-/E-core split waits until the core-index
//!   mapping is verified), accumulated through [`CpuTicks`] so a wrapped
//!   32-bit counter never runs the busy share backwards.
//! * **Memory** — `kern.memorystatus_vm_pressure_level` and
//!   `kern.memorystatus_level`; `host_statistics64(HOST_VM_INFO64)` swapins
//!   plus swapouts in `vm.pagesize` pages; `hw.memsize` once.
//! * **Heat, power** — `NSProcessInfo` `thermalState` and
//!   `isLowPowerModeEnabled`, at sample time.
//! * **Sweep** — `proc_listallpids` (at most [`MAX_PIDS`]); per pid
//!   `proc_pid_rusage(RUSAGE_INFO_V4)` (user + system time in Mach ticks,
//!   scaled by `mach_timebase_info`; `ri_phys_footprint`) and
//!   `PROC_PIDT_SHORTBSDINFO` (parent, uid, `comm` — it answers for another
//!   user's pid too, where `proc_name` returns empty); `proc_pidpath` only for
//!   the rows [`rows_to_name`] picks (at most [`MAX_PATH_READS`] new ones a
//!   sweep), cached per `(pid, comm)` and given to every row each sweep.
//! * **Memory in use** — `HOST_VM_INFO64`'s internal less purgeable, plus
//!   wired, plus compressor pages ([`used_mib_of`]), as Activity Monitor
//!   counts it; `kern.memorystatus_level` is the share still FREE and feeds
//!   only the gauge.
//!
//! EPERM from `proc_pid_rusage` keeps the row with `cpu_ns: None`: an
//! unprivileged process cannot read another user's time (measured on the m1:
//! 224 of 506 pids, `WindowServer`, `mds_stores` and `kernel_task` among
//! them), and the engine books that time to the services by subtraction.

use std::collections::HashMap;
use std::ffi::{CStr, c_int, c_void};
use std::sync::OnceLock;

use aterm_objc::send::{send_bool, send_bool_sel, send_id, send_isize};
use aterm_objc::{autoreleasepool, class, sel};

use crate::{
    CpuTicks, Ident, Instant, MAX_PATH_READS, MAX_PIDS, PATH_CAP, ProcRow, Reading, Usage, c_text,
    fill_cached_paths, logical_cores, pressure_of, row_of, rows_to_name, thermal_of, ticks_to_ns,
    used_mib_of, used_of_level,
};

/// `PROC_PIDT_SHORTBSDINFO` (`sys/proc_info.h`). Declared here rather than in
/// the first-party `libc` because the reference `libc` 0.2.186 does not export
/// it, so the oracle has nothing to check it against; the layout is pinned by
/// the size assertion below (64 bytes, measured against the SDK) and by the
/// live test that reads this process's own row back.
const PROC_PIDT_SHORTBSDINFO: c_int = 13;

/// `struct proc_bsdshortinfo`.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(
    dead_code,
    reason = "the kernel's layout, field for field; the sweep reads ppid, uid and comm"
)]
struct ProcBsdShortInfo {
    pbsi_pid: u32,
    pbsi_ppid: u32,
    pbsi_pgid: u32,
    pbsi_status: u32,
    pbsi_comm: [u8; 16],
    pbsi_flags: u32,
    pbsi_uid: u32,
    pbsi_gid: u32,
    pbsi_ruid: u32,
    pbsi_rgid: u32,
    pbsi_svuid: u32,
    pbsi_svgid: u32,
    pbsi_rfu: u32,
}

const SHORT_INFO_SIZE: usize = 64;
const _: () = assert!(std::mem::size_of::<ProcBsdShortInfo>() == SHORT_INFO_SIZE);
const _: () = assert!(std::mem::size_of::<libc::rusage_info_v4>() == 296);
const _: () = assert!(PATH_CAP == libc::PROC_PIDPATHINFO_MAXSIZE as usize);

/// The host port, taken once per process: every `mach_host_self()` adds a
/// user reference to the send right, so a probe rebuilt on each switch-on
/// must not take another.
fn host_port() -> libc::mach_port_t {
    static PORT: OnceLock<libc::mach_port_t> = OnceLock::new();
    // SAFETY: no arguments; returns the task's send right to the host port.
    *PORT.get_or_init(|| unsafe { libc::mach_host_self() })
}

/// An integer sysctl (4 or 8 bytes, native-endian).
fn sysctl_int(name: &CStr) -> Option<i64> {
    let mut buf = [0u8; 8];
    let mut len: libc::size_t = buf.len();
    // SAFETY: `name` is NUL-terminated; `buf` is writable for `len` bytes and
    // the kernel writes at most `len`, reporting how many in `len`.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast::<c_void>(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    match len {
        4 => Some(i64::from(i32::from_ne_bytes([
            buf[0], buf[1], buf[2], buf[3],
        ]))),
        8 => Some(i64::from_ne_bytes(buf)),
        _ => None,
    }
}

/// The last OS error number.
fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// One process's usage.
fn rusage_of(pid: i32, numer: u32, denom: u32) -> Usage {
    // SAFETY: `rusage_info_v4` is plain integers; all-zero is a valid value.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a writable `rusage_info_v4` and the flavor names
    // exactly that layout (asserted 296 bytes above); libproc's prototype
    // types the buffer as `rusage_info_t *`, which is this pointer.
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            (&raw mut info).cast::<libc::rusage_info_t>(),
        )
    };
    if rc == 0 {
        let ticks = info.ri_user_time.saturating_add(info.ri_system_time);
        return Usage::Measured {
            cpu_ns: ticks_to_ns(ticks, numer, denom),
            footprint_kib: info.ri_phys_footprint / 1024,
        };
    }
    match errno() {
        libc::ESRCH => Usage::Gone,
        _ => Usage::Refused,
    }
}

/// One process's parent, uid and `comm`.
fn ident(pid: i32) -> Option<Ident> {
    // SAFETY: plain integers and bytes; all-zero is valid.
    let mut info: ProcBsdShortInfo = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is writable for `SHORT_INFO_SIZE` bytes, the size passed.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDT_SHORTBSDINFO,
            0,
            (&raw mut info).cast::<c_void>(),
            SHORT_INFO_SIZE as c_int,
        )
    };
    (usize::try_from(n).ok() == Some(SHORT_INFO_SIZE)).then(|| Ident {
        ppid: info.pbsi_ppid,
        uid: info.pbsi_uid,
        comm: c_text(&info.pbsi_comm),
    })
}

/// One process's executable path, read into a [`PATH_CAP`] buffer.
fn path(pid: i32) -> Option<String> {
    let mut buf = [0u8; PATH_CAP];
    // SAFETY: `buf` is writable for `PATH_CAP` bytes, the size passed.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast::<c_void>(), PATH_CAP as u32) };
    let n = usize::try_from(n).ok().filter(|n| *n > 0)?;
    Some(String::from_utf8_lossy(&buf[..n.min(PATH_CAP)]).into_owned())
}

/// `NSProcessInfo` `thermalState` and `isLowPowerModeEnabled`.
fn heat_and_power() -> (Option<crate::Thermal>, Option<bool>) {
    autoreleasepool(|_| {
        let cls = class(c"NSProcessInfo");
        if cls.is_null() {
            return (None, None);
        }
        // SAFETY: `+processInfo` is `-(NSProcessInfo *)`, an immortal
        // singleton, thread-safe; `-thermalState` is `-(NSInteger)`,
        // `-respondsToSelector:` `-(BOOL)(SEL)` and `-isLowPowerModeEnabled`
        // `-(BOOL)` (macOS 12+, hence the check) — the prototypes each helper
        // names. All are side-effect-free reads, safe off the main thread.
        unsafe {
            let info = send_id(cls.as_id(), sel!(processInfo));
            if info.is_null() {
                return (None, None);
            }
            let thermal = thermal_of(send_isize(info, sel!(thermalState)));
            let low = send_bool_sel(info, sel!(respondsToSelector:), sel!(isLowPowerModeEnabled))
                .then(|| send_bool(info, sel!(isLowPowerModeEnabled)));
            (thermal, low)
        }
    })
}

/// The macOS samplers.
pub(crate) struct Mac {
    host: libc::mach_port_t,
    numer: u32,
    denom: u32,
    cores: u16,
    mem_mib: u32,
    page_kib: u32,
    our_uid: u32,
    ticks: CpuTicks,
    /// The last sweep's cumulative CPU per pid (for [`rows_to_name`]).
    prev: HashMap<u32, u64>,
    /// `pid -> (comm, path)`: a path is read once per process image.
    paths: HashMap<u32, (String, Option<String>)>,
    pids: Vec<c_int>,
}

impl Mac {
    pub(crate) fn new() -> Self {
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: `tb` is a writable `mach_timebase_info`.
        let ok = unsafe { libc::mach_timebase_info(&raw mut tb) } == 0 && tb.denom != 0;
        let (numer, denom) = if ok { (tb.numer, tb.denom) } else { (1, 1) };
        let mem = sysctl_int(c"hw.memsize").and_then(|b| u64::try_from(b).ok());
        let page = sysctl_int(c"vm.pagesize").and_then(|b| u32::try_from(b).ok());
        Self {
            host: host_port(),
            numer,
            denom,
            cores: logical_cores(),
            mem_mib: mem.map_or(0, |b| u32::try_from(b >> 20).unwrap_or(u32::MAX)),
            page_kib: page.map_or(0, |b| b / 1024),
            // SAFETY: no arguments, cannot fail.
            our_uid: unsafe { libc::getuid() },
            ticks: CpuTicks::default(),
            prev: HashMap::new(),
            paths: HashMap::new(),
            pids: vec![0; MAX_PIDS],
        }
    }

    fn cpu(&mut self) -> Option<(u64, u64)> {
        let mut info = libc::host_cpu_load_info { cpu_ticks: [0; 4] };
        let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
        // SAFETY: `info` is a writable `host_cpu_load_info` of exactly
        // `HOST_CPU_LOAD_INFO_COUNT` words, the count passed in.
        let kr = unsafe {
            libc::host_statistics(
                self.host,
                libc::HOST_CPU_LOAD_INFO,
                (&raw mut info).cast::<libc::integer_t>(),
                &raw mut count,
            )
        };
        if kr != 0 {
            return None;
        }
        let t = info.cpu_ticks;
        let at = |i: c_int| t[usize::try_from(i).unwrap_or(0)];
        Some(self.ticks.advance([
            at(libc::CPU_STATE_USER),
            at(libc::CPU_STATE_SYSTEM),
            at(libc::CPU_STATE_IDLE),
            at(libc::CPU_STATE_NICE),
        ]))
    }

    /// Pages swapped in plus out since boot, and memory in use (MiB).
    fn vm_info(&self) -> (Option<u64>, Option<u32>) {
        // SAFETY: all-integer struct; all-zero is valid.
        let mut vm: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
        // Upstream's count IS this layout's size in words (38, rev1), so the
        // kernel writes nothing past it (see the struct's note in `libc`).
        let mut count = libc::HOST_VM_INFO64_COUNT;
        // SAFETY: `vm` is writable for `count` words, the count passed in.
        let kr = unsafe {
            libc::host_statistics64(
                self.host,
                libc::HOST_VM_INFO64,
                (&raw mut vm).cast::<libc::integer_t>(),
                &raw mut count,
            )
        };
        if kr != 0 {
            return (None, None);
        }
        let (i, o) = (vm.swapins, vm.swapouts);
        let used = used_mib_of(
            u64::from(vm.internal_page_count),
            u64::from(vm.purgeable_count),
            u64::from(vm.wire_count),
            u64::from(vm.compressor_page_count),
            u64::from(self.page_kib) * 1024,
        );
        (Some(i.saturating_add(o)), used)
    }

    pub(crate) fn reading(&mut self) -> Reading {
        let busy_ticks = self.cpu();
        let pressure = sysctl_int(c"kern.memorystatus_vm_pressure_level").and_then(pressure_of);
        let mem_used_pm = sysctl_int(c"kern.memorystatus_level").and_then(used_of_level);
        let (swap_pages, mem_used_mib) = self.vm_info();
        let (thermal, low_power) = heat_and_power();
        Reading {
            at: Instant::now(),
            cores: self.cores,
            mem_mib: self.mem_mib,
            busy_ticks,
            pressure,
            mem_used_pm,
            mem_used_mib,
            swap_pages,
            page_kib: self.page_kib,
            thermal,
            low_power,
            psi: None,
        }
    }

    pub(crate) fn scan(&mut self) -> Vec<ProcRow> {
        let bytes = c_int::try_from(self.pids.len() * std::mem::size_of::<c_int>()).unwrap_or(0);
        // SAFETY: `pids` is writable for `bytes` bytes, the size passed; the
        // call returns how many pids it wrote.
        let n = unsafe { libc::proc_listallpids(self.pids.as_mut_ptr().cast::<c_void>(), bytes) };
        let n = usize::try_from(n).unwrap_or(0).min(self.pids.len());
        let mut rows = Vec::with_capacity(n);
        for &pid in &self.pids[..n] {
            let Ok(upid) = u32::try_from(pid) else {
                continue;
            };
            if upid == 0 {
                continue; // kernel_task: refused, and its time is the services'.
            }
            let usage = rusage_of(pid, self.numer, self.denom);
            if usage == Usage::Gone {
                continue;
            }
            if let Some(row) = row_of(upid, ident(pid), usage, self.our_uid) {
                rows.push(row);
            }
        }
        // Every row takes its cached path (free), so an app's process keeps
        // its app from sweep to sweep; then the rows worth naming that have
        // none yet are read, at most MAX_PATH_READS a sweep.
        fill_cached_paths(&mut rows, &self.paths);
        let mut reads = 0;
        for i in rows_to_name(&rows, &self.prev) {
            if reads == MAX_PATH_READS {
                break;
            }
            let row = &mut rows[i];
            let cached = self
                .paths
                .get(&row.pid)
                .is_some_and(|(comm, _)| *comm == row.name);
            if cached {
                continue;
            }
            reads += 1;
            let p = i32::try_from(row.pid).ok().and_then(path);
            self.paths.insert(row.pid, (row.name.clone(), p.clone()));
            row.bundle = p;
        }
        self.prev = rows
            .iter()
            .filter_map(|r| r.cpu_ns.map(|ns| (r.pid, ns)))
            .collect();
        let live: std::collections::HashSet<u32> = rows.iter().map(|r| r.pid).collect();
        self.paths.retain(|pid, _| live.contains(pid));
        rows
    }
}

/// The encodings the heat-and-power sends assume, read from the runtime:
/// `(thermalState, isLowPowerModeEnabled)`.
#[cfg(test)]
pub(crate) fn sent_encodings() -> (Option<String>, Option<String>) {
    let cls = class(c"NSProcessInfo");
    // SAFETY: a live class and cached selectors; a pure runtime lookup.
    unsafe {
        (
            aterm_objc::method_types(cls, sel!(thermalState)),
            aterm_objc::method_types(cls, sel!(isLowPowerModeEnabled)),
        )
    }
}

/// This process's executable path as `proc_pidpath` reads it.
#[cfg(test)]
pub(crate) fn own_path() -> Option<String> {
    path(i32::try_from(std::process::id()).ok()?)
}

/// This process's own short-info row, for the layout test.
#[cfg(test)]
pub(crate) fn own_ident() -> Option<(u32, Ident)> {
    let pid = std::process::id();
    ident(i32::try_from(pid).ok()?).map(|i| (pid, i))
}
