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
use crate::{
    Cap, CapSource, Caps, CoreTicks, CpuLayout, GroupMemory, LinkReading, MachineWords,
    MemberUsage, MemoryDetail, Sweep, TcpSummary, VolumeReading, Why,
};

const _: () = assert!(
    size_of::<usize>() == 8,
    "the Windows arm decodes the 64-bit layouts"
);

type BOOL = i32;
type HANDLE = isize;
type NTSTATUS = i32;

const SYSTEM_PROCESS_INFORMATION: i32 = 5;
const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: i32 = 8;
const SYSTEM_PAGEFILE_INFORMATION: i32 = 18;
/// `PROCESSINFOCLASS::ProcessConsoleHostProcess`: the pid of the console host a process is
/// attached to, flags in its two low bits.
const PROCESS_CONSOLE_HOST_PROCESS: u32 = 49;
const STATUS_INFO_LENGTH_MISMATCH: NTSTATUS = 0xC000_0004_u32.cast_signed();
const LOW_MEMORY_RESOURCE_NOTIFICATION: i32 = 0;
const PROCESS_TABLE_CAP: usize = 64 << 20;
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
    fn ProcessIdToSessionId(dwProcessId: u32, pSessionId: *mut u32) -> BOOL;
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
    fn NtQueryInformationProcess(
        ProcessHandle: *mut c_void,
        ProcessInformationClass: u32,
        ProcessInformation: *mut c_void,
        ProcessInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> NTSTATUS;
}

pub(crate) fn as_bytes(words: &[u64]) -> &[u8] {
    // SAFETY: `u8` has alignment 1 and no invalid values; the slice covers exactly the words'
    // bytes, borrowed as long as they are.
    unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), size_of_val(words)) }
}

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

    /// One reading: each group's processors in turn, padded (or cut) to the
    /// group's width so the logical ids stay those [`cpu_layout`] uses.
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
            // SAFETY: input is one readable u16; `buf` is `cap` writable bytes and `got` a
            // writable u32.
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

fn memory_status() -> Option<MemoryStatusEx> {
    let mut status = MemoryStatusEx {
        length: u32::try_from(size_of::<MemoryStatusEx>()).ok()?,
        ..MemoryStatusEx::default()
    };
    // SAFETY: `status` is a writable MEMORYSTATUSEX with its length set.
    if unsafe { GlobalMemoryStatusEx(&raw mut status) } == 0 {
        return None;
    }
    Some(status)
}

/// [`crate::physical_memory_mib`] on Windows: `GlobalMemoryStatusEx`'s `ullTotalPhys`, the
/// memory Windows can use (what Task Manager calls the total), in MiB.
pub(crate) fn physical_memory_mib() -> Option<u64> {
    memory_status()
        .map(|s| s.total_phys >> 20)
        .filter(|mib| *mib > 0)
}

/// [`crate::swapped_kib`] on Windows: the page files' pages in use, in KiB.
pub(crate) fn swapped_kib() -> Option<u64> {
    let (used, _) = page_file_pages()?;
    Some(used.saturating_mul(performance_info()?.page_size) / 1024)
}

/// [`crate::memory_detail`] on Windows.
pub(crate) fn memory_detail() -> Option<MemoryDetail> {
    let status = memory_status()?;
    let perf = performance_info()?;
    memory_detail_of(&status, &perf, page_file_pages(), low_memory())
}

/// [`crate::room::task_room`] on Windows: the process and thread counts of one
/// `K32GetPerformanceInfo`; Windows states no limit for either, and no open-file count.
pub(crate) fn task_room() -> crate::room::TaskRoom {
    let perf = performance_info();
    let count = |n: Option<u32>| n.map(u64::from).filter(|n| *n > 0);
    crate::room::TaskRoom {
        procs: count(perf.map(|p| p.process_count)),
        threads: count(perf.map(|p| p.thread_count)),
        ..crate::room::TaskRoom::default()
    }
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
    // SAFETY: `root` is NUL-terminated; `fs` is 64 writable wide characters and every other
    // output is null (not asked for).
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

/// [`crate::own_session`] on Windows: this process's session id.
pub(crate) fn own_session() -> Option<u32> {
    let mut session = 0u32;
    // SAFETY: this process's own pid and a writable u32.
    let ok = unsafe { ProcessIdToSessionId(std::process::id(), &raw mut session) };
    (ok != 0).then_some(session)
}

/// The pid of the console host process `pid` is attached to (`NtQueryInformationProcess`,
/// `ProcessConsoleHostProcess`): the `conhost.exe` or `OpenConsole.exe` serving its console,
/// which every process started in that console shares. `None` for a process with no console,
/// one that has exited, and one this process may not open.
pub(crate) fn console_host(pid: u32) -> Option<u32> {
    use std::os::windows::io::AsRawHandle as _;
    let process =
        crate::win_paths::open_process(pid, crate::win_paths::PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut answer = 0u64;
    let mut got = 0u32;
    // SAFETY: `process` is a live handle opened with `PROCESS_QUERY_LIMITED_INFORMATION`, which
    // this class asks; `answer` is the one writable `ULONG_PTR` the class fills (8 bytes here).
    let status = unsafe {
        NtQueryInformationProcess(
            process.as_raw_handle(),
            PROCESS_CONSOLE_HOST_PROCESS,
            (&raw mut answer).cast(),
            8,
            &raw mut got,
        )
    };
    (status >= 0)
        .then(|| crate::windows::console_host_pid(answer))
        .flatten()
}

/// [`console_host`] of a sweep's row, for [`crate::windows::tree_adopting`]. The pid is
/// read after the sweep: one handed to another process since answers for that process, which
/// is attached to one of the tree's consoles only if it was started in the tree as well.
fn console_of(p: &crate::ProcessReading) -> Option<u32> {
    console_host(p.pid)
}

/// [`crate::group_memory`] on Windows: one sweep, then `pid`'s tree and the orphans in its
/// consoles.
pub(crate) fn group_memory(pid: u32) -> Option<GroupMemory> {
    crate::windows::tree_memory(&processes()?.procs, pid, console_of)
}

/// [`crate::group_usage`] on Windows: one sweep, then `root`'s tree and the orphans in its
/// consoles.
pub(crate) fn group_usage(root: u32) -> Option<Vec<MemberUsage>> {
    crate::windows::tree_usage(&processes()?.procs, root, console_of)
}

#[link(name = "iphlpapi.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    fn GetIfTable2(Table: *mut *mut c_void) -> u32;
    fn FreeMibTable(Memory: *mut c_void);
    fn GetTcpStatisticsEx(Statistics: *mut c_void, Family: u32) -> u32;
    fn GetTcpTable2(TcpTable: *mut c_void, SizePointer: *mut u32, Order: BOOL) -> u32;
    fn GetTcp6Table2(TcpTable: *mut c_void, SizePointer: *mut u32, Order: BOOL) -> u32;
}

/// The most rows of a `MIB_IF_TABLE2` that are read: far past any machine's (a laptop with a
/// VPN and a hypervisor lists about a hundred, filter layers included).
const IF_TABLE_ROWS_CAP: usize = 16 * 1024;
const AF_INET: u32 = 2;
const AF_INET6: u32 = 23;

/// A `MIB_IF_TABLE2` the IP Helper allocated, freed on drop.
struct IfTable(*mut c_void);

impl Drop for IfTable {
    fn drop(&mut self) {
        // SAFETY: the pointer `GetIfTable2` returned, freed once.
        unsafe { FreeMibTable(self.0) };
    }
}

/// [`crate::net_links_into`] on Windows: one `GetIfTable2`, every interface's row with its
/// counters and link speeds, decoded by [`crate::win_net::if_table2`].
pub(crate) fn net_links() -> Option<Vec<LinkReading>> {
    use crate::win_net::{IF_ROW2_SIZE, IF_TABLE2_ROWS, if_table2, if_table2_entries};
    let mut raw: *mut c_void = std::ptr::null_mut();
    // SAFETY: `raw` is a writable pointer the call fills with a table it allocates.
    if unsafe { GetIfTable2(&raw mut raw) } != 0 || raw.is_null() {
        return None;
    }
    let table = IfTable(raw);
    // SAFETY: a returned table starts with its `ULONG NumEntries`, readable as 4 bytes.
    let head = unsafe { std::slice::from_raw_parts(table.0.cast::<u8>(), 4) };
    let entries = if_table2_entries(head)?.min(IF_TABLE_ROWS_CAP);
    let len = IF_TABLE2_ROWS + entries * IF_ROW2_SIZE;
    // SAFETY: the table holds `NumEntries` rows of `IF_ROW2_SIZE` bytes from offset
    // `IF_TABLE2_ROWS` (`MIB_IF_TABLE2`, 64-bit), so `len` bytes are readable; `u8` has
    // alignment 1; the borrow ends before `table` drops.
    let bytes = unsafe { std::slice::from_raw_parts(table.0.cast::<u8>(), len) };
    if_table2(bytes)
}

fn tcp_established(family: u32) -> Option<u32> {
    let mut stats = [0u32; crate::win_net::TCPSTATS_SIZE / 4];
    // SAFETY: `stats` is a writable `MIB_TCPSTATS` (fifteen DWORDs).
    if unsafe { GetTcpStatisticsEx(stats.as_mut_ptr().cast(), family) } != 0 {
        return None;
    }
    let bytes: Vec<u8> = stats.iter().flat_map(|w| w.to_le_bytes()).collect();
    crate::win_net::tcp_established(&bytes)
}

/// `ERROR_INSUFFICIENT_BUFFER`: the table needs the size the call wrote back.
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
/// The most bytes of one family's connection table that are read: over half a million IPv4
/// connections, far past what a machine holds open.
const TCP_TABLE_CAP: usize = 16 << 20;

/// One family's connection table (`GetTcpTable2`, or `GetTcp6Table2` for `v6`) as its sockets
/// by state ([`crate::win_net::tcp_table_counts`]). Any process may read it. `None` when the
/// call fails (a family that is off) or the table keeps outgrowing the buffer.
fn tcp_table(v6: bool) -> Option<crate::TcpCounts> {
    use crate::win_net::{
        TCP_ROW2_SIZE, TCP_ROW2_STATE, TCP6_ROW2_SIZE, TCP6_ROW2_STATE, tcp_table_counts,
    };
    let mut buf = vec![0u64; 4096];
    for _ in 0..4 {
        let mut size = u32::try_from(buf.len() * 8).ok()?;
        // SAFETY: `buf` is `size` writable bytes, aligned past the table's `DWORD`s; `size` is
        // a writable u32 the call sets to the bytes it needs. `Order` 0 asks for no sorting.
        let status = unsafe {
            if v6 {
                GetTcp6Table2(buf.as_mut_ptr().cast(), &raw mut size, 0)
            } else {
                GetTcpTable2(buf.as_mut_ptr().cast(), &raw mut size, 0)
            }
        };
        match status {
            0 if v6 => return tcp_table_counts(as_bytes(&buf), TCP6_ROW2_SIZE, TCP6_ROW2_STATE),
            0 => return tcp_table_counts(as_bytes(&buf), TCP_ROW2_SIZE, TCP_ROW2_STATE),
            ERROR_INSUFFICIENT_BUFFER => {
                let need = usize::try_from(size).ok()?;
                if need > TCP_TABLE_CAP {
                    return None;
                }
                // Room for the connections that open before the next call.
                buf = vec![0u64; need.div_ceil(8) + 1024];
            }
            _ => return None,
        }
    }
    None
}

/// [`crate::tcp_summary_into`] on Windows: both families' connection tables, every state
/// counted; where neither reads, `GetTcpStatisticsEx`'s established count alone.
pub(crate) fn tcp_summary() -> Option<TcpSummary> {
    crate::win_net::tcp_summary_of_tables(tcp_table(false), tcp_table(true)).or_else(|| {
        crate::win_net::tcp_summary_of(tcp_established(AF_INET), tcp_established(AF_INET6))
    })
}

#[link(name = "advapi32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    fn RegGetValueW(
        hkey: HANDLE,
        lpSubKey: *const u16,
        lpValue: *const u16,
        dwFlags: u32,
        pdwType: *mut u32,
        pvData: *mut c_void,
        pcbData: *mut u32,
    ) -> i32;
}

/// `HKEY_LOCAL_MACHINE`: `(LONG)0x80000002`, sign-extended to a handle.
pub(crate) const HKEY_LOCAL_MACHINE: HANDLE = -2_147_483_646;
/// `RRF_RT_REG_SZ`: a string value, refused when the value is of another type.
const RRF_RT_REG_SZ: u32 = 0x0000_0002;
/// `RRF_RT_REG_DWORD`: a 32-bit value, refused when the value is of another type.
const RRF_RT_REG_DWORD: u32 = 0x0000_0010;
/// `RRF_RT_REG_QWORD`: a 64-bit value, refused when the value is of another type.
const RRF_RT_REG_QWORD: u32 = 0x0000_0040;
/// Where Windows keeps its copy of the firmware's product fields (SMBIOS type 1).
const BIOS_KEY: &str = r"HARDWARE\DESCRIPTION\System\BIOS";
/// The first processor's description: every processor of a machine carries the same brand.
const CPU_KEY: &str = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";
/// The running Windows' name, build and update revision.
const VERSION_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

pub(crate) fn wide_z(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// One string value of the `HKEY_LOCAL_MACHINE` key `key`, at most 255 UTF-16 units; `None`
/// where it is missing, longer, or not a string.
pub(crate) fn reg_string(key: &str, name: &str) -> Option<String> {
    let key = wide_z(key);
    let value = wide_z(name);
    let mut buf = [0u16; 256];
    let mut bytes = u32::try_from(size_of_val(&buf)).ok()?;
    // SAFETY: `key` and `value` are NUL-terminated UTF-16 strings that outlive the call; the
    // predefined key needs no open or close; `buf` is writable for `bytes` bytes, which the call
    // writes at most (the terminator included) and then sets to the bytes written.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &raw mut bytes,
        )
    };
    if status != 0 {
        return None;
    }
    let units = &buf[..(bytes as usize / 2).min(buf.len())];
    let len = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16(&units[..len]).ok()
}

/// One 32-bit value of the `HKEY_LOCAL_MACHINE` key `key`; `None` where it is missing or of
/// another type.
pub(crate) fn reg_dword(key: &str, name: &str) -> Option<u32> {
    let key = wide_z(key);
    let value = wide_z(name);
    let mut out = 0u32;
    let mut bytes = 4u32;
    // SAFETY: as `reg_string`; `out` is writable for the 4 bytes a DWORD takes.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut out).cast(),
            &raw mut bytes,
        )
    };
    (status == 0 && bytes == 4).then_some(out)
}

/// One 64-bit value of the `HKEY_LOCAL_MACHINE` key `key`; `None` where it is missing or of
/// another type.
pub(crate) fn reg_qword(key: &str, name: &str) -> Option<u64> {
    let key = wide_z(key);
    let value = wide_z(name);
    let mut out = 0u64;
    let mut bytes = 8u32;
    // SAFETY: as `reg_string`; `out` is writable for the 8 bytes a QWORD takes.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_QWORD,
            std::ptr::null_mut(),
            (&raw mut out).cast(),
            &raw mut bytes,
        )
    };
    (status == 0 && bytes == 8).then_some(out)
}

/// [`crate::machine_type::read`] on Windows: the firmware's product fields as the registry keeps
/// them, through the same ladder as Linux's DMI table (never a WMI query or a child process).
pub(crate) fn machine_type() -> crate::machine_type::MachineType {
    let vendor = reg_string(BIOS_KEY, "SystemManufacturer");
    let name = reg_string(BIOS_KEY, "SystemProductName");
    let version = reg_string(BIOS_KEY, "SystemVersion");
    let family = reg_string(BIOS_KEY, "SystemFamily");
    crate::machine_type::dmi_type(&crate::machine_type::Dmi {
        vendor: vendor.as_deref(),
        name: name.as_deref(),
        version: version.as_deref(),
        family: family.as_deref(),
    })
}

/// [`crate::machine_model`] on Windows: the firmware's product name as the registry keeps it.
pub(crate) fn machine_model() -> Option<String> {
    reg_string(BIOS_KEY, "SystemProductName")
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// [`crate::os_build`] on Windows: the build and its update revision (`26200.7171`).
pub(crate) fn os_build() -> Option<String> {
    crate::windows::os_build(
        reg_string(VERSION_KEY, "CurrentBuildNumber").as_deref(),
        reg_dword(VERSION_KEY, "UBR"),
    )
}

/// [`crate::machine_words`] on Windows: the processor's brand string and the OS words, both
/// from the registry (never a WMI query or a child process).
pub(crate) fn machine_words() -> MachineWords {
    MachineWords {
        cpu: reg_string(CPU_KEY, "ProcessorNameString")
            .map(|brand| brand.trim().to_owned())
            .filter(|brand| !brand.is_empty()),
        os: crate::windows::os_words(
            reg_string(VERSION_KEY, "ProductName").as_deref(),
            reg_string(VERSION_KEY, "CurrentBuildNumber").as_deref(),
        ),
    }
}

/// [`crate::caps`] on Windows: each row is what its reader answered just now.
pub(crate) fn caps() -> Caps {
    let read = |ok: bool| {
        if ok {
            Cap::Yes
        } else {
            Cap::No(Why::Unsupported)
        }
    };
    let estimate = |ok: bool| {
        if ok {
            Cap::Estimate
        } else {
            Cap::No(Why::Unsupported)
        }
    };
    let layout = layout_and_parked();
    let memory = memory_detail();
    let words = machine_words();
    let swept = processes();
    let sweep = swept.is_some();
    let tcp = tcp_summary();
    // The store's working set, as a whole reading names it (`windows::with_compression_store`);
    // a Windows with compression off (Server's default) has no store to read.
    let compressor = match (memory, swept.as_ref()) {
        (Some(detail), Some(s))
            if crate::windows::with_compression_store(detail, &s.procs)
                .compressed
                .is_some() =>
        {
            Cap::Estimate
        }
        (Some(_), Some(_)) => Cap::No(Why::NotEnabled("memory_compression")),
        _ => Cap::No(Why::Unsupported),
    };
    let tree = swept.is_some_and(|s| {
        crate::windows::tree_memory(&s.procs, std::process::id(), console_of).is_some()
    });
    Caps {
        os: "windows",
        per_core: read(Ticks::new().sample().is_some()),
        core_kinds: read(layout.is_some()),
        cores_withdrawn: read(layout.is_some()),
        memory_classes: estimate(memory.is_some()),
        pressure: estimate(memory.as_ref().is_some_and(|m| m.pressure.is_some())),
        commit: read(memory.as_ref().is_some_and(|m| m.commit.is_some())),
        compressor,
        swap: read(memory.is_some()),
        gpu_device: read(crate::win_pdh::gpu().is_some()),
        gpu_per_proc: read(crate::win_pdh::gpu_clients().is_some()),
        proc_cpu_others: read(sweep),
        proc_memory_others: read(sweep),
        throttle: crate::win_pdh::throttle().map_or_else(Cap::No, |_| Cap::Estimate),
        volume: Cap::Yes,
        coalitions: Cap::No(Why::Unsupported),
        more: vec![
            (
                "physical_memory",
                read(physical_memory_mib().is_some()),
                "the machine's physical memory",
            ),
            (
                "paging",
                estimate(swapped_kib().is_some()),
                "paging to swap (Windows: the page files' growth, not pages moved)",
            ),
            (
                "proc_owner",
                read(sweep && own_session().is_some()),
                "whose each process is (Windows: its session; session 0 is services)",
            ),
            (
                "proc_io",
                read(sweep),
                "each process's bytes read and written (Windows: any handle's, so no disk figure)",
            ),
            (
                "proc_state",
                estimate(sweep),
                "running, waiting and suspended processes (Windows: from each thread's state)",
            ),
            (
                "tree_memory",
                read(tree),
                "the memory of a process and its descendants (Windows: orphans by their console)",
            ),
            (
                "machine_words",
                read(words.cpu.is_some() && words.os.is_some()),
                "the CPU's brand and the OS's name and build",
            ),
            (
                "net_links",
                read(net_links().is_some()),
                "each network interface's counters and link speed",
            ),
            (
                "tcp_established",
                read(tcp.is_some()),
                "established TCP connections, machine-wide",
            ),
            (
                "tcp_states",
                read(tcp.is_some_and(|s| !s.counters_only)),
                "TCP sockets by state (Windows: from the connection tables; no round-trip time)",
            ),
            (
                "proc_sockets",
                Cap::No(Why::NotRead),
                "open sockets per process",
            ),
            (
                "device_io",
                Cap::No(Why::NotRead),
                "each storage device's throughput",
            ),
            (
                "load",
                Cap::No(Why::Unsupported),
                "the load average and the run queue",
            ),
        ],
        capacity: CapSource::Host,
    }
}
