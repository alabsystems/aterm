// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The macOS arm.
//!
//! * **Another process** — `PROC_PIDVNODEPATHINFO` (its working directory),
//!   `proc_pidpath` (its executable), `PROC_PIDT_SHORTBSDINFO` (its process
//!   group) and `proc_listpids(PROC_PGRP_ONLY)` with
//!   `proc_pid_rusage(RUSAGE_INFO_V4)` per member (the group's
//!   `ri_phys_footprint`), and the same listing for whether a group's leader
//!   is alone in it.
//! * **Memory** — `kern.memorystatus_vm_pressure_level`;
//!   `host_statistics64(HOST_VM_INFO64)` for memory available and in use
//!   ([`available_mib_of`](crate::available_mib_of),
//!   [`used_mib_of`](crate::used_mib_of)) and
//!   swapins plus swapouts in `vm.pagesize` pages; `hw.memsize` once.
//! * **The machine now** ([`crate::machine`]) —
//!   `host_processor_info(PROCESSOR_CPU_LOAD_INFO)` per core;
//!   `hw.perflevelN.*` and the IORegistry's `IODeviceTree:/cpus` for the
//!   clusters; the same `HOST_VM_INFO64` read by page class with
//!   `vm.swapusage`; the first `IOAccelerator`'s `PerformanceStatistics`;
//!   `statfs` of a path.

use std::ffi::{CStr, c_int, c_void};
use std::sync::OnceLock;

use crate::iokit::{
    self, CFTypeRef, MAIN_PORT_DEFAULT, io_object_t, io_registry_entry_t, io_service_t,
};
use crate::machine::{DtCpu, Level, VmPages};
use crate::{CoreTicks, CpuLayout, GpuReading, MemoryDetail, PATH_CAP, VolumeReading, pressure_of};

/// `PROC_PIDT_SHORTBSDINFO` (`sys/proc_info.h`). Declared here rather than in
/// the first-party `libc` because the reference `libc` 0.2.186 does not export
/// it, so the oracle has nothing to check it against; the layout is pinned by
/// the size assertion below (64 bytes, measured against the SDK) and by the
/// live test that reads this process's own group back.
const PROC_PIDT_SHORTBSDINFO: c_int = 13;

/// `struct proc_bsdshortinfo`.
#[repr(C)]
#[derive(Clone, Copy)]
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

/// `PROC_PIDVNODEPATHINFO` (`sys/proc_info.h`): a process's current and
/// root directory vnodes with their paths. Declared here for the same reason
/// as [`PROC_PIDT_SHORTBSDINFO`]; the layout is pinned by the size
/// assertion below (2352 bytes: two `vnode_info_path`s of a 152-byte
/// `vnode_info` and a `MAXPATHLEN` path) and by the live test that reads
/// this process's own directory back.
const PROC_PIDVNODEPATHINFO: c_int = 9;

/// `struct vnode_info_path`: the vnode's `vnode_info` (read as opaque bytes
/// — only the path is used) and its `MAXPATHLEN` path.
#[repr(C)]
#[derive(Clone, Copy)]
struct VnodeInfoPath {
    vip_vi: [u8; 152],
    vip_path: [u8; 1024],
}

/// `struct proc_vnodepathinfo`: the current directory, then the root.
#[repr(C)]
#[derive(Clone, Copy)]
struct ProcVnodePathInfo {
    pvi_cdir: VnodeInfoPath,
    pvi_rdir: VnodeInfoPath,
}

const VNODE_PATH_INFO_SIZE: usize = 2352;
const _: () = assert!(std::mem::size_of::<ProcVnodePathInfo>() == VNODE_PATH_INFO_SIZE);

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
pub(crate) fn sysctl_int(name: &CStr) -> Option<i64> {
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

/// One process's physical footprint, KiB (`proc_pid_rusage(RUSAGE_INFO_V4)`'s
/// `ri_phys_footprint`); `None` when the process is gone or not ours.
fn footprint_kib(pid: i32) -> Option<u64> {
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
    (rc == 0).then_some(info.ri_phys_footprint / 1024)
}

/// One process's working directory (`PROC_PIDVNODEPATHINFO`'s current
/// directory vnode path); `None` when the call is refused or short.
pub(crate) fn process_cwd(pid: u32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    let pid = i32::try_from(pid).ok().filter(|p| *p > 0)?;
    // SAFETY: plain byte arrays; all-zero is valid.
    let mut info: ProcVnodePathInfo = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is writable for `VNODE_PATH_INFO_SIZE` bytes, the size
    // passed.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDVNODEPATHINFO,
            0,
            (&raw mut info).cast::<c_void>(),
            VNODE_PATH_INFO_SIZE as c_int,
        )
    };
    if usize::try_from(n).ok() != Some(VNODE_PATH_INFO_SIZE) {
        return None;
    }
    let bytes = &info.pvi_cdir.vip_path;
    let len = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(&bytes[..len]));
    path.is_absolute().then(|| path.to_path_buf())
}

/// One process's executable path as the kernel holds it (`proc_pidpath`),
/// bytes intact; `None` when the call is refused (the process is gone or not
/// ours) or answers no absolute path.
pub(crate) fn process_path(pid: u32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    let pid = i32::try_from(pid).ok().filter(|p| *p > 0)?;
    let mut buf = [0u8; PATH_CAP];
    let n = path_into(pid, &mut buf)?;
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(&buf[..n]));
    path.is_absolute().then(|| path.to_path_buf())
}

/// `proc_pidpath` into `buf`: the length it wrote, `None` when it wrote none.
fn path_into(pid: i32, buf: &mut [u8; PATH_CAP]) -> Option<usize> {
    // SAFETY: `buf` is writable for `PATH_CAP` bytes, the size passed.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast::<c_void>(), PATH_CAP as u32) };
    usize::try_from(n)
        .ok()
        .filter(|n| *n > 0)
        .map(|n| n.min(PATH_CAP))
}

/// `PROC_PGRP_ONLY` (`sys/proc_info.h`): `proc_listpids`' filter for the
/// members of one process group.
const PROC_PGRP_ONLY: u32 = 2;

unsafe extern "C" {
    /// libproc's `proc_listpids` (`libproc.h`): the pids `kind`/`typeinfo`
    /// select, written into `buffer`; returns the number of BYTES written
    /// (`proc_listallpids`, which the first-party `libc` declares, returns a
    /// count). Declared here because the first-party `libc` does not.
    fn proc_listpids(kind: u32, typeinfo: u32, buffer: *mut c_void, buffersize: c_int) -> c_int;
}

/// One process's short BSD info, whole or `None`.
fn short_info(pid: i32) -> Option<ProcBsdShortInfo> {
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
    (usize::try_from(n).ok() == Some(SHORT_INFO_SIZE) && info.pbsi_pid == pid.unsigned_abs())
        .then_some(info)
}

/// [`crate::group_memory`] on macOS: `pid`'s group from its short info, the
/// group's members from `proc_listpids(PROC_PGRP_ONLY)` (at most
/// [`crate::MAX_GROUP_PIDS`]), each one's footprint from `proc_pid_rusage`.
/// A member gone or refused meanwhile is skipped; a group none of whose
/// members could be measured is `None`.
pub(crate) fn group_memory(pid: u32) -> Option<crate::GroupMemory> {
    let pid = i32::try_from(pid).ok().filter(|p| *p > 0)?;
    let pgid = short_info(pid)?.pbsi_pgid;
    if pgid == 0 {
        return None;
    }
    let mut pids = [0 as c_int; crate::MAX_GROUP_PIDS];
    let bytes = c_int::try_from(std::mem::size_of_val(&pids)).ok()?;
    // SAFETY: `pids` is writable for `bytes` bytes, the size passed; the call
    // returns how many bytes it wrote.
    let n = unsafe { proc_listpids(PROC_PGRP_ONLY, pgid, pids.as_mut_ptr().cast(), bytes) };
    let n = usize::try_from(n).ok()? / std::mem::size_of::<c_int>();
    let (mut kib, mut measured) = (0u64, 0u32);
    for &member in pids.iter().take(n.min(crate::MAX_GROUP_PIDS)) {
        if member <= 0 {
            continue;
        }
        if let Some(footprint) = footprint_kib(member) {
            kib = kib.saturating_add(footprint);
            measured += 1;
        }
    }
    (measured > 0).then_some(crate::GroupMemory {
        pgid,
        footprint_mib: kib / 1024,
        measured,
    })
}

/// [`crate::lone_group_leader`] on macOS: `pid` leads its group (its short
/// info's `pbsi_pgid` is its own pid) and `proc_listpids(PROC_PGRP_ONLY)`
/// names no other member; then its `pbsi_comm`.
pub(crate) fn lone_group_leader(pid: u32) -> Option<String> {
    let pid = i32::try_from(pid).ok().filter(|p| *p > 0)?;
    let info = short_info(pid)?;
    if info.pbsi_pgid != pid.unsigned_abs() {
        return None;
    }
    let mut pids = [0 as c_int; crate::MAX_GROUP_PIDS];
    let bytes = c_int::try_from(std::mem::size_of_val(&pids)).ok()?;
    // SAFETY: `pids` is writable for `bytes` bytes, the size passed; the call
    // returns how many bytes it wrote.
    let n = unsafe {
        proc_listpids(
            PROC_PGRP_ONLY,
            info.pbsi_pgid,
            pids.as_mut_ptr().cast(),
            bytes,
        )
    };
    if !complete_lone_group(pid, n, &pids) {
        return None;
    }
    let len = info
        .pbsi_comm
        .iter()
        .position(|b| *b == 0)
        .unwrap_or(info.pbsi_comm.len());
    let comm = String::from_utf8_lossy(&info.pbsi_comm[..len]).into_owned();
    (!comm.is_empty()).then_some(comm)
}

/// Admit only a complete byte count with exactly the requested positive pid.
/// A full buffer may have omitted members; zero bytes prove no membership.
pub(crate) fn complete_lone_group(pid: c_int, bytes: c_int, pids: &[c_int]) -> bool {
    let Ok(bytes) = usize::try_from(bytes) else {
        return false;
    };
    let width = std::mem::size_of::<c_int>();
    if pid <= 0
        || bytes == 0
        || bytes >= std::mem::size_of_val(pids)
        || !bytes.is_multiple_of(width)
    {
        return false;
    }
    let mut members = pids[..bytes / width].iter().copied().filter(|p| *p != 0);
    members.next() == Some(pid) && members.next().is_none()
}

fn hw_memsize() -> Option<u64> {
    static MEMSIZE: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *MEMSIZE.get_or_init(|| sysctl_int(c"hw.memsize").and_then(|b| u64::try_from(b).ok()))
}

fn vm_pagesize() -> Option<u64> {
    static PAGESIZE: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *PAGESIZE.get_or_init(|| sysctl_int(c"vm.pagesize").and_then(|b| u64::try_from(b).ok()))
}

/// [`crate::physical_memory_mib`] on macOS: `hw.memsize`.
pub(crate) fn physical_memory_mib() -> Option<u64> {
    hw_memsize().map(|b| b >> 20).filter(|mib| *mib > 0)
}

/// [`crate::memory_pressure`] on macOS.
pub(crate) fn memory_pressure() -> Option<crate::MemoryLevel> {
    sysctl_int(c"kern.memorystatus_vm_pressure_level").and_then(pressure_of)
}

/// One `host_statistics64(HOST_VM_INFO64)` of the host; `None` where the
/// kernel refused it.
fn vm_statistics() -> Option<libc::vm_statistics64> {
    // SAFETY: all-integer struct; all-zero is valid.
    let mut vm: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    // Upstream's count IS this layout's size in words (38, rev1), so the
    // kernel writes nothing past it (see the struct's note in `libc`).
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: `vm` is writable for `count` words, the count passed in.
    let kr = unsafe {
        libc::host_statistics64(
            host_port(),
            libc::HOST_VM_INFO64,
            (&raw mut vm).cast::<libc::integer_t>(),
            &raw mut count,
        )
    };
    (kr == 0).then_some(vm)
}

/// [`crate::machine_memory`] on macOS: one `HOST_VM_INFO64` read, sized by
/// `vm.pagesize`.
pub(crate) fn machine_memory() -> Option<crate::MachineMemory> {
    let page_bytes = vm_pagesize()?;
    let vm = vm_statistics()?;
    let available_mib = crate::available_mib_of(
        u64::from(vm.free_count),
        u64::from(vm.inactive_count),
        u64::from(vm.purgeable_count),
        page_bytes,
        physical_memory_mib(),
    )?;
    let used_mib = crate::used_mib_of(
        u64::from(vm.internal_page_count),
        u64::from(vm.purgeable_count),
        u64::from(vm.wire_count),
        u64::from(vm.compressor_page_count),
        page_bytes,
    )?;
    Some(crate::MachineMemory {
        available_mib,
        used_mib: u64::from(used_mib),
    })
}

/// [`crate::swapped_kib`] on macOS: `HOST_VM_INFO64`'s swapins plus
/// swapouts, in `vm.pagesize` pages, as KiB.
pub(crate) fn swapped_kib() -> Option<u64> {
    let page_kib = vm_pagesize().map(|b| b / 1024).filter(|k| *k > 0)?;
    let vm = vm_statistics()?;
    Some(
        vm.swapins
            .saturating_add(vm.swapouts)
            .saturating_mul(page_kib),
    )
}

// ---------------------------------------------------------------------------
// The machine now: per-core ticks, the cluster layout, memory by page class,
// the GPU and a volume.
// ---------------------------------------------------------------------------

unsafe extern "C" {
    /// libSystem's cached send right to this task (`mach_task_self()` is a
    /// C macro reading it). Declared here because the reference `libc`
    /// exposes it only as a `static mut` the oracle's generator does not
    /// emit; the same declaration is in `aterm-containment`'s actuator and
    /// `aterm-gui`'s keymap.
    static mach_task_self_: libc::mach_port_t;
}

/// The cores in `words`, `host_processor_info(PROCESSOR_CPU_LOAD_INFO)`'s
/// array: `count` runs of `CPU_STATE_MAX` 32-bit counters, indexed by
/// `CPU_STATE_USER`, `CPU_STATE_SYSTEM`, `CPU_STATE_IDLE`, `CPU_STATE_NICE`.
/// `None` when the array is shorter than `count` runs. PURE.
pub(crate) fn cores_of(words: &[libc::integer_t], count: usize) -> Option<Vec<CoreTicks>> {
    const RUN: usize = libc::CPU_STATE_MAX as usize;
    let words = words.get(..count.checked_mul(RUN)?)?;
    let tick = |w: libc::integer_t| u64::from(w.cast_unsigned());
    let (runs, _) = words.as_chunks::<RUN>();
    Some(
        runs.iter()
            .map(|run| CoreTicks {
                user: tick(run[libc::CPU_STATE_USER as usize]),
                system: tick(run[libc::CPU_STATE_SYSTEM as usize]),
                idle: tick(run[libc::CPU_STATE_IDLE as usize]),
                nice: tick(run[libc::CPU_STATE_NICE as usize]),
            })
            .collect(),
    )
}

/// [`crate::Probe::core_ticks`] on macOS: one
/// `host_processor_info(PROCESSOR_CPU_LOAD_INFO)`, then the `vm_deallocate`
/// of the array the kernel mapped into this task for the answer.
pub(crate) fn core_ticks() -> Option<Vec<CoreTicks>> {
    let mut count: libc::natural_t = 0;
    let mut info: libc::processor_info_array_t = std::ptr::null_mut();
    let mut words: libc::mach_msg_type_number_t = 0;
    // SAFETY: the three out-parameters are writable; on success the kernel
    // maps `words` 32-bit words at `info` into this task, which this
    // function owns and deallocates below.
    let kr = unsafe {
        libc::host_processor_info(
            host_port(),
            libc::PROCESSOR_CPU_LOAD_INFO,
            &raw mut count,
            &raw mut info,
            &raw mut words,
        )
    };
    if kr != 0 || info.is_null() {
        return None;
    }
    let len = usize::try_from(words).unwrap_or(0);
    // SAFETY: on success `info` points at `words` initialised 32-bit words
    // that stay mapped until the `vm_deallocate` below.
    let cores = cores_of(
        unsafe { std::slice::from_raw_parts(info, len) },
        count as usize,
    );
    // SAFETY: `info` and its size are exactly what the kernel mapped for this
    // call (MIG out-of-line memory), released once; `mach_task_self_` is
    // this task's own port.
    unsafe {
        libc::vm_deallocate(
            mach_task_self_,
            info.addr(),
            len * std::mem::size_of::<libc::integer_t>(),
        );
    }
    cores
}

/// A string sysctl (at most 63 bytes), NUL-trimmed; `None` where unreadable
/// or empty.
fn sysctl_text(name: &CStr) -> Option<String> {
    let mut buf = [0u8; 64];
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
    let bytes = &buf[..len.min(buf.len())];
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    (!text.is_empty()).then_some(text)
}

/// `hw.perflevelN.{name,logicalcpu,l2cachesize}` for each of
/// `hw.nperflevels` levels, fastest first; empty when any level's name or
/// core count is unreadable (an Intel Mac has no levels).
fn perf_levels() -> Vec<Level> {
    let n = sysctl_int(c"hw.nperflevels").unwrap_or(0).clamp(0, 16);
    let mut levels = Vec::new();
    for i in 0..n {
        let int = |field: &str| {
            std::ffi::CString::new(format!("hw.perflevel{i}.{field}"))
                .ok()
                .and_then(|c| sysctl_int(&c))
        };
        let name = std::ffi::CString::new(format!("hw.perflevel{i}.name"))
            .ok()
            .and_then(|c| sysctl_text(&c));
        let logical = int("logicalcpu").and_then(|c| u16::try_from(c).ok());
        let (Some(name), Some(logical)) = (name, logical) else {
            return Vec::new();
        };
        let l2 = int("l2cachesize").and_then(|b| u64::try_from(b).ok());
        levels.push(Level { name, logical, l2 });
    }
    levels
}

/// A +1 CoreFoundation object, released on drop.
struct Cf(CFTypeRef);

impl Cf {
    /// Adopt a +1 reference; `None` for NULL.
    fn adopt(object: CFTypeRef) -> Option<Self> {
        // Lazy, never `then_some`: its argument is built before the test, so
        // a NULL would become a `Cf(NULL)` dropped on the spot, and
        // `CFRelease(NULL)` traps (an absent registry property is NULL).
        (!object.is_null()).then(|| Self(object))
    }

    /// A new CFString of `text`.
    fn string(text: &CStr) -> Option<Self> {
        // SAFETY: `text` is NUL-terminated UTF-8; the result is +1 or NULL.
        Self::adopt(
            unsafe {
                iokit::CFStringCreateWithCString(
                    std::ptr::null(),
                    text.as_ptr(),
                    iokit::kCFStringEncodingUTF8,
                )
            }
            .cast(),
        )
    }

    /// `entry`'s registry property `key`.
    fn property(entry: io_registry_entry_t, key: CFTypeRef) -> Option<Self> {
        // SAFETY: `entry` is a live registry entry and `key` a live CFString;
        // the result is +1 (the Create rule) or NULL.
        Self::adopt(unsafe {
            iokit::IORegistryEntryCreateCFProperty(entry, key.cast(), std::ptr::null(), 0)
        })
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        // SAFETY: the +1 this owner holds, released once.
        unsafe { iokit::CFRelease(self.0) };
    }
}

/// A borrowed CF value read as an integer: a CFNumber's value, or a
/// CFData's 1–8 little-endian bytes (the device tree's encoding).
fn cf_u64(value: CFTypeRef) -> Option<u64> {
    if value.is_null() {
        return None;
    }
    // SAFETY: `value` is a live CF object (borrowed for this call); each type
    // is checked before the reader for that type touches it.
    unsafe {
        let ty = iokit::CFGetTypeID(value);
        if ty == iokit::CFNumberGetTypeID() {
            let mut n: i64 = 0;
            let ok = iokit::CFNumberGetValue(
                value.cast(),
                iokit::kCFNumberSInt64Type,
                (&raw mut n).cast::<c_void>(),
            ) != 0;
            return ok.then(|| u64::try_from(n).ok()).flatten();
        }
        if ty == iokit::CFDataGetTypeID() {
            return le_u64(cf_bytes(value)?);
        }
    }
    None
}

/// A borrowed CFData's bytes; `None` for any other type.
///
/// # Safety
/// `value` is a live CF object that outlives the returned slice.
unsafe fn cf_bytes<'a>(value: CFTypeRef) -> Option<&'a [u8]> {
    // SAFETY: the caller's; the type is checked before the data is read, and
    // CFData's byte pointer is valid for its length while it lives.
    unsafe {
        if iokit::CFGetTypeID(value) != iokit::CFDataGetTypeID() {
            return None;
        }
        let len = usize::try_from(iokit::CFDataGetLength(value.cast())).ok()?;
        let ptr = iokit::CFDataGetBytePtr(value.cast());
        if ptr.is_null() {
            return (len == 0).then_some(&[]);
        }
        Some(std::slice::from_raw_parts(ptr, len))
    }
}

/// 1–8 little-endian bytes as an integer. PURE.
pub(crate) fn le_u64(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || bytes.len() > 8 {
        return None;
    }
    let mut word = [0u8; 8];
    word[..bytes.len()].copy_from_slice(bytes);
    Some(u64::from_le_bytes(word))
}

/// A device-tree `cluster-type`'s letter: the first byte when it is an
/// ASCII capital (`b"E\0"` is `E`). PURE.
pub(crate) fn cluster_kind(bytes: &[u8]) -> Option<u8> {
    bytes.first().copied().filter(u8::is_ascii_uppercase)
}

/// A borrowed CF value as text: a CFString, or a CFData's bytes up to the
/// first NUL. At most 127 bytes.
fn cf_text(value: CFTypeRef) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let mut buf = [0 as libc::c_char; 128];
    // SAFETY: `value` is live (borrowed); its type is checked before each
    // reader; `buf` is writable for the length passed.
    let bytes: Vec<u8> = unsafe {
        if iokit::CFGetTypeID(value) == iokit::CFStringGetTypeID() {
            if iokit::CFStringGetCString(
                value.cast(),
                buf.as_mut_ptr(),
                buf.len() as iokit::CFIndex,
                iokit::kCFStringEncodingUTF8,
            ) == 0
            {
                return None;
            }
            CStr::from_ptr(buf.as_ptr()).to_bytes().to_vec()
        } else {
            let data = cf_bytes(value)?;
            let end = data.iter().position(|b| *b == 0).unwrap_or(data.len());
            data[..end.min(127)].to_vec()
        }
    };
    let text = String::from_utf8_lossy(&bytes).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// An IOKit object, released on drop.
struct Io(io_object_t);

impl Drop for Io {
    fn drop(&mut self) {
        // SAFETY: the reference this owner holds, released once.
        unsafe { iokit::IOObjectRelease(self.0) };
    }
}

/// Every node under `IODeviceTree:/cpus` with its `logical-cpu-id`,
/// `cluster-type` and `l2-cache-size` (at most 1024 nodes); a node with no
/// readable id is left out. Empty where the device tree cannot be read.
fn dt_cpus() -> Vec<DtCpu> {
    let (Some(id_key), Some(kind_key), Some(l2_key)) = (
        Cf::string(c"logical-cpu-id"),
        Cf::string(c"cluster-type"),
        Cf::string(c"l2-cache-size"),
    ) else {
        return Vec::new();
    };
    // SAFETY: a NUL-terminated registry path; the entry is +1 or 0.
    let cpus = unsafe {
        iokit::IORegistryEntryFromPath(MAIN_PORT_DEFAULT, c"IODeviceTree:/cpus".as_ptr())
    };
    if cpus == 0 {
        return Vec::new();
    }
    let cpus = Io(cpus);
    let mut iter: io_object_t = 0;
    // SAFETY: `cpus` is a live entry, the plane name NUL-terminated, `iter`
    // writable; on success the iterator is +1.
    let kr = unsafe {
        iokit::IORegistryEntryGetChildIterator(cpus.0, c"IODeviceTree".as_ptr(), &raw mut iter)
    };
    if kr != 0 || iter == 0 {
        return Vec::new();
    }
    let iter = Io(iter);
    let mut out = Vec::new();
    while out.len() < 1024 {
        // SAFETY: `iter` is a live iterator; the child is +1 or 0 at the end.
        let child = unsafe { iokit::IOIteratorNext(iter.0) };
        if child == 0 {
            break;
        }
        let child = Io(child);
        let Some(id) = Cf::property(child.0, id_key.0)
            .and_then(|v| cf_u64(v.0))
            .and_then(|id| u16::try_from(id).ok())
        else {
            continue;
        };
        let kind = Cf::property(child.0, kind_key.0).and_then(|v| {
            // SAFETY: `v` lives to the end of this closure.
            unsafe { cf_bytes(v.0) }.and_then(cluster_kind)
        });
        let l2 = Cf::property(child.0, l2_key.0).and_then(|v| cf_u64(v.0));
        out.push(DtCpu { id, kind, l2 });
    }
    out
}

/// [`crate::cpu_layout`] on macOS: the perflevels, the device tree's nodes
/// (read only where there are levels to tell apart), and `hw.logicalcpu`.
pub(crate) fn cpu_layout() -> CpuLayout {
    let logical = sysctl_int(c"hw.logicalcpu")
        .and_then(|n| u16::try_from(n).ok())
        .unwrap_or(0);
    let levels = perf_levels();
    let cpus = if levels.len() > 1 {
        dt_cpus()
    } else {
        Vec::new()
    };
    crate::machine::layout_of(&levels, &cpus, logical)
}

/// The levels and device-tree nodes [`cpu_layout`] reads, for the live test
/// that checks the device tree's map holds on this host.
#[cfg(test)]
pub(crate) fn layout_inputs() -> (Vec<Level>, Vec<DtCpu>, u16) {
    let logical = sysctl_int(c"hw.logicalcpu")
        .and_then(|n| u16::try_from(n).ok())
        .unwrap_or(0);
    (perf_levels(), dt_cpus(), logical)
}

/// `vm.swapusage`: swap `(used, total)` bytes; `None` where unreadable.
fn swap_usage() -> Option<(u64, u64)> {
    // SAFETY: integers only; all-zero is a value.
    let mut xsw: libc::xsw_usage = unsafe { std::mem::zeroed() };
    let mut len: libc::size_t = std::mem::size_of::<libc::xsw_usage>();
    // SAFETY: `xsw` is writable for `len` bytes and the kernel writes at
    // most that, reporting how many in `len`.
    let rc = unsafe {
        libc::sysctlbyname(
            c"vm.swapusage".as_ptr(),
            (&raw mut xsw).cast::<c_void>(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && len == std::mem::size_of::<libc::xsw_usage>())
        .then_some((xsw.xsu_used, xsw.xsu_total))
}

/// [`crate::memory_detail`] on macOS: one `HOST_VM_INFO64` read by page
/// class, `vm.swapusage` and the pressure level.
pub(crate) fn memory_detail() -> Option<MemoryDetail> {
    let page_bytes = vm_pagesize()?;
    let total = hw_memsize()?;
    let vm = vm_statistics()?;
    let pages = VmPages {
        free: u64::from(vm.free_count),
        speculative: u64::from(vm.speculative_count),
        active: u64::from(vm.active_count),
        inactive: u64::from(vm.inactive_count),
        wired: u64::from(vm.wire_count),
        compressor: u64::from(vm.compressor_page_count),
        stored: vm.total_uncompressed_pages_in_compressor,
        purgeable: u64::from(vm.purgeable_count),
        external: u64::from(vm.external_page_count),
        internal: u64::from(vm.internal_page_count),
    };
    Some(crate::machine::memory_detail_of(
        &pages,
        page_bytes,
        total,
        swap_usage(),
        memory_pressure(),
    ))
}

/// The keys of `PerformanceStatistics`, made once.
struct GpuKeys {
    stats: CFTypeRef,
    device: CFTypeRef,
    renderer: CFTypeRef,
    tiler: CFTypeRef,
    in_use: CFTypeRef,
}

/// The first `IOAccelerator`, matched once per process, with what never
/// changes about it.
struct Accel {
    service: io_service_t,
    keys: GpuKeys,
    name: Option<String>,
    cores: Option<u16>,
}

// SAFETY: the service is a Mach port name and the keys immutable CFStrings,
// all kept for the process (never released) and usable from any thread;
// Rust dereferences none of them.
unsafe impl Send for Accel {}
// SAFETY: as for `Send`.
unsafe impl Sync for Accel {}

/// A process-lifetime CFString: the +1 is deliberately kept.
fn kept_string(text: &CStr) -> Option<CFTypeRef> {
    Cf::string(text).map(|s| {
        let raw = s.0;
        std::mem::forget(s);
        raw
    })
}

fn accel() -> Option<&'static Accel> {
    static ACCEL: OnceLock<Option<Accel>> = OnceLock::new();
    ACCEL
        .get_or_init(|| {
            // SAFETY: a NUL-terminated class name; the dictionary is +1 and
            // consumed by `IOServiceGetMatchingService`, whose service is
            // kept for the process.
            let service = unsafe {
                let matching = iokit::IOServiceMatching(c"IOAccelerator".as_ptr());
                if matching.is_null() {
                    return None;
                }
                iokit::IOServiceGetMatchingService(MAIN_PORT_DEFAULT, matching.cast_const())
            };
            if service == 0 {
                return None;
            }
            let keys = GpuKeys {
                stats: kept_string(c"PerformanceStatistics")?,
                device: kept_string(c"Device Utilization %")?,
                renderer: kept_string(c"Renderer Utilization %")?,
                tiler: kept_string(c"Tiler Utilization %")?,
                in_use: kept_string(c"In use system memory")?,
            };
            let name = Cf::string(c"model")
                .and_then(|k| Cf::property(service, k.0))
                .and_then(|v| cf_text(v.0))
                .or_else(|| {
                    let mut class = [0 as libc::c_char; 128];
                    // SAFETY: `service` is live and `class` is an `io_name_t`
                    // (128 bytes) the call fills NUL-terminated.
                    let kr = unsafe { iokit::IOObjectGetClass(service, class.as_mut_ptr()) };
                    // SAFETY: on success `class` holds a NUL-terminated name.
                    let text = (kr == 0)
                        .then(|| unsafe { CStr::from_ptr(class.as_ptr()) }.to_string_lossy());
                    text.map(|t| t.into_owned()).filter(|t| !t.is_empty())
                });
            let cores = Cf::string(c"gpu-core-count")
                .and_then(|k| Cf::property(service, k.0))
                .and_then(|v| cf_u64(v.0))
                .and_then(|n| u16::try_from(n).ok());
            Some(Accel {
                service,
                keys,
                name,
                cores,
            })
        })
        .as_ref()
}

/// [`crate::gpu`] on macOS: the cached accelerator's `PerformanceStatistics`,
/// copied once per call.
pub(crate) fn gpu() -> Option<GpuReading> {
    let a = accel()?;
    let mut reading = GpuReading {
        cores: a.cores,
        name: a.name.clone(),
        ..GpuReading::default()
    };
    let Some(stats) = Cf::property(a.service, a.keys.stats) else {
        return Some(reading);
    };
    // SAFETY: `stats` is live; its type is checked before it is read as a
    // dictionary, and each value is borrowed (the Get rule) while it lives.
    let value = |key: CFTypeRef| unsafe {
        cf_u64(iokit::CFDictionaryGetValue(stats.0.cast(), key)).and_then(|v| i64::try_from(v).ok())
    };
    // SAFETY: `stats` is a live CF object.
    if unsafe { iokit::CFGetTypeID(stats.0) == iokit::CFDictionaryGetTypeID() } {
        reading.device_pct = value(a.keys.device).and_then(crate::machine::percent_of);
        reading.renderer_pct = value(a.keys.renderer).and_then(crate::machine::percent_of);
        reading.tiler_pct = value(a.keys.tiler).and_then(crate::machine::percent_of);
        reading.in_use_bytes = value(a.keys.in_use).and_then(|v| u64::try_from(v).ok());
    }
    Some(reading)
}

/// A NUL-terminated `c_char` array as text.
fn c_text(chars: &[libc::c_char]) -> String {
    let end = chars.iter().position(|c| *c == 0).unwrap_or(chars.len());
    let bytes: Vec<u8> = chars[..end].iter().map(|c| c.cast_unsigned()).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// [`crate::volume`] on macOS: one `statfs` of `path`.
pub(crate) fn volume(path: &std::path::Path) -> Option<VolumeReading> {
    let mut buf = [0u8; PATH_CAP];
    let c = crate::machine::c_path(path, &mut buf)?;
    // SAFETY: `statfs` is integers and arrays of them: all-zero is a value.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is NUL-terminated and `st` is a writable `statfs`.
    if unsafe { libc::statfs(c.as_ptr(), &raw mut st) } != 0 {
        return None;
    }
    Some(crate::machine::volume_of(
        st.f_blocks,
        st.f_bfree,
        st.f_bavail,
        u64::from(st.f_bsize),
        std::path::PathBuf::from(c_text(&st.f_mntonname)),
        c_text(&st.f_fstypename),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registry property an entry lacks comes back NULL (a paravirtual
    /// GPU's `model`): it adopts as `None` and nothing releases it — a
    /// `CFRelease(NULL)` traps the whole process, as it did the vitals thread.
    #[test]
    fn an_absent_property_adopts_as_none_without_a_release() {
        assert!(Cf::adopt(std::ptr::null()).is_none());
        // SAFETY: a NUL-terminated registry path (the platform expert
        // device, on every Mac and VM); the entry is +1 or 0.
        let device =
            unsafe { iokit::IORegistryEntryFromPath(MAIN_PORT_DEFAULT, c"IOService:/".as_ptr()) };
        assert_ne!(device, 0, "the platform expert device");
        let device = Io(device);
        let present = Cf::string(c"model").expect("a CFString");
        let model = Cf::property(device.0, present.0).and_then(|v| cf_text(v.0));
        assert!(model.is_some(), "a key the entry has reads back");
        let absent = Cf::string(c"aterm-no-such-key").expect("a CFString");
        assert!(Cf::property(device.0, absent.0).is_none());
    }
}
