// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows `extern "system"` FFI calls for [`crate::windows`].

#![allow(
    non_snake_case,
    clippy::upper_case_acronyms,
    reason = "Win32 names are kept verbatim (`BOOL`, `GetDriveTypeW`) to check against the SDK"
)]

use std::ffi::c_void;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::windows::{
    MemoryStatusEx, PerformanceInformation64, ProcessorPerformance, cpu_sets, drive_is_local,
    drive_root, layout_from_cpu_sets, memory_detail_of, page_files, processor_times, root_wide,
};
use crate::{Cap, CapSource, Caps, CoreTicks, CpuLayout, MemoryDetail, Sweep, VolumeReading, Why};

const _: () = assert!(
    size_of::<usize>() == 8,
    "the Windows arm decodes the 64-bit layouts"
);

type BOOL = i32;
type HANDLE = isize;
type NTSTATUS = i32;

/// `SYSTEM_INFORMATION_CLASS` values.
const SYSTEM_PROCESS_INFORMATION: i32 = 5;
const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: i32 = 8;
const SYSTEM_PAGEFILE_INFORMATION: i32 = 18;
/// `STATUS_INFO_LENGTH_MISMATCH`.
const STATUS_INFO_LENGTH_MISMATCH: NTSTATUS = 0xC000_0004_u32.cast_signed();
/// `MEMORY_RESOURCE_NOTIFICATION_TYPE::LowMemoryResourceNotification`.
const LOW_MEMORY_RESOURCE_NOTIFICATION: i32 = 0;
/// The largest process table read (64 MiB).
const PROCESS_TABLE_CAP: usize = 64 << 20;
/// The largest CPU Set table read (1 MiB: some 32,000 CPU Sets).
const CPU_SET_CAP: u32 = 1 << 20;

#[link(name = "kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    fn GetActiveProcessorGroupCount() -> u16;
    fn GetActiveProcessorCount(GroupNumber: u16) -> u32;
    fn GetSystemCpuSetInformation(
        Information: *mut c_void,
        BufferLength: u32,
        ReturnedLength: *mut u32,
        Process: HANDLE,
        Flags: u32,
    ) -> BOOL;
    fn GlobalMemoryStatusEx(lpBuffer: *mut MemoryStatusEx) -> BOOL;
    fn K32GetPerformanceInfo(
        pPerformanceInformation: *mut PerformanceInformation64,
        cb: u32,
    ) -> BOOL;
    fn CreateMemoryResourceNotification(NotificationType: i32) -> HANDLE;
    fn QueryMemoryResourceNotification(
        ResourceNotificationHandle: HANDLE,
        ResourceState: *mut BOOL,
    ) -> BOOL;
    fn GetDriveTypeW(lpRootPathName: *const u16) -> u32;
    fn GetDiskFreeSpaceExW(
        lpDirectoryName: *const u16,
        lpFreeBytesAvailableToCaller: *mut u64,
        lpTotalNumberOfBytes: *mut u64,
        lpTotalNumberOfFreeBytes: *mut u64,
    ) -> BOOL;
    fn GetVolumeInformationW(
        lpRootPathName: *const u16,
        lpVolumeNameBuffer: *mut u16,
        nVolumeNameSize: u32,
        lpVolumeSerialNumber: *mut u32,
        lpMaximumComponentLength: *mut u32,
        lpFileSystemFlags: *mut u32,
        lpFileSystemNameBuffer: *mut u16,
        nFileSystemNameSize: u32,
    ) -> BOOL;
}

#[link(name = "ntdll.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    fn NtQuerySystemInformation(
        SystemInformationClass: i32,
        SystemInformation: *mut c_void,
        SystemInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> NTSTATUS;
    fn NtQuerySystemInformationEx(
        SystemInformationClass: i32,
        InputBuffer: *const c_void,
        InputBufferLength: u32,
        SystemInformation: *mut c_void,
        SystemInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> NTSTATUS;
}

/// A `u64` buffer (8-byte aligned, as the NT information classes want) seen as its bytes.
fn as_bytes(words: &[u64]) -> &[u8] {
    // SAFETY: `u8` has alignment 1 and no invalid values, covering the slice's exact byte span.
    unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), size_of_val(words)) }
}

/// Each active processor group's width (`GetActiveProcessorCount`), in group order.
fn group_widths() -> Vec<u32> {
    // SAFETY: takes nothing, returns a count.
    let groups = unsafe { GetActiveProcessorGroupCount() };
    // SAFETY: a group number below the active count; returns a count.
    (0..groups)
        .map(|g| unsafe { GetActiveProcessorCount(g) })
        .collect()
}

/// The reader behind [`crate::Probe::core_ticks`] on Windows.
#[derive(Debug)]
pub(crate) struct Ticks {
    widths: Vec<u32>,
    buf: Vec<u64>,
}

impl Ticks {
    pub(crate) fn new() -> Self {
        Self {
            widths: group_widths(),
            buf: Vec::new(),
        }
    }

    /// One reading: each group's processors in turn, padded.
    pub(crate) fn sample(&mut self) -> Option<Vec<CoreTicks>> {
        let mut out = Vec::new();
        let single = self.widths.len() == 1;
        for (g, &width) in self.widths.iter().enumerate() {
            let width = usize::try_from(width).ok()?;
            let len = width.checked_mul(size_of::<ProcessorPerformance>())?;
            self.buf.clear();
            self.buf.resize(len.div_ceil(8).max(1), 0);
            let group = u16::try_from(g).ok()?;
            let cap = u32::try_from(self.buf.len() * 8).ok()?;
            let mut got = 0u32;
            // SAFETY: input is one readable u16; `buf` and `got` are writable.
            let mut status = unsafe {
                NtQuerySystemInformationEx(
                    SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION,
                    (&raw const group).cast(),
                    2,
                    self.buf.as_mut_ptr().cast(),
                    cap,
                    &raw mut got,
                )
            };
            if status < 0 && single {
                // SAFETY: as above, with no input buffer.
                status = unsafe {
                    NtQuerySystemInformation(
                        SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION,
                        self.buf.as_mut_ptr().cast(),
                        cap,
                        &raw mut got,
                    )
                };
            }
            if status < 0 {
                return None;
            }
            let n = usize::try_from(got).ok()?.min(len);
            let mut cores = processor_times(&as_bytes(&self.buf)[..n]);
            cores.resize(width, CoreTicks::default());
            out.extend(cores);
        }
        (!out.is_empty()).then_some(out)
    }
}

/// The CPU Set table's bytes.
fn cpu_set_bytes() -> Option<Vec<u8>> {
    let mut len = 0u32;
    // SAFETY: a null buffer of length zero asks only for the size.
    unsafe { GetSystemCpuSetInformation(std::ptr::null_mut(), 0, &raw mut len, 0, 0) };
    if len == 0 || len > CPU_SET_CAP {
        return None;
    }
    let mut buf = vec![0u64; usize::try_from(len).ok()?.div_ceil(8)];
    let mut got = 0u32;
    // SAFETY: `buf` holds at least `len` writable bytes, `got` is writable.
    let ok =
        unsafe { GetSystemCpuSetInformation(buf.as_mut_ptr().cast(), len, &raw mut got, 0, 0) };
    if ok == 0 {
        return None;
    }
    let n = usize::try_from(got.min(len)).ok()?;
    Some(as_bytes(&buf)[..n].to_vec())
}

/// The layout and the parked cores from the CPU Sets.
fn layout_and_parked() -> Option<(CpuLayout, Vec<u16>)> {
    layout_from_cpu_sets(&cpu_sets(&cpu_set_bytes()?), &group_widths())
}

/// [`crate::cpu_layout`] on Windows.
pub(crate) fn cpu_layout() -> CpuLayout {
    layout_and_parked().map_or_else(
        || {
            let total: u32 = group_widths().iter().sum();
            CpuLayout::single(u16::try_from(total).unwrap_or(u16::MAX))
        },
        |(layout, _)| layout,
    )
}

/// [`crate::withdrawn_cores`] on Windows.
pub(crate) fn withdrawn() -> Option<Vec<u16>> {
    layout_and_parked().map(|(_, parked)| parked)
}

/// The page files' `(in use, size)` in pages, `None` when unread.
fn page_file_pages() -> Option<(u64, u64)> {
    let mut len = 1024usize;
    while len <= 64 * 1024 {
        let mut buf = vec![0u64; len / 8];
        let mut got = 0u32;
        let cap = u32::try_from(len).ok()?;
        // SAFETY: `buf` is `cap` writable bytes, `got` a writable u32.
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_PAGEFILE_INFORMATION,
                buf.as_mut_ptr().cast(),
                cap,
                &raw mut got,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            len *= 4;
            continue;
        }
        if status < 0 {
            return None;
        }
        let n = usize::try_from(got).ok()?.min(len);
        return page_files(&as_bytes(&buf)[..n]);
    }
    None
}

/// Whether the low-memory resource notification is set.
fn low_memory() -> bool {
    static NOTE: OnceLock<HANDLE> = OnceLock::new();
    // SAFETY: takes an enum value, returns a handle or 0.
    let note = *NOTE.get_or_init(|| unsafe {
        CreateMemoryResourceNotification(LOW_MEMORY_RESOURCE_NOTIFICATION)
    });
    if note == 0 {
        return false;
    }
    let mut state: BOOL = 0;
    // SAFETY: `note` is a live notification handle (never closed) and `state` a writable BOOL.
    let ok = unsafe { QueryMemoryResourceNotification(note, &raw mut state) };
    ok != 0 && state != 0
}

/// One `K32GetPerformanceInfo`, or `None` where it fails.
fn performance_info() -> Option<PerformanceInformation64> {
    let cb = u32::try_from(size_of::<PerformanceInformation64>()).ok()?;
    let mut perf = PerformanceInformation64 {
        cb,
        ..PerformanceInformation64::default()
    };
    // SAFETY: `perf` is `cb` writable bytes of PERFORMANCE_INFORMATION.
    if unsafe { K32GetPerformanceInfo(&raw mut perf, cb) } == 0 {
        return None;
    }
    Some(perf)
}

/// [`crate::memory_detail`] on Windows.
pub(crate) fn memory_detail() -> Option<MemoryDetail> {
    let mut status = MemoryStatusEx {
        length: u32::try_from(size_of::<MemoryStatusEx>()).ok()?,
        ..MemoryStatusEx::default()
    };
    // SAFETY: `status` is a writable MEMORYSTATUSEX with its length set.
    if unsafe { GlobalMemoryStatusEx(&raw mut status) } == 0 {
        return None;
    }
    let perf = performance_info()?;
    memory_detail_of(&status, &perf, page_file_pages(), low_memory())
}

/// [`crate::pid_counts`] on Windows: the process count of one `K32GetPerformanceInfo`.
pub(crate) fn pid_counts() -> (Option<u64>, Option<u64>) {
    let all = performance_info()
        .map(|p| u64::from(p.process_count))
        .filter(|n| *n > 0);
    (all, None)
}

/// [`crate::processes`] on Windows.
pub(crate) fn processes() -> Option<Sweep> {
    static HINT: AtomicUsize = AtomicUsize::new(512 * 1024);
    let mut len = HINT.load(Ordering::Relaxed);
    for _ in 0..6 {
        if len > PROCESS_TABLE_CAP {
            return None;
        }
        let mut buf = vec![0u64; len.div_ceil(8)];
        let cap = u32::try_from(buf.len() * 8).ok()?;
        let mut got = 0u32;
        // SAFETY: `buf` is `cap` writable, 8-byte-aligned bytes; `got` a writable u32.
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_PROCESS_INFORMATION,
                buf.as_mut_ptr().cast(),
                cap,
                &raw mut got,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            len = usize::try_from(got)
                .unwrap_or(len)
                .max(len.saturating_mul(2))
                .saturating_add(64 * 1024);
            continue;
        }
        if status < 0 {
            return None;
        }
        HINT.store(len, Ordering::Relaxed);
        let n = usize::try_from(got).ok()?.min(buf.len() * 8);
        let base = u64::try_from(buf.as_ptr().addr()).ok()?;
        let at = Some(std::time::Instant::now());
        let mut sweep = crate::windows::processes(&as_bytes(&buf)[..n], base);
        for p in &mut sweep.procs {
            p.read_at = at;
        }
        return Some(sweep);
    }
    None
}

/// A NUL-terminated (or full) wide buffer as text.
fn wide_text(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// [`crate::volume`] on Windows.
pub(crate) fn volume(path: &Path) -> Option<VolumeReading> {
    let letter = drive_root(&path.to_string_lossy())?;
    let root = root_wide(letter);
    // SAFETY: `root` is a NUL-terminated wide string.
    if !drive_is_local(unsafe { GetDriveTypeW(root.as_ptr()) }) {
        return None;
    }
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: `root` is NUL-terminated; the three outputs are writable u64s.
    let ok = unsafe {
        GetDiskFreeSpaceExW(root.as_ptr(), &raw mut avail, &raw mut total, &raw mut free)
    };
    if ok == 0 {
        return None;
    }
    let mut fs = [0u16; 64];
    // SAFETY: `root` is NUL-terminated.
    let named = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            64,
        )
    };
    Some(crate::machine::volume_of(
        total,
        free,
        avail,
        1,
        PathBuf::from(format!("{letter}:\\")),
        if named == 0 {
            String::new()
        } else {
            wide_text(&fs)
        },
    ))
}

/// [`crate::caps`] on Windows.
pub(crate) fn caps() -> Caps {
    let read = |ok: bool| {
        if ok {
            Cap::Yes
        } else {
            Cap::No(Why::Unsupported)
        }
    };
    let layout = layout_and_parked();
    let memory = memory_detail();
    Caps {
        os: "windows",
        per_core: read(Ticks::new().sample().is_some()),
        core_kinds: read(layout.is_some()),
        cores_withdrawn: read(layout.is_some()),
        memory_classes: if memory.is_some() {
            Cap::Estimate
        } else {
            Cap::No(Why::Unsupported)
        },
        pressure: if memory.is_some() {
            Cap::Estimate
        } else {
            Cap::No(Why::Unsupported)
        },
        commit: read(memory.is_some_and(|m| m.commit.is_some())),
        compressor: Cap::No(Why::NotRead),
        swap: read(memory.is_some()),
        gpu_device: Cap::No(Why::NotRead),
        gpu_per_proc: Cap::No(Why::NotRead),
        proc_cpu_others: Cap::Yes,
        proc_memory_others: Cap::Yes,
        throttle: Cap::No(Why::Unsupported),
        volume: Cap::Yes,
        coalitions: Cap::No(Why::Unsupported),
        capacity: CapSource::Host,
    }
}
