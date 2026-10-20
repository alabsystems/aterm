// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MACHINE NOW — the readers the system-awareness stats call, once a
//! second on their probe thread while a window shows them (design
//! `docs/DESIGN-system-awareness-2026-10-16.md` §4.3, §5.2, §9):
//!
//! * [`Probe::core_ticks`] — every logical core's cumulative ticks;
//! * [`cpu_layout`] — the cores grouped into the clusters the OS names;
//! * [`memory_detail`] — memory by the OS's own page classes, swap, pressure;
//! * [`gpu`] — the GPU's device-wide use;
//! * [`volume`] — the size and free space of the volume a path lives on.
//!
//! Each answers once, when asked: no thread, no timer, no subprocess, no
//! unbounded read. Nothing here derives a rate; the caller takes deltas.
//!
//! # Cost
//!
//! Measured on the owner's Apple M5 (4 Super + 6 Efficiency cores, 32 GiB,
//! macOS 27) on 2026-10-19 at load average 30–32 (every core busy), release
//! build, by `examples/live.rs`: the median of 2,000 back-to-back calls each,
//! quoted in each reader's doc. No reader allocates beyond what it returns,
//! except the kernel's and the IORegistry's own: `host_processor_info` maps
//! its answer into this task (released before [`Probe::core_ticks`]
//! returns) and [`gpu`] copies one CoreFoundation dictionary per call.

use std::path::{Path, PathBuf};

use crate::MemoryLevel;

/// One logical core's cumulative ticks since boot (100 Hz on macOS and on
/// Linux's `USER_HZ`). The caller takes the deltas of two readings.
///
/// macOS keeps each count in 32 bits, so a core's count of one state wraps
/// after 2³² ticks (about 497 days of that state at 100 Hz): a delta that
/// goes backwards across a wrap reads as nothing for one reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreTicks {
    /// Running user code at normal or raised priority.
    pub user: u64,
    /// Running the kernel (Linux: `system + irq + softirq`).
    pub system: u64,
    /// Idle (Linux: `idle + iowait`, since a core waiting on a disk is free
    /// to run anything else).
    pub idle: u64,
    /// Running user code at lowered priority.
    pub nice: u64,
}

impl CoreTicks {
    /// Busy ticks: `user + system + nice`, saturating.
    #[must_use]
    pub const fn busy(&self) -> u64 {
        self.user
            .saturating_add(self.system)
            .saturating_add(self.nice)
    }

    /// Every tick: [`busy`](Self::busy) plus `idle`, saturating.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.busy().saturating_add(self.idle)
    }
}

/// The per-core sampler. It holds only what the next
/// [`core_ticks`](Self::core_ticks) reuses (on Linux the `/proc/stat` buffer
/// and the present core count; nothing on macOS): no thread, no timer, no
/// syscall until the first read. `Send`, so the host builds it anywhere and
/// moves it to its probe thread.
#[derive(Debug)]
pub struct Probe {
    #[cfg(target_os = "linux")]
    stat: crate::linux::StatReader,
    _private: (),
}

impl Default for Probe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe {
    /// A sampler that has read nothing. On Linux it sizes its buffer
    /// ([`crate::linux::STAT_CAP`]) and reads how many cores are present.
    #[must_use]
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            stat: crate::linux::StatReader::new(Path::new("/"), linux_present().len()),
            _private: (),
        }
    }

    /// Every logical core's cumulative ticks, in logical-id order (index `i`
    /// is logical core `i`, the ids [`cpu_layout`] groups); `None` where
    /// unreadable.
    ///
    /// * **macOS**: one `host_processor_info(PROCESSOR_CPU_LOAD_INFO)` and the
    ///   `vm_deallocate` of the array the kernel mapped into this task for
    ///   it. The array's index is the processor number, which is the device
    ///   tree's `logical-cpu-id` (see [`cpu_layout`]). 6.7 µs median on the
    ///   M5 (p90 8.3 µs).
    /// * **Linux**: one read of `/proc/stat`, at most
    ///   [`crate::linux::STAT_CAP`] bytes into a buffer the probe keeps
    ///   ([`crate::linux::proc_stat_cores`]). An offline core reads all zero.
    /// * Elsewhere: `None`.
    pub fn core_ticks(&mut self) -> Option<Vec<CoreTicks>> {
        #[cfg(target_os = "macos")]
        {
            crate::macos::core_ticks()
        }
        #[cfg(target_os = "linux")]
        {
            self.stat.sample()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            None
        }
    }
}

/// One CPU cluster as the OS names it ("Super", "Performance",
/// "Efficiency", or whatever `hw.perflevelN.name` says on this Mac), with
/// the logical core ids in it, ascending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cluster {
    /// The OS's own name for the cluster; `"CPU"` where it names none.
    pub name: String,
    /// The logical ids of its cores, ascending (indexes into
    /// [`Probe::core_ticks`]).
    pub logical: Vec<u16>,
}

/// Every logical id in exactly one cluster, clusters ordered fastest first;
/// one cluster named `"CPU"` holding every core where the OS names none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpuLayout {
    /// The clusters, fastest first.
    pub clusters: Vec<Cluster>,
}

impl CpuLayout {
    /// One cluster named `"CPU"` holding logical ids `0..cores`.
    #[must_use]
    pub fn single(cores: u16) -> Self {
        Self {
            clusters: vec![Cluster {
                name: "CPU".to_owned(),
                logical: (0..cores).collect(),
            }],
        }
    }

    /// How many logical cores the layout holds.
    #[must_use]
    pub fn logical_cores(&self) -> usize {
        self.clusters.iter().map(|c| c.logical.len()).sum()
    }
}

/// The machine's cores grouped into the clusters the OS names, fastest
/// first. Topology: read it once, not every tick.
///
/// * **macOS**: the clusters are `hw.perflevelN` (`N = 0` fastest), named by
///   `hw.perflevelN.name` and sized by `hw.perflevelN.logicalcpu`. Which
///   logical ids are in which is READ from the IORegistry's device tree:
///   each `IODeviceTree:/cpus/cpuN` node carries its `logical-cpu-id`, its
///   `l2-cache-size` (matched to `hw.perflevelN.l2cachesize`) and its
///   `cluster-type` (`P` or `E`, used where the L2 sizes do not tell two
///   levels apart). The map is used only when it places every logical id
///   exactly once and fills every level to its `logicalcpu`; otherwise the
///   convention Apple silicon has shipped with is used, the slowest level
///   holding the lowest ids.
///
///   VERIFIED on the owner's M5 (`Mac17,4`, 2026-10-19), the device tree read
///   and the convention agreeing: `cpu0`–`cpu5` carry `cluster-type E` and a
///   6 MiB L2 (perflevel1 "Efficiency"), `cpu6`–`cpu9` `cluster-type P` and
///   16 MiB (perflevel0 "Super"), each `logical-cpu-id` its node number; the
///   layout is Super {6–9}, Efficiency {0–5}. The kernel's CPU numbers agree:
///   ten background-QoS spinners, which the scheduler keeps on the efficiency
///   cores, saw only CPUs 0–5 through `pthread_cpu_number_np` while
///   utility and user-interactive ones saw all ten (`examples/live.rs
///   --qos-check`). xnu indexes `host_processor_info`'s array by that same
///   CPU number. 187 µs median: eight sysctls and three registry reads per
///   device-tree node, so read it once, not every tick.
/// * **Linux**: one cluster `"CPU"` holding the present cores
///   (`/sys/devices/system/cpu/present`).
/// * Elsewhere: one cluster `"CPU"` of `available_parallelism` cores.
#[must_use]
pub fn cpu_layout() -> CpuLayout {
    #[cfg(target_os = "macos")]
    {
        crate::macos::cpu_layout()
    }
    #[cfg(target_os = "linux")]
    {
        let present = linux_present();
        CpuLayout {
            clusters: vec![Cluster {
                name: "CPU".to_owned(),
                logical: present,
            }],
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        CpuLayout::single(parallelism())
    }
}

/// `available_parallelism` as a core count, `0` where unknown.
#[cfg(not(target_os = "macos"))]
fn parallelism() -> u16 {
    std::thread::available_parallelism().map_or(0, |n| u16::try_from(n.get()).unwrap_or(u16::MAX))
}

/// The present cores' logical ids (Linux `/sys/devices/system/cpu/present`),
/// else `0..available_parallelism`.
#[cfg(target_os = "linux")]
fn linux_present() -> Vec<u16> {
    crate::with_bounded(Path::new("/sys/devices/system/cpu/present"), |t| {
        crate::linux::cpulist(t)
    })
    .flatten()
    .unwrap_or_else(|| (0..parallelism()).collect())
}

/// One performance level as macOS names it (`hw.perflevelN.*`).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Level {
    /// `hw.perflevelN.name`.
    pub name: String,
    /// `hw.perflevelN.logicalcpu`.
    pub logical: u16,
    /// `hw.perflevelN.l2cachesize`, bytes.
    pub l2: Option<u64>,
}

/// One device-tree CPU node (`IODeviceTree:/cpus/cpuN`).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DtCpu {
    /// `logical-cpu-id`.
    pub id: u16,
    /// `cluster-type`'s letter (`b'P'`, `b'E'`).
    pub kind: Option<u8>,
    /// `l2-cache-size`, bytes.
    pub l2: Option<u64>,
}

/// Which level a device-tree node belongs to: the one level whose L2 size
/// is the node's, else, between exactly two levels, `P` the faster and `E`
/// the slower; one level holds every node.
#[cfg(any(target_os = "macos", test))]
fn level_of(levels: &[Level], cpu: &DtCpu) -> Option<usize> {
    if let Some(l2) = cpu.l2 {
        let mut hits = levels.iter().enumerate().filter(|(_, l)| l.l2 == Some(l2));
        if let (Some((i, _)), None) = (hits.next(), hits.next()) {
            return Some(i);
        }
    }
    match (levels.len(), cpu.kind) {
        (1, _) => Some(0),
        (2, Some(b'P')) => Some(0),
        (2, Some(b'E')) => Some(1),
        _ => None,
    }
}

/// The device tree's assignment of logical ids to levels, each level's ids
/// ascending: `None` unless every id in `0..logical` is placed exactly once
/// and each level holds exactly its `logical` count. PURE.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn dt_assignment(
    levels: &[Level],
    cpus: &[DtCpu],
    logical: u16,
) -> Option<Vec<Vec<u16>>> {
    if levels.is_empty() || cpus.len() != usize::from(logical) {
        return None;
    }
    let mut groups = vec![Vec::new(); levels.len()];
    let mut seen = vec![false; usize::from(logical)];
    for cpu in cpus {
        let slot = seen.get_mut(usize::from(cpu.id))?;
        if *slot {
            return None;
        }
        *slot = true;
        groups.get_mut(level_of(levels, cpu)?)?.push(cpu.id);
    }
    for (ids, level) in groups.iter_mut().zip(levels) {
        if ids.len() != usize::from(level.logical) {
            return None;
        }
        ids.sort_unstable();
    }
    Some(groups)
}

/// The convention: contiguous ids, the slowest level first (lowest ids), the
/// fastest last. `None` unless the levels' counts sum to `logical`. PURE.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn conventional_assignment(levels: &[Level], logical: u16) -> Option<Vec<Vec<u16>>> {
    let sum: u32 = levels.iter().map(|l| u32::from(l.logical)).sum();
    if levels.is_empty() || sum != u32::from(logical) {
        return None;
    }
    let mut groups = vec![Vec::new(); levels.len()];
    let mut next = 0u16;
    for (ids, level) in groups.iter_mut().zip(levels).rev() {
        let end = next.checked_add(level.logical)?;
        *ids = (next..end).collect();
        next = end;
    }
    Some(groups)
}

/// The layout from the levels, the device tree's nodes and the logical core
/// count: the device tree's assignment where it holds
/// ([`dt_assignment`]), else the convention ([`conventional_assignment`]),
/// else one `"CPU"` cluster. A level with no cores is left out. PURE.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn layout_of(levels: &[Level], cpus: &[DtCpu], logical: u16) -> CpuLayout {
    let groups =
        dt_assignment(levels, cpus, logical).or_else(|| conventional_assignment(levels, logical));
    let Some(groups) = groups else {
        return CpuLayout::single(logical);
    };
    CpuLayout {
        clusters: levels
            .iter()
            .zip(groups)
            .filter(|(_, ids)| !ids.is_empty())
            .map(|(level, logical)| Cluster {
                name: level.name.clone(),
                logical,
            })
            .collect(),
    }
}

/// The machine's memory by the OS's own page classes, bytes.
///
/// On macOS the classes come from ONE `host_statistics64(HOST_VM_INFO64)`
/// read (pages × `vm.pagesize`), so they are consistent with each other, and
/// `wired + active + inactive + speculative + free + compressed` is physical
/// memory less what the VM never manages (firmware and early-kernel carve-outs:
/// 0.82 GiB of 32 on the M5). `app + file_backed = active + inactive +
/// speculative` (the pageable queues). On Linux see
/// [`crate::linux::memory_detail`] for what each field holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryDetail {
    /// Physical memory (`hw.memsize`; Linux `MemTotal`).
    pub total: u64,
    /// Truly free: `free_count − speculative_count`. The kernel's
    /// `free_count` holds the speculative pages, which are given apart in
    /// [`speculative`](Self::speculative) (as `vm_stat`'s "Pages free" is).
    pub free: u64,
    /// Speculative read-ahead pages (`speculative_count`): free to take.
    pub speculative: u64,
    /// The active queue (`active_count`).
    pub active: u64,
    /// The inactive queue (`inactive_count`).
    pub inactive: u64,
    /// Wired: never paged out or compressed (`wire_count`).
    pub wired: u64,
    /// Physical memory holding compressed data (`compressor_page_count`).
    pub compressed: u64,
    /// The uncompressed size of what the compressor holds
    /// (`total_uncompressed_pages_in_compressor`): NOT physical memory, and
    /// it can exceed [`total`](Self::total).
    pub stored: u64,
    /// Purgeable pages (`purgeable_count`).
    pub purgeable: u64,
    /// File-backed pages (`external_page_count`).
    pub file_backed: u64,
    /// Anonymous memory of processes: `internal_page_count −
    /// purgeable_count` (Activity Monitor's "App Memory"), zero where
    /// purgeable exceeds internal.
    pub app: u64,
    /// Swap in use (`vm.swapusage` `xsu_used`; Linux `SwapTotal − SwapFree`).
    pub swap_used: u64,
    /// Swap allocated (`vm.swapusage` `xsu_total`; Linux `SwapTotal`). macOS
    /// grows and shrinks its swap files on demand, so this moves.
    pub swap_total: u64,
    /// The kernel's pressure level ([`crate::memory_pressure`]).
    pub pressure: Option<MemoryLevel>,
}

/// The `HOST_VM_INFO64` page counts [`memory_detail_of`] reads.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct VmPages {
    pub free: u64,
    pub speculative: u64,
    pub active: u64,
    pub inactive: u64,
    pub wired: u64,
    pub compressor: u64,
    pub stored: u64,
    pub purgeable: u64,
    pub external: u64,
    pub internal: u64,
}

/// [`MemoryDetail`] from one read's page counts, the page size, physical
/// memory, swap `(used, total)` and the pressure level. Every product
/// saturates; `free` and `app` are differences that stop at zero. PURE.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn memory_detail_of(
    p: &VmPages,
    page_bytes: u64,
    total: u64,
    swap: Option<(u64, u64)>,
    pressure: Option<MemoryLevel>,
) -> MemoryDetail {
    let b = |pages: u64| pages.saturating_mul(page_bytes);
    let (swap_used, swap_total) = swap.unwrap_or((0, 0));
    MemoryDetail {
        total,
        free: b(p.free.saturating_sub(p.speculative)),
        speculative: b(p.speculative),
        active: b(p.active),
        inactive: b(p.inactive),
        wired: b(p.wired),
        compressed: b(p.compressor),
        stored: b(p.stored),
        purgeable: b(p.purgeable),
        file_backed: b(p.external),
        app: b(p.internal.saturating_sub(p.purgeable)),
        swap_used,
        swap_total,
        pressure,
    }
}

/// The machine's memory by page class, swap and pressure; `None` where
/// unreadable.
///
/// * **macOS**: one `host_statistics64(HOST_VM_INFO64)`, one
///   `vm.swapusage` and one `kern.memorystatus_vm_pressure_level`
///   (`hw.memsize` and `vm.pagesize` are read once per process). 3.3 µs
///   median on the M5.
/// * **Linux**: one bounded read of `/proc/meminfo`
///   ([`crate::linux::memory_detail`]); no pressure level.
/// * Elsewhere: `None`.
#[must_use]
pub fn memory_detail() -> Option<MemoryDetail> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::memory_detail()
    }
    #[cfg(target_os = "linux")]
    {
        crate::with_bounded(Path::new("/proc/meminfo"), crate::linux::memory_detail).flatten()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// The GPU, device-wide (Apple GPUs report device-wide use only, in whole
/// percent, refreshed on the driver's own schedule).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuReading {
    /// `PerformanceStatistics` "Device Utilization %", at most 100.
    pub device_pct: Option<u8>,
    /// "Renderer Utilization %", at most 100.
    pub renderer_pct: Option<u8>,
    /// "Tiler Utilization %", at most 100.
    pub tiler_pct: Option<u8>,
    /// "In use system memory", bytes.
    pub in_use_bytes: Option<u64>,
    /// `gpu-core-count` on the accelerator entry, where present.
    pub cores: Option<u16>,
    /// The accelerator's `model` (`"Apple M5"`), else its IOKit class name.
    pub name: Option<String>,
}

/// A driver's utilization figure as a percentage: negative is unknown, past
/// 100 is 100. PURE.
#[cfg(any(target_os = "macos", test))]
#[must_use]
pub(crate) fn percent_of(value: i64) -> Option<u8> {
    u8::try_from(value.clamp(-1, 100)).ok()
}

/// The GPU's device-wide use now; `None` where there is no reader or no
/// accelerator.
///
/// * **macOS**: the first `IOAccelerator` service, matched once per process
///   and cached with its name and core count; each call copies its
///   `PerformanceStatistics` property (one IORegistry read, no user client
///   opened). 26 µs median on the M5. A Mac with two GPUs reads the first
///   one matched.
/// * Elsewhere: `None`.
#[must_use]
pub fn gpu() -> Option<GpuReading> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::gpu()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The volume a path lives on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeReading {
    /// Its size in bytes.
    pub total: u64,
    /// Bytes free (`f_bfree`), at most `total`.
    pub free: u64,
    /// Bytes an unprivileged writer can use now (`f_bavail`), at most `free`.
    /// On APFS every volume of one container reports the container's space;
    /// purgeable space is not counted.
    pub available: u64,
    /// Where it is mounted.
    pub mount: PathBuf,
    /// Its file system type (`apfs`, `ext4`).
    pub fs: String,
}

/// [`VolumeReading`] from block counts and the block size: `free` held to at
/// most `total` and `available` to at most `free`, every product
/// saturating. PURE.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
#[must_use]
pub(crate) fn volume_of(
    blocks: u64,
    bfree: u64,
    bavail: u64,
    block_bytes: u64,
    mount: PathBuf,
    fs: String,
) -> VolumeReading {
    let total = blocks.saturating_mul(block_bytes);
    let free = bfree.saturating_mul(block_bytes).min(total);
    let available = bavail.saturating_mul(block_bytes).min(free);
    VolumeReading {
        total,
        free,
        available,
        mount,
        fs,
    }
}

/// `path` as a C string in `buf` (no heap): `None` when it holds a NUL or
/// does not fit with its terminator.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn c_path<'a>(path: &Path, buf: &'a mut [u8]) -> Option<&'a std::ffi::CStr> {
    use std::os::unix::ffi::OsStrExt as _;
    let bytes = path.as_os_str().as_bytes();
    if bytes.contains(&0) || bytes.len() >= buf.len() {
        return None;
    }
    buf[..bytes.len()].copy_from_slice(bytes);
    buf[bytes.len()] = 0;
    std::ffi::CStr::from_bytes_with_nul(&buf[..=bytes.len()]).ok()
}

/// The volume `path` lives on, read now; `None` where it cannot be read.
///
/// * **macOS**: one `statfs` of `path` itself, which gives the mount point
///   and type with the figures. It ASKS THE FILE SYSTEM holding `path`: a
///   path on a stalled network mount blocks the call, so pass a path the
///   caller knows is local (`/`, the home folder), or place a path that may
///   not be through [`crate::volumes::mount_of`] and
///   [`crate::volumes::read`], which never ask a remote mount. 1.4 µs
///   median on the M5 for `/`.
/// * **Linux**: the mount table ([`crate::volumes::table`]) names the mount
///   and its type; a mount it does not call local, or one the automounter
///   owns, is `None` without being asked; else one `statvfs` of `path`.
/// * Elsewhere: `None`.
#[must_use]
pub fn volume(path: &Path) -> Option<VolumeReading> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::volume(path)
    }
    #[cfg(target_os = "linux")]
    {
        linux_volume(path)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = path;
        None
    }
}

#[cfg(target_os = "linux")]
#[cfg_attr(
    target_pointer_width = "64",
    expect(
        clippy::useless_conversion,
        reason = "statvfs's c_ulong and fsblkcnt_t fields are u64 on LP64 and u32 on 32-bit"
    )
)]
fn linux_volume(path: &Path) -> Option<VolumeReading> {
    let table = crate::volumes::table();
    let m = crate::volumes::mount_of(path, &table)?;
    if !m.local || m.automounted {
        return None;
    }
    let mut buf = [0u8; crate::PATH_CAP];
    let c = c_path(path, &mut buf)?;
    // SAFETY: `statvfs` is integers (its padding private): all-zero is a value.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is NUL-terminated and `st` is a writable `statvfs`.
    if unsafe { libc::statvfs(c.as_ptr(), &raw mut st) } != 0 {
        return None;
    }
    Some(volume_of(
        u64::from(st.f_blocks),
        u64::from(st.f_bfree),
        u64::from(st.f_bavail),
        u64::from(st.f_frsize),
        m.on.clone(),
        m.fstype.clone(),
    ))
}
