// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows arm's DECODERS (design `docs/DESIGN-system-awareness-2026-10-16.md`
//! §5.4, §9.6, §16): pure functions over the bytes and numbers the Win32 and
//! NT calls answer, compiled and tested on every platform. The private `win`
//! module (64-bit Windows only) makes the calls and hands their buffers here.
//!
//! | call | decoder | into |
//! |---|---|---|
//! | `NtQuerySystemInformationEx(SystemProcessorPerformanceInformation)` | [`processor_times`] | [`crate::Probe::core_ticks`] |
//! | `GetSystemCpuSetInformation` | [`cpu_sets`], [`layout_from_cpu_sets`] | [`crate::cpu_layout`], [`crate::withdrawn_cores`] |
//! | `GlobalMemoryStatusEx`, `K32GetPerformanceInfo` | [`memory_detail_of`] | [`crate::memory_detail`] |
//! | `K32GetPerformanceInfo` | [`PerformanceInformation64::process_count`] | [`crate::pid_counts`] |
//! | `NtQuerySystemInformation(SystemPageFileInformation)` | [`page_files`] | swap |
//! | `NtQuerySystemInformation(SystemProcessInformation)` | [`processes`] | [`crate::processes`] |
//! | a path's spelling | [`drive_root`], [`drive_is_local`] | [`crate::volume`] |
//!
//! Every layout is the 64-bit one (`HANDLE`, `SIZE_T` and pointers 8
//! bytes), which both Windows cells (x86_64 and aarch64) use; each struct
//! below is transcribed from the Windows SDK (`winternl.h`, `winnt.h`,
//! `psapi.h`, `sysinfoapi.h`) with its size and every offset a decoder reads
//! asserted at compile time. Windows is little-endian on both, so fields are
//! read as little-endian bytes, never through a cast pointer.

use std::collections::HashMap;
use std::mem::{offset_of, size_of};

use crate::jobs::RunState;
use crate::machine::{Commit, layout_by_rank};
use crate::procs::{MAX_PROCESSES, ProcessReading, Sweep};
use crate::{CoreTicks, CpuLayout, MemoryDetail, MemoryLevel};

/// `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION` (`winternl.h`: `IdleTime`,
/// `KernelTime`, `UserTime`, then `Reserved1[2]` — DPC and interrupt time —
/// and `Reserved2`, the interrupt count), one per processor; times in 100 ns
/// units, `KernelTime` including `IdleTime`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessorPerformance {
    /// Idle time.
    pub idle_time: i64,
    /// Kernel time, idle included.
    pub kernel_time: i64,
    /// User time.
    pub user_time: i64,
    /// Time in deferred procedure calls (inside `kernel_time`).
    pub dpc_time: i64,
    /// Time in interrupts (inside `kernel_time`).
    pub interrupt_time: i64,
    /// Interrupts taken.
    pub interrupt_count: u32,
}
const _: () = assert!(size_of::<ProcessorPerformance>() == 48);

/// `UNICODE_STRING`, 64-bit: `Length` and `MaximumLength` in bytes, then the
/// buffer's address.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnicodeString64 {
    /// Bytes of text.
    pub length: u16,
    /// Bytes the buffer holds.
    pub maximum_length: u16,
    /// The buffer's address.
    pub buffer: u64,
}
const _: () = assert!(size_of::<UnicodeString64>() == 16);
const _: () = assert!(offset_of!(UnicodeString64, buffer) == 8);

/// `SYSTEM_PROCESS_INFORMATION`, 64-bit (`winternl.h`, whose `Reserved1[48]`
/// holds the private working set, hard faults, the thread high-water mark,
/// cycle time, create time, user time and kernel time, in that order).
/// `NumberOfThreads` `SYSTEM_THREAD_INFORMATION` records follow each entry;
/// `next_entry_offset` skips them.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessInformation64 {
    /// Bytes to the next entry; 0 on the last.
    pub next_entry_offset: u32,
    /// Threads.
    pub number_of_threads: u32,
    /// Private working set, bytes.
    pub working_set_private_size: i64,
    /// Hard page faults.
    pub hard_fault_count: u32,
    /// The most threads it has had.
    pub number_of_threads_high_watermark: u32,
    /// CPU cycles.
    pub cycle_time: u64,
    /// When it started (100 ns since 1601).
    pub create_time: i64,
    /// User time, 100 ns.
    pub user_time: i64,
    /// Kernel time, 100 ns.
    pub kernel_time: i64,
    /// Its image name.
    pub image_name: UnicodeString64,
    /// Base priority.
    pub base_priority: i32,
    /// Its pid.
    pub unique_process_id: u64,
    /// The pid of the process that created it.
    pub inherited_from_unique_process_id: u64,
    /// Open handles.
    pub handle_count: u32,
    /// Its session (0: services).
    pub session_id: u32,
    /// Reserved.
    pub unique_process_key: u64,
    /// Peak virtual size, bytes.
    pub peak_virtual_size: u64,
    /// Virtual size, bytes.
    pub virtual_size: u64,
    /// Page faults.
    pub page_fault_count: u32,
    /// Peak working set, bytes.
    pub peak_working_set_size: u64,
    /// Working set, bytes.
    pub working_set_size: u64,
    /// Peak paged-pool quota, bytes.
    pub quota_peak_paged_pool_usage: u64,
    /// Paged-pool quota, bytes.
    pub quota_paged_pool_usage: u64,
    /// Peak nonpaged-pool quota, bytes.
    pub quota_peak_non_paged_pool_usage: u64,
    /// Nonpaged-pool quota, bytes.
    pub quota_non_paged_pool_usage: u64,
    /// Page-file-backed commit, bytes.
    pub pagefile_usage: u64,
    /// Peak page-file-backed commit, bytes.
    pub peak_pagefile_usage: u64,
    /// Private commit, bytes (named in pages, counted in bytes).
    pub private_page_count: u64,
    /// I/O counters: read, write and other operations, then bytes.
    pub io: [i64; 6],
}
const _: () = assert!(size_of::<ProcessInformation64>() == 256);
const _: () = assert!(offset_of!(ProcessInformation64, create_time) == 32);
const _: () = assert!(offset_of!(ProcessInformation64, user_time) == 40);
const _: () = assert!(offset_of!(ProcessInformation64, kernel_time) == 48);
const _: () = assert!(offset_of!(ProcessInformation64, image_name) == 56);
const _: () = assert!(offset_of!(ProcessInformation64, unique_process_id) == 80);
const _: () = assert!(offset_of!(ProcessInformation64, inherited_from_unique_process_id) == 88);
const _: () = assert!(offset_of!(ProcessInformation64, private_page_count) == 200);

/// `SYSTEM_PAGEFILE_INFORMATION`, 64-bit: sizes in pages.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageFileInformation64 {
    /// Bytes to the next entry; 0 on the last.
    pub next_entry_offset: u32,
    /// The page file's size, pages.
    pub total_size: u32,
    /// Pages in use.
    pub total_in_use: u32,
    /// The most pages it has had in use.
    pub peak_usage: u32,
    /// Its path.
    pub page_file_name: UnicodeString64,
}
const _: () = assert!(size_of::<PageFileInformation64>() == 32);

/// `SYSTEM_CPU_SET_INFORMATION` with its `CpuSet` arm (`winnt.h`); records
/// are walked by their own `Size`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuSetInformation {
    /// This record's size, bytes.
    pub size: u32,
    /// `CPU_SET_INFORMATION_TYPE`: 0 is `CpuSetInformation`.
    pub kind: u32,
    /// The CPU Set's id.
    pub id: u32,
    /// Its processor group.
    pub group: u16,
    /// Its processor's number in the group.
    pub logical_processor_index: u8,
    /// Its core's index.
    pub core_index: u8,
    /// Its last-level cache's index.
    pub last_level_cache_index: u8,
    /// Its NUMA node.
    pub numa_node_index: u8,
    /// Higher is faster and less efficient; 0 on every core of a
    /// homogeneous machine.
    pub efficiency_class: u8,
    /// Bit 0 `Parked`, 1 `Allocated`, 2 `AllocatedToTargetProcess`, 3
    /// `RealTime`.
    pub all_flags: u8,
    /// `SchedulingClass` (a union with `Reserved`).
    pub reserved: u32,
    /// `AllocationTag`.
    pub allocation_tag: u64,
}
const _: () = assert!(size_of::<CpuSetInformation>() == 32);
const _: () = assert!(offset_of!(CpuSetInformation, group) == 12);
const _: () = assert!(offset_of!(CpuSetInformation, logical_processor_index) == 14);
const _: () = assert!(offset_of!(CpuSetInformation, efficiency_class) == 18);
const _: () = assert!(offset_of!(CpuSetInformation, all_flags) == 19);

/// `MEMORYSTATUSEX` (`sysinfoapi.h`); `length` must be set to its size
/// before the call.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryStatusEx {
    /// `dwLength`: the struct's size.
    pub length: u32,
    /// Percent of physical memory in use.
    pub memory_load: u32,
    /// Physical memory, bytes.
    pub total_phys: u64,
    /// Physical memory available (standby, free and zeroed), bytes.
    pub avail_phys: u64,
    /// The commit limit, bytes.
    pub total_page_file: u64,
    /// Commit still available, bytes.
    pub avail_page_file: u64,
    /// This process's user address space, bytes.
    pub total_virtual: u64,
    /// Of which unreserved, bytes.
    pub avail_virtual: u64,
    /// Reserved.
    pub avail_extended_virtual: u64,
}
const _: () = assert!(size_of::<MemoryStatusEx>() == 64);

/// `PERFORMANCE_INFORMATION` (`psapi.h`), 64-bit: `SIZE_T` counts in pages
/// of `page_size` bytes; `cb` must be set to its size before the call.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PerformanceInformation64 {
    /// The struct's size.
    pub cb: u32,
    /// Commit charge.
    pub commit_total: u64,
    /// Commit limit: RAM plus page files.
    pub commit_limit: u64,
    /// The highest commit charge since boot.
    pub commit_peak: u64,
    /// Physical memory.
    pub physical_total: u64,
    /// Physical memory available.
    pub physical_available: u64,
    /// The system cache (standby list and system working set).
    pub system_cache: u64,
    /// The kernel's pools.
    pub kernel_total: u64,
    /// Paged pool.
    pub kernel_paged: u64,
    /// Nonpaged pool.
    pub kernel_nonpaged: u64,
    /// The page size, bytes.
    pub page_size: u64,
    /// Open handles.
    pub handle_count: u32,
    /// Processes.
    pub process_count: u32,
    /// Threads.
    pub thread_count: u32,
}
const _: () = assert!(size_of::<PerformanceInformation64>() == 104);
const _: () = assert!(offset_of!(PerformanceInformation64, commit_total) == 8);
const _: () = assert!(offset_of!(PerformanceInformation64, page_size) == 80);
const _: () = assert!(offset_of!(PerformanceInformation64, process_count) == 92);

fn bytes_at<const N: usize>(b: &[u8], at: usize) -> Option<[u8; N]> {
    b.get(at..at.checked_add(N)?)?.try_into().ok()
}
fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    bytes_at(b, at).map(u16::from_le_bytes)
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    bytes_at(b, at).map(u32::from_le_bytes)
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    bytes_at(b, at).map(u64::from_le_bytes)
}
fn i64_at(b: &[u8], at: usize) -> Option<i64> {
    bytes_at(b, at).map(i64::from_le_bytes)
}

/// A non-negative 100 ns count; a negative one (never written by the kernel)
/// reads 0.
fn hundred_ns(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

/// Every processor's ticks from one group's
/// `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION` array, in its order: `user` is
/// user time, `system` kernel time less idle time (DPCs and interrupts are
/// inside it), `idle` idle time, `nice` 0; 100 ns units. A trailing partial
/// record is left out. PURE.
#[must_use]
pub fn processor_times(bytes: &[u8]) -> Vec<CoreTicks> {
    let (records, _) = bytes.as_chunks::<{ size_of::<ProcessorPerformance>() }>();
    records
        .iter()
        .map(|r| {
            let at = |off| i64_at(r, off).map_or(0, hundred_ns);
            let idle = at(offset_of!(ProcessorPerformance, idle_time));
            let kernel = at(offset_of!(ProcessorPerformance, kernel_time));
            CoreTicks {
                user: at(offset_of!(ProcessorPerformance, user_time)),
                system: kernel.saturating_sub(idle),
                idle,
                nice: 0,
            }
        })
        .collect()
}

/// One CPU Set: where its processor sits and how fast its core is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuSet {
    /// The processor group.
    pub group: u16,
    /// The processor's number in its group.
    pub index: u8,
    /// Higher is faster.
    pub efficiency: u8,
    /// The OS parked it (it runs nothing now).
    pub parked: bool,
}

/// The CPU Sets in a `GetSystemCpuSetInformation` buffer, walked by each
/// record's `Size`; a record of another type is skipped, and a size too small
/// to hold the fields read (or one that runs past the buffer) ends the walk.
/// PURE.
#[must_use]
pub fn cpu_sets(bytes: &[u8]) -> Vec<CpuSet> {
    let need = offset_of!(CpuSetInformation, all_flags) + 1;
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(size) = u32_at(bytes, at).and_then(|s| usize::try_from(s).ok()) {
        let Some(rec) = at.checked_add(size).and_then(|end| bytes.get(at..end)) else {
            break;
        };
        if size < need {
            break;
        }
        if u32_at(rec, offset_of!(CpuSetInformation, kind)) == Some(0) {
            let byte = |off: usize| rec[off];
            out.push(CpuSet {
                group: u16_at(rec, offset_of!(CpuSetInformation, group)).unwrap_or(0),
                index: byte(offset_of!(CpuSetInformation, logical_processor_index)),
                efficiency: byte(offset_of!(CpuSetInformation, efficiency_class)),
                parked: byte(offset_of!(CpuSetInformation, all_flags)) & 1 != 0,
            });
        }
        at += size;
    }
    out
}

/// The layout and the parked cores from the CPU Sets and each processor
/// group's width (`GetActiveProcessorCount`): logical id = the widths of
/// the groups before the set's, plus its number in its group (the order
/// [`crate::Probe::core_ticks`] reads them in). One cluster per efficiency
/// class, highest first ([`crate::machine::layout_by_rank`]: "Performance"
/// and "Efficiency" for two, one `"CPU"` cluster where every core has the
/// same class). A set outside the groups' widths is left out; `None` for
/// no sets. PURE.
#[must_use]
pub fn layout_from_cpu_sets(sets: &[CpuSet], widths: &[u32]) -> Option<(CpuLayout, Vec<u16>)> {
    let mut offsets = Vec::with_capacity(widths.len());
    let mut next = 0u32;
    for w in widths {
        offsets.push(next);
        next = next.saturating_add(*w);
    }
    let mut ranked = Vec::with_capacity(sets.len());
    let mut parked = Vec::new();
    for s in sets {
        let g = usize::from(s.group);
        let (Some(&off), Some(&width)) = (offsets.get(g), widths.get(g)) else {
            continue;
        };
        if u32::from(s.index) >= width {
            continue;
        }
        let Ok(id) = u16::try_from(off + u32::from(s.index)) else {
            continue;
        };
        ranked.push((id, u32::from(s.efficiency)));
        if s.parked {
            parked.push(id);
        }
    }
    parked.sort_unstable();
    parked.dedup();
    layout_by_rank(&ranked).map(|layout| (layout, parked))
}

/// Swap `(in use, size)` in pages from a `SystemPageFileInformation` buffer:
/// every page file summed. An empty buffer is no page file, `(0, 0)`. `None`
/// for a record cut short or a `NextEntryOffset` past the buffer. PURE.
#[must_use]
pub fn page_files(bytes: &[u8]) -> Option<(u64, u64)> {
    let (mut used, mut total) = (0u64, 0u64);
    if bytes.is_empty() {
        return Some((0, 0));
    }
    let mut at = 0usize;
    loop {
        let rec = bytes.get(at..at.checked_add(size_of::<PageFileInformation64>())?)?;
        total = total.saturating_add(u64::from(u32_at(
            rec,
            offset_of!(PageFileInformation64, total_size),
        )?));
        used = used.saturating_add(u64::from(u32_at(
            rec,
            offset_of!(PageFileInformation64, total_in_use),
        )?));
        let next = usize::try_from(u32_at(
            rec,
            offset_of!(PageFileInformation64, next_entry_offset),
        )?)
        .ok()?;
        if next == 0 {
            break;
        }
        at = at.checked_add(next)?;
    }
    Some((used, total))
}

/// The commit charge at or past this many thousandths of the limit is
/// [`MemoryLevel::Warn`] (design §9.6: Windows runs out of commit while RAM
/// is free).
pub const COMMIT_WARN_PERMILLE: u64 = 900;

/// The pressure level on Windows: warn when the commit charge reaches
/// [`COMMIT_WARN_PERMILLE`] of its limit or the low-memory resource
/// notification is set, else normal. Windows has no level that says the
/// kernel is about to kill, so this never reads critical. `None` for a zero
/// limit. PURE.
#[must_use]
pub fn commit_level(commit: &Commit, low_memory: bool) -> Option<MemoryLevel> {
    let permille = commit.permille()?;
    Some(if permille >= COMMIT_WARN_PERMILLE || low_memory {
        MemoryLevel::Warn
    } else {
        MemoryLevel::Normal
    })
}

/// [`MemoryDetail`] from `GlobalMemoryStatusEx`, `K32GetPerformanceInfo`,
/// the page files' `(in use, size)` in pages and the low-memory
/// notification, in design §9.6's classes. Windows does not partition
/// physical memory into these exactly ([`MemoryDetail::exact`] false; the
/// standby list alone is published only to an administrator, through
/// `SystemMemoryListInformation`), and the split of available memory into
/// cache and free is an estimate:
///
/// | field | from |
/// |---|---|
/// | `total` | `ullTotalPhys` |
/// | `file_backed` | the cache: `SystemCache` pages, at most what is available. `SystemCache` is the standby list plus the system cache's working set, which is in use, so this is an upper bound on the standby list (design §9.6's table takes it for the standby list alone) |
/// | `free` | `ullAvailPhys` less that: a lower bound on the free and zeroed lists, short by the system cache's working set |
/// | `wired` | the kernel's pools: `KernelPaged + KernelNonpaged` pages, at most what is in use |
/// | `app` | in use (`ullTotalPhys − ullAvailPhys`) less the pools |
/// | `swap_used`, `swap_total` | the page files' pages in use and size |
/// | `commit` | `CommitTotal` against `CommitLimit`, exact |
/// | `pressure` | [`commit_level`] |
/// | `compressed`, `stored` | `None`: the compression store's size needs the `Memory Compression` process's working set, unverified for a non-admin reader |
/// | `speculative`, `active`, `inactive`, `purgeable`, `shared` | `None` |
///
/// `None` for a zero page size or no physical memory. PURE.
#[must_use]
pub fn memory_detail_of(
    status: &MemoryStatusEx,
    perf: &PerformanceInformation64,
    swap_pages: Option<(u64, u64)>,
    low_memory: bool,
) -> Option<MemoryDetail> {
    let page = perf.page_size;
    if page == 0 || status.total_phys == 0 {
        return None;
    }
    let pages = |n: u64| n.saturating_mul(page);
    let total = status.total_phys;
    let available = status.avail_phys.min(total);
    let in_use = total - available;
    let cache = pages(perf.system_cache).min(available);
    let kernel = pages(perf.kernel_paged.saturating_add(perf.kernel_nonpaged)).min(in_use);
    let commit = Commit {
        charge: pages(perf.commit_total),
        limit: pages(perf.commit_limit),
        exact: true,
    };
    let (swap_used, swap_total) = swap_pages.map_or((0, 0), |(u, t)| (pages(u), pages(t)));
    Some(MemoryDetail {
        total,
        free: available - cache,
        speculative: None,
        active: None,
        inactive: None,
        wired: kernel,
        compressed: None,
        stored: None,
        purgeable: None,
        file_backed: cache,
        app: in_use - kernel,
        shared: None,
        swap_used,
        swap_total,
        pressure: commit_level(&commit, low_memory),
        commit: Some(commit),
        exact: false,
        counters: None,
        kernel_available_pct: None,
    })
}

/// A UTF-16LE name `length` bytes long at address `ptr` inside a buffer
/// whose first byte is at address `base`: empty when the address is null or
/// outside the buffer.
fn name_at(bytes: &[u8], base: u64, ptr: u64, length: u16) -> String {
    let Some(off) = ptr.checked_sub(base).and_then(|o| usize::try_from(o).ok()) else {
        return String::new();
    };
    let Some(text) = off
        .checked_add(usize::from(length))
        .and_then(|end| bytes.get(off..end))
    else {
        return String::new();
    };
    let (units, _) = text.as_chunks::<2>();
    let wide: Vec<u16> = units.iter().map(|u| u16::from_le_bytes(*u)).collect();
    String::from_utf16_lossy(&wide)
}

/// An image name as a short name: its `.exe` (any case) dropped.
fn short_image_name(name: String) -> String {
    let n = name.len();
    if n > 4 && name.is_char_boundary(n - 4) && name[n - 4..].eq_ignore_ascii_case(".exe") {
        name[..n - 4].to_owned()
    } else {
        name
    }
}

/// One sweep from a `SystemProcessInformation` buffer whose first byte is at
/// address `base` (the image names point into it). Each process's parent
/// (`InheritedFromUniqueProcessId`), CPU time (user plus kernel, 100 ns →
/// ns), private commit (`PrivatePageCount`, bytes) and image name less
/// `.exe`. The System Idle Process (pid 0), whose times are the cores' idle
/// time, is left out. A parent link is cut (`ppid` 0) when the parent's pid
/// now belongs to a process created after the child: Windows reuses pids,
/// and the creator may have exited. At most [`MAX_PROCESSES`]; an entry cut
/// short ends the walk. PURE.
#[must_use]
pub fn processes(bytes: &[u8], base: u64) -> Sweep {
    const SIZE: usize = size_of::<ProcessInformation64>();
    let field = |rec: &[u8], off: usize| u64_at(rec, off).unwrap_or(0);
    let mut found: Vec<(ProcessReading, i64)> = Vec::new();
    let mut truncated = false;
    let mut at = 0usize;
    while let Some(rec) = at.checked_add(SIZE).and_then(|end| bytes.get(at..end)) {
        let pid = field(rec, offset_of!(ProcessInformation64, unique_process_id));
        if pid != 0 {
            if found.len() >= MAX_PROCESSES {
                truncated = true;
                break;
            }
            let time = |off| i64_at(rec, off).map_or(0, hundred_ns);
            let cpu_100ns = time(offset_of!(ProcessInformation64, user_time))
                .saturating_add(time(offset_of!(ProcessInformation64, kernel_time)));
            let name_off = offset_of!(ProcessInformation64, image_name);
            let name = name_at(
                bytes,
                base,
                field(rec, name_off + offset_of!(UnicodeString64, buffer)),
                u16_at(rec, name_off).unwrap_or(0),
            );
            found.push((
                ProcessReading {
                    pid: u32::try_from(pid).unwrap_or(u32::MAX),
                    start: i64_at(rec, offset_of!(ProcessInformation64, create_time))
                        .and_then(|value| u64::try_from(value).ok())
                        .filter(|value| *value > 0),
                    ppid: u32::try_from(field(
                        rec,
                        offset_of!(ProcessInformation64, inherited_from_unique_process_id),
                    ))
                    .unwrap_or(0),
                    cpu_ns: Some(cpu_100ns.saturating_mul(100)),
                    footprint: Some(field(
                        rec,
                        offset_of!(ProcessInformation64, private_page_count),
                    )),
                    disk_read: None,
                    disk_written: None,
                    name: short_image_name(name),
                    // No process groups, Unix sessions or controlling
                    // terminals on Windows; the run state is not in this
                    // record (its threads' states are).
                    pgid: 0,
                    uid: None,
                    state: RunState::Unknown,
                    session_leader: None,
                    has_tty: None,
                    runnable_ns: None,
                    peak_footprint: None,
                    // One system call reads every process at once: the
                    // caller stamps them all with its time.
                    read_at: None,
                    coalition: None,
                },
                i64_at(rec, offset_of!(ProcessInformation64, create_time)).unwrap_or(0),
            ));
        }
        let next = u32_at(rec, 0)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0);
        if next < SIZE {
            break;
        }
        at = at.saturating_add(next);
    }
    let born: HashMap<u32, i64> = found.iter().map(|(p, t)| (p.pid, *t)).collect();
    let procs = found
        .into_iter()
        .map(|(mut p, created)| {
            if born.get(&p.ppid).is_some_and(|&parent| parent > created) {
                p.ppid = 0;
            }
            p
        })
        .collect();
    Sweep { procs, truncated }
}

/// The drive letter a path names, from its spelling alone (uppercase): `C`
/// for `C:\Users\x`, `c:/x` and `\\?\C:\x`. `None` for a UNC path
/// (`\\server\share`, `\\?\UNC\…`), a volume GUID path, a device path
/// (`\\.\…`), a drive-relative path (`C:x`) or a relative one: none of those
/// is judged without asking the file system, and a network share must never
/// be asked. PURE.
#[must_use]
pub fn drive_root(path: &str) -> Option<char> {
    let rest = path.strip_prefix(r"\\?\").unwrap_or(path);
    let b = rest.as_bytes();
    match b {
        [letter, b':', b'\\' | b'/', ..] if letter.is_ascii_alphabetic() => {
            Some(char::from(letter.to_ascii_uppercase()))
        }
        _ => None,
    }
}

/// `GetDriveTypeW`'s `DRIVE_FIXED`.
pub const DRIVE_FIXED: u32 = 3;
/// `GetDriveTypeW`'s `DRIVE_RAMDISK`.
pub const DRIVE_RAMDISK: u32 = 6;

/// Whether a `GetDriveTypeW` answer is a drive read for its free space: a
/// fixed disk or a RAM disk. A network drive (`DRIVE_REMOTE`) can block, a
/// removable or optical one can ask for its medium, and an unknown one is
/// not judged. PURE.
#[must_use]
pub const fn drive_is_local(kind: u32) -> bool {
    matches!(kind, DRIVE_FIXED | DRIVE_RAMDISK)
}

/// A drive's root as a NUL-terminated wide string (`C:\`).
#[must_use]
pub fn root_wide(letter: char) -> [u16; 4] {
    let mut buf = [0u16; 2];
    let l = letter.encode_utf16(&mut buf)[0];
    [l, u16::from(b':'), u16::from(b'\\'), 0]
}
