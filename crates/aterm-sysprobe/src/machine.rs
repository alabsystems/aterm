// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Machine-wide point-in-time system probes (CPU, memory, GPU, storage, network, and limits).

use std::path::{Path, PathBuf};

use crate::{CapSource, Caps, MemoryLevel, Why};

/// Cumulative ticks for one logical core since boot, in platform units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreTicks {
    /// Running user code at normal or raised priority.
    pub user: u64,
    /// Kernel time (Linux: `system + irq + softirq`; Windows: kernel minus idle).
    pub system: u64,
    /// Idle time (Linux: `idle + iowait`).
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

/// Per-core CPU and thermal throttle sampler holding reusable platform buffers.
#[derive(Debug)]
pub struct Probe {
    #[cfg(target_os = "linux")]
    stat: crate::linux::StatReader,
    #[cfg(target_os = "linux")]
    throttle: crate::linux::ThrottleReader,
    #[cfg(all(windows, target_pointer_width = "64"))]
    win: crate::win::Ticks,
    _private: (),
}

impl Default for Probe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe {
    /// Create a sampler (initializes Linux core-count/scratch state and Windows processor-group
    /// widths; empty on macOS).
    #[must_use]
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            stat: crate::linux::StatReader::new(
                Path::new("/"),
                crate::linux::Root::system().present_or_parallelism().len(),
            ),
            #[cfg(target_os = "linux")]
            throttle: crate::linux::ThrottleReader::new(crate::linux::Root::system()),
            #[cfg(all(windows, target_pointer_width = "64"))]
            win: crate::win::Ticks::new(),
            _private: (),
        }
    }

    /// Cumulative ticks for each logical core in logical-id order, or `None` if unreadable.
    pub fn core_ticks(&mut self) -> Option<Vec<CoreTicks>> {
        #[cfg(target_os = "macos")]
        {
            crate::macos::core_ticks()
        }
        #[cfg(target_os = "linux")]
        {
            self.stat.sample()
        }
        #[cfg(all(windows, target_pointer_width = "64"))]
        {
            self.win.sample()
        }
        #[cfg(not(any(
            target_os = "macos",
            target_os = "linux",
            all(windows, target_pointer_width = "64")
        )))]
        {
            None
        }
    }

    /// Current thermal throttle level, or [`Why`] it is unavailable.
    ///
    /// # Errors
    /// Returns [`Why`] when the platform or machine provides no reading (always on macOS).
    pub fn throttle(&mut self) -> Result<Throttle, Why> {
        #[cfg(target_os = "linux")]
        {
            self.throttle.sample()
        }
        #[cfg(target_os = "macos")]
        {
            Err(Why::NotRead)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(Why::Unsupported)
        }
    }
}

/// Ordered CPU thermal throttle severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ThrottleLevel {
    /// No thermal throttling active.
    Nominal,
    /// Light thermal throttling.
    Light,
    /// Heavy thermal throttling.
    Heavy,
    /// Severe thermal throttling.
    Severe,
}

impl ThrottleLevel {
    /// The wire word: `none`, `light`, `heavy`, `severe`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            ThrottleLevel::Nominal => "none",
            ThrottleLevel::Light => "light",
            ThrottleLevel::Heavy => "heavy",
            ThrottleLevel::Severe => "severe",
        }
    }
}

/// A throttle reading ([`Probe::throttle`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Throttle {
    /// The level.
    pub level: ThrottleLevel,
    /// True for an OS-reported level; false for a heuristic from temperatures/counters.
    pub exact: bool,
}

/// One OS-named CPU performance cluster and its ascending logical core IDs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cluster {
    /// The OS's own name for the cluster; `"CPU"` where it names none.
    pub name: String,
    /// Ascending logical core IDs (indices into [`Probe::core_ticks`]).
    pub logical: Vec<u16>,
}

/// CPU clusters ordered fastest first, or a single `"CPU"` cluster when unnamed.
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

/// Logical cores grouped into OS performance clusters, fastest first.
#[must_use]
pub fn cpu_layout() -> CpuLayout {
    #[cfg(target_os = "macos")]
    {
        crate::macos::cpu_layout()
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux::Root::system().cpu_layout()
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        crate::win::cpu_layout()
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        all(windows, target_pointer_width = "64")
    )))]
    {
        CpuLayout::single(parallelism())
    }
}

/// Logical IDs of cores currently offline or parked by the OS, or `None` if unread.
#[must_use]
pub fn withdrawn_cores() -> Option<Vec<u16>> {
    #[cfg(target_os = "linux")]
    {
        crate::linux::Root::system().withdrawn()
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        crate::win::withdrawn()
    }
    #[cfg(not(any(target_os = "linux", all(windows, target_pointer_width = "64"))))]
    {
        None
    }
}

/// `available_parallelism` as a core count, `0` where unknown.
pub(crate) fn parallelism() -> u16 {
    std::thread::available_parallelism().map_or(0, |n| u16::try_from(n.get()).unwrap_or(u16::MAX))
}

/// Names for `n` ranked performance levels, fastest first.
#[must_use]
pub fn level_names(n: usize) -> Vec<String> {
    match n {
        0 => Vec::new(),
        1 => vec!["CPU".to_owned()],
        _ => (0..n)
            .map(|i| {
                if i == 0 {
                    "Performance".to_owned()
                } else if i + 1 == n {
                    "Efficiency".to_owned()
                } else if n == 3 {
                    "Mid".to_owned()
                } else {
                    format!("Mid {i}")
                }
            })
            .collect(),
    }
}

/// Group `(logical_id, rank)` pairs into clusters ordered by descending rank.
#[must_use]
pub fn layout_by_rank(ranked: &[(u16, u32)]) -> Option<CpuLayout> {
    let mut ranks: Vec<u32> = ranked.iter().map(|(_, r)| *r).collect();
    ranks.sort_unstable_by(|a, b| b.cmp(a));
    ranks.dedup();
    if ranks.is_empty() {
        return None;
    }
    let names = level_names(ranks.len());
    let clusters = ranks
        .iter()
        .zip(names)
        .map(|(rank, name)| {
            let mut logical: Vec<u16> = ranked
                .iter()
                .filter(|(_, r)| r == rank)
                .map(|(id, _)| *id)
                .collect();
            logical.sort_unstable();
            logical.dedup();
            Cluster { name, logical }
        })
        .collect();
    Some(CpuLayout { clusters })
}

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

/// Promised virtual memory charge against the system commit limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Commit {
    /// Bytes charged (Windows `CommitTotal`; Linux `Committed_AS`).
    pub charge: u64,
    /// Commit limit in bytes (RAM plus swap/page files).
    pub limit: u64,
    /// True when the kernel enforces the limit (Windows); false when advisory (Linux).
    pub exact: bool,
}

impl Commit {
    /// Charge in permille of `limit`, or `None` when `limit == 0`.
    #[must_use]
    pub fn permille(&self) -> Option<u64> {
        (self.limit > 0).then(|| {
            u64::try_from(u128::from(self.charge) * 1000 / u128::from(self.limit))
                .unwrap_or(u64::MAX)
        })
    }
}

/// Machine memory breakdown by OS page class, swap, commit, and pressure in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryDetail {
    /// Total physical memory in bytes.
    pub total: u64,
    /// Immediately free memory in bytes (`free - speculative` on macOS, `MemFree` on Linux).
    pub free: u64,
    /// Speculative read-ahead pages in bytes (macOS only).
    pub speculative: Option<u64>,
    /// The active queue (`active_count`; Linux `Active`).
    pub active: Option<u64>,
    /// The inactive queue (`inactive_count`; Linux `Inactive`).
    pub inactive: Option<u64>,
    /// Non-pageable kernel/wired memory in bytes.
    pub wired: u64,
    /// Physical bytes used by the memory compressor or zswap pool.
    pub compressed: Option<u64>,
    /// Uncompressed bytes held in the compressor or zswap.
    pub stored: Option<u64>,
    /// Purgeable pages (`purgeable_count`). macOS only.
    pub purgeable: Option<u64>,
    /// Reclaimable file-backed page cache in bytes.
    pub file_backed: u64,
    /// Anonymous process memory in bytes (`internal - purgeable` on macOS, `AnonPages` on Linux).
    pub app: u64,
    /// Shared memory (`Shmem`) in bytes (Linux only).
    pub shared: Option<u64>,
    /// Swap or pagefile bytes in use.
    pub swap_used: u64,
    /// Total allocated swap or pagefile bytes.
    pub swap_total: u64,
    /// Commit charge and limit (Linux and Windows).
    pub commit: Option<Commit>,
    /// Kernel memory pressure level.
    pub pressure: Option<MemoryLevel>,
    /// True when the reported classes partition managed physical memory (macOS).
    pub exact: bool,
    /// Cumulative VM event counters from the same sample, if available.
    pub counters: Option<VmCounters>,
    /// Kernel-reported available memory percentage (`kern.memorystatus_level` on macOS).
    pub kernel_available_pct: Option<u8>,
}

/// Cumulative VM paging and compressor counters since boot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VmCounters {
    /// Bytes swapped in (`swapins` × page size).
    pub swapin_bytes: u64,
    /// Bytes swapped out (`swapouts` × page size).
    pub swapout_bytes: u64,
    /// Pages the compressor decompressed (`decompressions`).
    pub decompressions: u64,
    /// Pages the compressor compressed (`compressions`).
    pub compressions: u64,
    /// Pages read in from files (`pageins`).
    pub pageins: u64,
}

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
    pub swapins: u64,
    pub swapouts: u64,
    pub decompressions: u64,
    pub compressions: u64,
    pub pageins: u64,
}

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
        speculative: Some(b(p.speculative)),
        active: Some(b(p.active)),
        inactive: Some(b(p.inactive)),
        wired: b(p.wired),
        compressed: Some(b(p.compressor)),
        stored: Some(b(p.stored)),
        purgeable: Some(b(p.purgeable)),
        file_backed: b(p.external),
        app: b(p.internal.saturating_sub(p.purgeable)),
        shared: None,
        swap_used,
        swap_total,
        commit: None,
        pressure,
        exact: true,
        counters: Some(VmCounters {
            swapin_bytes: b(p.swapins),
            swapout_bytes: b(p.swapouts),
            decompressions: p.decompressions,
            compressions: p.compressions,
            pageins: p.pageins,
        }),
        kernel_available_pct: None,
    }
}

/// CPU brand and OS version strings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MachineWords {
    /// CPU brand string.
    pub cpu: Option<String>,
    /// The OS and its version (`macOS 27.0.1`; Linux `PRETTY_NAME`).
    pub os: Option<String>,
}

/// The machine's brand and OS words; empty fields where unreadable.
#[must_use]
pub fn machine_words() -> MachineWords {
    #[cfg(target_os = "macos")]
    {
        crate::macos::machine_words()
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux::Root::system().machine_words()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        MachineWords::default()
    }
}

/// Snapshot machine memory classes, swap, commit, and pressure, or `None` if unreadable.
#[must_use]
pub fn memory_detail() -> Option<MemoryDetail> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::memory_detail()
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux::Root::system().memory_detail()
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        crate::win::memory_detail()
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        all(windows, target_pointer_width = "64")
    )))]
    {
        None
    }
}

/// Device-wide GPU utilization, memory, and identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuReading {
    /// Overall device utilization percentage (`0..=100`), or `None` (see [`Self::unread`]).
    pub device_pct: Option<u8>,
    /// "Renderer Utilization %", at most 100.
    pub renderer_pct: Option<u8>,
    /// "Tiler Utilization %", at most 100.
    pub tiler_pct: Option<u8>,
    /// "In use system memory", bytes.
    pub in_use_bytes: Option<u64>,
    /// Driver-allocated system memory in bytes.
    pub alloc_bytes: Option<u64>,
    /// Cumulative GPU fault recoveries since driver start.
    pub recoveries: Option<u64>,
    /// `gpu-core-count` on the accelerator entry, where present.
    pub cores: Option<u16>,
    /// Accelerator model/class name (macOS) or DRM driver name (Linux).
    pub name: Option<String>,
    /// Reason [`Self::device_pct`] is `None` (such as [`Why::Asleep`] or [`Why::Unsupported`]).
    pub unread: Option<Why>,
}

#[cfg(any(target_os = "macos", test))]
#[must_use]
pub(crate) fn percent_of(value: i64) -> Option<u8> {
    u8::try_from(value.clamp(-1, 100)).ok()
}

/// Read device-wide GPU utilization and memory, or `None` if absent or unsupported.
#[must_use]
pub fn gpu() -> Option<GpuReading> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::gpu()
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux::Root::system().gpu()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// The GPU driver's identity: the accelerator entry's `CFBundleIdentifier` and `IOSourceVersion`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuDriver {
    /// The driver kext's bundle identifier (such as `com.apple.AGXG17G`), where the entry gives it.
    pub bundle: Option<String>,
    /// The driver's source version (such as `360.34.5`), where the entry gives it.
    pub version: Option<String>,
}

/// Read the GPU driver's identity afresh, or `None` where no accelerator entry is found (always off macOS).
#[must_use]
pub fn gpu_driver() -> Option<GpuDriver> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::gpu_driver()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// One GPU user client: a process's connection to the accelerator, with the GPU time its queues
/// ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuClient {
    /// The client's registry entry id, unique for the boot.
    pub entry: u64,
    /// The process that opened it (`IOUserClientCreator`).
    pub pid: u32,
    /// That process's short name as the creator string gives it (`p_comm`, 16 bytes at most).
    pub name: String,
    /// GPU time its queues ran, summed, ns (`AppUsage[].accumulatedGPUTime`): cumulative while the
    /// client lives.
    pub gpu_ns: u64,
}

/// Every GPU user client the accelerator holds, read together ([`gpu_clients`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuClients {
    /// The clients whose creator and `AppUsage` parsed.
    pub rows: Vec<GpuClient>,
    /// Every child whose registry entry id was read, including children whose properties did
    /// not parse as a client. A later counter read from one of these is no newly born client.
    pub entries: Vec<u64>,
    /// The accelerator's children walked.
    pub children: u32,
    /// Whether the walk stopped at [`MAX_GPU_CLIENTS`].
    pub truncated: bool,
    /// Whether every child's entry id was read: not truncated, no identity read failed, and no
    /// client came or went under the iterator.
    pub complete: bool,
}

impl GpuClients {
    /// Keep a walked child's identity even when its properties do not yield a counter row.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn record(
        &mut self,
        entry: u64,
        creator: Option<&str>,
        usage: Option<&[Option<u64>]>,
    ) {
        self.entries.push(entry);
        if let Some(client) = client_of(entry, creator, usage) {
            self.rows.push(client);
        }
    }
}

/// The most accelerator children one [`gpu_clients`] read walks.
pub const MAX_GPU_CLIENTS: usize = 4096;

/// An `IOUserClientCreator` string, `"pid N, name"`, as `(pid, name)`; `None` for pid 0 (the
/// kernel) or any other shape. The name is everything after the first `", "`, commas included.
#[must_use]
pub fn creator_of(text: &str) -> Option<(u32, &str)> {
    let (pid, name) = text.strip_prefix("pid ")?.split_once(", ")?;
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let pid = pid.parse::<u32>().ok().filter(|p| *p > 0)?;
    Some((pid, name))
}

/// One client from its registry properties: `creator` its `IOUserClientCreator`, `usage` its
/// `AppUsage` array's `accumulatedGPUTime` per queue (`None` for a queue without one), or `None`
/// where `AppUsage` is absent or no array. A queue without the key adds nothing; the sum saturates.
#[cfg(any(target_os = "macos", test))]
#[must_use]
pub(crate) fn client_of(
    entry: u64,
    creator: Option<&str>,
    usage: Option<&[Option<u64>]>,
) -> Option<GpuClient> {
    let (pid, name) = creator_of(creator?)?;
    let gpu_ns = usage?
        .iter()
        .flatten()
        .fold(0u64, |sum, ns| sum.saturating_add(*ns));
    Some(GpuClient {
        entry,
        pid,
        name: name.to_owned(),
        gpu_ns,
    })
}

/// Read every GPU user client's creator and cumulative GPU time, or `None` where no accelerator
/// answers (every OS but macOS). Registry reads only: no client is opened and no GPU device made.
#[must_use]
pub fn gpu_clients() -> Option<GpuClients> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::gpu_clients()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Capacity and mount metadata for a filesystem volume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeReading {
    /// Its size in bytes.
    pub total: u64,
    /// Bytes free (`f_bfree`), at most `total`.
    pub free: u64,
    /// Bytes available to an unprivileged writer (`f_bavail`), capped at `free`.
    pub available: u64,
    /// Where it is mounted.
    pub mount: PathBuf,
    /// Its file system type (`apfs`, `ext4`).
    pub fs: String,
}

#[cfg(any(
    target_os = "macos",
    target_os = "linux",
    all(windows, target_pointer_width = "64"),
    test
))]
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

/// Read size and free space for the volume holding `path`, or `None` if unreadable or non-local.
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
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        crate::win::volume(path)
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        all(windows, target_pointer_width = "64")
    )))]
    {
        let _ = path;
        None
    }
}

/// Effective CPU and memory limits for this process (Linux cgroup v2 or host).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capacity {
    /// CPU quota in millicores (`cpu.max`), or `None` if unconstrained below host cores.
    pub cpu_millicores: Option<u32>,
    /// Memory limit in bytes (`memory.max`), or `None` if unconstrained below host RAM.
    pub mem_bytes: Option<u64>,
    /// Where the figures come from.
    pub source: CapSource,
}

/// Read effective cgroup v2 CPU and memory limits for this process.
#[must_use]
pub fn capacity() -> Capacity {
    #[cfg(target_os = "linux")]
    {
        crate::linux::Root::system().capacity()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Capacity::default()
    }
}

/// Probe which system metrics the running machine and kernel expose.
#[must_use]
pub fn caps() -> Caps {
    #[cfg(target_os = "macos")]
    {
        crate::macos::caps()
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux::Root::system().caps()
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        crate::win::caps()
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        all(windows, target_pointer_width = "64")
    )))]
    {
        Caps::unsupported("other")
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

/// Network interface classification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// Wired Ethernet or USB interface.
    Wired,
    /// Wi-Fi wireless interface.
    Wifi,
    /// Cellular modem interface.
    Cellular,
    /// Other or unclassified interface.
    #[default]
    Other,
}

impl LinkKind {
    /// Canonical lowercase name (`"wired"`, `"wifi"`, `"cellular"`, `"other"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wired => "wired",
            Self::Wifi => "wifi",
            Self::Cellular => "cellular",
            Self::Other => "other",
        }
    }
}

/// Cumulative single-direction interface counters since interface creation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkCounters {
    /// Bytes.
    pub bytes: u64,
    /// Packets.
    pub packets: u64,
    /// Errors.
    pub errors: u64,
    /// Dropped packets (`ifi_iqdrops` / unsigned `ifm_snd_drops` on macOS).
    pub drops: u64,
}

/// Point-in-time state and cumulative traffic counters for one network interface.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkReading {
    /// Its name (`en0`, `wlan0`).
    pub name: String,
    /// Its interface index.
    pub index: u32,
    /// What it is ([`LinkKind`]).
    pub kind: LinkKind,
    /// Administratively up with an active link.
    pub up: bool,
    /// A loopback interface (`IFF_LOOPBACK`).
    pub loopback: bool,
    /// Whether backed by a physical device.
    pub physical: Option<bool>,
    /// Driver-reported link rate in bits per second, if known.
    pub rate_bps: Option<u64>,
    /// Received.
    pub rx: LinkCounters,
    /// Sent.
    pub tx: LinkCounters,
}

/// Maximum number of interfaces returned by [`net_links`].
pub const MAX_LINKS: usize = 1024;

const NOT_MACHINE_LINKS: &[&str] = &[
    "lo", "utun", "ipsec", "ppp", "gif", "stf", "bridge", "awdl", "llw", "nan", "ap", "anpi",
    "vmenet",
];

#[must_use]
pub(crate) fn never_the_machines(name: &str) -> bool {
    NOT_MACHINE_LINKS
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// True when `link` is an active physical non-loopback machine interface.
#[must_use]
pub fn counts_toward_machine(link: &LinkReading) -> bool {
    if !link.up || link.loopback || never_the_machines(&link.name) {
        return false;
    }
    match link.physical {
        Some(physical) => physical,
        None => link.name.starts_with("en") || link.name.starts_with("pdp_ip"),
    }
}

#[must_use]
pub(crate) fn kind_by_name(name: &str) -> LinkKind {
    let any = |prefixes: &[&str]| prefixes.iter().any(|p| name.starts_with(p));
    if any(&["pdp_ip", "wwan", "rmnet"]) {
        LinkKind::Cellular
    } else if any(&["wl"]) {
        LinkKind::Wifi
    } else if any(&["en", "eth"]) {
        LinkKind::Wired
    } else {
        LinkKind::Other
    }
}

/// Read up to [`MAX_LINKS`] network interfaces using a temporary scratch buffer.
#[must_use]
pub fn net_links() -> Option<Vec<LinkReading>> {
    net_links_into(&mut Vec::new())
}

/// Read up to [`MAX_LINKS`] network interfaces using caller-provided `scratch` storage.
#[must_use]
pub fn net_links_into(scratch: &mut Vec<u8>) -> Option<Vec<LinkReading>> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::net_links_into(scratch)
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux::links_under_into(Path::new("/"), scratch)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = scratch;
        None
    }
}

/// Point-in-time TCP socket counts across the machine's protocol table, grouped by state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TcpCounts {
    /// `ESTABLISHED` connections.
    pub established: u32,
    /// `SYN_SENT` connections.
    pub syn_sent: u32,
    /// `SYN_RECEIVED` connections.
    pub syn_recv: u32,
    /// `FIN_WAIT_1` connections.
    pub fin_wait1: u32,
    /// `FIN_WAIT_2` connections.
    pub fin_wait2: u32,
    /// `TIME_WAIT` connections.
    pub time_wait: u32,
    /// `CLOSED` sockets in the table.
    pub close: u32,
    /// `CLOSE_WAIT` connections.
    pub close_wait: u32,
    /// `LAST_ACK` connections.
    pub last_ack: u32,
    /// `LISTEN` sockets.
    pub listen: u32,
    /// `CLOSING` connections.
    pub closing: u32,
}

impl TcpCounts {
    /// Total TCP sockets counted across all states.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.established
            .saturating_add(self.syn_sent)
            .saturating_add(self.syn_recv)
            .saturating_add(self.fin_wait1)
            .saturating_add(self.fin_wait2)
            .saturating_add(self.time_wait)
            .saturating_add(self.close)
            .saturating_add(self.close_wait)
            .saturating_add(self.last_ack)
            .saturating_add(self.listen)
            .saturating_add(self.closing)
    }

    /// Increment the counter for `state`.
    pub const fn record(&mut self, state: crate::jobs::TcpState) {
        use crate::jobs::TcpState;
        match state {
            TcpState::Established => self.established = self.established.saturating_add(1),
            TcpState::SynSent => self.syn_sent = self.syn_sent.saturating_add(1),
            TcpState::SynReceived => self.syn_recv = self.syn_recv.saturating_add(1),
            TcpState::FinWait1 => self.fin_wait1 = self.fin_wait1.saturating_add(1),
            TcpState::FinWait2 => self.fin_wait2 = self.fin_wait2.saturating_add(1),
            TcpState::TimeWait => self.time_wait = self.time_wait.saturating_add(1),
            TcpState::Closed => self.close = self.close.saturating_add(1),
            TcpState::CloseWait => self.close_wait = self.close_wait.saturating_add(1),
            TcpState::LastAck => self.last_ack = self.last_ack.saturating_add(1),
            TcpState::Listen => self.listen = self.listen.saturating_add(1),
            TcpState::Closing => self.closing = self.closing.saturating_add(1),
            TcpState::Other => {}
        }
    }
}

/// Machine-wide TCP state counts together with passive smoothed RTT across established remote peers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TcpSummary {
    /// TCP socket counts grouped by state.
    pub counts: TcpCounts,
    /// Median kernel smoothed RTT (`t_srtt`, milliseconds) across established non-loopback connections, if any.
    pub srtt_ms: Option<f32>,
}

/// Read machine-wide TCP connection counts using a temporary scratch buffer.
#[must_use]
pub fn tcp_counts() -> Option<TcpCounts> {
    tcp_counts_into(&mut Vec::new())
}

/// Read machine-wide TCP connection counts using caller-provided `scratch` storage.
#[must_use]
pub fn tcp_counts_into(scratch: &mut Vec<u8>) -> Option<TcpCounts> {
    tcp_summary_into(scratch).map(|s| s.counts)
}

/// Read machine-wide TCP state counts and passive smoothed RTT using a temporary scratch buffer.
#[must_use]
pub fn tcp_summary() -> Option<TcpSummary> {
    tcp_summary_into(&mut Vec::new())
}

/// Read machine-wide TCP state counts and passive smoothed RTT using caller-provided `scratch` storage.
#[must_use]
pub fn tcp_summary_into(scratch: &mut Vec<u8>) -> Option<TcpSummary> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::tcp_summary_into(scratch)
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux::tcp_counts_under_into(Path::new("/"), scratch).map(|counts| TcpSummary {
            counts,
            srtt_ms: crate::linux::tcp_srtt_ms(),
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = scratch;
        None
    }
}

/// Count open socket file descriptors held by `pid` without taking descriptor I/O references (`PROC_PIDLISTFDS` on macOS, `/proc/<pid>/fd` symlinks on Linux), or `None` when unreadable.
#[must_use]
pub fn open_socket_count(pid: u32) -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::open_socket_count(pid)
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux::Root::system().open_socket_count(pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

/// System-wide and per-user process/task table ceilings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessLimits {
    /// Maximum processes/tasks system-wide.
    pub system: Option<u64>,
    /// Maximum processes/tasks for one user.
    pub per_user: Option<u64>,
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
#[must_use]
pub(crate) fn smaller(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (one, None) | (None, one) => one,
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
#[must_use]
pub(crate) fn rlimit_ceiling(soft: u64, infinity: u64) -> Option<u64> {
    (soft != infinity).then_some(soft)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[cfg_attr(
    target_pointer_width = "64",
    expect(
        clippy::useless_conversion,
        reason = "rlim_t is u64 on LP64 and may be u32 on 32-bit"
    )
)]
pub(crate) fn nproc_rlimit() -> Option<u64> {
    let mut r = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `r` is a writable `rlimit`.
    if unsafe { libc::getrlimit(libc::RLIMIT_NPROC, &raw mut r) } != 0 {
        return None;
    }
    rlimit_ceiling(u64::from(r.rlim_cur), u64::from(libc::RLIM_INFINITY))
}

/// Read system-wide and per-user process limits.
#[must_use]
pub fn process_limits() -> ProcessLimits {
    #[cfg(target_os = "macos")]
    {
        crate::macos::process_limits()
    }
    #[cfg(target_os = "linux")]
    {
        let read = |path: &str| {
            crate::with_bounded(Path::new(path), |t| t.trim().parse::<u64>().ok()).flatten()
        };
        ProcessLimits {
            system: smaller(
                read("/proc/sys/kernel/pid_max"),
                read("/proc/sys/kernel/threads-max"),
            ),
            per_user: nproc_rlimit(),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        ProcessLimits::default()
    }
}

/// Kernel workqueue thread-pool ceilings per process (`workqueue_limits`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkqueueLimits {
    /// Maximum constrained (non-overcommit) workqueue threads per process.
    pub constrained: Option<u64>,
    /// Maximum total workqueue threads per process (`kern.wq_max_threads`).
    pub total: Option<u64>,
}

/// Read kernel workqueue thread limits (`sysctl` on macOS, `None` elsewhere).
#[must_use]
pub fn workqueue_limits() -> WorkqueueLimits {
    #[cfg(target_os = "macos")]
    {
        crate::macos::workqueue_limits()
    }
    #[cfg(not(target_os = "macos"))]
    {
        WorkqueueLimits::default()
    }
}

/// Kernel 1-, 5-, and 15-minute load averages via `getloadavg`, or `None` if unavailable.
#[must_use]
pub fn load_average() -> Option<[f64; 3]> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        unsafe extern "C" {
            fn getloadavg(loadavg: *mut f64, nelem: libc::c_int) -> libc::c_int;
        }
        let mut load = [0f64; 3];
        // SAFETY: `load` is a writable 3-element `f64` array.
        let n = unsafe { getloadavg(load.as_mut_ptr(), 3) };
        (n == 3 && load.iter().all(|l| l.is_finite() && *l >= 0.0)).then_some(load)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Count total processes/tasks on the machine and processes owned by real user `uid`.
#[must_use]
pub fn pid_counts(uid: u32) -> (Option<u64>, Option<u64>) {
    #[cfg(target_os = "macos")]
    {
        crate::macos::pid_counts(uid)
    }
    #[cfg(target_os = "linux")]
    {
        let _ = uid;
        (crate::linux::Root::system().tasks(), None)
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        let _ = uid;
        crate::win::pid_counts()
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        all(windows, target_pointer_width = "64")
    )))]
    {
        let _ = uid;
        (None, None)
    }
}

#[cfg(any(target_os = "macos", test))]
pub(crate) const MAX_PIDS: usize = 1 << 17;

#[cfg(any(target_os = "macos", test))]
#[must_use]
pub(crate) fn pid_room(estimate: i64) -> Option<usize> {
    let pids = usize::try_from(estimate).ok().filter(|n| *n > 0)?;
    Some(pids.saturating_add(pids / 4).min(MAX_PIDS))
}

#[cfg(any(target_os = "macos", test))]
#[must_use]
pub(crate) fn listed_count(listed: i64, room: usize) -> Option<u64> {
    let n = usize::try_from(listed)
        .ok()
        .filter(|n| *n > 0 && *n < room)?;
    u64::try_from(n).ok()
}

#[cfg(any(target_os = "macos", test))]
#[must_use]
pub(crate) fn owned_count(listed: i64, errno: i32, room: usize) -> Option<u64> {
    if listed == 0 && errno == 0 {
        Some(0)
    } else {
        listed_count(listed, room)
    }
}
