// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MACHINE NOW — the readers the system-awareness stats call, once a
//! second on their probe thread while a window shows them (design
//! `docs/DESIGN-system-awareness-2026-10-16.md` §4.3, §5.2, §5.4, §9, §16):
//!
//! * [`Probe::core_ticks`] — every logical core's cumulative ticks;
//! * [`Probe::throttle`] — the throttle level (`none` … `severe`);
//! * [`cpu_layout`] — the cores grouped into the clusters the OS names;
//! * [`memory_detail`] — memory by the OS's own classes, swap, commit,
//!   pressure;
//! * [`gpu`] — the GPU's device-wide use;
//! * [`volume`] — the size and free space of the volume a path lives on;
//! * [`net_links`] — every network interface's kind, state, link rate and
//!   cumulative counters ([`net_links_into`] reads into a buffer the caller
//!   keeps), with [`counts_toward_machine`] naming the links whose traffic
//!   is the machine's own;
//! * [`process_limits`] — how many processes the machine, and one user, may
//!   run;
//! * [`pid_counts`] — how many processes the machine, and one user, run;
//! * [`workqueue_limits`] — how many pool threads the kernel gives one
//!   process;
//! * [`load_average`] — the kernel's 1, 5 and 15 minute load averages;
//! * [`capacity`] — what this process may use (a Linux cgroup's limits);
//! * [`caps`] — what of all this the running machine can say.
//!
//! Each answers once, when asked: no thread, no timer, no subprocess, no
//! unbounded read. Nothing here derives a rate; the caller takes deltas.
//! What an OS cannot give is `None` (with a [`Why`] where the caller needs
//! one), never a zero.
//!
//! The macOS arm is the private `macos` module; the Linux arm is
//! [`crate::linux`]'s readers under a root directory (`/` here, a fixture
//! tree in tests); the Windows arm is the private `win` module over
//! [`crate::windows`]'s pure decoders.
//!
//! # Cost
//!
//! Measured on the owner's Apple M5 (4 Super + 6 Efficiency cores, 32 GiB,
//! macOS 27) on 2026-10-19 at load average 30–32 (every core busy), release
//! build, by `examples/live.rs`: the median of 2,000 back-to-back calls each,
//! quoted in each reader's doc. No reader allocates beyond what it returns,
//! except the kernel's and the IORegistry's own and two buffers:
//! `host_processor_info` maps its answer into this task (released before
//! [`Probe::core_ticks`] returns) and [`gpu`] copies one CoreFoundation
//! dictionary per call; [`pid_counts`] lists the pids into a buffer it
//! frees before it returns (macOS: room for the machine's processes and a
//! quarter more, 4 bytes a pid); [`net_links_into`] reads into the caller's
//! buffer, made at least 64 KiB (macOS: the interface list, grown up to
//! 4 MiB where the kernel asks for more; Linux: `/proc/net/dev`, then one
//! path buffer for every interface's sysfs files, and the rows it parsed).
//! [`net_links`] makes that buffer per call and frees it before it returns;
//! a caller that reads every second keeps one, and holds what it grew to,
//! up to 4 MiB, for as long as it keeps it.

use std::path::{Path, PathBuf};

use crate::{CapSource, Caps, MemoryLevel, Why};

/// One logical core's cumulative ticks since boot, in the platform's unit
/// (100 Hz on macOS and on Linux's `USER_HZ`; 100 ns on Windows). Only the
/// ratios of two readings' deltas mean anything, so the unit never leaves
/// the caller's arithmetic.
///
/// macOS keeps each count in 32 bits, so a core's count of one state wraps
/// after 2³² ticks (about 497 days of that state at 100 Hz): a delta that
/// goes backwards across a wrap reads as nothing for one reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreTicks {
    /// Running user code at normal or raised priority.
    pub user: u64,
    /// Running the kernel (Linux: `system + irq + softirq`; Windows: kernel
    /// time less idle time, DPCs and interrupts included).
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
/// [`core_ticks`](Self::core_ticks) and [`throttle`](Self::throttle) reuse
/// (on Linux the `/proc/stat` buffer, the present core count and the last
/// throttle count; on Windows the processor groups' widths and a buffer;
/// nothing on macOS): no thread, no timer, no syscall until the first read.
/// `Send`, so the host builds it anywhere and moves it to its probe thread.
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
    /// A sampler that has read nothing. On Linux it sizes its buffer
    /// ([`crate::linux::STAT_CAP`]) and reads how many cores are present; on
    /// Windows it reads the processor groups' widths.
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
    /// * **Windows**: one `NtQuerySystemInformationEx(
    ///   SystemProcessorPerformanceInformation)` per processor group
    ///   ([`crate::windows::processor_times`]); logical id `i` is the group's
    ///   offset (the widths of the groups before it) plus the processor's
    ///   number in its group, as [`cpu_layout`] numbers them.
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

    /// The throttle level now, or why it is unavailable.
    ///
    /// * **Linux**: a HEURISTIC level ([`Throttle::exact`] false) from the
    ///   thermal zones' temperatures against their trip points and whether
    ///   the CPUs' thermal throttle counters rose since the last call
    ///   ([`crate::linux::throttle_level`]); [`Why::Absent`] on a machine
    ///   with neither (most virtual machines).
    /// * **macOS**: [`Why::NotRead`] — the public thermal notify level is not
    ///   read by this build (design §5.2's validated reader is owed).
    /// * **Windows**: [`Why::Unsupported`] (design §8.1).
    ///
    /// On Linux one call reads up to 512 per-core counter files (256 cores,
    /// two counters each) and, for each of at most 32 thermal zones, its
    /// temperature and up to 16 trip points' type and temperature: about
    /// 1,600 small sysfs reads on the largest machine. It is an on-demand
    /// reading (a `sys` query), bounded by those caps, and nothing calls it
    /// per tick; a caller that would poll it every second keeps the trip
    /// points it read, which do not change, rather than re-read them.
    ///
    /// # Errors
    ///
    /// The [`Why`] when no level can be given.
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

/// How hard the machine is throttling its cores (design §8.1): the OS's own
/// level mapped onto four words, ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ThrottleLevel {
    /// Full speed.
    Nominal,
    /// Throttling has begun.
    Light,
    /// Throttling hard.
    Heavy,
    /// At the point the OS shuts work, or the machine, down.
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
    /// The OS's own level (`true`), or a heuristic from temperatures and
    /// counters (`false`: Linux).
    pub exact: bool,
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
/// * **Linux**: the present cores (`/sys/devices/system/cpu/present`) split
///   by the hybrid PMUs (`/sys/devices/cpu_core/cpus` "Performance",
///   `cpu_atom` "Efficiency") where the kernel lists them, else by each
///   core's `cpu_capacity` (ARM big.LITTLE and its kin, fastest first),
///   else one cluster `"CPU"` ([`crate::linux::layout_from`]).
/// * **Windows**: the CPU Sets (`GetSystemCpuSetInformation`) grouped by
///   `EfficiencyClass`, highest (fastest) first; one `"CPU"` cluster where
///   every core has the same class ([`crate::windows::layout_from_cpu_sets`]).
/// * Elsewhere: one cluster `"CPU"` of `available_parallelism` cores.
///
/// Linux and Windows name no level: two levels read "Performance" and
/// "Efficiency" (the Deck's `P` and `E`), and any between them "Mid"
/// ([`level_names`]).
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

/// The cores the OS took away now, by logical id, ascending; `None` where
/// unread.
///
/// * **Linux**: present but offline (`/sys/devices/system/cpu/{present,
///   online}`, [`crate::linux::Root::withdrawn`]).
/// * **Windows**: the parked CPU Sets (`GetSystemCpuSetInformation`).
/// * **macOS**: `None` — `kern.sched_recommended_cores` is not read by this
///   build ([`crate::caps`] says `not_read`).
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

/// The names of `n` performance levels the OS ranks but does not name,
/// fastest first: one level is `"CPU"`; two are `"Performance"` and
/// `"Efficiency"`; one between those two is `"Mid"`, and two or more are
/// `"Mid 1"`, `"Mid 2"`, … fastest first, so no two levels share a name
/// (or the Deck's short label, `M1`, `M2`). PURE.
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

/// Clusters from logical ids each carrying a rank where a HIGHER rank is
/// faster (a Linux `cpu_capacity`, a Windows `EfficiencyClass`): one
/// cluster per distinct rank, fastest first, each named by
/// [`level_names`] and holding its ids ascending. `None` for no ids. PURE.
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

/// Commit: the memory the OS has promised processes, against what it can
/// back (design §9.6, "commit is the core on Windows").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Commit {
    /// Bytes charged (Windows `CommitTotal`; Linux `Committed_AS`).
    pub charge: u64,
    /// The limit (Windows `CommitLimit`, RAM plus page files; Linux
    /// `CommitLimit`).
    pub limit: u64,
    /// The limit is enforced (Windows: an allocation past it fails), not
    /// advisory (Linux: under overcommit the charge passes it freely, so the
    /// figure is a heuristic).
    pub exact: bool,
}

impl Commit {
    /// The charge as thousandths of the limit (`None` for a zero limit);
    /// past the limit reads past 1000.
    #[must_use]
    pub fn permille(&self) -> Option<u64> {
        (self.limit > 0).then(|| {
            u64::try_from(u128::from(self.charge) * 1000 / u128::from(self.limit))
                .unwrap_or(u64::MAX)
        })
    }
}

/// The machine's memory by the OS's own classes, bytes. A class this OS does
/// not have is `None`, never 0.
///
/// On macOS the classes come from ONE `host_statistics64(HOST_VM_INFO64)`
/// read (pages × `vm.pagesize`), so they are consistent with each other, and
/// `wired + active + inactive + speculative + free + compressed` is physical
/// memory less what the VM never manages (firmware and early-kernel carve-outs:
/// 0.82 GiB of 32 on the M5). `app + file_backed = active + inactive +
/// speculative` (the pageable queues). On Linux see
/// [`crate::linux::memory_detail`] and on Windows
/// [`crate::windows::memory_detail_of`] for what each field holds: neither
/// partitions physical memory exactly ([`exact`](Self::exact) is false), so
/// `total` less the classes is memory no class names (design §9.6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryDetail {
    /// Physical memory (`hw.memsize`; Linux `MemTotal`; Windows
    /// `ullTotalPhys`).
    pub total: u64,
    /// Truly free: `free_count − speculative_count`. The kernel's
    /// `free_count` holds the speculative pages, which are given apart in
    /// [`speculative`](Self::speculative) (as `vm_stat`'s "Pages free" is).
    /// Linux `MemFree`; Windows the free and zeroed lists (available less
    /// the standby cache).
    pub free: u64,
    /// Speculative read-ahead pages (`speculative_count`): free to take.
    /// macOS only.
    pub speculative: Option<u64>,
    /// The active queue (`active_count`; Linux `Active`).
    pub active: Option<u64>,
    /// The inactive queue (`inactive_count`; Linux `Inactive`).
    pub inactive: Option<u64>,
    /// Wired: never paged out or compressed (`wire_count`). Linux: kernel
    /// memory, `Slab + KernelStack + PageTables`; Windows: the kernel's
    /// paged and nonpaged pools, at most what is in use.
    pub wired: u64,
    /// Physical memory holding compressed data (`compressor_page_count`;
    /// Linux zswap's pool, `Zswap`, where the kernel reports it).
    pub compressed: Option<u64>,
    /// The uncompressed size of what the compressor holds
    /// (`total_uncompressed_pages_in_compressor`; Linux `Zswapped`): NOT
    /// physical memory, and it can exceed [`total`](Self::total).
    pub stored: Option<u64>,
    /// Purgeable pages (`purgeable_count`). macOS only.
    pub purgeable: Option<u64>,
    /// The reclaimable cache: file-backed pages (`external_page_count`).
    /// Linux `Cached − Shmem + Buffers` (shared memory is listed under
    /// `Cached` but only swap can reclaim it); Windows the standby cache
    /// (`SystemCache`), at most what is available.
    pub file_backed: u64,
    /// Anonymous memory of processes: `internal_page_count −
    /// purgeable_count` (Activity Monitor's "App Memory"), zero where
    /// purgeable exceeds internal. Linux `AnonPages`; Windows what is in use
    /// less the kernel's pools.
    pub app: u64,
    /// Shared memory, which only swap can reclaim (Linux `Shmem`: tmpfs and
    /// shared mappings). Linux only.
    pub shared: Option<u64>,
    /// Swap in use (`vm.swapusage` `xsu_used`; Linux `SwapTotal − SwapFree`;
    /// Windows the page files in use).
    pub swap_used: u64,
    /// Swap allocated (`vm.swapusage` `xsu_total`; Linux `SwapTotal`;
    /// Windows the page files' size). macOS grows and shrinks its swap files
    /// on demand, so this moves.
    pub swap_total: u64,
    /// Commit charge and limit (Linux, Windows); `None` on macOS, which has
    /// no commit limit.
    pub commit: Option<Commit>,
    /// The kernel's pressure level ([`crate::memory_pressure`] on macOS;
    /// Linux PSI, [`crate::linux::psi_level`]; Windows commit against its
    /// limit, [`crate::windows::commit_level`]). `None` where unread (Linux
    /// without PSI).
    pub pressure: Option<MemoryLevel>,
    /// The classes partition physical memory exactly (macOS), or leave a
    /// remainder no class names (Linux, Windows).
    pub exact: bool,
    /// The VM's cumulative event counters from the same read, where the OS
    /// gives them (macOS `HOST_VM_INFO64`; not read on Linux).
    pub counters: Option<VmCounters>,
    /// The percent of memory the kernel counts available before it acts
    /// (macOS `kern.memorystatus_level`, the figure jetsam reads; not read on
    /// Linux).
    pub kernel_available_pct: Option<u8>,
}

/// The VM's cumulative event counters since boot, from the same read as the
/// page classes. Two readings give rates.
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
    pub swapins: u64,
    pub swapouts: u64,
    pub decompressions: u64,
    pub compressions: u64,
    pub pageins: u64,
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

/// What the machine is called and what it runs, read once for `sys global`
/// and `aterm help sys`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MachineWords {
    /// The CPU's brand (`Apple M5`; macOS `machdep.cpu.brand_string`, Linux
    /// `/proc/cpuinfo`'s `model name`).
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

/// The machine's memory by class, swap, commit and pressure; `None` where
/// unreadable.
///
/// * **macOS**: one `host_statistics64(HOST_VM_INFO64)`, one
///   `vm.swapusage` and one `kern.memorystatus_vm_pressure_level`
///   (`hw.memsize` and `vm.pagesize` are read once per process). 3.3 µs
///   median on the M5.
/// * **Linux**: one bounded read of `/proc/meminfo`
///   ([`crate::linux::memory_detail`]) and one of `/proc/pressure/memory`
///   for the level ([`crate::linux::psi_level`]; `None` without PSI).
/// * **Windows**: `GlobalMemoryStatusEx`, `K32GetPerformanceInfo`, the page
///   files (`NtQuerySystemInformation(SystemPageFileInformation)`) and the
///   low-memory resource notification ([`crate::windows::memory_detail_of`]).
/// * Elsewhere: `None`.
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

/// The GPU, device-wide (Apple GPUs report device-wide use only, in whole
/// percent, refreshed on the driver's own schedule).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuReading {
    /// `PerformanceStatistics` "Device Utilization %" (Linux amdgpu's
    /// `gpu_busy_percent`, NVIDIA's NVML utilization), at most 100; `None`
    /// with [`unread`](Self::unread) saying why.
    pub device_pct: Option<u8>,
    /// "Renderer Utilization %", at most 100.
    pub renderer_pct: Option<u8>,
    /// "Tiler Utilization %", at most 100.
    pub tiler_pct: Option<u8>,
    /// "In use system memory", bytes.
    pub in_use_bytes: Option<u64>,
    /// "Alloc system memory": what the driver has allocated, in use or not,
    /// bytes.
    pub alloc_bytes: Option<u64>,
    /// "recoveryCount": how many times the driver has recovered the GPU
    /// from a fault (a GPU reset) since it started.
    pub recoveries: Option<u64>,
    /// `gpu-core-count` on the accelerator entry, where present.
    pub cores: Option<u16>,
    /// The accelerator's `model` (`"Apple M5"`), else its IOKit class name;
    /// on Linux the DRM driver's name (`amdgpu`, `i915`, `nvidia`).
    pub name: Option<String>,
    /// Why [`device_pct`](Self::device_pct) is `None`: the device sleeps
    /// ([`Why::Asleep`], known idle and not woken), its driver publishes no
    /// figure ([`Why::Unsupported`]), or only a privilege could read one.
    pub unread: Option<Why>,
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
///   opened) and reads six keys of it. 26 µs median on the M5. A Mac with
///   two GPUs reads the first one matched.
/// * **Linux**: the DRM cards in `/sys/class/drm` ([`crate::linux::Root::gpu`]):
///   a card's `power/runtime_status` first (a suspended one is
///   [`Why::Asleep`] and never read, so never woken), then amdgpu's
///   `gpu_busy_percent` or, for NVIDIA's proprietary driver, NVML's
///   utilization (`libnvidia-ml.so.1`, opened and initialised once per
///   process on the first ask), each only where the read cannot keep a
///   runtime-PM card awake; i915 and xe publish none without
///   `CAP_PERFMON`, and the rest none at all.
/// * **Windows**: `None` — the public source, PDH's `GPU Engine` counters,
///   expands one instance per engine per process and costs more per read
///   than the whole sampler ([`crate::caps`] says `not_read`).
/// * Elsewhere: `None`.
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
/// * **Windows**: the drive is judged from the path's own spelling
///   ([`crate::windows::drive_root`]: a UNC path is never asked), then
///   `GetDriveTypeW` on its root: only a fixed disk or a RAM disk is read,
///   by `GetDiskFreeSpaceExW` (quota-aware: `available` is what this user
///   may write) and `GetVolumeInformationW` for the file system's name.
///   The mount is the drive's root (`C:\`).
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

/// What this process may use ([`capacity`]): a Linux cgroup's limits, else
/// the host's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capacity {
    /// CPU this process may use, thousandths of a core (`cpu.max`'s quota
    /// over its period, the tightest on the cgroup's path); `None`: no limit
    /// below the host's cores.
    pub cpu_millicores: Option<u32>,
    /// Memory this process may use (`memory.max`, the tightest on the
    /// path); `None`: no limit below the host's memory.
    pub mem_bytes: Option<u64>,
    /// Where the figures come from.
    pub source: CapSource,
}

/// What this process may use now.
///
/// * **Linux**: cgroup v2 ([`crate::linux::Root::capacity`]): the process's
///   cgroup from `/proc/self/cgroup`, then `cpu.max` and `memory.max` in it
///   and every ancestor under `/sys/fs/cgroup`, the tightest of each. Inside
///   a container this is the container's ceiling while `/proc` shows the
///   host. Under cgroup v1 or with no limit set it is the host's.
/// * Elsewhere: the host's ([`CapSource::Host`], no figures).
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

/// What the running machine's readers can say ([`Caps`]), measured now: a
/// few file probes on Linux (PSI, zswap, the thermal zones, the DRM cards,
/// another user's `/proc` entry), one GPU read on macOS and Linux, the
/// coalitions' self-check on macOS, nothing else. Not for every tick: the
/// report changes with the machine's configuration, not its load.
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

/// What a network link is, as the OS classes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// Ethernet: built in, USB or Thunderbolt.
    Wired,
    /// Wi-Fi.
    Wifi,
    /// Cellular data.
    Cellular,
    /// Anything else: loopback, a tunnel, a bridge, a peer-to-peer or
    /// companion radio, a coprocessor or guest link (whatever type the OS
    /// gives it: see [`net_links`]), or a link the OS does not class.
    #[default]
    Other,
}

impl LinkKind {
    /// Return the canonical lowercase name (`"wired"`, `"wifi"`,
    /// `"cellular"`, `"other"`).
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

/// One direction's cumulative counters on a link, since the interface was
/// created. 64-bit on both platforms, so they do not wrap, except one: on
/// macOS the sent side's [`drops`](Self::drops) is 32-bit and wraps after
/// 2³².
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkCounters {
    /// Bytes.
    pub bytes: u64,
    /// Packets.
    pub packets: u64,
    /// Errors.
    pub errors: u64,
    /// Packets dropped (macOS: on input `ifi_iqdrops`; on output the send
    /// queue's `ifm_snd_drops`, the one 32-bit counter, which the kernel
    /// keeps as a signed `int`: it is read unsigned, so it wraps to zero
    /// after 2³² as a 32-bit counter does).
    pub drops: u64,
}

/// One network interface now ([`net_links`]).
///
/// An interface destroyed and created again (an adapter plugged back in, a
/// VPN reconnected) can come back under another index with its counters at
/// zero: a caller keys its baseline by name and index, and reads a counter
/// that went down as a new baseline.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkReading {
    /// Its name (`en0`, `wlan0`).
    pub name: String,
    /// Its interface index.
    pub index: u32,
    /// What it is ([`LinkKind`]).
    pub kind: LinkKind,
    /// Up and running: administratively up and with a link (macOS
    /// `IFF_UP` and `IFF_RUNNING`; Linux `IFF_UP` with `operstate` `up`, or
    /// `unknown` for a driver that keeps none).
    pub up: bool,
    /// A loopback interface (`IFF_LOOPBACK`).
    pub loopback: bool,
    /// Whether a device backs it (Linux: `/sys/class/net/<if>/device`
    /// exists; a bridge, a tunnel or a container's `veth` has none); `None`
    /// where the reader cannot tell (macOS).
    pub physical: Option<bool>,
    /// The link rate the driver reports, bits per second (macOS
    /// `ifi_baudrate`: Wi-Fi's current rate, which moves with the radio;
    /// Linux `/sys/class/net/<if>/speed`); `None` where it reports none.
    pub rate_bps: Option<u64>,
    /// Received.
    pub rx: LinkCounters,
    /// Sent.
    pub tx: LinkCounters,
}

/// The most interfaces one [`net_links`] reading holds.
pub const MAX_LINKS: usize = 1024;

/// Name prefixes of links whose traffic is never the machine's own: the
/// loopback (`lo`); tunnels, whose traffic is counted again on the link
/// that carries it (`utun`, `ipsec`, `ppp`, `gif`, `stf`); bridges, which
/// count their members' traffic again (`bridge`); Apple's peer-to-peer and
/// companion radios (`awdl`, `llw`, `nan`, `ap`); the internal links to the
/// Mac's coprocessors (`anpi`, which reports itself wired); and the
/// Virtualization framework's guest links (`vmenet`).
const NOT_MACHINE_LINKS: &[&str] = &[
    "lo", "utun", "ipsec", "ppp", "gif", "stf", "bridge", "awdl", "llw", "nan", "ap", "anpi",
    "vmenet",
];

/// Whether `name` starts with one of [`NOT_MACHINE_LINKS`]' prefixes: a
/// loopback, tunnel, bridge, peer-to-peer or companion radio, coprocessor
/// or guest link, whose traffic is never the machine's own and whose kind
/// is [`LinkKind::Other`] whatever the OS says of it. PURE.
#[must_use]
pub(crate) fn never_the_machines(name: &str) -> bool {
    NOT_MACHINE_LINKS
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Whether `link`'s traffic counts toward the machine's own: it is up and
/// running, not a loopback, and its name starts with none of the prefixes
/// of tunnels, bridges, peer-to-peer radios and virtual links (`lo`,
/// `utun`, `ipsec`, `ppp`, `gif`, `stf`, `bridge`, `awdl`, `llw`, `nan`,
/// `ap`, `anpi`, `vmenet`). Then where the reader knows whether a device
/// backs it (Linux), that decides; where it does not (macOS), the name
/// does: `en*` (built-in, USB and Thunderbolt Ethernet, and Wi-Fi) or
/// `pdp_ip*` (cellular). A VPN's tunnel stays out because its traffic is
/// counted again on the link that carries it; inside a container's network
/// namespace no link has a device, so none counts. PURE.
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

/// A link's kind from its name alone, where the OS does not class it:
/// `pdp_ip*`, `wwan*` and `rmnet*` cellular; `wl*` Wi-Fi; `en*` and `eth*`
/// wired; anything else other. On a Mac whose functional-type ask failed
/// this reads Wi-Fi's `en0` as wired. PURE.
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

/// Every network interface's kind, state, link rate and cumulative
/// counters now, at most [`MAX_LINKS`]; `None` where unreadable: what
/// [`net_links_into`] reads, into a buffer made for this call and freed
/// before it returns.
#[must_use]
pub fn net_links() -> Option<Vec<LinkReading>> {
    net_links_into(&mut Vec::new())
}

/// Every network interface's kind, state, link rate and cumulative
/// counters now, at most [`MAX_LINKS`], read into `scratch`, which the
/// caller keeps from one reading to the next so a reading allocates no
/// buffer (only its answer); `None` where unreadable. The links whose
/// traffic is the machine's own are the ones [`counts_toward_machine`]
/// admits. `scratch` is made at least 64 KiB and keeps the size it was
/// last read at (on macOS up to 4 MiB, where the list outgrew it); its
/// bytes are of no use to the caller.
///
/// * **macOS**: one `sysctl {CTL_NET, PF_ROUTE, 0, 0, NET_RT_IFLIST2, 0}`
///   into the whole of `scratch` (grown to what the kernel asks for, up to
///   4 MiB, where that is too small), decoded by
///   [`crate::iflist::links_of`]: the 64-bit `if_data64` counters and the
///   name the message carries. One datagram socket then asks the
///   functional type (`SIOCGIFFUNCTIONALTYPE`: wired, Wi-Fi, cellular),
///   which names Wi-Fi apart from Ethernet where the name cannot, of every
///   interface whose name [`counts_toward_machine`] does not refuse; where
///   the ask fails the name decides ([`crate::iflist::link_kind`]). A
///   refused name is [`LinkKind::Other`] unasked: macOS gives `utun`,
///   `bridge` and `anpi` links the wired type and `llw` and `ap` links the
///   Wi-Fi one. 44 µs median on the owner's M5 Max (24 interfaces, 8 of
///   them asked, 2026-10-20), of which the list's `sysctl` is 33 µs; a
///   kept `scratch` saves little of it (44.7 µs median against
///   [`net_links`]' 45.3 µs, the same Mac at load 13, later that day).
/// * **Linux**: `/proc/net/dev` (this process's network namespace) read
///   into the first [`crate::linux::NET_DEV_CAP`] bytes of `scratch`, then
///   per interface its `/sys/class/net/<if>/` `ifindex`, `flags`,
///   `operstate`, `speed` (a link with a device only), each read into one
///   stack buffer, and whether `device` and `wireless` exist
///   ([`crate::linux::links_under_into`]).
/// * Elsewhere: `None`, and `scratch` is left as it came.
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

/// How many processes the process table admits ([`process_limits`]): the
/// ceilings a `fork` fails at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessLimits {
    /// The most processes the whole machine may run (macOS `kern.maxproc`;
    /// Linux the smaller of `kernel/pid_max` and `kernel/threads-max`,
    /// which every thread counts against too).
    pub system: Option<u64>,
    /// The most processes one user may run: on macOS the smaller of
    /// `kern.maxprocperuid` and this process's soft `RLIMIT_NPROC`, on
    /// Linux the soft `RLIMIT_NPROC` (which counts the user's threads too);
    /// `None` where unknown or unlimited. Children inherit the limit, so a
    /// tab's shell runs under aterm's.
    pub per_user: Option<u64>,
}

/// The smaller of two ceilings, either of which may be unknown. PURE.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
#[must_use]
pub(crate) fn smaller(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (one, None) | (None, one) => one,
    }
}

/// A soft resource limit as a ceiling: `None` for the platform's
/// `RLIM_INFINITY`. PURE.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
#[must_use]
pub(crate) fn rlimit_ceiling(soft: u64, infinity: u64) -> Option<u64> {
    (soft != infinity).then_some(soft)
}

/// This process's soft `RLIMIT_NPROC` (one `getrlimit`); `None` where
/// unlimited or refused.
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

/// How many processes the machine, and one user, may run, read now (they
/// are tunables: read them once, or rarely).
///
/// * **macOS**: `kern.maxproc`, `kern.maxprocperuid` and one `getrlimit`.
///   1.2 µs median on the M5 Max; it read 16,000 and 10,666 there.
/// * **Linux**: `/proc/sys/kernel/pid_max` and `threads-max`, each read
///   into one stack buffer, and one `getrlimit`.
/// * Elsewhere: every field `None`.
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

/// How many threads the kernel's thread pool (the workqueue) gives one
/// process ([`workqueue_limits`]): the pool every macOS daemon serves its
/// requests from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkqueueLimits {
    /// The most constrained (not overcommit) workqueue threads one process
    /// may have scheduled at once (macOS `kern.wq_max_constrained_threads`:
    /// the larger of 64 and five per core; 90 on an 18-core Mac). A daemon
    /// whose every such thread is blocked can start no request that would
    /// unblock them.
    pub constrained: Option<u64>,
    /// The most workqueue threads of any kind one process may have (macOS
    /// `kern.wq_max_threads`).
    pub total: Option<u64>,
}

/// The kernel's workqueue ceilings, read now: two `sysctl`s on macOS (they
/// are fixed per boot), every field `None` elsewhere.
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

/// The kernel's load averages over 1, 5 and 15 minutes: the mean count of
/// threads running or waiting to run (Linux also counts a thread waiting on
/// a disk). One `getloadavg` (a `sysctl` on macOS, 0.3 µs median on the M5
/// Max; `/proc/loadavg` on Linux); `None` where it answers fewer than three,
/// or a value that is not a finite non-negative number, and on every other
/// platform.
#[must_use]
pub fn load_average() -> Option<[f64; 3]> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        unsafe extern "C" {
            /// `<stdlib.h>`; declared here: the first-party `libc` does not
            /// export it.
            fn getloadavg(loadavg: *mut f64, nelem: libc::c_int) -> libc::c_int;
        }
        let mut load = [0f64; 3];
        // SAFETY: `load` is a writable array of three doubles, and
        // `getloadavg` writes at most the count it is given.
        let n = unsafe { getloadavg(load.as_mut_ptr(), 3) };
        (n == 3 && load.iter().all(|l| l.is_finite() && *l >= 0.0)).then_some(load)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// How many processes the machine runs, and how many of them `uid` owns,
/// read now: `(all, uid's)`, the counts [`process_limits`]' two ceilings
/// bound. Each is `None` where unread, never a zero for unknown.
///
/// * **macOS**: a `proc_listallpids` that asks the room, then, into one
///   buffer of that room and a quarter more, `proc_listallpids` (every
///   process, a zombie included) and `proc_listpids(PROC_RUID_ONLY, uid)`
///   (the processes whose real user is `uid`, which `kern.maxprocperuid`
///   and `RLIMIT_NPROC` count). libproc answers zero both for a failure
///   and for a listing that matched nothing, so an empty listing of every
///   process is `None`, and one user's is a count of zero only where the
///   call left `errno`, cleared before it, clear (a uid that owns no
///   process). A listing that fills the buffer is `None` too (it may have
///   been cut). 17 µs median on the owner's M5 Max (894 processes,
///   2026-10-20).
/// * **Linux**: `/proc/loadavg`'s count of every thread on the machine,
///   the unit [`process_limits`] counts in there
///   ([`crate::linux::Root::tasks`]); one user's is `None`, since counting
///   it walks `/proc`.
/// * **Windows**: the process count of the `K32GetPerformanceInfo` that
///   [`memory_detail`] reads too; one user's is `None`.
/// * Elsewhere: `(None, None)`.
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

/// The most pids one [`pid_counts`] listing is offered room for: past the
/// whole of xnu's pid space (pids run below `PID_MAX`, 99,999), so no
/// listing needs more; 512 KiB of pids.
#[cfg(any(target_os = "macos", test))]
pub(crate) const MAX_PIDS: usize = 1 << 17;

/// The room, in pids, a [`pid_counts`] listing is offered: what the size
/// ask answered (xnu's process count and 20) and a quarter more, for the
/// processes started between the two asks, at most [`MAX_PIDS`]; `None`
/// where the ask failed (it answered no room). PURE.
#[cfg(any(target_os = "macos", test))]
#[must_use]
pub(crate) fn pid_room(estimate: i64) -> Option<usize> {
    let pids = usize::try_from(estimate).ok().filter(|n| *n > 0)?;
    Some(pids.saturating_add(pids / 4).min(MAX_PIDS))
}

/// The count of a listing of `listed` pids into a buffer of `room`; `None`
/// where it listed none (libproc answers zero for a failure too, so a zero
/// is never read as a count) or filled the buffer (the listing may have
/// been cut). PURE.
#[cfg(any(target_os = "macos", test))]
#[must_use]
pub(crate) fn listed_count(listed: i64, room: usize) -> Option<u64> {
    let n = usize::try_from(listed)
        .ok()
        .filter(|n| *n > 0 && *n < room)?;
    u64::try_from(n).ok()
}

/// The count of a listing of the pids one uid owns, `listed` of them into
/// a buffer of `room`, where `errno` is the thread's `errno` after the
/// call and was cleared before it: zero with `errno` still clear is a uid
/// that owns no process, a count of zero (a failure sets `errno`); any
/// other answer is [`listed_count`]'s. PURE.
#[cfg(any(target_os = "macos", test))]
#[must_use]
pub(crate) fn owned_count(listed: i64, errno: i32, room: usize) -> Option<u64> {
    if listed == 0 && errno == 0 {
        Some(0)
    } else {
        listed_count(listed, room)
    }
}
