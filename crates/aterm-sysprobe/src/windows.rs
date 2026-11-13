// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows arm's decoders: pure functions over the buffers Windows fills, so every host
//! compiles and tests them (design `docs/DESIGN-system-awareness-2026-10-16.md` §5.4, §9.6, §16).

use std::collections::{HashMap, HashSet};
use std::mem::{offset_of, size_of};

use crate::jobs::RunState;
use crate::machine::{Commit, layout_by_rank};
use crate::procs::{MAX_PROCESSES, ProcessReading, Sweep};
use crate::{
    CoreTicks, CpuLayout, GroupMemory, MAX_GROUP_PIDS, MemberUsage, MemoryDetail, MemoryLevel,
};

/// `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION` (`winternl.h`), one per processor.
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

/// `UNICODE_STRING`, 64-bit.
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

/// 64-bit `SYSTEM_PROCESS_INFORMATION` (`winternl.h`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessInformation64 {
    /// Bytes to the next entry.
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
    /// I/O counters.
    pub io: [i64; 6],
}
const _: () = assert!(size_of::<ProcessInformation64>() == 256);
const _: () = assert!(offset_of!(ProcessInformation64, create_time) == 32);
const _: () = assert!(offset_of!(ProcessInformation64, user_time) == 40);
const _: () = assert!(offset_of!(ProcessInformation64, kernel_time) == 48);
const _: () = assert!(offset_of!(ProcessInformation64, image_name) == 56);
const _: () = assert!(offset_of!(ProcessInformation64, unique_process_id) == 80);
const _: () = assert!(offset_of!(ProcessInformation64, inherited_from_unique_process_id) == 88);
const _: () = assert!(offset_of!(ProcessInformation64, session_id) == 100);
const _: () = assert!(offset_of!(ProcessInformation64, working_set_size) == 144);
const _: () = assert!(offset_of!(ProcessInformation64, peak_pagefile_usage) == 192);
const _: () = assert!(offset_of!(ProcessInformation64, private_page_count) == 200);
const _: () = assert!(offset_of!(ProcessInformation64, io) == 208);

/// 64-bit `SYSTEM_THREAD_INFORMATION` (`winternl.h`): a process record is followed by one per
/// thread.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThreadInformation64 {
    /// Kernel time, 100 ns.
    pub kernel_time: i64,
    /// User time, 100 ns.
    pub user_time: i64,
    /// When it started (100 ns since 1601).
    pub create_time: i64,
    /// How long it has waited, ticks.
    pub wait_time: u32,
    /// Where it started.
    pub start_address: u64,
    /// Its process's pid.
    pub unique_process: u64,
    /// Its thread id.
    pub unique_thread: u64,
    /// Its priority now.
    pub priority: i32,
    /// Its base priority.
    pub base_priority: i32,
    /// Context switches.
    pub context_switches: u32,
    /// `KTHREAD_STATE`: 1 ready, 2 running, 3 standby, 5 waiting, 7 deferred ready.
    pub thread_state: u32,
    /// `KWAIT_REASON`, read while waiting: 5 `Suspended`, 12 `WrSuspended`.
    pub wait_reason: u32,
}
const _: () = assert!(size_of::<ThreadInformation64>() == 80);
const _: () = assert!(offset_of!(ThreadInformation64, thread_state) == 68);
const _: () = assert!(offset_of!(ThreadInformation64, wait_reason) == 72);

/// `SYSTEM_PAGEFILE_INFORMATION`, 64-bit.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageFileInformation64 {
    /// Bytes to the next entry.
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

/// `SYSTEM_CPU_SET_INFORMATION` with its `CpuSet` arm (`winnt.h`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuSetInformation {
    /// This record's size, bytes.
    pub size: u32,
    /// `CPU_SET_INFORMATION_TYPE`.
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
    /// Higher is faster and less efficient.
    pub efficiency_class: u8,
    /// Bit 0 `Parked`, 1 `Allocated`, 2 `AllocatedToTargetProcess`, 3 `RealTime`.
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

/// `MEMORYSTATUSEX` (`sysinfoapi.h`).
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

/// `PERFORMANCE_INFORMATION` (`psapi.h`), 64-bit.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PerformanceInformation64 {
    /// The struct's size.
    pub cb: u32,
    /// Commit charge.
    pub commit_total: u64,
    /// Commit limit.
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
const _: () = assert!(offset_of!(PerformanceInformation64, thread_count) == 96);

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

/// A non-negative count (100 ns of time, bytes transferred); a negative one (never written by
/// the kernel) reads 0.
fn hundred_ns(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

/// Every processor's ticks from one group's `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION` array, in its order.
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

/// The CPU Sets in a `GetSystemCpuSetInformation` buffer, walked by each record's `Size`.
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

/// The layout and the parked cores from the CPU Sets and each processor group's width (`GetActiveProcessorCount`).
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

/// Swap `(in use, size)` in pages from a `SystemPageFileInformation` buffer.
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

/// Commit-charge threshold in thousandths of the limit for [`MemoryLevel::Warn`].
pub const COMMIT_WARN_PERMILLE: u64 = 900;

/// The pressure level on Windows.
#[must_use]
pub fn commit_level(commit: &Commit, low_memory: bool) -> Option<MemoryLevel> {
    let permille = commit.permille()?;
    Some(if permille >= COMMIT_WARN_PERMILLE || low_memory {
        MemoryLevel::Warn
    } else {
        MemoryLevel::Normal
    })
}

/// Build [`MemoryDetail`] from `GlobalMemoryStatusEx`, `K32GetPerformanceInfo`, page-file usage, and low-memory state.
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
        available,
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
        memorystatus_level_pct: None,
    })
}

/// The image name of Windows' memory compression store: a process of session 0 whose working
/// set is the compressed pages (what Task Manager shows as `In use (Compressed)`). A machine
/// with compression off (Windows Server by default) has no such process.
pub const MEMORY_COMPRESSION: &str = "Memory Compression";

/// `detail` with the memory compression store named as its own class: the working set of the
/// [`MEMORY_COMPRESSION`] row of a sweep's `procs` becomes [`MemoryDetail::compressed`], taken
/// out of [`MemoryDetail::app`], which counted it (the store is memory in use that is no
/// kernel pool), so the memory used ([`MemoryDetail::used`]) is the same figure. `detail`
/// unchanged where the sweep has no such row in session 0 (compression is off, which is not
/// zero bytes compressed) or the row has no working set. [`memory_detail_of`] reads no sweep,
/// so a caller that holds one names the class with this.
#[must_use]
pub fn with_compression_store(mut detail: MemoryDetail, procs: &[ProcessReading]) -> MemoryDetail {
    let store = procs
        .iter()
        .find(|p| p.name == MEMORY_COMPRESSION && p.uid == Some(0))
        .and_then(|p| p.resident);
    if let Some(store) = store {
        let store = store.min(detail.app);
        detail.app -= store;
        detail.compressed = Some(store);
    }
    detail
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
pub(crate) fn short_image_name(name: String) -> String {
    let n = name.len();
    if n > 4 && name.is_char_boundary(n - 4) && name[n - 4..].eq_ignore_ascii_case(".exe") {
        name[..n - 4].to_owned()
    } else {
        name
    }
}

/// The thread records of the process record `rec` at `at`: `NumberOfThreads` of them straight
/// after it, inside its entry (to the next entry, or the buffer's end for the last). `None` for
/// a record with no threads or one the buffer does not hold whole.
fn threads_of<'a>(bytes: &'a [u8], at: usize, rec: &[u8]) -> Option<&'a [u8]> {
    const SIZE: usize = size_of::<ProcessInformation64>();
    let count = usize::try_from(u32_at(
        rec,
        offset_of!(ProcessInformation64, number_of_threads),
    )?)
    .ok()
    .filter(|n| *n > 0)?;
    let next = usize::try_from(u32_at(rec, 0)?).ok()?;
    let end = if next == 0 {
        bytes.len()
    } else {
        at.checked_add(next)?.min(bytes.len())
    };
    let from = at.checked_add(SIZE)?;
    let to = from.checked_add(count.checked_mul(size_of::<ThreadInformation64>())?)?;
    (to <= end).then(|| bytes.get(from..to)).flatten()
}

/// A process's state from its thread records (design §16 "stopped, zombie": Windows has no
/// zombie, and a process is stopped when every thread is suspended): [`RunState::Running`] when
/// any thread is on a core or ready for one, [`RunState::Stopped`] when every thread waits
/// suspended (a debugger's, `NtSuspendProcess`, a suspended packaged app), else
/// [`RunState::Sleeping`]; [`RunState::Unknown`] for no whole record.
#[must_use]
pub fn run_state(threads: &[u8]) -> RunState {
    let (records, _) = threads.as_chunks::<{ size_of::<ThreadInformation64>() }>();
    if records.is_empty() {
        return RunState::Unknown;
    }
    let mut suspended = 0usize;
    for r in records {
        let state = u32_at(r, offset_of!(ThreadInformation64, thread_state)).unwrap_or(0);
        let reason = u32_at(r, offset_of!(ThreadInformation64, wait_reason)).unwrap_or(0);
        match state {
            1 | 2 | 3 | 7 => return RunState::Running,
            5 if matches!(reason, 5 | 12) => suspended += 1,
            _ => {}
        }
    }
    if suspended == records.len() {
        RunState::Stopped
    } else {
        RunState::Sleeping
    }
}

/// One sweep from a `SystemProcessInformation` buffer whose first byte is at address `base` (the
/// image names point into it). Each row keeps what the record holds: its private commit as the
/// footprint with the peak commit beside it, its working set as the resident figure, the bytes
/// it moved through any handle ([`ProcessReading::io_read`] and
/// [`ProcessReading::io_written`]: files, pipes, consoles and sockets alike, so never a disk
/// figure, and [`ProcessReading::disk_read`] and [`ProcessReading::disk_written`] stay unread),
/// its session as its owner
/// ([`ProcessReading::uid`]: session 0 is the services', [`session_class`]) and its state from
/// its threads ([`run_state`]). A parent pid whose process was created after the row's own is a
/// reused pid, not its parent, and reads 0.
#[must_use]
pub fn processes(bytes: &[u8], base: u64) -> Sweep {
    const SIZE: usize = size_of::<ProcessInformation64>();
    const IO: usize = offset_of!(ProcessInformation64, io);
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
            let count = |off| i64_at(rec, off).map_or(0, hundred_ns);
            let cpu_100ns = count(offset_of!(ProcessInformation64, user_time))
                .saturating_add(count(offset_of!(ProcessInformation64, kernel_time)));
            let name_off = offset_of!(ProcessInformation64, image_name);
            let name = name_at(
                bytes,
                base,
                field(rec, name_off + offset_of!(UnicodeString64, buffer)),
                u16_at(rec, name_off).unwrap_or(0),
            );
            let start = i64_at(rec, offset_of!(ProcessInformation64, create_time))
                .and_then(|value| u64::try_from(value).ok())
                .filter(|value| *value > 0);
            found.push((
                ProcessReading {
                    scheduling: None,
                    pid: u32::try_from(pid).unwrap_or(u32::MAX),
                    start,
                    ppid: u32::try_from(field(
                        rec,
                        offset_of!(ProcessInformation64, inherited_from_unique_process_id),
                    ))
                    .unwrap_or(0),
                    cpu_ns: Some(cpu_100ns.saturating_mul(100)),
                    child_cpu_ns: None,
                    footprint: Some(field(
                        rec,
                        offset_of!(ProcessInformation64, private_page_count),
                    )),
                    resident: Some(field(
                        rec,
                        offset_of!(ProcessInformation64, working_set_size),
                    )),
                    disk_read: None,
                    disk_written: None,
                    io_read: Some(count(IO + 3 * 8)),
                    io_written: Some(count(IO + 4 * 8)),
                    energy_nj: None,
                    name: short_image_name(name),
                    pgid: 0,
                    uid: u32_at(rec, offset_of!(ProcessInformation64, session_id)),
                    state: threads_of(bytes, at, rec).map_or(RunState::Unknown, run_state),
                    session_leader: None,
                    sid: 0,
                    has_tty: None,
                    exec: None,
                    runnable_ns: None,
                    cores: None,
                    peak_footprint: Some(field(
                        rec,
                        offset_of!(ProcessInformation64, peak_pagefile_usage),
                    )),
                    read_at: None,
                    coalition: None,
                    version: None,
                    birth_ms: start.and_then(crate::procs::birth_of_filetime),
                    reaps_unwaited: None,
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

/// The drive letter a path names, from its spelling alone (uppercase).
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

/// Whether a `GetDriveTypeW` answer is a drive read for its free space.
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

/// Whose a process is, from its session (design §12 and §16 "OTHERS / SERVICES split": the
/// user's session against Session 0, from the one sweep).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionClass {
    /// Session 0: Windows' services and the kernel's own processes.
    Services,
    /// The session the reader itself runs in: this person's programs, elevated ones included.
    Own,
    /// Another person's session (fast user switching, a remote desktop).
    Other,
}

/// The class of a process in `session` ([`ProcessReading::uid`] on Windows) for a reader in
/// session `own` ([`crate::own_session`]). A reader that is itself a service reads every
/// Session 0 process as [`SessionClass::Services`], its own included.
#[must_use]
pub const fn session_class(session: u32, own: u32) -> SessionClass {
    if session == 0 {
        SessionClass::Services
    } else if session == own {
        SessionClass::Own
    } else {
        SessionClass::Other
    }
}

/// `root` and the live processes that name it an ancestor in one sweep's rows, `root` first,
/// then breadth first: at most [`MAX_GROUP_PIDS`]. A row is a child by its parent pid, which
/// the sweep's decoder ([`processes`]) already cleared where the pid named a process created
/// after the row, so a reused pid adopts nothing. Windows gives a process whose parent exited
/// no new parent, so that process and everything under it is in no tree here but its own
/// ([`tree_adopting`] finds the ones that share a console with the tree). Empty when `root` is
/// 0 or not in the sweep.
#[must_use]
pub fn tree(procs: &[ProcessReading], root: u32) -> Vec<&ProcessReading> {
    let Some(first) = procs.iter().find(|p| p.pid == root && root != 0) else {
        return Vec::new();
    };
    let children = children_of(procs);
    let mut out = vec![first];
    let mut seen = HashSet::from([root]);
    descend(&children, &mut out, &mut seen, 0);
    out
}

/// Each parent pid's rows in a sweep.
fn children_of(procs: &[ProcessReading]) -> HashMap<u32, Vec<&ProcessReading>> {
    let mut children: HashMap<u32, Vec<&ProcessReading>> = HashMap::new();
    for p in procs {
        if p.ppid != 0 && p.ppid != p.pid {
            children.entry(p.ppid).or_default().push(p);
        }
    }
    children
}

/// Adds to `out` every descendant of its rows from index `at` on, breadth first, up to
/// [`MAX_GROUP_PIDS`] rows in all.
fn descend<'a>(
    children: &HashMap<u32, Vec<&'a ProcessReading>>,
    out: &mut Vec<&'a ProcessReading>,
    seen: &mut HashSet<u32>,
    mut at: usize,
) {
    while let Some(parent) = out.get(at).map(|p| p.pid) {
        at += 1;
        for child in children.get(&parent).into_iter().flatten() {
            if out.len() >= MAX_GROUP_PIDS {
                return;
            }
            if seen.insert(child.pid) {
                out.push(child);
            }
        }
    }
}

/// The most orphans one [`tree_adopting`] asks the console of: each is a process opened and
/// queried, so a machine with thousands of them costs a bounded read.
pub const MAX_CONSOLE_READS: usize = 4096;

/// The pid in what `NtQueryInformationProcess(ProcessConsoleHostProcess)` answers: the console
/// host's pid with flags in the two low bits (a pid is a multiple of four). `None` for a
/// process attached to no console, which answers the flags alone.
#[must_use]
pub fn console_host_pid(answer: u64) -> Option<u32> {
    u32::try_from(answer & !3).ok().filter(|pid| *pid != 0)
}

/// [`tree`], and the orphans that belong to it. Windows never gives a process a new parent, so
/// when one in the middle of a tree exits, everything it started names a pid that is gone: an
/// MSYS or Cygwin `exec` (Git Bash running a script, `make`, an `npm` wrapper) starts the new
/// image from a process that then exits, and so does `cmd /c start`. Such an orphan is still
/// attached to the console it was started in, so it is counted when it was created no earlier
/// than `root` and `console_of` (the pid of the console host a row's process is attached to,
/// `None` for none or unread) names
///
/// - a member of the tree: a console one of the tree's processes created, whose host is that
///   process's child (how Claude Code's Bash tool runs each command), which only that
///   process's descendants are attached to; or
/// - the console `root` itself is attached to (a tab's): what was started in it since `root`
///   was. A program the tab's shell started there after `root`, whose own parent has exited,
///   is counted too; while `root` runs in the foreground the shell starts none.
///
/// Each adopted orphan brings its own descendants, and the search repeats while it adds a
/// member, so an orphan attached to a console an adopted process created is found as well.
/// `console_of` is asked once for `root` and once for each orphan created since it, at most
/// [`MAX_CONSOLE_READS`], and not at all in a sweep with no such orphan. Still not counted: an
/// orphan with no console (a detached or windowed program), one whose console's own creator
/// has exited, and one this reader may not open (another session's, a protected process).
#[must_use]
pub fn tree_adopting(
    procs: &[ProcessReading],
    root: u32,
    mut console_of: impl FnMut(&ProcessReading) -> Option<u32>,
) -> Vec<&ProcessReading> {
    let mut out = tree(procs, root);
    let Some((first, since)) = out.first().and_then(|p| Some((*p, p.start?))) else {
        return out;
    };
    let mut seen: HashSet<u32> = out.iter().map(|p| p.pid).collect();
    let live: HashSet<u32> = procs.iter().map(|p| p.pid).collect();
    let orphans: Vec<&ProcessReading> = procs
        .iter()
        .filter(|p| {
            p.pid != 0
                && !seen.contains(&p.pid)
                && (p.ppid == p.pid || !live.contains(&p.ppid))
                && p.start.is_some_and(|born| born >= since)
        })
        .take(MAX_CONSOLE_READS)
        .collect();
    if orphans.is_empty() {
        return out;
    }
    let own = console_of(first).filter(|host| live.contains(host));
    let mut waiting: Vec<(&ProcessReading, u32)> = orphans
        .into_iter()
        .filter_map(|p| Some((p, console_of(p)?)))
        .collect();
    let children = children_of(procs);
    loop {
        let before = out.len();
        waiting.retain(|&(p, host)| {
            if seen.contains(&p.pid) {
                return false;
            }
            let ours = Some(host) == own || seen.contains(&host);
            if !ours || out.len() >= MAX_GROUP_PIDS {
                return true;
            }
            seen.insert(p.pid);
            out.push(p);
            let at = out.len() - 1;
            descend(&children, &mut out, &mut seen, at);
            false
        });
        if out.len() == before || out.len() >= MAX_GROUP_PIDS {
            return out;
        }
    }
}

/// [`crate::group_memory`] on Windows, from one sweep's rows: the private commit of `root` and
/// the live processes under it ([`tree_adopting`], with `console_of` as there) summed, in MiB,
/// under `root` as the group's id. Windows has no process group, and an agent's tool calls are
/// plain child processes (design §11.3), so the tree is the unit. `None` when `root` is not in
/// the sweep.
#[must_use]
pub fn tree_memory(
    procs: &[ProcessReading],
    root: u32,
    console_of: impl FnMut(&ProcessReading) -> Option<u32>,
) -> Option<GroupMemory> {
    let members = tree_adopting(procs, root, console_of);
    if members.is_empty() {
        return None;
    }
    let bytes = members
        .iter()
        .fold(0u64, |sum, p| sum.saturating_add(p.footprint.unwrap_or(0)));
    Some(GroupMemory {
        pgid: root,
        footprint_mib: bytes >> 20,
        measured: u32::try_from(members.len()).unwrap_or(u32::MAX),
    })
}

/// [`crate::group_usage`] on Windows, from one sweep's rows: what `root` and each live process
/// under it ([`tree_adopting`], with `console_of` as there) has used, each row read as every
/// sweep's is ([`crate::activity::member`]). `start` is the creation `FILETIME`, `io_bytes`
/// every byte it moved through any handle (files, pipes, consoles and sockets alike),
/// `footprint` its private commit and `peak_footprint` the most that has been; Windows keeps
/// no runnable time, which reads 0. `None` when `root` is not in the sweep.
#[must_use]
pub fn tree_usage(
    procs: &[ProcessReading],
    root: u32,
    console_of: impl FnMut(&ProcessReading) -> Option<u32>,
) -> Option<Vec<MemberUsage>> {
    let members = tree_adopting(procs, root, console_of);
    if members.is_empty() {
        return None;
    }
    Some(members.into_iter().map(crate::activity::member).collect())
}

/// The first build of Windows 11: every later build is Windows 11 or newer, while the registry's
/// `ProductName` still says `Windows 10` there.
pub const WINDOWS_11_FIRST_BUILD: u32 = 22_000;

/// The OS words from the registry's `ProductName` and `CurrentBuildNumber` (`Windows 11 Home
/// 26200`): the product name with the build after it. Microsoft froze `ProductName` at `Windows
/// 10 …` for client Windows, so from build [`WINDOWS_11_FIRST_BUILD`] a leading `Windows 10` is
/// read as `Windows 11` (the rule `winver` and Settings follow); a server's name is its own.
/// `None` without a product name; the name alone without a build.
#[must_use]
pub fn os_words(product: Option<&str>, build: Option<&str>) -> Option<String> {
    let product = product.map(str::trim).filter(|p| !p.is_empty())?;
    let build = build.map(str::trim).filter(|b| !b.is_empty());
    let eleven = build
        .and_then(crate::parse_uint::<u32>)
        .is_some_and(|b| b >= WINDOWS_11_FIRST_BUILD);
    let name = match product.strip_prefix("Windows 10") {
        Some(rest) if eleven && (rest.is_empty() || rest.starts_with(' ')) => {
            format!("Windows 11{rest}")
        }
        _ => product.to_owned(),
    };
    Some(match build {
        Some(build) => format!("{name} {build}"),
        None => name,
    })
}

/// The OS build from the registry's `CurrentBuildNumber` and `UBR` (the update revision, which
/// every cumulative update raises): `26200.7171`, or the build alone where the revision is
/// unread. `None` without a build number.
#[must_use]
pub fn os_build(build: Option<&str>, revision: Option<u32>) -> Option<String> {
    let build = build.map(str::trim).filter(|b| !b.is_empty())?;
    Some(match revision {
        Some(revision) => format!("{build}.{revision}"),
        None => build.to_owned(),
    })
}
