// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The macOS arm.
//!
//! * **Another process** — `PROC_PIDVNODEPATHINFO` (its working directory),
//!   `proc_pidpath` (its executable), `PROC_PIDT_SHORTBSDINFO` (its process
//!   group) and `proc_listpids(PROC_PGRP_ONLY)` with
//!   `proc_pid_rusage(RUSAGE_INFO_V4)` per member (the group's
//!   `ri_phys_footprint`).
//! * **Memory** — `kern.memorystatus_vm_pressure_level`;
//!   `host_statistics64(HOST_VM_INFO64)` for memory available and in use
//!   ([`available_mib_of`](crate::available_mib_of),
//!   [`used_mib_of`](crate::used_mib_of)) and
//!   swapins plus swapouts in `vm.pagesize` pages; `hw.memsize` once.

use std::ffi::{CStr, c_int, c_void};
use std::sync::OnceLock;

use crate::{PATH_CAP, pressure_of};

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
