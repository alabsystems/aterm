// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! System vitals model: absorbs [`Sample`] readings into an immutable [`Snapshot`].

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::explain::{IdMap, IdSet};
use crate::history::{self, Filling, FreeHistory, FreeWeeks, Rates};
use crate::net_counter::{ByteCounter, ByteDelta, ByteSample, DeltaError, byte_delta};
use crate::peaks::{DailyMax, NET_LEARN_S, PROC_LEARN_S, Peaks, TOKENS_LEARN_S};
use crate::ring::{
    Agg, BUCKET_MS, BucketRing, Cell, GapKind, MultiScaleSpec, NOW_WINDOW_MS, ProgSlot, Reading,
    Recent, ScaleCell, SlotGrid, Taken, Tiers, TimeGap, columns_of, multi_scale_cells,
};
use crate::trend::{History, Trend, TrendView};

/// One logical core's cumulative ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ticks {
    /// Busy ticks (user, system, nice).
    pub busy: u64,
    /// All ticks (busy and idle).
    pub total: u64,
}

/// One CPU cluster as the OS names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClusterSpec {
    /// The OS's name (`Performance`, `Efficiency`, …; `CPU` where it names none).
    pub name: String,
    /// The logical core ids in it.
    pub logical: Vec<u16>,
}

/// The fewest bytes any reader of a process name cuts it to (Linux's `comm`, and the short BSD
/// info's `p_comm` on macOS, keep 15; the kernel record's `p_comm` 16; `p_name` 31).
const NAME_CUT_MIN: usize = 15;

/// Whether `a` and `b`, two readers' names for a process, can name one program: they are equal,
/// or the shorter begins the longer and may have been cut, which it may where it is
/// [`NAME_CUT_MIN`] bytes long or longer or ends in the mark `…` or in the `U+FFFD` a cut through
/// a character leaves (either left off before comparing). A whole name that begins another
/// (`mds` and `mds_stores`) names another program.
#[must_use]
pub fn same_process_name(a: &str, b: &str) -> bool {
    fn bare(name: &str) -> (&str, bool) {
        name.strip_suffix(['\u{2026}', '\u{FFFD}'])
            .map_or((name, false), |cut| (cut, true))
    }
    let ((a, a_cut), (b, b_cut)) = (bare(a), bare(b));
    let ((short, cut), long) = if a.len() <= b.len() {
        ((a, a_cut), b)
    } else {
        ((b, b_cut), a)
    };
    short == long || (long.starts_with(short) && (cut || short.len() >= NAME_CUT_MIN))
}

/// One process in a sweep.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcSample {
    /// Darwin scheduling observations from this process's current reading.
    pub scheduling: Option<crate::scheduling::Scheduling>,
    /// The pid.
    pub pid: u32,
    /// Its kernel start record, compared only within this boot.
    pub start: Option<u64>,
    /// Its parent's pid.
    pub ppid: u32,
    /// Cumulative CPU time, ns; `None` where the OS refused it. A zombie's is its final one.
    pub cpu_ns: Option<u64>,
    /// Cumulative CPU time of the children it has reaped, ns: at each reap the kernel adds the
    /// child's own time and its reaped children's (macOS); `None` where unread, and off macOS.
    pub child_cpu_ns: Option<u64>,
    /// Physical footprint, bytes; `None` where unreadable.
    pub footprint: Option<u64>,
    /// Its name, whole where a reader held it whole, else ending in `…`
    /// (`aterm_sysprobe::procs::whole_name`); [`same_process_name`] matches it to another
    /// reader's.
    pub name: String,
    /// Cumulative bytes read from storage, or `None` if unreadable.
    pub disk_read: Option<u64>,
    /// Cumulative bytes written to storage; `None` where unreadable (another user's process,
    /// Windows).
    pub disk_written: Option<u64>,
    /// Cumulative energy its threads used, nanojoules (macOS `ri_energy_nj`); `None` where
    /// unread, and off macOS.
    pub energy_nj: Option<u64>,
    /// Its real user, where read.
    pub uid: Option<u32>,
    /// Its process group (0: unknown).
    pub pgid: u32,
    /// Its session's id, its leader's pid (0: unknown).
    pub sid: u32,
    /// Its scheduler state.
    pub state: ProcState,
    /// Cumulative runnable (running or queued) time in ns, or `None` if unreadable.
    pub runnable_ns: Option<u64>,
    /// Its largest footprint so far, bytes; `None` where unread.
    pub peak: Option<u64>,
    /// Timestamp (ns on [`Sample::at_ns`]) when this process's counters were read.
    pub at_ns: Option<u64>,
    /// macOS resource coalition ID (read on split ticks only).
    pub coalition: Option<u64>,
    /// macOS pid version (`p_idversion`), new at each process and exec: read on split ticks, and
    /// on every whole reading for a process whose [`Self::start`] is unread (another user's).
    pub version: Option<i32>,
    /// When it started, unix ms (a fork's time, which an exec keeps); the host reads it on
    /// every whole reading (one with a sweep), `None` where unread.
    pub birth_ms: Option<i64>,
    /// Whether the kernel reaps its children for it (macOS `P_NOCLDWAIT`: it ignores `SIGCHLD`
    /// or set `SA_NOCLDWAIT`), adding each child's time to its [`Self::child_cpu_ns`] twice, as
    /// the sweep read it for every process: nothing records it between two readings or after the
    /// process exits. `None` where unread, and off macOS.
    pub reaps_unwaited: Option<bool>,
}

/// Process scheduler state (`Run` means neither stopped, nor a zombie, nor in an uninterruptible
/// wait the sweep could see).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProcState {
    /// Running or sleeping.
    Run,
    /// In an uninterruptible wait (Linux's `D`; a disk or a mount that does not answer). macOS
    /// reports every live process as running, so its sweep never says this: its threads do
    /// ([`crate::blockage`]).
    Uninterruptible,
    /// Stopped.
    Stopped,
    /// Exited, not yet reaped.
    Zombie,
    /// Not read.
    #[default]
    Unknown,
}

impl ProcState {
    /// The word `sys procs` prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Uninterruptible => "uninterruptible",
            Self::Stopped => "stopped",
            Self::Zombie => "zombie",
            Self::Unknown => "unknown",
        }
    }
}

/// The portable thermal level, ranked 0 to 3: the OS's own level where [`Thermal::exact`]
/// (nominal, fair, serious, critical), the counter heuristic's where not (none, light, heavy,
/// severe). [`Thermal::word`] names it; only [`Thermal::throttled`] says whether it throttles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Throttle {
    /// Rank 0: nominal; the heuristic counts no throttling.
    #[default]
    None,
    /// Rank 1: fair (macOS: "slightly elevated", no throttling claimed); the heuristic's light.
    Light,
    /// Rank 2: serious; the heuristic's heavy, the chip well below its top clock.
    Heavy,
    /// Rank 3: critical; the heuristic's severe.
    Severe,
}

impl Throttle {
    /// The counter heuristic's word for the level.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Light => "light",
            Self::Heavy => "heavy",
            Self::Severe => "severe",
        }
    }

    /// The OS's own word for the level (`NSProcessInfoThermalState`'s names).
    #[must_use]
    pub fn os_word(self) -> &'static str {
        match self {
            Self::None => "nominal",
            Self::Light => "fair",
            Self::Heavy => "serious",
            Self::Severe => "critical",
        }
    }

    /// The level's rank, 0 to 3.
    #[must_use]
    pub fn rank(self) -> usize {
        match self {
            Self::None => 0,
            Self::Light => 1,
            Self::Heavy => 2,
            Self::Severe => 3,
        }
    }
}

/// The machine's thermal state and power mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Thermal {
    /// The portable level.
    pub level: Throttle,
    /// Raw OS thermal level (`0..=3`).
    pub raw: u8,
    /// Low Power Mode, where the OS says.
    pub low_power: Option<bool>,
    /// True for an OS-reported level; false for a temperature/counter heuristic.
    pub exact: bool,
}

impl Thermal {
    /// The level's word: the OS's own where exact (`nominal`, `fair`, `serious`, `critical`),
    /// the counter heuristic's where not (`none`, `light`, `heavy`, `severe`).
    #[must_use]
    pub fn word(self) -> &'static str {
        if self.exact {
            self.level.os_word()
        } else {
            self.level.word()
        }
    }

    /// The clause every surface prints: `thermal <word>` for an OS level, and for the counter
    /// heuristic, which counts throttle events, `throttled: <word>` (`throttle none` at rank 0).
    #[must_use]
    pub fn phrase(self) -> String {
        if self.exact {
            format!("thermal {}", self.word())
        } else if self.level == Throttle::None {
            "throttle none".into()
        } else {
            format!("throttled: {}", self.word())
        }
    }

    /// Whether the level claims throttling: an exact level of serious or above, or a heuristic
    /// level of light or above. Fair claims none. Low Power Mode is [`Self::low_power`].
    #[must_use]
    pub fn throttled(self) -> bool {
        self.level
            >= if self.exact {
                Throttle::Heavy
            } else {
                Throttle::Light
            }
    }

    /// The CPU row's state, its well's ● (`docs/DESIGN-stats-deck-resources-2026-11-04.md`
    /// §3.2): [`State::Warn`] at the OS's exact serious, [`State::Critical`] at its exact
    /// critical. Nominal and fair draw none, nor does Low Power Mode alone, nor any level the
    /// counter heuristic estimates: a deck mark comes only from a documented OS level.
    #[must_use]
    pub fn state(self) -> Option<State> {
        if !self.exact {
            return None;
        }
        match self.level {
            Throttle::Heavy => Some(State::Warn),
            Throttle::Severe => Some(State::Critical),
            Throttle::None | Throttle::Light => None,
        }
    }
}

/// The Neural Engine as its reader last read it
/// (`docs/DESIGN-stats-deck-resources-2026-11-04.md` §3.1). macOS reports no busy share, so it
/// says only whether the engine is powered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AneSample {
    /// Its cores, read once.
    pub cores: u16,
    /// Whether macOS kept it powered at the reading. Powered is not busy: it stays on for a
    /// while after its work ends.
    pub powered: bool,
}

/// The Neural Engine reader's self-check that failed (A2 §3.1). The reader reads no more for
/// the process, and the snapshot carries the check in place of a reading. Nothing reads it yet:
/// A2 §3.1 has the deck draw no tile for it and keep no space (S6), and `sys` name it (S9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AneCheck {
    /// No registry entry of the engine's class matched (an Intel Mac).
    Matched,
    /// Its core count read no positive integer.
    Cores,
    /// Its power state read no integer from 0 to its maximum.
    PowerState,
    /// Its reads cost over 0.2 ms at p95 over the first 60.
    Cost,
}

/// The kernel's memory pressure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pressure {
    /// No pressure.
    Normal,
    /// The kernel is reclaiming.
    Warn,
    /// The kernel is killing or about to.
    Critical,
    /// Not read.
    #[default]
    Unknown,
}

impl Pressure {
    /// The word the sentences print.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Warn => "warn",
            Self::Critical => "critical",
            Self::Unknown => "unknown",
        }
    }

    /// Its rank in `normal < warn < critical`; `None` while unknown.
    #[must_use]
    pub fn rank(self) -> Option<u8> {
        match self {
            Self::Normal => Some(0),
            Self::Warn => Some(1),
            Self::Critical => Some(2),
            Self::Unknown => None,
        }
    }

    /// A word back to its level (`unknown` is not a level).
    #[must_use]
    pub fn from_word(w: &str) -> Option<Self> {
        Some(match w {
            "normal" => Self::Normal,
            "warn" => Self::Warn,
            "critical" => Self::Critical,
            _ => return None,
        })
    }

    /// The row state it raises: warn and critical theirs, none while normal or unknown.
    #[must_use]
    pub fn state(self) -> Option<State> {
        match self {
            Self::Warn => Some(State::Warn),
            Self::Critical => Some(State::Critical),
            Self::Normal | Self::Unknown => None,
        }
    }

    /// The level a kept [`Self::rank`] stands for: the worst whose rank it reaches.
    fn of_rank(rank: f64) -> Self {
        [Self::Critical, Self::Warn]
            .into_iter()
            .find(|p| p.rank().is_some_and(|r| rank >= f64::from(r)))
            .unwrap_or(Self::Normal)
    }
}

/// The VM's cumulative counters (two readings give rates).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemCounters {
    /// Bytes swapped in.
    pub swapin_bytes: u64,
    /// Bytes swapped out.
    pub swapout_bytes: u64,
    /// Pages decompressed.
    pub decompressions: u64,
    /// Pages compressed.
    pub compressions: u64,
    /// Pages read in from files.
    pub pageins: u64,
    /// The VM page size the page counts are in, bytes.
    pub page_bytes: u64,
    /// Anonymous pages pushed out of RAM (`aterm_sysprobe::VmCounters::pushed`: a Mac's
    /// compressions less its single-value ones, Linux's swap-outs); `None` where unread.
    pub pushed: Option<u64>,
    /// Pages that came back after the kernel pushed them out or queued them to go
    /// (`aterm_sysprobe::VmCounters::refaulted`: a Mac's decompressions and reactivations,
    /// Linux's anonymous refaults); `None` where unread.
    pub refaulted: Option<u64>,
    /// Linux's PSI memory stall totals, `some` and `full`, µs; `None` elsewhere.
    pub stall_us: Option<(u64, u64)>,
}

/// Paging per second over the memory reading's window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MemRates {
    /// Bytes swapped in per second.
    pub swapin_bps: f64,
    /// Bytes swapped out per second.
    pub swapout_bps: f64,
    /// Pages decompressed per second.
    pub decompress_ps: f64,
    /// Pages compressed per second.
    pub compress_ps: f64,
    /// Pages read in per second.
    pub pagein_ps: f64,
    /// Compressor churn: bytes compressed plus bytes decompressed per second (the pages × the
    /// page size), the waffle's compressed tile's heat (A1 §4.3).
    pub churn_bps: f64,
    /// Anonymous memory pushed out of RAM, bytes per second ([`MemCounters::pushed`]); `None`
    /// where either reading lacks it or it went back.
    pub push_bps: Option<f64>,
    /// Memory that came back after it was pushed out or queued to go, bytes per second
    /// ([`MemCounters::refaulted`]); `None` as [`Self::push_bps`].
    pub refault_bps: Option<f64>,
    /// Anonymous memory read back into RAM, bytes per second: the pages decompressed (a Mac's
    /// compressor, Linux's zswap) times the page size, and the bytes swapped in. A page swapped
    /// in and decompressed after may count in both; no file-backed page counts.
    pub anon_back_bps: f64,
    /// The share of the window some task, and every non-idle task, stalled on memory (Linux's
    /// PSI, [`MemCounters::stall_us`]), 0 to 1; `None` elsewhere.
    pub stall: Option<(f64, f64)>,
    /// The window, ns.
    pub window_ns: u64,
}

impl MemRates {
    /// Swap traffic, in plus out, bytes per second.
    #[must_use]
    pub fn swap_bps(&self) -> f64 {
        self.swapin_bps + self.swapout_bps
    }
}

/// Swap traffic threshold (KiB/s) for thrashing under warn/critical pressure.
pub const THRASH_SWAP_KIB_S: f64 = 4096.0;

/// Machine memory breakdown by OS page class in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemSample {
    /// Physical memory.
    pub total: u64,
    /// Process memory in bytes (macOS anonymous minus purgeable; Linux anon + shmem; Windows
    /// in-use minus pools).
    pub app: u64,
    /// Non-pageable kernel/wired memory in bytes.
    pub wired: u64,
    /// Physical memory holding compressed pages (macOS compressor or Linux zswap); `None` where
    /// the OS does not say (Linux with zswap off or absent, Windows' compression store, which this
    /// build does not read), so a surface keeps it unknown rather than show a false zero.
    pub compressed: Option<u64>,
    /// The uncompressed bytes the compressor or zswap holds in [`Self::compressed`], if reported.
    pub stored: Option<u64>,
    /// Reclaimable file cache in bytes (macOS file-backed pages, the speculative ones among
    /// them, plus purgeable; Linux page cache and buffers less shared memory, plus reclaimable
    /// slab).
    pub cached: u64,
    /// Free memory in bytes (speculative pages excluded on macOS).
    pub free: u64,
    /// Available memory in bytes: what the kernel can hand out now without paging, as the OS
    /// reader counts it (`aterm_sysprobe::MemoryDetail::available`). Not a VM class: it overlaps
    /// [`Self::free`], [`Self::cached`] and, on a Mac, what compressing idle anonymous pages frees.
    pub available: u64,
    /// Swap in use; `None` where the measurement was not read.
    pub swap_used: Option<u64>,
    /// Swap allocated; `None` where the measurement was not read.
    pub swap_total: Option<u64>,
    /// The pressure level.
    pub pressure: Pressure,
    /// The VM's counters, where read.
    pub counters: Option<MemCounters>,
    /// macOS's `kern.memorystatus_level` percentage (`aterm_sysprobe::MemoryDetail::memorystatus_level_pct`):
    /// the kernel's own figure, which counts the active queue too, so not [`Self::available`].
    pub memorystatus_level_pct: Option<u8>,
    /// The commit charge against its limit, where the OS keeps one.
    pub commit: Option<CommitSample>,
    /// The classes partition the memory the VM manages (macOS), so [`Self::not_in_vm`] is the
    /// memory the OS keeps outside it rather than what the classes leave unnamed.
    pub exact: bool,
    /// What the kernel can hand out without pushing anyone's anonymous pages out of RAM, bytes
    /// (`aterm_sysprobe::MemoryDetail::headroom`): a Mac's free, speculative and purgeable
    /// pages and its file cache above the kernel's floor; Linux's `MemAvailable`. `None` where
    /// unread.
    pub headroom: Option<u64>,
    /// The compressor's nearer wall, what it holds and its limit: its segments or its compressed
    /// pages (`aterm_sysprobe::MemoryDetail::wall`, macOS 27.2); `None` where unread.
    pub wall: Option<(u64, u64)>,
    /// The share of the pages the kernel's pressure test weighs that are not the compressor's,
    /// per mille (`aterm_sysprobe::MemoryDetail::kept_permille`, macOS); `None` where unread.
    pub kept_permille: Option<u16>,
}

/// One physical-memory page class, or the capacity the reported classes leave over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryClassKind {
    /// Process memory.
    App,
    /// Non-pageable kernel memory.
    Wired,
    /// Physical memory holding compressed pages.
    Compressed,
    /// Reclaimable file cache.
    Cache,
    /// Immediately free memory.
    Free,
    /// Capacity outside an exact VM partition, or left unclassified otherwise.
    Residual,
}

/// A physical class or derived residual; an unavailable byte count remains unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryClass {
    /// Which class the reading describes.
    pub kind: MemoryClassKind,
    /// Physical bytes, never uncompressed stored bytes or a process's charged footprint.
    pub bytes: Option<u64>,
}

/// Whether the reported page classes and their residual form a physical partition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryPartition {
    /// Complete trusted macOS VM classes, with their residual, partition physical capacity.
    Exact,
    /// The platform or reader does not promise disjoint complete classes.
    Inexact,
    /// The physical size of the compressed class was not reported.
    Incomplete,
    /// Reported classes together exceed physical capacity.
    Overfull,
    /// Zero capacity cannot supply a denominator for physical shares.
    NoCapacity,
}

/// The physical page-class evidence shared by the GUI and `sys`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalMemoryClasses {
    /// App, wired, compressed, cache, free and residual, in that order.
    pub classes: [MemoryClass; 6],
    /// The physical capacity, independent of the classes' reported sum.
    pub total: u64,
    /// The sum of the five reported classes, excluding unknown counts and the residual.
    pub counted: u128,
    /// How far the reported sum exceeds physical capacity; never saturated to a byte counter.
    pub excess: u128,
    /// Whether these readings can honestly be drawn as a physical partition.
    pub partition: MemoryPartition,
}

/// Virtual memory promised against the system commit limit (enforced on Windows, advisory on
/// Linux).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommitSample {
    /// Committed bytes.
    pub charge: u64,
    /// The limit, bytes.
    pub limit: u64,
    /// Whether the OS enforces the limit (Windows), so the share is a fact.
    pub exact: bool,
}

/// Commit percentage threshold (`90%`) at which an enforced limit warns.
pub const COMMIT_NEAR_PCT: u64 = 90;

impl CommitSample {
    /// Whole percentage of `limit` charged (`None` if `limit == 0`).
    #[must_use]
    pub fn pct(&self) -> Option<u64> {
        (self.limit > 0).then(|| {
            u64::try_from(u128::from(self.charge) * 100 / u128::from(self.limit))
                .unwrap_or(u64::MAX)
        })
    }

    /// Percentage when `exact` is true and charge is at or above [`COMMIT_NEAR_PCT`].
    #[must_use]
    pub fn near_limit(&self) -> Option<u64> {
        self.pct().filter(|p| self.exact && *p >= COMMIT_NEAR_PCT)
    }
}

impl MemSample {
    /// Project physical page classes without treating available memory, stored bytes, swap or
    /// charged footprints as additional physical classes. Only a complete, trusted macOS
    /// reading within a positive physical capacity establishes an exact partition; an unknown
    /// platform never inherits the build host's identity.
    #[must_use]
    pub fn physical_classes(
        &self,
        platform: Option<crate::words::Platform>,
    ) -> PhysicalMemoryClasses {
        let values = [
            Some(self.app),
            Some(self.wired),
            self.compressed,
            Some(self.cached),
            Some(self.free),
        ];
        let counted: u128 = values.into_iter().flatten().map(u128::from).sum();
        let total = u128::from(self.total);
        let residual = self
            .total
            .saturating_sub(u64::try_from(counted).unwrap_or(u64::MAX));
        let partition = if self.total == 0 {
            MemoryPartition::NoCapacity
        } else if counted > total {
            MemoryPartition::Overfull
        } else if self.compressed.is_none() {
            MemoryPartition::Incomplete
        } else if self.exact && platform == Some(crate::words::Platform::MacOs) {
            MemoryPartition::Exact
        } else {
            MemoryPartition::Inexact
        };
        PhysicalMemoryClasses {
            classes: [
                MemoryClass {
                    kind: MemoryClassKind::App,
                    bytes: values[0],
                },
                MemoryClass {
                    kind: MemoryClassKind::Wired,
                    bytes: values[1],
                },
                MemoryClass {
                    kind: MemoryClassKind::Compressed,
                    bytes: values[2],
                },
                MemoryClass {
                    kind: MemoryClassKind::Cache,
                    bytes: values[3],
                },
                MemoryClass {
                    kind: MemoryClassKind::Free,
                    bytes: values[4],
                },
                MemoryClass {
                    kind: MemoryClassKind::Residual,
                    bytes: Some(residual),
                },
            ],
            total: self.total,
            counted,
            excess: counted.saturating_sub(total),
            partition,
        }
    }

    /// The VM classes' sum: wired, app, compressed, cache and free.
    #[must_use]
    pub fn in_vm(&self) -> u64 {
        u64::try_from(self.physical_classes(None).counted).unwrap_or(u64::MAX)
    }

    /// Physical memory in no VM class (A1 §4.3's "not in VM"): `total` less [`Self::in_vm`], so
    /// the classes and it sum to `total` wherever the classes do not overlap past it (then 0).
    #[must_use]
    pub fn not_in_vm(&self) -> u64 {
        self.total.saturating_sub(self.in_vm())
    }

    /// Memory in use (`app + wired + compressed`), capped at `total`: the reader's
    /// `aterm_sysprobe::MemoryDetail::used`, whose shared memory [`Self::app`] holds.
    #[must_use]
    pub fn used(&self) -> u64 {
        self.app
            .saturating_add(self.wired)
            .saturating_add(self.compressed.unwrap_or(0))
            .min(self.total)
    }
}

#[cfg(test)]
#[path = "physical_memory_tests.rs"]
mod physical_memory_tests;

/// The GPU, device-wide.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuSample {
    /// Device utilization, whole percent.
    pub device_pct: Option<u8>,
    /// GPU cores, where the OS says.
    pub cores: Option<u16>,
    /// A short name for it.
    pub name: Option<String>,
    /// GPU memory in use, bytes.
    pub in_use_bytes: Option<u64>,
    /// GPU memory the driver has allocated, in use or not, bytes.
    pub alloc_bytes: Option<u64>,
    /// Cumulative GPU fault recoveries since driver start.
    pub recoveries: Option<u64>,
    /// Renderer utilization, whole percent.
    pub renderer_pct: Option<u8>,
    /// Tiler utilization, whole percent.
    pub tiler_pct: Option<u8>,
    /// Whether it has no memory of its own and works in the system's, as its reader says
    /// (`aterm_sysprobe::GpuReading::unified`).
    pub unified: bool,
}

/// The volume a path lives on, bytes (`aterm_sysprobe::VolumeReading`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VolumeSample {
    /// Capacity.
    pub total: u64,
    /// Free: what this user may still write (`f_bavail`).
    pub free: u64,
    /// Free blocks the volume keeps back from this user (`f_bfree − f_bavail`, an ext4 root
    /// reserve), never in [`Self::free`].
    pub reserved: u64,
    /// Where it is mounted.
    pub mount: String,
}

impl VolumeSample {
    /// Bytes used: capacity less what this user could still write ([`Self::free`]), so free
    /// blocks the volume reserves from this user count as used, as the Disk row's % does.
    #[must_use]
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
}

/// One tab the host shows, and the root of its process tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabRoot {
    /// The host's stable key for it (its session id).
    pub key: u64,
    /// Key of the tab this pane belongs to (the least key among the panes the host lists in it,
    /// a pane shown in two tabs listed in the first alone; or its own).
    pub tab: u64,
    /// The host's identity for that tab (its `TabId`: never reused while aterm runs, and kept
    /// when the tab moves to another window), which a pane shared into another tab does not
    /// carry; `None` for a session in no tab.
    pub tab_id: Option<u64>,
    /// Its shell's pid; 0 when it has none (a native tab).
    pub shell_pid: u32,
    /// Its label, `Tab 2`.
    pub label: String,
    /// Foreground program name (`claude`, `zsh`), or empty if unknown.
    pub program: String,
    /// Its agent's process group leader, where the host names an agent reading its screen; `None`
    /// where none is named. The leftover pass reads its unique id as the tab's agent start.
    pub agent_leader: Option<u32>,
}

/// Current identity-proven members of one explicitly delegated job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DelegatedJob {
    /// The registered process root.
    pub root: crate::delegated::ProcessKey,
    /// The owning live tab, or no tab after its session closed.
    pub tab: Option<u64>,
    /// Live members attributed by the host's start-checked ownership ledger.
    pub members: Vec<crate::delegated::ProcessKey>,
}

/// The host's app-image proof for one process generation. A kernel PID version also changes on
/// exec and is readable when a system service's resource usage/start record is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedRoot {
    /// The observed process ID.
    pub pid: u32,
    /// Its kernel start token, where readable.
    pub start: Option<u64>,
    /// Its kernel PID version, changed by PID reuse and exec, where readable.
    pub version: Option<i32>,
}

/// A native app-server job's original process, authenticated by its peer identity and source
/// session. Its ordinary process and reaped-child counters belong to the same natural owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeRoot {
    /// The actor's validated process generation.
    pub process: crate::delegated::ProcessKey,
    /// The source session's local tab key, where that session is still present.
    pub tab: Option<u64>,
}

impl SharedRoot {
    fn matches(self, process: &ProcSample) -> bool {
        self.pid == process.pid
            && (self.start.is_some() || self.version.is_some())
            && self.start.is_none_or(|start| process.start == Some(start))
            && self
                .version
                .is_none_or(|version| process.version == Some(version))
    }
}

/// One delegated job's contribution from the same sweep as the normal scopes.
#[derive(Clone, Debug, PartialEq)]
pub struct DelegatedUsage {
    /// The registered process root.
    pub root: crate::delegated::ProcessKey,
    /// Current CPU in cores over the sweep interval.
    pub cores: f32,
    /// Current physical footprint, where readable.
    pub footprint: Option<u64>,
    /// Current process count.
    pub procs: u32,
}

/// Standing of a volume against disk-watch thresholds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskLevel {
    /// At or above the warning line.
    Ok,
    /// Under the warning line.
    Low,
    /// Under the cleaning line: aterm is cleaning.
    Short,
    /// Under the emergency line.
    Emergency,
    /// Out of file slots (the inode emergency line), whatever bytes are free.
    EmergencyFiles,
}

impl DiskLevel {
    /// Deck row [`State`] corresponding to this disk level.
    #[must_use]
    pub fn state(self) -> Option<State> {
        match self {
            Self::Ok => None,
            Self::Low | Self::Short => Some(State::Warn),
            Self::Emergency | Self::EmergencyFiles => Some(State::Critical),
        }
    }
}

/// Directory sized by the disk survey or a cleanup pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hog {
    /// The folder, as the lens prints it (`~/aterm/target`).
    pub path: String,
    /// Its size, bytes.
    pub bytes: u64,
    /// Whether the size was cut short by its bound (printed "≥ 96 GB").
    pub partial: bool,
    /// What would free it, in words.
    pub remedy: String,
}

/// What a network link is: the host's mirror of the probe's kinds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// Wi-Fi.
    Wifi,
    /// Ethernet: built in, USB or Thunderbolt.
    Ethernet,
    /// Cellular data.
    Cellular,
    /// A tunnel, whose bytes are counted again on the link carrying them.
    Vpn,
    /// A bridge, which counts its members' bytes again.
    Bridge,
    /// Anything else.
    #[default]
    Other,
}

impl LinkKind {
    /// Canonical lowercase word for `sys` output.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Wifi => "wifi",
            Self::Ethernet => "ethernet",
            Self::Cellular => "cellular",
            Self::Vpn => "vpn",
            Self::Bridge => "bridge",
            Self::Other => "other",
        }
    }
}

/// An account among the tabs', as the host numbers them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AccountIx(pub u8);

/// One network link's raw counters and their provenance.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkSample {
    /// Its name (`en0`).
    pub name: String,
    /// Interface index, compared across the counter window.
    pub index: u32,
    /// Effective byte-counter source, width and quantization.
    pub byte_counter: ByteCounter,
    /// What it is.
    pub kind: LinkKind,
    /// True when the interface is up and running (`IFF_UP` and `IFF_RUNNING`).
    pub up: bool,
    /// True when the interface is loopback (`IFF_LOOPBACK`).
    pub loopback: bool,
    /// True when traffic on this interface counts toward machine-level totals.
    pub counts: bool,
    /// Raw received bytes; a modulo source is not a lifetime total.
    pub rx: u64,
    /// Raw sent bytes; a modulo source is not a lifetime total.
    pub tx: u64,
    /// Packets received, cumulative.
    pub rx_packets: u64,
    /// Packets sent, cumulative.
    pub tx_packets: u64,
    /// Receive errors, cumulative.
    pub rx_errors: u64,
    /// Send errors, cumulative.
    pub tx_errors: u64,
    /// Inbound packets dropped, cumulative.
    pub rx_drops: u64,
    /// Outbound packets dropped, cumulative (`None` where the OS does not expose it).
    pub tx_drops: Option<u64>,
    /// The link rate the driver reports, bits per second.
    pub rate_bps: Option<u64>,
}

impl LinkSample {
    /// Byte and packet counters kept together for the common delta rule.
    #[must_use]
    pub fn byte_sample(&self) -> ByteSample {
        ByteSample {
            index: self.index,
            counter: self.byte_counter,
            rx: self.rx,
            tx: self.tx,
            rx_packets: self.rx_packets,
            tx_packets: self.tx_packets,
            rate_bps: self.rate_bps,
        }
    }
}

/// Machine-wide TCP socket counts by connection state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TcpCounts {
    /// `ESTABLISHED` connections.
    pub established: u32,
    /// `SYN_SENT` sockets.
    pub syn_sent: u32,
    /// `SYN_RECV` sockets.
    pub syn_recv: u32,
    /// `FIN_WAIT1` sockets.
    pub fin_wait1: u32,
    /// `FIN_WAIT2` sockets.
    pub fin_wait2: u32,
    /// `TIME_WAIT` sockets.
    pub time_wait: u32,
    /// `CLOSE` sockets.
    pub close: u32,
    /// `CLOSE_WAIT` sockets.
    pub close_wait: u32,
    /// `LAST_ACK` sockets.
    pub last_ack: u32,
    /// `LISTEN` sockets.
    pub listen: u32,
    /// `CLOSING` sockets.
    pub closing: u32,
    /// Only `established` and `time_wait` were read, from the kernel's counters: Linux before its
    /// first all-states dump, or once that dump's self-check retired it. Every other state is
    /// unread, not 0 ([`Self::other`]), and `established` may hold `CLOSE_WAIT`.
    pub counters_only: bool,
}

impl TcpCounts {
    /// `n`, a state other than `established` and `time_wait`, when it was read.
    #[must_use]
    pub const fn other(&self, n: u32) -> Option<u32> {
        if self.counters_only { None } else { Some(n) }
    }

    /// [`Self::total`] when every state was read.
    #[must_use]
    pub const fn known_total(&self) -> Option<u32> {
        self.other(self.total())
    }

    /// Total TCP sockets across all states (with [`Self::counters_only`], `established` and
    /// `time_wait` alone).
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

    /// Sockets in handshake states (`SYN_SENT + SYN_RECV`).
    #[must_use]
    pub const fn connecting(&self) -> u32 {
        self.syn_sent.saturating_add(self.syn_recv)
    }

    /// Sockets in teardown states: `FIN_WAIT1`, `FIN_WAIT2`, `TIME_WAIT`, `CLOSE_WAIT`,
    /// `LAST_ACK` and `CLOSING`.
    #[must_use]
    pub const fn closing_total(&self) -> u32 {
        self.fin_wait1
            .saturating_add(self.fin_wait2)
            .saturating_add(self.time_wait)
            .saturating_add(self.close_wait)
            .saturating_add(self.last_ack)
            .saturating_add(self.closing)
    }
}

/// Per-second network throughput, packet, error, and drop rates over the last sample interval.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LinkRates {
    /// Bytes received per second.
    pub rx_bps: f64,
    /// Bytes sent per second.
    pub tx_bps: f64,
    /// Packets received per second.
    pub rx_pps: f64,
    /// Packets sent per second.
    pub tx_pps: f64,
    /// Receive errors per second.
    pub rx_err_ps: f64,
    /// Send errors per second.
    pub tx_err_ps: f64,
    /// Inbound packet drops per second.
    pub rx_drop_ps: f64,
    /// Outbound packet drops per second, where both readings carried `tx_drops`.
    pub tx_drop_ps: Option<f64>,
}

/// One network interface as `sys net` and the Network lens show it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LinkView {
    /// Raw counters, their provenance and attributes from this reading.
    pub sample: LinkSample,
    /// Per-second rates over the last sample window (`None` on a first reading or a counter reset).
    pub rates: Option<LinkRates>,
    /// Why this window does not establish a rate, independently of sample freshness.
    pub rates_why: Option<DeltaError>,
}

/// Live connectivity standing derived from network links, traffic, errors, and TCP sockets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NetState {
    /// Link up and idle (no active established connections or significant traffic).
    #[default]
    Idle,
    /// Link up with active traffic or established connections.
    Online,
    /// Link up with elevated packet errors/drops or high TCP latency.
    Degraded,
    /// No non-loopback network link is up.
    Offline,
}

impl NetState {
    /// Canonical lowercase word (`"idle"`, `"online"`, `"degraded"`, `"offline"`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Online => "online",
            Self::Degraded => "degraded",
            Self::Offline => "offline",
        }
    }

    /// Title-cased UI badge word (`"Idle"`, `"Online"`, `"Degraded"`, `"Offline"`).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Online => "Online",
            Self::Degraded => "Degraded",
            Self::Offline => "Offline",
        }
    }
}

/// Network interfaces, throughput rates, connectivity state, latency, and TCP socket counts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetView {
    /// Every interface read on this sample, in probe order.
    pub links: Vec<LinkView>,
    /// Rates summed across counted (`counts == true`) interfaces over the last sample window.
    pub rates: Option<LinkRates>,
    /// Why a counted link prevents a complete machine rate.
    pub rates_why: Option<DeltaError>,
    /// Sample window duration in nanoseconds when `rates` is present.
    pub window_ns: Option<u64>,
    /// Machine-wide TCP socket counts by state, where readable.
    pub tcp: Option<TcpCounts>,
    /// Passive kernel TCP smoothed RTT median in milliseconds across established remote peers
    /// (on Linux, at most 64 followed connections, the first to each destination prefix first:
    /// `aterm_sysprobe::TcpSummary::srtt_ms`).
    pub srtt_ms: Option<f32>,
    /// Live connectivity state across counted links.
    pub state: NetState,
    /// Open socket descriptors held by `aterm` and its tab subtrees, where read.
    pub aterm_sockets: Option<u32>,
    /// Open socket descriptors held by each tab (`TabRoot::key`), where read.
    pub tab_sockets: Vec<(u64, u32)>,
}

/// This Mac's network place (where it is, whether the internet answers, how much of the ALab
/// LAN it reaches), as aterm-netplace words it: the host fills it from the place record and this
/// crate prints it as it came, so `sys` says what every other reporter says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceView {
    /// The `place` line's words, after its label.
    pub line: String,
    /// The `place` JSON object's fields, in order.
    pub fields: Vec<(&'static str, PlaceValue)>,
    /// The kind of network the place is, as netplace typed it: what the Machine row's place
    /// glyph and the situation's place stripe draw (A4 §8). The same place as [`Self::fields`],
    /// typed where the deck reads it.
    pub kind: PlaceKind,
}

/// The kind of network this Mac's place is on (netplace's `Kind`, or none), as the Machine row
/// draws it (A4 §8): one glyph per kind, the LAN's whatever its medium.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceKind {
    /// A LAN or Wi-Fi network (netplace's `Kind::Lan`), the ALab LAN or another.
    Lan,
    /// A phone's Personal Hotspot (by the path monitor's `expensive` flag on a non-cellular
    /// link, or by its address range).
    Hotspot,
    /// A cellular link.
    Cellular,
    /// A tunnel holds the default route.
    Vpn,
    /// No network.
    Offline,
    /// Not known yet: netplace is still checking and has no kind.
    Checking,
}

impl PlaceKind {
    /// The word `sys` and `await sys` say: `lan`, `hotspot`, `cellular`, `vpn`, `offline`,
    /// `checking`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Lan => "lan",
            Self::Hotspot => "hotspot",
            Self::Cellular => "cellular",
            Self::Vpn => "vpn",
            Self::Offline => "offline",
            Self::Checking => "checking",
        }
    }
}

/// One value of a [`PlaceView`]'s JSON object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaceValue {
    /// A word or an address.
    Text(String),
    /// A count or a unix-ms time.
    Count(u64),
    /// A yes or no.
    Flag(bool),
    /// Not known; the word is one of the `sys` unavailable words (`not_sampled`).
    Absent(&'static str),
    /// A nested object, its fields in order.
    Object(Vec<(&'static str, PlaceValue)>),
}

/// Category of vertical event marker overlaid on a multi-scale time-series strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StripEventKind {
    /// System sleep/wake boundary.
    SleepWake,
    /// Critical memory pressure, the top thermal level, or GPU fault recovery spike.
    PressureSpike,
    /// Instance or session restart.
    Restart,
    /// Abnormal crash or fatal signal termination.
    Crash,
}

/// Vertical event marker anchored to a wall-clock timestamp on a time-series strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StripEvent {
    /// Wall-clock timestamp when the event occurred (Unix ms).
    pub at_unix_ms: i64,
    /// Visual and semantic category of the marker.
    pub kind: StripEventKind,
    /// Short human-readable label (e.g. `"SIGSEGV"`, `"GPU reset"`, `"woke from sleep"`).
    pub label: String,
    /// Immutable original producer authority; a local key alone grants no access.
    pub scope: crate::events::EventScope,
    /// An interval captured by the original producer, or none for a point marker.
    pub span: Option<crate::events::EventSpan>,
}

/// What the machine's task tables hold and the most each may hold, as the host read them
/// (`aterm_sysprobe::TaskRoom`): the Processes row's count and limit, and the Room's four
/// pairs (`docs/DESIGN-stats-deck-flow-2026-11-02.md` §8.1). `None` where the OS does not say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaskRoom {
    /// Processes on the machine (tasks on Linux, where each thread takes a pid).
    pub procs: Option<u64>,
    /// The most [`Self::procs`] may be (on Linux the smaller of `pid_max` and `threads-max`).
    pub procs_max: Option<u64>,
    /// Processes this user runs.
    pub user_procs: Option<u64>,
    /// The most one user may run; `None` where unknown or unlimited.
    pub user_procs_max: Option<u64>,
    /// Threads on the machine.
    pub threads: Option<u64>,
    /// The most [`Self::threads`] may be.
    pub threads_max: Option<u64>,
    /// Open files on the machine.
    pub files: Option<u64>,
    /// The most [`Self::files`] may be.
    pub files_max: Option<u64>,
}

/// Disk-watch and directory-survey status copied from the harness.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiskFacts {
    /// The home volume's level; `None` when the watch has not judged it.
    pub level: Option<DiskLevel>,
    /// The warning line, bytes free.
    pub warn: Option<u64>,
    /// The cleaning line, bytes free.
    pub floor: Option<u64>,
    /// The emergency line, bytes free.
    pub emergency: Option<u64>,
    /// Sessions stuck on a full disk.
    pub stuck: u16,
    /// Other volumes the watch judges: name, level and bytes free.
    pub others: Vec<(String, DiskLevel, u64)>,
    /// When the watch's beat stalled (unix ms), while it is stalled.
    pub stalled_since: Option<i64>,
    /// Bytes freed today, last cleanup timestamp (Unix ms), and summary.
    pub freed_today: Option<(u64, i64, String)>,
    /// Largest sized directories, descending by size.
    pub hogs: Vec<Hog>,
    /// When `hogs` were sized (unix ms).
    pub hogs_at: Option<i64>,
    /// Whether the watch runs (the harness is on).
    pub watch: bool,
    /// True while a directory-sizing survey is running.
    pub sizing: bool,
    /// What the disk journal says the software did, newest first, at most 8 runs: each run of
    /// adjacent rows of one kind by one session is one deed
    /// (`docs/DESIGN-disk-custody-2026-11-12.md` §4).
    pub deeds: Vec<DiskDeed>,
    /// The journal's removals by hour: each hour's start (unix ms) and the bytes removed in it,
    /// oldest first, within the last 3 weeks.
    pub removals: Vec<(i64, u64)>,
    /// Who holds home's volume, as the host last gathered `sys disk held`'s rows; `None` where it
    /// gathered none.
    pub held: Option<Arc<crate::sys::held::Holdings>>,
    /// Whole seconds from that gather to this reading: the rows are that old, since the host
    /// gathers them at its own pace and not while every window is hidden. `None` where it
    /// gathered none.
    pub held_age_s: Option<u64>,
    /// DC-04's rebuildable budget as the watch last sized it on home's volume
    /// ([`crate::sys::BudgetRead`]): its cap, the kept and rebuildable bytes and the target the
    /// next pass holds the rebuildable bytes to. A reading, never a level.
    pub budget: crate::sys::BudgetRead,
    /// Whether the probe thread has read the disk journal ([`Self::deeds`],
    /// [`Self::removals`]): it read the file, or found there is none. `false` before its first
    /// read, so an empty list is not drawn as an empty journal.
    pub journal_read: bool,
    /// Whether the disk watch's last look read removal as on (`disk.auto_free_gib` above 0);
    /// `None` before a look of this process read it. With it off, no pass of aterm's removes
    /// anything, the budget's included.
    pub cleanup: Option<bool>,
    /// Where the budget's last pass on home's volume ended unable to meet its target under the
    /// cap [`Self::budget`] reads, its memo still holding the next pass off: the unix second it
    /// ended. A pass plans again once a live session ends or starts, the rebuildable bytes grow,
    /// or a day has passed.
    pub budget_unmet_unix: Option<i64>,
}

impl DiskFacts {
    /// The most used bytes DC-04's budget holds the volume to, where the watch sized one.
    #[must_use]
    pub fn cap(&self) -> Option<u64> {
        match self.budget {
            crate::sys::BudgetRead::Sized(b) => Some(b.cap),
            _ => None,
        }
    }
}

/// Whose a [`DiskDeed`] is: the subject of what it records. That is who did it, except where the
/// row names the session a free-space ask concerns ([`DeedKind::Asked`]): that session is its
/// subject, and aterm asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeedBy {
    /// aterm itself: its disk watch, its cleaner, its reserve or its free-space ask.
    Aterm,
    /// A session running in a tab: the tab ([`TabRoot::tab`]) while it is open, `None` once the
    /// session is not one of this window set's.
    Tab(Option<u64>),
}

/// What a [`DiskDeed`] records, as the disk journal's row says it, its subject the deed's
/// [`DeedBy`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeedKind {
    /// A pass checked the disk (a `report` row).
    Checked,
    /// It began removing what is named (an intent with no outcome yet).
    Began(String),
    /// It removed what is named.
    Removed(String),
    /// It tried to remove what is named and could not.
    CouldNotRemove(String),
    /// It refused to remove what is named.
    Refused(String),
    /// A pass kept what is named: nothing of it went (a sweep's or a worktree step's `kept`).
    Kept(String),
    /// The build-cache cleaner stopped.
    Stopped,
    /// It released the reserve file.
    ReserveReleased,
    /// It wrote the reserve file.
    ReserveWritten,
    /// It could not change the reserve file.
    ReserveRefused,
    /// It ran short of disk space (a `poke` row): a session's screen said the disk is full, or
    /// the session waits for more free space than the volume has, or a write of aterm's failed
    /// for want of space.
    RanShort,
    /// A step of the free-space ask (a `free-ask` row): its `said` word, or `step` and the
    /// turn-break step's word. Its subject is the session the row names, else aterm.
    Asked(String),
    /// A row of a kind this list does not know: its kind, and its action when it has one.
    Other(String),
}

/// One run of the disk journal's rows: adjacent rows of one kind by one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskDeed {
    /// When its newest row was written, unix ms.
    pub at_unix_ms: i64,
    /// How many rows it stands for.
    pub n: u32,
    /// What it records.
    pub kind: DeedKind,
    /// Whose it is.
    pub by: DeedBy,
    /// The bytes its rows name, summed; `None` when none names any.
    pub bytes: Option<u64>,
    /// The bytes free its newest row read; `None` when it read none.
    pub free: Option<u64>,
}

/// Cumulative token totals for one agent session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabTokens {
    /// The tab it runs in (its [`TabRoot::key`]).
    pub key: u64,
    /// The session's id; the host sends each session once.
    pub session: String,
    /// Output tokens, cumulative.
    pub out_total: u64,
    /// Input tokens, cumulative (cache reads included).
    pub in_total: u64,
    /// Transcripts the fold has not read yet.
    pub unread: usize,
    /// Whether more transcripts wait beyond `unread`.
    pub unread_more: bool,
    /// Whether the totals came from a complete fold.
    pub complete: bool,
    /// Monotonic timestamp (ms) when these totals were folded, or `None` if folded every sample.
    pub read_ms: Option<i64>,
}

/// Plan usage snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanFact {
    /// The binding window, in words (`weekly`, `Opus weekly`, `5-hour`).
    pub window_word: String,
    /// Its share used, tenths of a percent.
    pub used_x10: u16,
    /// When it was read (unix ms).
    pub read_at: i64,
    /// Whose account it is.
    pub account: AccountIx,
}

/// One sample of everything, from the host's probe thread.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sample {
    /// This process's bracketed thread CPU observations, read only where a reader of them asked
    /// ([`Self::self_cpu_read`]), or the host's reader of them reads nothing and says why at once
    /// (off macOS, `unsupported`); a missing or failed read clears continuity rather than carrying
    /// an earlier attribution.
    pub self_cpu: Option<Result<crate::self_cpu::OwnCpuCut, &'static str>>,
    /// Whether a reader of this process's thread CPU asked the reading for it
    /// ([`Self::self_cpu`]); a reading nothing asks leaves that `None`, unless the host's reader
    /// reads nothing and says why at once (off macOS).
    pub self_cpu_read: bool,
    /// End of the frontend sweep on the two native, cross-process CPU clocks.
    pub native_cpu_cut: NativeCpuCut,
    /// Authenticated final process counters collected by their original native owning wait.
    /// They stay separate from live rows so a reused PID cannot replace the ended generation.
    pub native_terminals: Vec<NativeTerminal>,
    /// The current native origin or custodian ownership reading is incomplete.
    pub native_incomplete: bool,
    /// Historical native CPU evidence was lost or could not be positioned in a window.
    /// This does not make a later complete process/coalition footprint inventory unreadable.
    pub native_source_lost: bool,
    /// Natural ownership evidence was lost or could not be revalidated on this reading.
    pub ownership_incomplete: bool,
    /// Independent native hardware and power-policy observations.
    pub hardware: Option<crate::hardware::Sample>,
    /// Whether the reading asked its sweep for each process's scheduling
    /// ([`ProcSample::scheduling`]), which a reading no reader of it asks leaves `None`.
    pub scheduling_read: bool,
    /// The host's monotonic clock, ns.
    pub at_ns: u64,
    /// Every logical core's cumulative ticks; `None` where unreadable.
    pub cores: Option<Vec<Ticks>>,
    /// The cluster layout (read once by the host, passed every time).
    pub clusters: Vec<ClusterSpec>,
    /// Memory; `None` where unreadable.
    pub mem: Option<MemSample>,
    /// The cgroup CPU quota (`cpu.max`), millicores (1000 per core); `None` when unlimited or off
    /// cgroup v2 (`aterm_sysprobe::Capacity::cpu_millicores`).
    pub cpu_millicores: Option<u32>,
    /// The cgroup memory limit (`memory.max`), bytes; `None` when unlimited or off cgroup v2
    /// (`aterm_sysprobe::Capacity::mem_bytes`).
    pub mem_bytes: Option<u64>,
    /// The GPU; `None` where unreadable.
    pub gpu: Option<GpuSample>,
    /// Every GPU user client's GPU time, where the host read them this sample (not every one).
    pub gpu_clients: Option<crate::gpu_clients::GpuClientsSample>,
    /// The volume the focused tab works on; `None` where unreadable.
    pub volume: Option<VolumeSample>,
    /// One process sweep; `None` when the host skipped it, or its lane stalled
    /// ([`Self::stalled`]).
    pub procs: Option<Vec<ProcSample>>,
    /// The reader lanes this sample found stalled ([`crate::lanes`]): the figures each feeds are
    /// unknown at it, and every other figure is read.
    pub stalled: Vec<crate::lanes::LaneStall>,
    /// This aterm instance's pid.
    pub self_pid: u32,
    /// Every tab, in the host's order.
    pub tabs: Vec<TabRoot>,
    /// Explicit jobs whose process identities were validated in this sweep.
    pub delegated: Vec<DelegatedJob>,
    /// Generation-checked processes running this app's loaded executable or a validated signed
    /// copy outside its instance tree.
    /// Their descendants are shared app work; a current tab or instance lineage takes priority.
    pub shared_roots: Vec<SharedRoot>,
    /// Tab keys whose current shell generation could not be proved by the host.
    pub unproved_tabs: Vec<u64>,
    /// Authenticated original jobs admitted by the native app server outside tab process trees.
    pub native_roots: Vec<NativeRoot>,
    /// Wall-clock timestamp of the sample in Unix ms: on a reading with a process sweep, read
    /// before the sweep lists the processes, so a process born at or after one reading's is in no
    /// earlier reading's sweep ([`crate::explain::reap`]).
    pub at_unix_ms: i64,
    /// Monotonic timestamp of the sample in ms (pauses during system sleep).
    pub mono_ms: u64,
    /// Active sampling cadence in ms: the gap rule's ([`GAP_CADENCES`]), and on the deck's own
    /// readings the slot each stands for in the rings ([`Self::period_ms`]).
    pub cadence_ms: u32,
    /// A control worker's pull (`sys` and the like), taken between the deck's own readings: it
    /// stands for an instant in the rings, so the deck's reading after it keeps its whole slot,
    /// and its cadence serves the gap rule alone (A1 §9, F4′).
    pub pull: bool,
    /// True when only machine counters were sampled (process sweep omitted).
    pub machine_only: bool,
    /// The 1-minute load average.
    pub load1: Option<f64>,
    /// The machine's runnable threads, every user's, as the host read them with the task tables
    /// ([`crate::runq`]); the over rule reads their 10 s mean in place of [`Self::load1`].
    pub run_queue: crate::runq::RunQueueRead,
    /// What the host read of threads in an uninterruptible wait ([`crate::blockage`]).
    pub dstate: crate::blockage::DStateRead,
    /// Whether the host's process sweep reads process states, which the zombie count needs.
    pub zombie_read: crate::blockage::ZombieRead,
    /// KiB swapped in plus out since boot.
    pub swapped_kib: Option<u64>,
    /// Every network link's counters.
    pub links: Option<Vec<LinkSample>>,
    /// Machine-wide TCP socket counts by state, where readable.
    pub tcp: Option<TcpCounts>,
    /// The machine's task tables: the Processes row's counts and limits.
    pub room: TaskRoom,
    /// The machine's process start counter, read at every reading where one reads: macOS's
    /// coalitions while their lane is on, Linux's forks ([`crate::starts`]).
    pub starts: Option<crate::starts::StartCount>,
    /// This process's user: only its processes' writes are counted.
    pub self_uid: Option<u32>,
    /// The disk watch's facts; `None` where the host read none.
    pub disk: Option<DiskFacts>,
    /// Every agent session's token totals, one row per session.
    pub tokens: Vec<TabTokens>,
    /// Provider observations from the individual terminal sessions on this machine.
    pub pane_token_observations: Vec<crate::pane_tokens::Observation>,
    /// The front tab's account's plan usage, while current.
    pub plan: Option<PlanFact>,
    /// The thermal state, where read.
    pub thermal: Option<Thermal>,
    /// The documented thermal pressure level (A4 §4), as the host read it when its notification
    /// fired: `None` on a system with no documented level, `Some(Err)` where its reader failed.
    pub thermal_pressure: Option<Result<crate::heat::ThermalPressure, &'static str>>,
    /// The chip's clock range, read once by the host from the power manager's tables: the heat
    /// chain's clock bounds ([`crate::heat::judge`]). `None` on a system with no such table,
    /// `Some(Err)` where the read gave none.
    pub clock_range: Option<Result<crate::heat::ClockRange, &'static str>>,
    /// The system's own watts readers' readings (Linux: RAPL, NVML), which stand where no
    /// coalition chain reads ([`crate::heat::HostWatts`]).
    pub host_watts: crate::heat::HostWatts,
    /// The Neural Engine reader's latest reading, or the self-check that retired it; `None`
    /// where no reader runs (every OS but macOS). The reading may be older than this sample:
    /// aterm's host reads the engine once, as its reader starts.
    pub ane: Option<Result<AneSample, AneCheck>>,
    /// The 1, 5 and 15 minute load averages, where read.
    pub load: Option<[f64; 3]>,
    /// Whether the process sweep stopped at its cap.
    pub procs_truncated: bool,
    /// Resource coalition counters for the split, on split ticks only (the host reads them at
    /// every reading for [`Self::starts`], and hands the split the same read on its ticks).
    pub coalitions: Option<crate::explain::CoalSample>,
    /// Coalition lead process identities read alongside [`Self::coalitions`].
    pub leads: Vec<crate::explain::LeadIdentity>,
    /// The machine's timeline, with [`Self::coalitions`].
    pub timeline: Option<crate::explain::TimelineSample>,
    /// Status of the host's coalition probe lane.
    pub coal_lane: crate::explain::CoalLane,
    /// Hardware processor brand/chip string (e.g. `"Apple M5 Max"`), where known.
    pub cpu_brand: Option<String>,
    /// Passive kernel TCP smoothed RTT median in milliseconds across established remote peers
    /// (on Linux, at most 64 followed connections, the first to each destination prefix first:
    /// `aterm_sysprobe::TcpSummary::srtt_ms`).
    pub tcp_srtt_ms: Option<f32>,
    /// Open socket descriptors across `aterm` and its tab subtrees, where sampled.
    pub aterm_sockets: Option<u32>,
    /// Open socket descriptors per tab (`TabRoot::key`), where sampled.
    pub tab_sockets: Vec<(u64, u32)>,
    /// Explicit time-series vertical event markers recorded by the host on this sample.
    pub note_events: Vec<StripEvent>,
    /// Session generations captured with these original sample roots, for event privacy.
    pub event_sources: Vec<crate::events::SessionScope>,
    /// Explicit wall-clock discontinuities recorded by the host on this sample.
    pub note_gaps: Vec<TimeGap>,
    /// The readable physical devices under home's volume and the front tab's, in request order,
    /// and their I/O counters ([`crate::units::BlockSample`]). An unread device has no entry.
    pub block: Vec<crate::units::BlockSample>,
    /// Home's device resolved in the same block read, before unread devices were omitted;
    /// `None` where home's mount or device could not be resolved. This supplies no counters.
    pub home_device_id: Option<String>,
    /// The machine's situation and type as the host's readers last heard them
    /// ([`crate::situation`]); unread throughout where the host read none.
    pub situation: crate::situation::Situation,
    /// The kind of netplace's place as the host read it for this reading, the one it sets on
    /// [`Snapshot::place`]: what the strip's situation spans keep ([`crate::situation::Span`]).
    /// `None` where the host read no place.
    pub place: Option<PlaceKind>,
    /// What the jobs engine's newest pass found stuck, as the host held it when it took this
    /// reading ([`crate::blockage::StuckWork`]); `None` until a pass has run.
    pub stuck_work: Option<crate::blockage::StuckWork>,
    /// Each volume's Spotlight indexing as the host's `mdutil` cadence last read it
    /// ([`crate::spotlight::Cadence`]); `None` unread.
    pub spotlight_volumes: Option<crate::spotlight::VolumesRead>,
}

/// End of a frontend sweep on clocks shared with native owning waits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeCpuCut {
    /// Continuous nanoseconds since boot, including sleep; never the frontend's local epoch.
    pub continuous_ns: Option<u64>,
    /// Kernel boot clock in the same platform-specific units as [`ProcSample::start`].
    /// macOS uses Mach ticks and Linux uses boot nanoseconds.
    pub boot: Option<u64>,
}

/// Immutable source timing retained with a final native receipt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeCpuWindow {
    /// Frontend cut preceding the receipt's first eligible delivery.
    pub previous: NativeCpuCut,
    /// Frontend cut of the receipt's first eligible delivery.
    pub current: NativeCpuCut,
    /// Exit confirmation before final reading and owning wait, continuous boot nanoseconds.
    pub exited_ns: Option<u64>,
}

/// A final native child reading, carried once from the process that owns its wait.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeTerminal {
    /// Source timing; rate eligibility is checked against the model's adopted whole sweeps.
    pub timing: NativeCpuWindow,
    /// Owning daemon identity and terminal sequence form the journal cursor.
    pub host: crate::delegated::ProcessKey,
    /// Monotonic terminal sequence within this daemon generation.
    pub seq: u64,
    /// Exact final own and reaped-child counters, with the child's original kernel start.
    /// `state` is zombie; unavailable counters remain `None`, never an invented split.
    pub process: ProcSample,
    /// Exact combined own and waited-descendant CPU collected by the original wait, when read.
    /// It remains an aggregate if the final per-process split was unavailable.
    pub wait_cpu_ns: Option<u64>,
    /// Owning live tab key, or aterm's own scope after its tab closed.
    pub tab: Option<u64>,
}

impl Sample {
    /// The slot this reading stands for in the rings, ms ([`crate::ring::Taken`]): its cadence,
    /// or 0, an instant, for a pull ([`Self::pull`]).
    #[inline]
    #[must_use]
    pub const fn period_ms(&self) -> u32 {
        if self.pull { 0 } else { self.cadence_ms }
    }
}

/// One core's load.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CoreView {
    /// Busy fraction (`0..=1`): raw last interval and smoothed fast average.
    pub load: TrendView,
    /// This interval had a positive total-counter delta; otherwise `load` is held for drawing.
    pub measured: bool,
}

impl CoreView {
    /// Current utilization, excluding a held drawing value.
    #[must_use]
    pub fn current(&self) -> Option<f32> {
        self.measured.then_some(self.load.raw)
    }
}

/// One cluster's cores.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClusterView {
    /// The OS's name.
    pub name: String,
    /// Short cluster label (`P`, `E`, `S`, `M2`, or `CPU`).
    pub short: String,
    /// Indices into [`CpuView::cores`].
    pub cores: Vec<usize>,
}

/// The CPU.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CpuView {
    /// Every logical core, by logical id.
    pub cores: Vec<CoreView>,
    /// The clusters, fastest first, covering every core once.
    pub clusters: Vec<ClusterView>,
    /// Cores-worth busy over the whole machine.
    pub busy: TrendView,
    /// The busy cores-worth over the last two minutes.
    pub history: Vec<Option<f32>>,
    /// Per-core busy fraction history over the last minute, oldest first.
    pub core_history: Vec<Vec<Option<f32>>>,
}

impl CpuView {
    /// Current busy cores, only when every core's counters advanced.
    #[must_use]
    pub fn current_busy(&self) -> Option<f32> {
        (!self.cores.is_empty()).then_some(())?;
        self.cores.iter().map(CoreView::current).sum()
    }

    /// Current busy cores in a cluster, only when all of its counters advanced.
    #[must_use]
    pub fn current_cluster_busy(&self, cluster: &ClusterView) -> Option<f32> {
        (!cluster.cores.is_empty()).then_some(())?;
        cluster
            .cores
            .iter()
            .map(|&i| self.cores.get(i)?.current())
            .sum()
    }

    /// Logical cores.
    #[must_use]
    pub fn total(&self) -> usize {
        self.cores.len()
    }
}

/// The GPU.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuView {
    /// Device utilization, `0..=1`.
    pub load: TrendView,
    /// GPU cores, where known.
    pub cores: Option<u16>,
    /// A short name.
    pub name: Option<String>,
    /// The utilization over the last two minutes, `0..=1`.
    pub history: Vec<Option<f32>>,
    /// The sample's figures as read.
    pub sample: GpuSample,
    /// Client time by owner, while client shares are held and the sweep ran.
    pub split: Option<crate::gpu_clients::GpuSplit>,
}

/// Memory.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemView {
    /// The classes as read.
    pub sample: MemSample,
    /// Used bytes, smoothed (as `f32` GiB).
    pub used_gib: TrendView,
    /// Used GiB over the last two minutes.
    pub history: Vec<Option<f32>>,
    /// Paging per second; `None` until two readings with counters exist.
    pub rates: Option<MemRates>,
}

impl MemView {
    /// True when pressure is warn/critical and swap traffic is at or above [`THRASH_SWAP_KIB_S`].
    #[must_use]
    pub fn thrashing(&self) -> Option<bool> {
        let rank = self.sample.pressure.rank()?;
        let rates = self.rates?;
        Some(rank >= 1 && rates.swap_bps() >= THRASH_SWAP_KIB_S * 1024.0)
    }
}

/// One scope: the machine, aterm, or a tab.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScopeView {
    /// Published process-counter growth rate; the machine scope has independent tick figures.
    pub cores: TrendView,
    /// Basis of process/scope CPU rates and their history, independent of unread coverage.
    /// The machine's tick-derived `cores` has no process accounting metadata.
    pub cpu_accounting: Option<crate::published_cpu::CpuAccounting>,
    /// Current physical footprint, bytes: a proved app coalition total where available,
    /// otherwise the readable processes' sum. Separate from physical VM classes.
    pub footprint: Option<u64>,
    /// Unread memory contributions or ownership/accounting gaps. A readable footprint is a
    /// lower bound while this is nonzero, independently of CPU coverage.
    pub footprint_unread: u32,
    /// Footprint, smoothed (as `f32` GiB), for its motion.
    pub footprint_gib: TrendView,
    /// Processes in the scope.
    pub procs: u32,
    /// Cores-worth over the last two minutes.
    pub history: Vec<Option<f32>>,
    /// The busiest process in the scope this sweep, with its cores-worth.
    pub top: Option<(String, f32)>,
    /// Reported runnable-minus-CPU counter growth per second; not physical waiting.
    pub wait: Option<f32>,
    /// Storage read rate in bytes/s across readable members in this scope.
    pub read_bps: Option<f64>,
    /// Storage write rate in bytes/s across readable members in this scope.
    pub write_bps: Option<f64>,
    /// Members whose CPU time the OS refused.
    pub refused: u32,
    /// The members' GPU share of the device (`1.0` the whole GPU), while client shares are held.
    pub gpu: Option<f32>,
}

impl ScopeView {
    /// Combined storage read + write rate in bytes/s, where either counter was read.
    #[must_use]
    pub fn io_bps(&self) -> Option<f64> {
        plus(self.read_bps, self.write_bps, |a, b| a + b)
    }

    /// This scope and `other`, a disjoint one read in the same sweeps (two panes of one tab), as
    /// one: counts, figures and averages add (`mad` too, as a bound), a figure only one of them
    /// read is that one's, and the busier top process wins.
    #[must_use]
    pub fn sum(&self, other: &Self) -> Self {
        let trend = |a: TrendView, b: TrendView| TrendView {
            raw: a.raw + b.raw,
            level: a.level + b.level,
            slow: a.slow + b.slow,
            mad: a.mad + b.mad,
        };
        let n = self.history.len().max(other.history.len());
        let at = |h: &[Option<f32>], i: usize| (i + h.len()).checked_sub(n).and_then(|j| h[j]);
        Self {
            cores: trend(self.cores, other.cores),
            cpu_accounting: self.cpu_accounting.or(other.cpu_accounting),
            footprint: plus(self.footprint, other.footprint, u64::saturating_add),
            footprint_unread: self.footprint_unread.saturating_add(other.footprint_unread),
            footprint_gib: trend(self.footprint_gib, other.footprint_gib),
            procs: self.procs.saturating_add(other.procs),
            history: (0..n)
                .map(|i| plus(at(&self.history, i), at(&other.history, i), |a, b| a + b))
                .collect(),
            top: match (&self.top, &other.top) {
                (Some(a), Some(b)) if b.1 > a.1 => Some(b.clone()),
                (a, b) => a.clone().or_else(|| b.clone()),
            },
            wait: plus(self.wait, other.wait, |a, b| a + b),
            read_bps: plus(self.read_bps, other.read_bps, |a, b| a + b),
            write_bps: plus(self.write_bps, other.write_bps, |a, b| a + b),
            refused: self.refused.saturating_add(other.refused),
            gpu: plus(self.gpu, other.gpu, |a, b| a + b),
        }
    }
}

/// `a` and `b` combined by `f`, or whichever was read.
fn plus<T>(a: Option<T>, b: Option<T>, f: impl FnOnce(T, T) -> T) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(f(a, b)),
        (a, b) => a.or(b),
    }
}

/// Innermost attribution scope for a process row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowScope {
    /// The aterm instance itself.
    SelfProc,
    /// A tab, by the host's key.
    Tab(u64),
    /// aterm's other descendants (its helpers).
    Helper,
    /// Other instances and detached services proven to run this app or a validated signed copy.
    Shared,
    /// The user's own processes outside aterm.
    Others,
    /// Other users' processes.
    Services,
    /// Outside aterm, with ownership the reader could not establish.
    Unknown,
}

impl RowScope {
    /// The word `scope=` prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::SelfProc => "self",
            Self::Tab(_) => "tab",
            Self::Helper => "helpers",
            Self::Shared => "shared-aterm",
            Self::Others => "others",
            Self::Services => "services",
            Self::Unknown => "unknown",
        }
    }

    /// Whether the process is aterm's.
    #[must_use]
    pub fn in_aterm(self) -> bool {
        matches!(
            self,
            Self::SelfProc | Self::Tab(_) | Self::Helper | Self::Shared
        )
    }
}

/// Raw process CPU accounting, retained separately from interval rates.
/// Reaped-child totals include descendants and may overlap CPU already observed
/// while those children were alive; they are not added to the displayed self rate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuAccounting {
    /// Published counters do not establish CPU execution within a read interval.
    pub published: crate::published_cpu::CpuAccounting,
    /// The process's own cumulative CPU nanoseconds.
    pub self_ns: Option<u64>,
    /// Kernel-reported cumulative CPU of children this process has reaped.
    pub reaped_children_ns: Option<u64>,
    /// Whether Darwin may double-charge unwaited children to that counter.
    pub auto_reaps_children: Option<bool>,
    /// Monotonic time when these process counters were read.
    pub sampled_at_ns: Option<u64>,
    /// Kernel start identity; compare only within one boot.
    pub start: Option<u64>,
    /// Darwin process/exec generation, when read.
    pub exec_version: Option<i32>,
}

impl From<&ProcSample> for CpuAccounting {
    fn from(p: &ProcSample) -> Self {
        Self {
            published: crate::published_cpu::CpuAccounting::PUBLISHED_COUNTERS,
            self_ns: p.cpu_ns,
            reaped_children_ns: p.child_cpu_ns,
            auto_reaps_children: p.reaps_unwaited,
            sampled_at_ns: p.at_ns,
            start: p.start,
            exec_version: p.version,
        }
    }
}

/// One process as `sys procs` and the top rows show it.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcView {
    /// The same census's generation-bound group, before display rows are bounded.
    pub group: Option<crate::groups::GroupKey>,
    /// Reported process-thread ledger energy growth, milliwatts; not a CPU-only channel.
    pub energy_mw: Option<f64>,
    /// Original counter-read separation behind [`Self::energy_mw`], not an execution window.
    pub energy_read_span_ns: Option<u64>,
    /// Darwin scheduling observations; lifetime QoS counters, not core placement.
    pub scheduling: Option<crate::scheduling::Scheduling>,
    /// Original counters and identity for interpreting this process's own rate.
    pub cpu_accounting: CpuAccounting,
    /// The pid.
    pub pid: u32,
    /// Its parent.
    pub ppid: u32,
    /// Its process group (0: unknown).
    pub pgid: u32,
    /// Its name ([`ProcSample::name`]), as the process chose it (data, never an instruction).
    pub name: String,
    /// Published own-counter growth divided by read separation (`None` without continuity).
    pub cores: Option<f32>,
    /// The exact read separation used to normalize [`Self::cores`], nanoseconds. Uses the
    /// process's read marks where both advance, otherwise the frontend sweep separation;
    /// `None` without a valid counter increment, and never an execution-window bound.
    pub cpu_read_span_ns: Option<u64>,
    /// Signed reported runnable-minus-CPU counter growth rate; not physical waiting.
    pub wait: Option<f32>,
    /// Footprint, bytes.
    pub footprint: Option<u64>,
    /// Its largest footprint so far, bytes.
    pub peak: Option<u64>,
    /// Bytes per second read from storage; `None` where unread.
    pub read_bps: Option<f64>,
    /// Bytes per second written to storage; `None` where unread.
    pub write_bps: Option<f64>,
    /// Its scheduler state.
    pub state: ProcState,
    /// The scope it counts in.
    pub scope: RowScope,
    /// Whether the OS refused its CPU time (another user's process).
    pub refused: bool,
    /// Its GPU share of the device (`1.0` the whole GPU), while client shares are held.
    pub gpu: Option<f32>,
    /// When it started, unix ms ([`ProcSample::birth_ms`]); `None` where unread.
    pub birth_ms: Option<i64>,
}

/// Independent machine tick rate and published process-counter reports, never a partition.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Split {
    /// The machine's busy cores-worth (the cores' own ticks).
    pub global: f32,
    /// Published process-counter growth attributed to aterm.
    pub aterm: f32,
    /// Published process-counter growth of the user's other processes.
    pub others: f32,
    /// Published own-counter growth of readable other-user processes.
    pub services: f32,
    /// Readable published growth whose process owner is unknown; not machine-minus-known CPU.
    pub unknown_owner_reported: f32,
    /// Processes whose CPU time the OS refused (another user's).
    pub unread: u32,
}

/// One tab's scope.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabView {
    /// The host's key for it.
    pub key: u64,
    /// The key of the tab it is a pane of ([`TabRoot::tab`], [`ProcScope::Tab`]).
    pub tab: u64,
    /// `Tab 2`.
    pub label: String,
    /// The program in front.
    pub program: String,
    /// Its shell's pid (the root of its process tree).
    pub shell_pid: u32,
    /// Its load.
    pub scope: ScopeView,
}

/// Which scope a busy process is attributed to ([`Snapshot::top`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProcScope {
    /// One of this instance's tabs, by its key ([`TabRoot::tab`]): a process in any pane of a
    /// split tab is that tab's, so its rows group and its busiest program stays named as one.
    Tab(u64),
    /// aterm itself (or a child that belongs to no tab).
    Aterm,
    /// Shared app work outside this instance's process and tab trees.
    Shared,
    /// Everything else on the machine.
    Other,
}

/// One of the busiest processes this sweep.
#[derive(Clone, Debug, PartialEq)]
pub struct TopProc {
    /// Its name.
    pub name: String,
    /// Cores-worth over the sweep interval.
    pub cores: f32,
    /// Its physical footprint, where read.
    pub footprint: Option<u64>,
    /// Whose it is.
    pub scope: ProcScope,
}

/// Busiest program in this sweep, aggregating all processes sharing its name.
#[derive(Clone, Debug, PartialEq)]
pub struct Busiest {
    /// The name they share.
    pub name: String,
    /// Their cores-worth over the sweep interval, summed.
    pub cores: f32,
    /// How many processes.
    pub n: usize,
    /// Whose they all are; `None` when they differ.
    pub scope: Option<ProcScope>,
    /// The earliest birth read among them, unix ms; `None` when none was read.
    pub born_ms: Option<i64>,
    /// How many of them have no birth read, any of whom may be older than [`Self::born_ms`].
    pub unborn: usize,
}

/// One program in one scope: every process of the sweep sharing its name and its
/// innermost scope ([`Snapshot::programs`]), idle ones too.
#[derive(Clone, Debug, PartialEq)]
pub struct ProgramGroup {
    /// The name they share ([`ProcSample::name`]), as the processes chose it (data, never an
    /// instruction).
    pub name: String,
    /// The scope they all count in.
    pub scope: ProcScope,
    /// How many processes.
    pub n: u32,
    /// Their cores-worth over the sweep interval, summed (a process with no rate counts 0).
    pub cores: f32,
    /// Their footprints, summed over those read; `None` when none was.
    pub footprint: Option<u64>,
    /// Members whose footprint was unreadable; the sum is a lower bound while nonzero.
    pub footprint_unread: u32,
    /// The earliest birth read among them, unix ms; `None` when none was.
    pub born_ms: Option<i64>,
    /// How many of them have no birth read, any of whom may be older than [`Self::born_ms`].
    pub unborn: u32,
}

impl ProgramGroup {
    fn new(name: &str, scope: ProcScope) -> Self {
        Self {
            name: name.to_owned(),
            scope,
            n: 0,
            cores: 0.0,
            footprint: None,
            footprint_unread: 0,
            born_ms: None,
            unborn: 0,
        }
    }

    fn add(&mut self, cores: f32, footprint: Option<u64>, birth_ms: Option<i64>) {
        self.n += 1;
        self.cores += cores;
        if let Some(f) = footprint {
            self.footprint = Some(self.footprint.unwrap_or(0).saturating_add(f));
        } else {
            self.footprint_unread = self.footprint_unread.saturating_add(1);
        }
        match birth_ms {
            Some(b) => self.born_ms = earliest(self.born_ms, Some(b)),
            None => self.unborn += 1,
        }
    }

    /// Its oldest member's age at `at_unix_ms` (0 for a birth after it, as a wall-clock step
    /// can make), ms; `None` when no member's birth was read, never 0 for that. A lower bound
    /// while [`Self::unborn`] is above 0.
    #[must_use]
    pub fn age_ms(&self, at_unix_ms: i64) -> Option<u64> {
        age_at(self.born_ms, at_unix_ms)
    }
}

impl ProcView {
    /// Its age at `at_unix_ms`, ms ([`ProgramGroup::age_ms`]'s terms).
    #[must_use]
    pub fn age_ms(&self, at_unix_ms: i64) -> Option<u64> {
        age_at(self.birth_ms, at_unix_ms)
    }
}

impl Busiest {
    /// Its oldest busy member's age at `at_unix_ms`, ms ([`ProgramGroup::age_ms`]'s terms).
    #[must_use]
    pub fn age_ms(&self, at_unix_ms: i64) -> Option<u64> {
        age_at(self.born_ms, at_unix_ms)
    }
}

fn earliest(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    a.into_iter().chain(b).min()
}

fn age_at(born_ms: Option<i64>, at_unix_ms: i64) -> Option<u64> {
    born_ms.map(|b| u64::try_from(at_unix_ms.saturating_sub(b)).unwrap_or(0))
}

/// Processes [`Snapshot::top`] keeps.
pub const TOP_PROCS: usize = 6;
/// Seconds each core's history keeps ([`CpuView::core_history`]): a minute.
pub const CORE_HISTORY_LEN: usize = 60;

/// Maximum sampling cadences allowed between two counter reads to form a rate.
pub const GAP_CADENCES: u64 = 3;
/// Maximum wall-vs-monotonic clock drift (ms) before an interval is treated as a sleep gap.
pub const SLEEP_SKEW_MS: i64 = 1_000;
/// Busy-core fraction threshold (`0.95`) for CPU overload while the run queue passes the cores
/// ([`crate::strain::cpu_over_depth`]).
pub const CPU_OVER_BUSY: f64 = 0.95;
/// Swap traffic threshold (bytes/s) for memory overload.
pub const SWAP_OVER_BPS: f64 = 4.0 * 1024.0 * 1024.0;
/// Per-session output token rate cap (tokens/s); intervals above this are discarded.
pub const TOKENS_CAP_PER_S: f64 = 5_000.0;
/// Averaging window (ms) for the Tokens row's current output rate.
pub const TOKENS_WINDOW_MS: i64 = 60_000;
/// Duration (ms) a session's last folded token rate is held without a new fold.
pub const TOKENS_HOLD_MS: i64 = 60_000;
/// Programs [`RowFacts::writers`] and [`RowFacts::hour_writers`] keep in each scope.
pub const TOP_WRITERS: usize = 4;
/// Consecutive readings a busier network link must lead before becoming the reference link.
pub const LINK_LEAD_READINGS: u32 = 5;
/// Minimum traffic (bytes/s) required for a link to count as leading.
pub const LINK_LEAD_BPS: f64 = 64_000.0;

/// Stats deck resource row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Resource {
    /// Busy cores against the core count.
    Cpu,
    /// The share of time the GPU was busy.
    Gpu,
    /// Memory in use against physical memory.
    Memory,
    /// The home volume's bytes used against its size.
    Disk,
    /// The busier direction's rate against its peak.
    Network,
    /// aterm's output tokens per minute against its peak.
    Tokens,
    /// Processes against the binding limit.
    Processes,
}

impl Resource {
    /// Every row, in the deck's order.
    pub const ALL: [Self; 7] = [
        Self::Cpu,
        Self::Gpu,
        Self::Memory,
        Self::Disk,
        Self::Network,
        Self::Tokens,
        Self::Processes,
    ];

    /// Its index in [`Self::ALL`] and [`Snapshot::resources`].
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Severity state of a resource row, ordered least to most severe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum State {
    /// A job stopped by job control.
    Stopped,
    /// A job waiting for the person (◇).
    Waiting,
    /// A job gone quiet (◔).
    Quiet,
    /// Zombies left unreaped across the machine (☠).
    Zombies,
    /// A stuck job (⊘).
    Stuck,
    /// Warn (◢ disk, ● memory, GPU, CPU or Processes Room).
    Warn,
    /// Critical (◢ disk, ● memory or CPU).
    Critical,
}

impl State {
    /// Derive the Processes row state from active job conditions and machine zombie piles.
    #[must_use]
    pub fn of_jobs(view: &crate::jobs::JobsView) -> Option<Self> {
        use crate::jobs::Verdict;
        let rows = view.rows.iter().filter_map(|r| match r.verdict {
            Verdict::Stuck => Some(Self::Stuck),
            Verdict::Quiet => Some(Self::Quiet),
            Verdict::WaitingYou => Some(Self::Waiting),
            Verdict::Parked => Some(Self::Stopped),
            _ => None,
        });
        let zombies = view
            .findings
            .zombies
            .iter()
            .any(|z| z.count > 0)
            .then_some(Self::Zombies);
        rows.chain(zombies).max()
    }
}

fn processes_state(
    room: Option<crate::room::Reading>,
    jobs: Option<&crate::jobs::JobsView>,
) -> Option<State> {
    room.filter(|room| room.near())
        .map(|_| State::Warn)
        .max(jobs.and_then(State::of_jobs))
}

/// Capacity basis for normalizing a resource row's heat.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CapBasis {
    /// Fixed hardware or OS ceiling (cores, physical RAM, volume size, PID limit).
    #[default]
    Fixed,
    /// The largest sustained rate of the last 7 days.
    Peak,
    /// The measured link's rate, while the network learns its peak.
    Link,
    /// Learning its peak with nothing to measure against yet.
    Learning,
}

impl CapBasis {
    /// Maximum heat fraction for this basis (`1.0` for `Peak` and `Link`, unbounded otherwise).
    #[must_use]
    pub fn heat_ceiling(self) -> f32 {
        match self {
            Self::Peak | Self::Link => 1.0,
            Self::Fixed | Self::Learning => f32::INFINITY,
        }
    }
}

/// Why a scope has no figure, where that is not "not read yet".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoFigure {
    /// The OS keeps no per-app figure (network; GPU while no client read is held).
    NotPerApp,
    /// The sweep read no process's counter (writes on Linux).
    NotReadHere,
    /// No agent runs in the scope (tokens).
    NoAgent,
}

/// Buckets of a scope's hour a snapshot carries ([`ScopeFig::run`]): the deck's 10 minutes.
pub const SCOPE_RUN_BUCKETS: usize = 60;

/// One scope's figure in a row: aterm's or a tab's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScopeFig {
    /// Trailing mean in the row's native unit, or `None` before the first full reading.
    pub now: Option<f64>,
    /// CPU scope/process counters behind `now` and every history cell; absent for other rows.
    /// Coverage does not provide an actual-window CPU bound.
    pub cpu_accounting: Option<crate::published_cpu::CpuAccounting>,
    /// Raw CPU charge over the same trailing window as `now`, where it differs from the
    /// independently observed own-counter channel. Never an actual-work lower bound.
    pub cpu_charge: Option<f64>,
    /// Largest number of unread contributions in the readings contributing to `now`.
    /// CPU counts coverage gaps only; memory footprint charges are partial while nonzero.
    pub unread: u32,
    /// Why there is no figure at all, where that is known.
    pub missing: Option<NoFigure>,
    /// The newest [`SCOPE_RUN_BUCKETS`] of the scope's 10 s cells, oldest first, raw; empty where
    /// the scope never read this row.
    pub run: Arc<[Cell]>,
    /// Bucket index of the last cell in `run` (Unix seconds `[10 * run_k, 10 * run_k + 10)`).
    pub run_k: i64,
}

impl ScopeFig {
    /// The run's raw columns ([`BucketRing::columns`]). A column older than the run's first cell
    /// is hollow although the model may hold it: the run carries [`SCOPE_RUN_BUCKETS`] alone.
    #[must_use]
    pub fn columns(&self, per_s: u32, ncols: usize, now_unix_ms: i64) -> Vec<Cell> {
        columns_of(&self.run, self.run_k, per_s, ncols, now_unix_ms)
    }

    /// One tab's figure from its panes: `now` and each run cell's mean and max (an upper bound)
    /// add over the panes that read them, so a bucket no pane read stays hollow. An optional
    /// ceiling bounds GPU shares whose independently held pane figures can sum past the
    /// device. Other figures retain their sums, including panes with different read coverage.
    /// One pane's is its own.
    fn sum(figs: &[&Self], ceiling: Option<&Self>) -> Self {
        if let [one] = figs {
            return (*one).clone();
        }
        let add = |a: Option<f64>, b: Option<f64>| plus(a, b, |a, b| a + b);
        let held = |v: Option<f64>, all: Option<f64>| v.map(|v| all.map_or(v, |a| v.min(a)));
        let read: Vec<&Self> = figs.iter().copied().filter(|f| !f.run.is_empty()).collect();
        let run_k = read.iter().map(|f| f.run_k).max().unwrap_or(0);
        let first = read
            .iter()
            .map(|f| f.run_start())
            .min()
            .map_or(run_k + 1, |k| k.max(run_k + 1 - SCOPE_RUN_BUCKETS as i64));
        let run = (first..=run_k)
            .map(|k| {
                let mut cell = Cell::default();
                // The panes read at the same times: the tab's bucket holds as much read time as
                // its fullest pane's, and as much over as its most over pane's.
                for c in read.iter().filter_map(|f| f.run_cell(k)) {
                    cell.mean = add(cell.mean, c.mean);
                    cell.max = add(cell.max, c.max);
                    cell.tau_ms = cell.tau_ms.max(c.tau_ms);
                    cell.over_ms = cell.over_ms.max(c.over_ms);
                }
                let all = ceiling.and_then(|within| within.run_cell(k));
                Cell {
                    mean: held(cell.mean, all.and_then(|c| c.mean)),
                    max: held(cell.max, all.and_then(|c| c.max)),
                    tau_ms: cell.tau_ms,
                    over_ms: cell.over_ms.min(cell.tau_ms),
                }
            })
            .collect();
        Self {
            now: held(
                figs.iter().fold(None, |sum, f| add(sum, f.now)),
                ceiling.and_then(|within| within.now),
            ),
            cpu_accounting: figs.iter().find_map(|f| f.cpu_accounting),
            cpu_charge: figs.iter().fold(None, |sum, f| add(sum, f.cpu_charge)),
            unread: figs
                .iter()
                .fold(0_u32, |sum, f| sum.saturating_add(f.unread)),
            missing: figs
                .iter()
                .map(|f| f.missing)
                .reduce(|a, b| if a == b { a } else { None })
                .flatten(),
            run,
            run_k,
        }
    }

    /// Bucket index of the run's first cell.
    fn run_start(&self) -> i64 {
        self.run_k + 1 - self.run.len() as i64
    }

    /// The run's cell for bucket `k`, while the run holds it.
    fn run_cell(&self, k: i64) -> Option<&Cell> {
        self.run.get(usize::try_from(k - self.run_start()).ok()?)
    }
}

/// One series of a row: its "now", capacity and hour of cells.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Series {
    /// Trailing mean of raw readings.
    pub now: Option<f64>,
    /// Its capacity now, raw; `None` where unknown.
    pub capacity: Option<f64>,
    /// One hour of 10 s cells, oldest first; the last is the open bucket [`ResourceView::ring_k`].
    pub ring: Arc<[Cell]>,
    /// Six hours of ten-minute cells, oldest first; the last is the open one
    /// [`ResourceView::long_k`].
    pub long: Arc<[Cell]>,
    /// Its last minute of readings, oldest first ([`ResourceView::heat_multi_scale`]).
    pub recent: Arc<[Reading]>,
    /// Whether this series binds now (its share of capacity is the row's).
    pub binds: bool,
}

/// One row of the deck: raw readings, capacity applied when painted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResourceView {
    /// Trailing 10 s mean of raw readings.
    pub now: Option<f64>,
    /// The capacity now, raw; `None` where unknown or still learning.
    pub capacity: Option<f64>,
    /// Current value normalized against capacity, clamped to [`CapBasis::heat_ceiling`].
    pub heat_now: Option<f32>,
    /// Whether the open bucket is over the resource's maximum (CPU, memory).
    pub over: bool,
    /// The row's worst state.
    pub state: Option<State>,
    /// The hour of 10 s cells, oldest first, raw.
    pub ring: Arc<[Cell]>,
    /// Bucket index of the last cell in `ring` (Unix seconds `[10 * ring_k, 10 * ring_k + 10)`).
    pub ring_k: i64,
    /// The six hours of ten-minute cells, oldest first, raw (the deck's 1 h boxes).
    pub long: Arc<[Cell]>,
    /// Bucket index of the last cell in `long` (Unix seconds `[600 * long_k, 600 * long_k + 600)`).
    pub long_k: i64,
    /// The first series' last minute of readings, oldest first ([`Self::heat_multi_scale`]).
    pub recent: Arc<[Reading]>,
    /// Memory's pressure, cell for cell with `ring`: the worst level read in each bucket, `None`
    /// (hollow) where none was read; empty on every other row ([`crate::strain`]'s hour).
    pub pressure: Arc<[Option<Pressure>]>,
    /// Memory's pressure ring's own 10 s cells, cell for cell with `ring`, each reading its
    /// level's [`Pressure::rank`] (`max` the worst read in it, `None` where none was read); empty
    /// on every other row. `pressure` is these cells' worst level.
    pub pressure_ring: Arc<[Cell]>,
    /// Memory's pressure ring's ten-minute cells, cell for cell with `long` by key, in ranks as
    /// `pressure_ring` is; an empty cell where the pressure ring holds none; empty on every
    /// other row.
    pub pressure_long: Arc<[Cell]>,
    /// Memory's last minute of pressure readings, oldest first, each its level's
    /// [`Pressure::rank`]; empty on every other row. With `pressure_ring` and `pressure_long` they
    /// are the tiers each strip box's state is cut from ([`Self::heat_multi_scale`]).
    pub pressure_recent: Arc<[Reading]>,
    /// aterm's figure.
    pub aterm: ScopeFig,
    /// Scope figure for the requested tab (or an empty tab in [`Snapshot::resources`]).
    pub tab: ScopeFig,
    /// Every tab's figure, by key, in the host's order.
    pub tabs: Vec<(u64, ScopeFig)>,
    /// CPU figures for complete logical tabs, each with its current pane keys. These include
    /// work whose logical tab is known after its originating pane closed.
    pub logical_cpu: Vec<(Vec<u64>, ScopeFig)>,
    /// What the capacity is.
    pub basis: CapBasis,
    /// Secondary series for a two-series row: Network's sending (Processes has one since F4′).
    pub second: Option<Series>,
}

/// One painted strip cell: its heat, with capacity applied now.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HeatCell {
    /// The column's mean as a share of capacity now; `None` is hollow.
    pub heat: Option<f32>,
    /// Its largest reading as a share of capacity now: the box's peak tick.
    pub max: Option<f32>,
    /// The share of its read time over the resource's maximum, `0..=1`: the over cap's length
    /// (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3); 0 where none was.
    pub over: f32,
    /// Discontinuity kind when this cell spans a sleep, crash, or pause gap.
    pub gap: Option<GapKind>,
    /// The worst state read in it: on Memory the worst kernel pressure read in the box
    /// ([`ResourceView::pressure`]), and on the live box of every row at least the row's state
    /// now ([`ResourceView::state`]); `None` at rest, or where no state was kept
    /// ([`ResourceView::heat_multi_scale`]).
    pub state: Option<State>,
}

/// One progressive box on a multi-scale heat strip, pairing [`HeatCell`] with [`ProgSlot`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HeatSlot {
    /// Heat values and discontinuity marker for this box.
    pub cell: HeatCell,
    /// The box's age window, weight and band.
    pub slot: ProgSlot,
}

/// How far a strip's last-minute projection may carry a neighboring reading through an
/// otherwise unread interval. The deck and `sys history` use this same bounded cover.
pub const STRIP_COVER_MS: i64 = 15_000;

/// One canonical strip box with its raw source series beside the cell the deck paints.
/// Source weights in [`Cell::tau_ms`] are resampling weights, not measured execution coverage.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ResourceHistoryCell {
    /// The first series, including the canonical slot geometry and discontinuity marker.
    pub primary: ScaleCell,
    /// The independent second series on a two-series row, with its own raw mean and maximum.
    pub secondary: Option<ScaleCell>,
    /// The deck's final heat, peak and over cap, against capacity now.
    pub painted: HeatSlot,
}

/// An immutable row projected into the same boxes as its strip, newest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResourceHistorySlots {
    /// Canonical boxes; a resolution filter selects these boxes without reconstructing them.
    pub cells: Vec<ResourceHistoryCell>,
    /// Capacity applied to the first series now, not an inferred historical capacity.
    pub capacity: Option<f64>,
    /// Capacity applied to the independent second series now, where that series exists.
    pub secondary_capacity: Option<f64>,
    /// What the capacities mean when normalizing the painted boxes.
    pub basis: CapBasis,
}

impl ResourceView {
    /// The first series' raw columns ([`BucketRing::columns`]).
    #[must_use]
    pub fn columns(&self, per_s: u32, ncols: usize, now_unix_ms: i64) -> Vec<Cell> {
        columns_of(&self.ring, self.ring_k, per_s, ncols, now_unix_ms)
    }

    /// Resample the 1-hour ring into `ncols` heat columns at `per_s` seconds per column.
    #[must_use]
    pub fn heat_columns(&self, per_s: u32, ncols: usize, now_unix_ms: i64) -> Vec<HeatCell> {
        let cells = |s: &Series| {
            self.heat_of_columns(
                columns_of(&s.ring, self.ring_k, per_s, ncols, now_unix_ms),
                s.capacity,
            )
        };
        let mut out = cells(&self.first());
        if let Some(second) = self.second.as_ref() {
            for (a, b) in out.iter_mut().zip(cells(second)) {
                a.heat = larger(a.heat, b.heat);
                a.max = larger(a.max, b.max);
                a.over = a.over.max(b.over);
            }
        }
        out
    }

    /// Resample this row's hour ring, six hours of ten-minute cells and last-minute per-sample
    /// readings into progressive multi-scale [`HeatSlot`]s for `spec` at `now_unix_ms`, preserving
    /// `gaps` without smearing. Each box's state ([`HeatCell::state`]) is the worst pressure read
    /// in it, Memory's three pressure tiers ([`Self::pressure`], [`Self::pressure_long`],
    /// [`Self::pressure_recent`]) laid through the same slots as the row's own cells; the live
    /// box's is at least the row's state now. A row that keeps no state per bucket (Disk) has
    /// none on its past boxes.
    #[must_use]
    pub fn heat_multi_scale(
        &self,
        spec: MultiScaleSpec,
        now_unix_ms: i64,
        cover_ms: i64,
        gaps: &[TimeGap],
    ) -> Vec<HeatSlot> {
        let ceiling = self.basis.heat_ceiling();
        let convert = |sc: ScaleCell, cap: Option<f64>| HeatSlot {
            cell: HeatCell {
                heat: heat_of(sc.cell.mean, cap).map(|h| h.min(ceiling)),
                max: heat_of(sc.cell.max, cap).map(|h| h.min(ceiling)),
                over: sc.cell.over_share() as f32,
                gap: sc.gap,
                state: None,
            },
            slot: sc.slot,
        };
        let slots_for = |s: &Series| -> Vec<HeatSlot> {
            let tiers = Tiers {
                ring: &s.ring,
                ring_k: self.ring_k,
                long: &s.long,
                long_k: self.long_k,
                recent: &s.recent,
            };
            multi_scale_cells(tiers, now_unix_ms, spec, cover_ms, gaps)
                .into_iter()
                .map(|sc| convert(sc, s.capacity))
                .collect()
        };
        let mut out = slots_for(&self.first());
        if let Some(second) = self.second.as_ref() {
            for (a, b) in out.iter_mut().zip(slots_for(second)) {
                a.cell.heat = larger(a.cell.heat, b.cell.heat);
                a.cell.max = larger(a.cell.max, b.cell.max);
                a.cell.over = a.cell.over.max(b.cell.over);
                a.cell.gap = a.cell.gap.or(b.cell.gap);
                if a.cell.gap.is_some() && b.cell.heat.is_none() && a.cell.heat.is_none() {
                    a.cell.over = 0.0;
                }
            }
        }
        let pressed = self.pressure_states(spec, now_unix_ms, cover_ms, gaps);
        for (i, a) in out.iter_mut().enumerate() {
            let read = pressed.get(i).copied().flatten();
            a.cell.state = if a.slot.is_live_now {
                read.max(self.state)
            } else {
                read
            };
        }
        out
    }

    /// The worst pressure read in each of `spec`'s boxes, as a state, from Memory's three
    /// pressure tiers through the same slots as [`Self::heat_multi_scale`]'s; empty where the
    /// row keeps no pressure.
    fn pressure_states(
        &self,
        spec: MultiScaleSpec,
        now_unix_ms: i64,
        cover_ms: i64,
        gaps: &[TimeGap],
    ) -> Vec<Option<State>> {
        if self.pressure_ring.is_empty()
            && self.pressure_long.is_empty()
            && self.pressure_recent.is_empty()
        {
            return Vec::new();
        }
        let tiers = Tiers {
            ring: &self.pressure_ring,
            ring_k: self.ring_k,
            long: &self.pressure_long,
            long_k: self.long_k,
            recent: &self.pressure_recent,
        };
        multi_scale_cells(tiers, now_unix_ms, spec, cover_ms, gaps)
            .into_iter()
            .map(|sc| {
                sc.cell
                    .max
                    .filter(|r| r.is_finite())
                    .and_then(|r| Pressure::of_rank(r).state())
            })
            .collect()
    }

    fn heat_of_columns(&self, cols: Vec<Cell>, capacity: Option<f64>) -> Vec<HeatCell> {
        let ceiling = self.basis.heat_ceiling();
        cols.into_iter()
            .map(|c| HeatCell {
                heat: heat_of(c.mean, capacity).map(|h| h.min(ceiling)),
                max: heat_of(c.max, capacity).map(|h| h.min(ceiling)),
                over: c.over_share() as f32,
                gap: None,
                state: None,
            })
            .collect()
    }

    /// `cells` of the first series (a lens trace's columns, [`Self::columns`]) as heat against
    /// its capacity now, as its strip boxes draw them: each column's mean and largest reading as
    /// shares, and its over share.
    #[must_use]
    pub fn heat_cells(&self, cells: Vec<Cell>) -> Vec<HeatCell> {
        self.heat_of_columns(cells, self.capacity)
    }

    /// A two-series row's columns as its lens trace draws them, the busier direction's
    /// (`docs/DESIGN-stats-deck-units-2026-11-04.md` §7; the R3 lens fixes and their review,
    /// 2026-11-06), each with its heat. Each column is one series' minute: the series whose mean
    /// is the larger share of its own capacity now (where neither share is known, the larger
    /// finite raw mean, with no heat), its mean the column and its largest reading the tick, each
    /// with its own share as its heat. The trace draws both on one raw axis, so a tick taken from
    /// the other series could stand below its column, where no tick is drawn; one series' largest
    /// reading never does. The column's heat is [`Self::heat_columns`]' (the deck row's), but the
    /// tick's can be cooler than the row's peak heat, which takes the larger share of either
    /// series. A column's count and over count are its series', or the other's where that one's
    /// over share is the larger, as the heat's over share is. A one-series row's are
    /// [`Self::columns`] and [`Self::heat_cells`].
    #[must_use]
    pub fn busier_columns(
        &self,
        per_s: u32,
        ncols: usize,
        now_unix_ms: i64,
    ) -> (Vec<Cell>, Vec<HeatCell>) {
        let first = self.columns(per_s, ncols, now_unix_ms);
        let Some(second) = self.second.as_ref() else {
            let heat = self.heat_cells(first.clone());
            return (first, heat);
        };
        let other = columns_of(&second.ring, self.ring_k, per_s, ncols, now_unix_ms);
        let ceiling = self.basis.heat_ceiling();
        let share = |v: Option<f64>, cap: Option<f64>| heat_of(v, cap).map(|h| h.min(ceiling));
        first
            .into_iter()
            .zip(other)
            .map(|(a, b)| {
                let from_b = match (
                    heat_of(a.mean, self.capacity),
                    heat_of(b.mean, second.capacity),
                ) {
                    (Some(x), Some(y)) => y > x,
                    (Some(_), None) => false,
                    (None, Some(_)) => true,
                    (None, None) => {
                        b.mean.filter(|v| v.is_finite()) > a.mean.filter(|v| v.is_finite())
                    }
                };
                let (own, rival, cap) = if from_b {
                    (b, a, second.capacity)
                } else {
                    (a, b, self.capacity)
                };
                let counts = if rival.over_share() > own.over_share() {
                    rival
                } else {
                    own
                };
                let cell = Cell {
                    mean: own.mean,
                    max: own.max,
                    ..counts
                };
                let heat = HeatCell {
                    heat: share(own.mean, cap),
                    max: share(own.max, cap),
                    over: counts.over_share() as f32,
                    gap: None,
                    state: None,
                };
                (cell, heat)
            })
            .unzip()
    }

    /// The live box's peak tick and over cap (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3,
    /// "Live box"): the largest reading of the trailing [`NOW_WINDOW_MS`] as a share of capacity
    /// now, the larger series' on a two-series row as [`Self::heat_now`] takes, and the share of
    /// those readings' read time over the resource's maximum, each reading weighed by the time it
    /// stands for ([`Reading::w_ms`]) as every other box's over share is. Its column is
    /// [`Self::heat_now`], the 10 s mean, weighed the same way.
    #[must_use]
    pub fn live_box(&self, now_unix_ms: i64) -> (Option<f32>, f32) {
        let ceiling = self.basis.heat_ceiling();
        let cutoff = i128::from(now_unix_ms) - i128::from(NOW_WINDOW_MS);
        let of = |recent: &[Reading], cap: Option<f64>| {
            let inside = || {
                recent
                    .iter()
                    .filter(|r| i128::from(r.at_ms) > cutoff && r.at_ms <= now_unix_ms)
                    .filter(|r| r.v.is_finite())
            };
            let read: u64 = inside().map(|r| u64::from(r.w_ms.max(1))).sum();
            let peak = inside().map(|r| r.v).reduce(f64::max);
            let over: u64 = inside()
                .filter(|r| r.over)
                .map(|r| u64::from(r.w_ms.max(1)))
                .sum();
            (
                heat_of(peak, cap).map(|h| h.min(ceiling)),
                if read > 0 {
                    (over as f64 / read as f64) as f32
                } else {
                    0.0
                },
            )
        };
        let (peak, over) = of(&self.recent, self.capacity);
        match self.second.as_ref() {
            Some(s) => {
                let (p2, o2) = of(&s.recent, s.capacity);
                (larger(peak, p2), over.max(o2))
            }
            None => (peak, over),
        }
    }

    /// The first series, as [`Series`].
    #[must_use]
    pub fn first(&self) -> Series {
        Series {
            now: self.now,
            capacity: self.capacity,
            ring: Arc::clone(&self.ring),
            long: Arc::clone(&self.long),
            recent: Arc::clone(&self.recent),
            binds: self.second.as_ref().is_none_or(|s| !s.binds),
        }
    }

    /// Project raw series and painted heat through the existing canonical strip resamplers.
    /// `at_unix_ms` is the source snapshot's original wall timestamp; this method reads no
    /// clock and changes no ring. Binned source weights do not establish physical coverage.
    #[must_use]
    pub fn history_slots(
        &self,
        spec: MultiScaleSpec,
        at_unix_ms: i64,
        gaps: &[TimeGap],
    ) -> ResourceHistorySlots {
        let raw = |series: &Series| {
            multi_scale_cells(
                Tiers {
                    ring: &series.ring,
                    ring_k: self.ring_k,
                    long: &series.long,
                    long_k: self.long_k,
                    recent: &series.recent,
                },
                at_unix_ms,
                spec,
                STRIP_COVER_MS,
                gaps,
            )
        };
        let primary = raw(&self.first());
        let secondary = self.second.as_ref().map(raw);
        let painted = self.heat_multi_scale(spec, at_unix_ms, STRIP_COVER_MS, gaps);
        let cells = primary
            .into_iter()
            .zip(painted)
            .enumerate()
            .map(|(index, (primary, painted))| ResourceHistoryCell {
                primary,
                secondary: secondary
                    .as_ref()
                    .and_then(|cells| cells.get(index))
                    .copied(),
                painted,
            })
            .collect();
        ResourceHistorySlots {
            cells,
            capacity: self.capacity,
            secondary_capacity: self.second.as_ref().and_then(|series| series.capacity),
            basis: self.basis,
        }
    }

    /// The series whose share is the row's: the amount and the % print it.
    #[must_use]
    pub fn binding(&self) -> Series {
        match self.second.as_ref() {
            Some(s) if s.binds => s.clone(),
            _ => self.first(),
        }
    }

    /// A row of `first` and its `second` series, if it has one. The second binds where its share
    /// of its capacity now is the larger; where neither share is known, where its finite raw
    /// "now" is the larger (the R3 lens fixes' review, 2026-11-06). That is the rule
    /// [`Self::busier_columns`] takes each column by, over another window: this binds on each
    /// series' 10 s "now", a column on its minute's mean. So the amount, the deck's arrow and
    /// no-heat fill and the lens's words name the direction the trace's newest column draws while
    /// each direction's rate holds through the minute; within a minute whose busier direction
    /// flips, that column can be the other direction's (the second review, 2026-11-06).
    fn of(first: Series, second: Option<Series>, ring_k: i64, over: bool, basis: CapBasis) -> Self {
        let a = heat_of(first.now, first.capacity);
        let b = second.as_ref().and_then(|s| heat_of(s.now, s.capacity));
        let second_binds = match (a, b) {
            (Some(a), Some(b)) => b > a,
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (None, None) => {
                let raw = |s: Option<&Series>| s.and_then(|s| s.now).filter(|v| v.is_finite());
                raw(second.as_ref()) > raw(Some(&first))
            }
        };
        Self {
            now: first.now,
            capacity: first.capacity,
            heat_now: larger(a, b).map(|h| h.min(basis.heat_ceiling())),
            over,
            basis,
            ring: first.ring,
            ring_k,
            long: first.long,
            recent: first.recent,
            second: second.map(|s| Series {
                binds: second_binds,
                ..s
            }),
            ..Self::default()
        }
    }
}

fn heat_of(v: Option<f64>, cap: Option<f64>) -> Option<f32> {
    let (v, cap) = (v.filter(|x| x.is_finite())?, cap.filter(|x| x.is_finite())?);
    Some(if cap > 0.0 {
        (v / cap).clamp(0.0, f64::from(f32::MAX)) as f32
    } else if v > 0.0 {
        1.0
    } else {
        0.0
    })
}

fn larger(a: Option<f32>, b: Option<f32>) -> Option<f32> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// A program writing to storage this sweep ([`RowFacts::writers`]): the processes of one
/// name in one scope that wrote, summed.
#[derive(Clone, Debug, PartialEq)]
pub struct Writer {
    /// The name they share.
    pub name: String,
    /// Bytes they wrote per second over the sweep interval.
    pub bytes_per_s: f64,
    /// Whose they are.
    pub scope: ProcScope,
    /// How many of its processes wrote.
    pub n: u32,
    /// Bytes they wrote over the sweep interval.
    pub bytes: u64,
}

/// Per-resource auxiliary metrics for the deck's details and lens views.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowFacts {
    /// CPU: hardware processor brand/chip string, where known.
    pub cpu_brand: Option<String>,
    /// CPU: the 1-minute load average.
    pub load1: Option<f64>,
    /// CPU: P10, the machine's runnable threads over the last 10 s, or why unread.
    pub runnable: crate::runq::Runnable,
    /// CPU: the queue the over rule reads ([`crate::strain::cpu_over_depth`]): P10, or
    /// [`Self::load1`] only where P10 is unread; `None` where neither was read.
    pub queue: Option<crate::runq::RunQueue>,
    /// CPU: the busiest program this sweep, its processes grouped by name.
    pub busiest: Option<Busiest>,
    /// Memory: swap traffic, bytes per second.
    pub swap_rate: Option<f64>,
    /// GPU: the latest reading, with its memory and stages.
    pub gpu: Option<GpuSample>,
    /// GPU: resets during this run, and when the first was seen (unix ms).
    pub gpu_resets: Option<(u64, i64)>,
    /// Disk: the watch's facts, where the host read them.
    pub disk: Option<DiskFacts>,
    /// Disk: growth of bytes used.
    pub disk_rates: Rates,
    /// Disk: the filling clause, when it applies.
    pub filling: Option<Filling>,
    /// Disk: the programs owned by this user that wrote most this sweep, busiest first, at most
    /// [`TOP_WRITERS`] in each scope: aterm, each tab, and the rest of the Mac.
    pub writers: Vec<Writer>,
    /// Disk: the programs owned by this user that wrote most in the last hour, busiest first, at
    /// most [`TOP_WRITERS`] in each scope ([`crate::writes::WriteHour`]).
    pub hour_writers: Vec<crate::writes::ProgramHour>,
    /// Disk: seconds of the last hour the readings behind [`Self::hour_writers`] covered (no
    /// sweep runs while no deck is shown); 0 where no process's writes are read.
    pub writes_covered_s: f64,
    /// Network: active reference link selected by `Rows::pick_link`.
    pub link: Option<LinkSample>,
    /// Network: seconds of readings toward learning its peak.
    pub net_read_s: u32,
    /// Tokens: aterm's input tokens per minute over the last minute.
    pub tokens_in: Option<f64>,
    /// Tokens: agent sessions read this sample.
    pub agents: usize,
    /// Tokens: seconds of readings toward learning the peak.
    pub tokens_read_s: u32,
    /// Tokens: the plan's usage, while current.
    pub plan: Option<PlanFact>,
    /// Processes: the task tables, their counts and limits.
    pub room: TaskRoom,
    /// Processes: this reading's fullest readable bounded task-table pair and the model's
    /// Room warning judgment; `None` where no pair is readable and bounded.
    pub process_room: Option<crate::room::Reading>,
    /// Processes: starts and exits a second over this reading's interval
    /// ([`crate::starts::TaskFlow`]); `None` where no start counter read at both ends, or no
    /// rate spans the interval.
    pub flow: Option<crate::starts::TaskFlow>,
    /// Processes: seconds of readings toward learning its start peak ([`PROC_LEARN_S`]).
    pub proc_read_s: u32,
    /// Processes: unreaped zombies by parent, machine-wide, from the sweep every whole reading
    /// takes ([`crate::blockage`]).
    pub zombies: crate::blockage::Zombies,
    /// Processes: threads in an uninterruptible wait ([`crate::blockage`]).
    pub dstate: crate::blockage::DState,
    /// Processes: what blocks their flow at this reading, by the one rule the strip's blockage
    /// ring keeps ([`crate::blockage::Blocked::judge`]).
    pub blocked: crate::blockage::Blocked,
}

/// Validated build-slot occupancy from the host's slot reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotsView {
    /// Not read, and why.
    Unread(SlotsUnread),
    /// Read.
    Read {
        /// Slot files present.
        n: u16,
        /// Proven held.
        held: u16,
        /// Proven free.
        free: u16,
        /// Not proven either way yet (a first read, one sighting, a change).
        pending: u16,
        /// Whose record does not parse; never counted free.
        unknown: u16,
        /// Free slots counted as busy because an orphaned build still runs.
        discounted: u16,
    },
}

/// Why the host read no build slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotsUnread {
    /// No reader on this platform.
    Unsupported,
    /// No slot directory, or no slot in it.
    NotSampled,
    /// The directory, or every slot in it, refused to this user.
    Refused,
    /// No slot's record parses: `build-slot` changed its record.
    UnreadFormat,
}

impl SlotsUnread {
    /// Canonical unavailable reason string ([`crate::sys::UNAVAILABLE_WORDS`]).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::NotSampled => "not_sampled",
            Self::Refused => "refused",
            Self::UnreadFormat => "unread_format",
        }
    }
}

/// Why a reading holds no value: one of the `sys` unavailable words, never a zero in its place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unread {
    /// No reader on this OS.
    Unsupported,
    /// Not read yet, or the read failed.
    NotSampled,
    /// The OS refused it to this user.
    Refused,
    /// The OS answered with values that fail their self-check.
    UnreadFormat,
}

impl Unread {
    /// Canonical unavailable reason string ([`crate::sys::UNAVAILABLE_WORDS`]).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::NotSampled => "not_sampled",
            Self::Refused => "refused",
            Self::UnreadFormat => "unread_format",
        }
    }
}

/// Everything at one moment. Immutable once made; the host shares it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Bounded immutable groups from the original uncapped observation.
    pub groups: crate::groups::View,
    /// Bounded boot observations and separate persistence status, supplied by the host.
    pub boot_cost: Option<Arc<crate::sys::boot::BootReport>>,
    /// This process's CPU bounds by observed thread name, separate from app and tab charges.
    pub self_cpu: crate::self_cpu::OwnCpuView,
    /// Whether a reader of this process's thread CPU asked the reading for it
    /// ([`Self::self_cpu`]); a reading nothing asks leaves that `not_sampled` (`unsupported` off
    /// macOS, whose reader reads nothing), which clears its continuity, so the next reading asked
    /// warms again.
    pub self_cpu_read: bool,
    /// Original native waits observed on this sample, including known aggregate CPU where a
    /// final own/child split was unavailable.
    pub native_terminals: Vec<NativeTerminal>,
    /// Most recent raw kernel core counters, for a bounded diagnostic export.
    pub cpu_counter_sample: Option<Vec<Ticks>>,
    /// Latest native hardware sample, with its own time window.
    pub hardware: Option<crate::hardware::Sample>,
    /// Whether the reading asked its sweep for each process's scheduling
    /// ([`ProcView::scheduling`]), which a reading no reader of it asks leaves `None`.
    pub scheduling_read: bool,
    /// Bounded raw evidence from a sustained E-busy/P-underused episode.
    pub cpu_diagnostics: Option<crate::hardware::Bundle>,
    /// A prior process's saved report, displayed only as historical evidence.
    pub historical_cpu_report: Option<std::sync::Arc<crate::sys::json::Jv>>,
    /// Explicitly authorized observations; their own wall-clock window is retained.
    pub authorized_cpu_policy: Option<std::sync::Arc<crate::sys::json::Jv>>,
    /// When the sample behind it was taken (the host's monotonic clock, ns).
    pub at_ns: u64,
    /// How many samples the model has absorbed (1 for the first).
    pub seq: u64,
    /// The CPU; `None` until two core readings exist.
    pub cpu: Option<CpuView>,
    /// The GPU, where readable.
    pub gpu: Option<GpuView>,
    /// Memory, where readable.
    pub mem: Option<MemView>,
    /// The focused tab's volume, where readable.
    pub volume: Option<VolumeSample>,
    /// Network interfaces, rates, and TCP connection counts, where readable.
    pub net: Option<NetView>,
    /// This Mac's network place, set by the host from the place record; `None` where no
    /// record or reading was had.
    pub place: Option<PlaceView>,
    /// The machine.
    pub global: ScopeView,
    /// App work: this instance, its tabs and proved shared app processes; no value until two sweeps.
    pub aterm: Option<ScopeView>,
    /// The shared app slice of [`Self::aterm`], outside this instance and its tabs.
    pub shared: Option<ScopeView>,
    /// Every tab, in the host's order; empty until two sweeps exist.
    pub tabs: Vec<TabView>,
    /// Busiest processes this sweep (at most [`TOP_PROCS`]) with attribution scope.
    pub top: Vec<TopProc>,
    /// Latest job-progress view from the host's stuck-job engine.
    pub jobs: Option<std::sync::Arc<crate::jobs::JobsView>>,
    /// How long before this reading the engine's pass that made [`Self::jobs`] ran, ms, as the
    /// host's pass clock reads both ([`crate::jobs::JobsView::at_ms`]): 0 when this reading's own
    /// pass made it. A reading the engine did not look at (one inside its cadence, a pull no
    /// `sys jobs` armed it for, a machine-only one) carries the last pass's view, every figure
    /// of which is this much older than the snapshot.
    pub jobs_age_ms: u64,
    /// Latest leftover view (`sys left`): what closed tabs and dead verify runs left running,
    /// and what macOS's program check holds, each with its proof.
    pub left: Option<std::sync::Arc<crate::left::LeftView>>,
    /// Complete delegated-job totals, from the same process rates as the scopes. Omitted while
    /// process or ownership coverage is incomplete; the host retains its prior dated receipt.
    pub delegated: Vec<DelegatedUsage>,
    /// Whether this reading omitted the process sweep and read machine counters only.
    pub machine_only: bool,
    /// The sample's wall clock, unix ms.
    pub at_unix_ms: i64,
    /// Where a strip drawn reading by reading puts its slots: the wall clock's seconds
    /// ([`SlotGrid::SECONDS`]), since readings land on the half-slot, each in its own second.
    pub slot_grid: SlotGrid,
    /// The deck's rows, in [`Resource::ALL`] order.
    pub resources: [ResourceView; 7],
    /// The home volume's free space over the last 24 hours.
    pub free_history: FreeHistory,
    /// The home volume's free space over the last 3 weeks, in hour buckets.
    pub free_weeks: FreeWeeks,
    /// What each row's Details and lens say beside its heat.
    pub facts: RowFacts,
    /// The process sweep's window, ns (`None` until two sweeps exist).
    pub procs_window_ns: Option<u64>,
    /// The cores' window, ns (`None` until two readings exist).
    pub cores_window_ns: Option<u64>,
    /// Busy CPU breakdown across scopes, once two full sweeps and core ticks are available.
    pub split: Option<Split>,
    /// Current common-window CPU account, including observed final own CPU and separate
    /// ambiguous kernel charges. Process-live rates remain in the scope and process views.
    pub scoped_cpu: Option<crate::explain::ScopeCpu>,
    /// Published pane CPU readings collected on the host's common monotonic clock.
    pub pane_cpu: Arc<crate::pane_cpu::Panes>,
    /// Process-footprint and directional storage observations of each local session.
    pub pane_resources: Arc<crate::pane_resources::Panes>,
    /// Counters and dated intervals owned by current agent runs, indexed by terminal session.
    pub pane_tokens: Arc<crate::pane_tokens::Panes>,
    /// Process rows: aterm's (up to [`ATERM_ROWS`]), the top [`TOP_ROWS`] by CPU and by
    /// footprint, and every other process with a GPU share.
    pub procs: Vec<ProcView>,
    /// Programs by scope, busiest first: every aterm and tab group (up to [`ATERM_ROWS`]) plus
    /// the machine's top [`TOP_ROWS`] by CPU and, separately, by footprint.
    pub programs: Vec<ProgramGroup>,
    /// Whether the process sweep stopped at its cap.
    pub procs_truncated: bool,
    /// Memory charges over the full process sweep, before display rows are bounded.
    pub memory_owners: Option<MemoryOwners>,
    /// The reader lanes this reading found stalled ([`Sample::stalled`]): the figures each feeds
    /// are unknown, never none.
    pub stalled: Vec<crate::lanes::LaneStall>,
    /// The thermal state, where read.
    pub thermal: Option<Thermal>,
    /// The heat chain (A4 §4): the thermal pressure level, compute watts, the Super and
    /// Efficiency clocks with their gates and the usual clock, work per joule, the cut; each
    /// value with its absence ([`crate::heat::NoHeat`]).
    pub heat: crate::heat::HeatView,
    /// The Neural Engine reader's latest reading, or the self-check that retired it; `None`
    /// where no reader runs (every OS but macOS). The reading may be older than this snapshot:
    /// aterm's host reads the engine once, as its reader starts.
    pub ane: Option<Result<AneSample, AneCheck>>,
    /// The 1, 5 and 15 minute load averages, where read.
    pub load: Option<[f64; 3]>,
    /// Build-slot occupancy, set by the host after [`Vitals::absorb`].
    pub slots: Option<SlotsView>,
    /// Age of [`Self::slots`] in ms relative to this snapshot (`None` if sampled together).
    pub slots_age_ms: Option<u64>,
    /// Sorted [`TabRoot::key`]s of tabs holding a build slot in this sweep.
    pub slot_tabs: Option<Vec<u64>>,
    /// This instance's pid.
    pub self_pid: u32,
    /// The events this sample raised ([`crate::events`]).
    pub events: Vec<crate::events::Event>,
    /// The memory starvation episode open at this reading ([`crate::starving`]), as the
    /// detector holds it after this reading; `None` while none is open.
    pub starving: Option<Box<crate::starving::Episode>>,
    /// Accepted marker changes lost before publication at their bounded staging limit.
    pub events_lost: u64,
    /// Most recent split-tick attribution facts ([`crate::explain`]).
    pub why: Option<Arc<crate::explain::WhyFacts>>,
    /// Whether the coalition lane reads (`Off` once a read misread the layout).
    pub coal_lane: crate::explain::CoalLane,
    /// macOS's services as read on their own, where a reply needed them and the split by
    /// coalition did not read them (a `sys why` while no Deck is in view); `None` otherwise
    /// ([`crate::services::over_in`]).
    pub services: Option<Arc<aterm_process_facts::services::Reading>>,
    /// The episodes ([`aterm_process_facts::services::Service::episode`]) whose restart the person put off: the
    /// window's alert for them was dismissed, folded or answered, so a reply offers no restart
    /// for them until a new process takes their place.
    pub services_filed: Vec<String>,
    /// Discontinuous wall-clock intervals (sleep, crash, sampling pauses) in the last six hours
    /// ([`STRIP_HORIZON_MS`]).
    pub time_gaps: Vec<TimeGap>,
    /// Vertical event markers (crashes, restarts, sleep/wake, pressure spikes) in the last six
    /// hours ([`STRIP_HORIZON_MS`]).
    pub strip_events: Vec<StripEvent>,
    /// What the deck's mechanism tiles draw ([`crate::units`]).
    pub units: crate::units::Units,
    /// The machine's situation and type, as the reading's [`Sample::situation`] carried them
    /// (A4 §8, §9): the Machine row's name and glyphs. Its place is [`Self::place`].
    pub situation: crate::situation::Situation,
    /// The Spotlight family over the last split by coalition, carried for
    /// [`crate::spotlight::READING_CARRIED_MS`]; `None` unread ([`crate::spotlight`]).
    pub spotlight: Option<crate::spotlight::Reading>,
    /// The Spotlight family's day ([`crate::spotlight::Family::history`]), shared with the model
    /// (a reading takes a reference, not a copy), so a page can draw it.
    pub spotlight_day: std::sync::Arc<crate::history::SpotlightHistory>,
}

/// A sum of process memory charges with its independent read coverage. Charges can include
/// compressed pages and are not a partition of the VM's physical-memory classes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryCharge {
    /// Readable members' charges, bytes.
    pub bytes: u64,
    /// Number of members counted, including unread members.
    pub members: u32,
    /// Members whose memory charge was unreadable.
    pub unread: u32,
}

impl MemoryCharge {
    /// An exact readable charge; no value where the group is absent or partly unread.
    pub(crate) fn value(self) -> Option<u64> {
        (self.members > 0 && self.unread == 0).then_some(self.bytes)
    }

    fn merge(&mut self, other: Self) {
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.members = self.members.saturating_add(other.members);
        self.unread = self.unread.saturating_add(other.unread);
    }

    pub(crate) fn add(&mut self, bytes: Option<u64>) {
        self.members = self.members.saturating_add(1);
        match bytes {
            Some(bytes) => self.bytes = self.bytes.saturating_add(bytes),
            None => self.unread = self.unread.saturating_add(1),
        }
    }
}

/// Ownership totals from process or coalition charges, without the UI's row cap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryOwners {
    /// This app's instances, tabs and proven shared work.
    pub aterm: MemoryCharge,
    /// Other work under the current user.
    pub others: MemoryCharge,
    /// Other users and system services.
    pub services: MemoryCharge,
    /// Processes whose owner could not be read, or mixed coalition charges whose members could
    /// not all be read to separate their owners.
    pub unknown: MemoryCharge,
    /// Whether the process or coalition list stopped before every member/group answered.
    pub truncated: bool,
    /// Some roots, current members or owner identities could not be reconciled. Measured
    /// charges remain known but their assignment among owners is incomplete.
    pub ownership_incomplete: bool,
}

impl MemoryOwners {
    /// Current coalition footprints are the same charged account as process footprints.
    /// A mixed coalition moves only a readable, complete app subset; the unexplained charge
    /// stays explicitly unknown. No footprint is subtracted from physical VM classes.
    pub(crate) fn of_coalitions(
        split: &crate::explain::CoalSplit,
        provenance_incomplete: bool,
    ) -> Self {
        use crate::explain::Part;
        let mut owners = Self {
            truncated: split.list_truncated || split.rows.len() < split.listed as usize,
            ownership_incomplete: provenance_incomplete,
            ..Self::default()
        };
        for row in &split.rows {
            let account = row.footprint_account();
            owners.ownership_incomplete |= account.incomplete;
            owners.aterm.merge(account.aterm);
            match account.outside_part {
                Some(Part::Others) => owners.others.merge(account.outside),
                Some(Part::Services) => owners.services.merge(account.outside),
                _ => owners.unknown.merge(account.outside),
            }
            owners.unknown.merge(account.unknown);
        }
        owners
    }

    fn of(rows: &[RowKey], truncated: bool, ownership_incomplete: bool) -> Self {
        let mut result = Self {
            truncated,
            ownership_incomplete,
            ..Self::default()
        };
        for row in rows {
            let charge = if row.scope.in_aterm() {
                &mut result.aterm
            } else {
                match row.scope {
                    RowScope::Others => &mut result.others,
                    RowScope::Services => &mut result.services,
                    _ => &mut result.unknown,
                }
            };
            charge.add(row.footprint);
        }
        result
    }
}

/// aterm's own process rows kept per snapshot, busiest first.
pub const ATERM_ROWS: usize = 512;
/// The machine's rows kept per snapshot by CPU and, separately, by footprint.
pub const TOP_ROWS: usize = 64;

impl Snapshot {
    /// The tab with key `key`.
    #[must_use]
    pub fn tab(&self, key: u64) -> Option<&TabView> {
        self.tabs.iter().find(|t| t.key == key)
    }

    /// The tab of the sessions `panes` (its split panes, focused first, as [`TabRoot::tab`]
    /// groups them): the first pane this sweep read names it, and its scope is every read
    /// pane's summed ([`ScopeView::sum`]); `None` where no pane was read.
    #[must_use]
    pub fn tab_of(&self, panes: &[u64]) -> Option<TabView> {
        let lead = panes.iter().find_map(|&k| self.tab(k))?;
        let scope = self
            .tabs
            .iter()
            .filter(|t| t.key != lead.key && panes.contains(&t.key))
            .fold(lead.scope.clone(), |sum, t| sum.sum(&t.scope));
        Some(TabView {
            scope,
            ..lead.clone()
        })
    }

    /// The [`ResourceView`] for `res`, its `tab` the summed figures of the sessions `panes` (one
    /// tab's split panes) that the row holds.
    #[must_use]
    pub fn resource(&self, res: Resource, panes: &[u64]) -> ResourceView {
        let mut v = self.resources[res.index()].clone();
        let complete: Vec<_> = v
            .logical_cpu
            .iter()
            .filter(|(keys, _)| !keys.is_empty() && keys.iter().all(|key| panes.contains(key)))
            .collect();
        let figs: Vec<&ScopeFig> = self.resources[res.index()]
            .tabs
            .iter()
            .filter(|(k, _)| {
                panes.contains(k) && !complete.iter().any(|(keys, _)| keys.contains(k))
            })
            .map(|(_, f)| f)
            .chain(complete.iter().map(|(_, fig)| fig))
            .collect();
        if !figs.is_empty() {
            v.tab = ScopeFig::sum(&figs, (res == Resource::Gpu).then_some(&v.aterm));
        }
        v
    }

    /// Normalized resource bar shares. Published process CPU counters provide no fraction
    /// of machine ticks, so CPU has no overlapping bar.
    #[must_use]
    pub fn bar_shares(&self, res: Resource, panes: &[u64]) -> Option<[f32; 3]> {
        let view = self.resource(res, panes);
        let clamp01 = |v: f32| {
            if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        match res {
            Resource::Cpu => None,
            Resource::Memory | Resource::Processes => {
                let b = view.binding();
                let cap = b.capacity.filter(|&c| c > 0.0)?;
                let global = clamp01((b.now? / cap) as f32);
                let aterm_val = view.aterm.now.or_else(|| match res {
                    Resource::Memory => self
                        .aterm
                        .as_ref()
                        .and_then(|a| a.footprint.map(|f| f as f64)),
                    Resource::Processes => self.aterm.as_ref().map(|a| f64::from(a.procs)),
                    _ => None,
                });
                let tab_val = view.tab.now.or_else(|| {
                    let t = self.tab_of(panes)?;
                    match res {
                        Resource::Memory => t.scope.footprint.map(|f| f as f64),
                        Resource::Processes => Some(f64::from(t.scope.procs)),
                        _ => None,
                    }
                });
                let aterm = aterm_val
                    .map(|v| clamp01((v / cap) as f32))
                    .unwrap_or(0.0)
                    .min(global);
                let tab = tab_val
                    .map(|v| clamp01((v / cap) as f32))
                    .unwrap_or(0.0)
                    .min(aterm);
                Some([global, aterm, tab])
            }
            Resource::Disk => {
                let cap = view.capacity.filter(|&c| c > 0.0)?;
                let global = clamp01((view.now? / cap) as f32);
                let aterm_io = self
                    .aterm
                    .as_ref()
                    .and_then(ScopeView::io_bps)
                    .or(view.aterm.now)
                    .unwrap_or(0.0)
                    .max(0.0);
                let tab_io = self
                    .tab_of(panes)
                    .and_then(|t| t.scope.io_bps())
                    .or(view.tab.now)
                    .unwrap_or(0.0)
                    .max(0.0)
                    .min(aterm_io);
                let global_io = self.global.io_bps().unwrap_or(0.0).max(aterm_io);
                let io_ref = global_io.max(50_000_000.0);
                let aterm = (global * clamp01((aterm_io / io_ref) as f32)).min(global);
                let tab = if aterm_io > 0.0 {
                    (aterm * clamp01((tab_io / aterm_io) as f32)).min(aterm)
                } else {
                    0.0
                };
                Some([global, aterm, tab])
            }
            Resource::Network => {
                let b = view.binding();
                let global = if let Some(h) = view.heat_now {
                    clamp01(h)
                } else {
                    let bps = b.now?;
                    clamp01((bps / 12_500_000.0) as f32)
                };
                let (a_sock, t_sock, denom_sock) = if let Some(net) = self.net.as_ref()
                    && let Some(a) = net.aterm_sockets
                {
                    let t: u32 = net
                        .tab_sockets
                        .iter()
                        .filter(|(k, _)| panes.contains(k))
                        .map(|(_, n)| *n)
                        .sum::<u32>()
                        .min(a);
                    let sys = net
                        .tcp
                        .and_then(|tcp| tcp.known_total())
                        .unwrap_or(0)
                        .max(a)
                        .max(1);
                    (f64::from(a), f64::from(t), f64::from(sys))
                } else if let Some(a) = view.aterm.now {
                    let t = view.tab.now.unwrap_or(0.0).min(a);
                    let sys = self
                        .net
                        .as_ref()
                        .and_then(|n| n.tcp.and_then(|tcp| tcp.known_total()).map(f64::from))
                        .unwrap_or(a)
                        .max(a)
                        .max(1.0);
                    (a, t, sys)
                } else {
                    (0.0, 0.0, 1.0)
                };
                let aterm = (global * clamp01((a_sock / denom_sock) as f32)).min(global);
                let tab = if a_sock > 0.0 {
                    (aterm * clamp01((t_sock / a_sock) as f32)).min(aterm)
                } else {
                    0.0
                };
                Some([global, aterm, tab])
            }
            Resource::Gpu | Resource::Tokens => None,
        }
    }

    /// Take the leftover reader's view.
    pub fn set_left(&mut self, left: Option<std::sync::Arc<crate::left::LeftView>>) {
        self.left = left;
    }

    /// Take the stuck engine's view, compose its state with this reading's Processes Room, and
    /// record any fatal job crash markers on the strip.
    pub fn set_jobs(&mut self, jobs: Option<std::sync::Arc<crate::jobs::JobsView>>) {
        self.set_jobs_observed(jobs, (self.at_unix_ms != 0).then_some(self.at_unix_ms), &[]);
    }

    /// Attach a jobs view with its original observation time and session-generation proof.
    /// Missing provenance stays unknown; it is never borrowed from a newer roster.
    pub fn set_jobs_observed(
        &mut self,
        jobs: Option<std::sync::Arc<crate::jobs::JobsView>>,
        at_unix_ms: Option<i64>,
        sources: &[crate::events::SessionScope],
    ) {
        self.resources[Resource::Processes.index()].state =
            processes_state(self.facts.process_room, jobs.as_deref());
        if let Some(view) = jobs.as_deref() {
            for row in &view.rows {
                if row.verdict != crate::jobs::Verdict::Exited {
                    continue;
                }
                if let (Some(sig_word), Some(at_unix_ms)) =
                    (row.exit.and_then(fatal_crash_signal_word), at_unix_ms)
                {
                    let label = format!("{} ({sig_word})", row.exe);
                    let scope = crate::events::EventScope::session(row.tab, sources);
                    let dup = self.strip_events.iter().any(|e| {
                        e.kind == StripEventKind::Crash
                            && e.label == label
                            && e.scope == scope
                            && awake_wall_span_ms(e.at_unix_ms, at_unix_ms, &self.time_gaps)
                                <= crate::jobs::EXITED_KEEP_MS
                    });
                    if !dup {
                        self.strip_events.push(StripEvent {
                            at_unix_ms,
                            kind: StripEventKind::Crash,
                            label,
                            scope,
                            span: None,
                        });
                    }
                }
            }
        }
        self.jobs = jobs;
    }

    /// Logical cores, where known.
    #[must_use]
    pub fn total_cores(&self) -> Option<usize> {
        self.cpu
            .as_ref()
            .map(CpuView::total)
            .filter(|n| *n > 0)
            .or_else(|| {
                self.cpu_counter_sample
                    .as_ref()
                    .map(Vec::len)
                    .filter(|n| *n > 0)
            })
    }

    /// Current GPU hardware and counters, even when utilization was not read.
    #[must_use]
    pub fn gpu_sample(&self) -> Option<&GpuSample> {
        self.facts
            .gpu
            .as_ref()
            .or_else(|| self.gpu.as_ref().map(|g| &g.sample))
    }
}

fn fatal_crash_signal_word(exit: i32) -> Option<&'static str> {
    if exit <= 128 {
        return None;
    }
    match exit - 128 {
        4 => Some("SIGILL"),
        5 => Some("SIGTRAP"),
        6 => Some("SIGABRT"),
        7 | 10 => Some("SIGBUS"),
        8 => Some("SIGFPE"),
        9 => Some("SIGKILL"),
        11 => Some("SIGSEGV"),
        _ => None,
    }
}

/// Wall-clock distance between `a` and `b` in milliseconds excluding [`GapKind::Sleep`] spans
/// (during which monotonic job-retention clocks pause).
fn awake_wall_span_ms<'a>(a: i64, b: i64, gaps: impl IntoIterator<Item = &'a TimeGap>) -> u64 {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let total = lo.abs_diff(hi);
    let mut sleeps: Vec<(i64, i64)> = gaps
        .into_iter()
        .filter(|g| g.kind == GapKind::Sleep)
        .filter_map(|g| {
            let s = g.start_ms.max(lo);
            let e = g.end_ms.min(hi);
            (e > s).then_some((s, e))
        })
        .collect();
    if sleeps.is_empty() {
        return total;
    }
    sleeps.sort_unstable();
    let mut asleep = 0_u64;
    let (mut cur_s, mut cur_e) = sleeps[0];
    for &(s, e) in &sleeps[1..] {
        if s <= cur_e {
            cur_e = cur_e.max(e);
        } else {
            asleep = asleep.saturating_add(cur_s.abs_diff(cur_e));
            cur_s = s;
            cur_e = e;
        }
    }
    asleep = asleep.saturating_add(cur_s.abs_diff(cur_e));
    total.saturating_sub(asleep)
}

#[derive(Clone, Debug, Default)]
struct ScopeState {
    cores: Trend,
    footprint: Trend,
    last_footprint_at: Option<u64>,
    history: History,
}

impl ScopeState {
    fn absorb(&mut self, acc: &ScopeAcc, dt_s: f32, at_ns: u64) -> ScopeView {
        self.cores.push(acc.cores, dt_s);
        if let Some(f) = acc.footprint {
            self.footprint
                .push(gib(f), interval_s(self.last_footprint_at, at_ns));
            self.last_footprint_at = Some(at_ns);
        }
        self.history.push_at(Some(acc.cores), at_ns);
        ScopeView {
            cores: self.cores.view(),
            cpu_accounting: Some(crate::published_cpu::CpuAccounting::PUBLISHED_COUNTERS),
            footprint: acc.footprint,
            footprint_unread: acc.footprint_unread,
            footprint_gib: self.footprint.view(),
            procs: acc.procs,
            history: self.history.to_vec(),
            top: acc.top.clone(),
            wait: acc.wait,
            read_bps: acc.reads,
            write_bps: acc.writes,
            refused: acc.refused,
            gpu: acc.gpu,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct ScopeAcc {
    cores: f32,
    /// Reconciled inherited-counter charge, separate from own-counter reported growth.
    cpu_charge: Option<f64>,
    footprint: Option<u64>,
    footprint_unread: u32,
    procs: u32,
    top: Option<(String, f32)>,
    /// Bytes read per second, where any member's counter gave a delta.
    reads: Option<f64>,
    /// Bytes written per second, where any member's counter gave a delta.
    writes: Option<f64>,
    /// Cores-worth waiting for a core across readable members.
    wait: Option<f32>,
    /// Members whose CPU time the OS refused.
    refused: u32,
    /// Additional gaps in the CPU scope figure: a rate with no baseline, or lost reap credit.
    cpu_incomplete: u32,
    /// GPU share of the device across members, while client shares are held.
    gpu: Option<f32>,
}

impl ScopeAcc {
    fn add(&mut self, p: &ProcSample, r: &ProcRates) {
        if let Some(g) = r.gpu {
            self.gpu = Some(self.gpu.unwrap_or(0.0) + g);
        }
        let cores = r.cores.unwrap_or(0.0);
        self.cores += cores;
        if let Some(f) = p.footprint {
            self.footprint = Some(self.footprint.unwrap_or(0).saturating_add(f));
        } else {
            self.footprint_unread = self.footprint_unread.saturating_add(1);
        }
        if let Some(rd) = r.read_bps {
            self.reads = Some(self.reads.unwrap_or(0.0) + rd);
        }
        if let Some(w) = r.write_bps {
            self.writes = Some(self.writes.unwrap_or(0.0) + w);
        }
        if let Some(w) = r.wait {
            self.wait = Some(self.wait.unwrap_or(0.0) + w);
        }
        if p.cpu_ns.is_none() {
            self.refused += 1;
        } else if r.cores.is_none() {
            self.cpu_incomplete += 1;
        }
        self.procs += 1;
        if cores > 0.0 && self.top.as_ref().is_none_or(|(_, c)| cores > *c) {
            self.top = Some((p.name.clone(), cores));
        }
    }
}

/// Per-process CPU, wait, and storage I/O rates over the sample interval.
#[derive(Clone, Copy, Debug, Default)]
struct ProcRates {
    read_at_ns: u64,
    io_read_span_ns: Option<u64>,
    cores: Option<f32>,
    cpu_read_span_ns: Option<u64>,
    wait: Option<f32>,
    read_bps: Option<f64>,
    write_bps: Option<f64>,
    gpu: Option<f32>,
    energy_mw: Option<f64>,
    energy_read_span_ns: Option<u64>,
}

/// One reported own counter, bound to a known process generation and observed exec version.
#[derive(Clone, Copy, Debug)]
struct ProcessCpuCounter {
    start: Option<u64>,
    version: Option<i32>,
    ns: u64,
}

impl ProcessCpuCounter {
    fn matches(self, p: &ProcSample) -> bool {
        let start_matches = self.start.zip(p.start).is_some_and(|(a, b)| a == b);
        let version_matches = self.version.zip(p.version).is_some_and(|(a, b)| a == b);
        let start_changed = self.start.zip(p.start).is_some_and(|(a, b)| a != b);
        let version_changed = self.version.zip(p.version).is_some_and(|(a, b)| a != b);
        (start_matches || version_matches) && !start_changed && !version_changed
    }

    fn growth(self, p: &ProcSample) -> Option<u64> {
        self.matches(p).then_some(())?;
        p.cpu_ns?.checked_sub(self.ns)
    }
}

/// When a reading was taken, on both clocks, and the cadence then.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Stamp {
    wall_ms: i64,
    mono_ms: u64,
    cadence_ms: u32,
}

impl Stamp {
    fn of(s: &Sample) -> Self {
        Self {
            wall_ms: s.at_unix_ms,
            mono_ms: s.mono_ms,
            cadence_ms: s.cadence_ms,
        }
    }

    fn rate_dt(self, then: Option<Self>) -> Option<f64> {
        let then = then?;
        let dt = self.mono_ms.checked_sub(then.mono_ms).filter(|&d| d > 0)?;
        let wall = self.wall_ms.checked_sub(then.wall_ms)?;
        let skew = wall.checked_sub(i64::try_from(dt).ok()?)?;
        let limit = GAP_CADENCES * u64::from(then.cadence_ms.max(self.cadence_ms));
        ((-SLEEP_SKEW_MS..=SLEEP_SKEW_MS).contains(&skew) && dt <= limit)
            .then_some(dt as f64 / 1000.0)
    }

    /// Whether this reading follows `then` with no gap the strip records between them: both
    /// clocks ran on together (no sleep, no clock step), and it came within the interval a rate
    /// reads over ([`Self::rate_dt`]) or under one strip box later. A reading late by less than
    /// a box reads no rate and still opens no gap, so what held through `then` held up to it.
    fn follows(self, then: Self) -> bool {
        self.rate_dt(Some(then)).is_some()
            || self
                .mono_ms
                .checked_sub(then.mono_ms)
                .filter(|&dt| dt > 0)
                .and_then(|dt| i64::try_from(dt).ok())
                .zip(self.wall_ms.checked_sub(then.wall_ms))
                .is_some_and(|(dt, wall)| {
                    (0..BUCKET_MS).contains(&wall)
                        && wall
                            .checked_sub(dt)
                            .is_some_and(|skew| (-SLEEP_SKEW_MS..=SLEEP_SKEW_MS).contains(&skew))
                })
    }
}

/// Last token fold and held per-minute rate for one agent session.
#[derive(Clone, Debug)]
struct Session {
    /// The last reading that carried the session: the gap rule's baseline.
    seen: Stamp,
    /// When the fold read the totals (monotonic ms).
    read_ms: i64,
    /// A fold carried across a gap cannot start the next rate interval.
    baseline: bool,
    out: u64,
    input: u64,
    unread: usize,
    unread_more: bool,
    complete: bool,
    /// Held `(output, input)` tokens/min from the last valid fold interval.
    held: Option<(f64, f64)>,
}

impl Session {
    fn of(t: &TabTokens, seen: Stamp, read_ms: i64, held: Option<(f64, f64)>) -> Self {
        Self {
            seen,
            read_ms,
            baseline: true,
            out: t.out_total,
            input: t.in_total,
            unread: t.unread,
            unread_more: t.unread_more,
            complete: t.complete,
            held,
        }
    }

    fn same_fold(&self, t: &TabTokens) -> bool {
        self.out == t.out_total
            && self.input == t.in_total
            && self.unread == t.unread
            && self.unread_more == t.unread_more
            && self.complete == t.complete
    }

    fn rates_to(&self, t: &TabTokens, read_ms: i64) -> Option<(f64, f64)> {
        if !self.baseline
            || self.unread != t.unread
            || self.unread_more != t.unread_more
            || !self.complete
            || !t.complete
        {
            return None;
        }
        let dt = read_ms.checked_sub(self.read_ms).filter(|&d| d > 0)? as f64 / 1000.0;
        let out = t.out_total.checked_sub(self.out)? as f64;
        let input = t.in_total.checked_sub(self.input)? as f64;
        (out <= TOKENS_CAP_PER_S * dt).then_some((out / dt * 60.0, input / dt * 60.0))
    }
}

/// One sample's token rates.
#[derive(Debug, Default)]
struct TokenPass {
    /// aterm's output and input tokens per minute, where a session gave one.
    out: Option<f64>,
    input: Option<f64>,
    /// Each tab's output tokens per minute, where a session gave one.
    tabs: HashMap<u64, f64>,
    /// Tabs with an agent session.
    with_agent: HashSet<u64>,
    /// Sessions read.
    agents: usize,
}

/// What one process sweep gives the rows' scope figures.
#[derive(Debug, Default)]
struct SweepFigs {
    aterm: ScopeAcc,
    /// In the sample's tab order.
    tabs: Vec<ScopeAcc>,
    /// Whether any process's write counter was read.
    writes_read: bool,
    /// Every program that wrote, busiest first.
    writers: Vec<Writer>,
    busiest: Option<Busiest>,
}

/// [`Vitals::absorb_procs`]'s result.
#[derive(Debug, Default)]
struct ProcPass {
    /// Canonical group keys and rates in the original full process order.
    group_keys: Vec<Option<crate::groups::GroupKey>>,
    rates: Vec<ProcRates>,
    aterm: Option<ScopeView>,
    shared: Option<ScopeView>,
    tabs: Vec<TabView>,
    top: Vec<TopProc>,
    delegated: Vec<DelegatedUsage>,
    figs: Option<SweepFigs>,
    /// `sys procs`' rows ([`keep_rows`]).
    rows: Vec<ProcView>,
    /// [`Snapshot::programs`] ([`keep_programs`]).
    programs: Vec<ProgramGroup>,
    memory_owners: Option<MemoryOwners>,
    /// App, user-other, service, unknown-owner published rates and unread CPU count.
    split_parts: Option<(f32, f32, f32, f32, u32)>,
    /// `(aterm, others, services)` GPU shares for [`crate::gpu_clients::GpuSplit`].
    gpu_parts: Option<(f32, f32, f32)>,
    /// The sweep's window, ns.
    window_ns: Option<u64>,
    /// Cached [`attribute_delegated`] result for [`crate::explain`].
    attribution: Option<(Vec<Membership>, Vec<Option<usize>>)>,
}

/// Running state for the deck's resource rows.
#[derive(Debug, Default)]
struct Rows {
    /// The exact history reading handed to this observation's resource rings.
    taken: Taken,
    /// Each row's hour ring, in [`Resource::ALL`] order: the first series.
    rings: [BucketRing; 7],
    /// Network's send direction.
    net_tx: BucketRing,
    /// How many processes run, beside the starts `rings[Processes]` holds where a start counter
    /// reads (record §8.2).
    proc_count: BucketRing,
    /// What `rings[Processes]` and the scopes' Processes rings hold: starts a second (`true`),
    /// the count against its limit (`false`), or nothing yet ([`Rows::proc_meaning`]).
    proc_starts: Option<bool>,
    /// Whether a reading of this run carried a start counter.
    starts_read: bool,
    /// The task-table Room warning's sole hysteresis latch; no count or limit is cached.
    room: crate::room::Tracker,
    /// Memory pressure's rank ([`Pressure::rank`]), pushed beside Memory's ring: a bucket's
    /// largest reading is the worst level read in it.
    pressure: BucketRing,
    /// Compute watts, CPU + GPU + Neural Engine ([`crate::heat::Watts::compute`]): the Heat
    /// row's strip (A4 §4).
    compute_w: BucketRing,
    /// The thermal pressure level, over while Moderate or more: each box's share at pressure and
    /// its worst level ([`crate::heat::PressureBox`]), the cap over the Heat strip.
    thermal: BucketRing,
    /// Process exits a second, beside the starts `rings[Processes]` holds (A4 §6.2); unread
    /// where the start counter says no exits (Linux's forks).
    proc_exits: BucketRing,
    /// What blocks the processes' flow, over while anything does: each box's blocked share and
    /// worst kind ([`crate::blockage::BlockageBox`]), the cap over the Processes strip.
    blocked: BucketRing,
    /// Work per joule ([`crate::heat::Judged::ipj`]), kept only to learn its peak: no row draws
    /// a strip of it, so the checkpoint holds none.
    work: BucketRing,
    /// Last known fixed capacity per row.
    caps: [Option<f64>; 7],
    /// aterm's figures' hour, by row.
    aterm: ScopeRings,
    /// Each tab's, by key.
    tabs: HashMap<u64, ScopeRings>,
    /// Logical-tab CPU series keyed by the ledger's stable tab identity.
    logical_cpu: HashMap<u64, ScopeRings>,
    /// aterm's input tokens per minute.
    tokens_in: Recent,
    peaks: Peaks,
    free: FreeHistory,
    /// The same readings as `free`, kept 3 weeks in hour buckets.
    weeks: FreeWeeks,
    cores_at: Option<Stamp>,
    sweep_at: Option<Stamp>,
    /// Each process of the last sweep: its start and cumulative bytes read (`None` unread).
    read_bytes: IdMap<u32, (Option<u64>, Option<u64>)>,
    /// Each process of the last sweep: its start and cumulative bytes written (`None` unread).
    written: IdMap<u32, (Option<u64>, Option<u64>)>,
    /// Cached hardware processor brand once supplied by the host.
    cpu_brand: Option<String>,
    /// Whether the last sweep read the whole process table (not stopped at its cap).
    sweep_whole: bool,
    /// The last hour of writes per program.
    write_hour: crate::writes::WriteHour,
    links_at: Option<Stamp>,
    /// Each link's counters at the last reading, by name.
    links: HashMap<String, LinkSample>,
    /// Name of the currently selected reference network link.
    link_pick: Option<String>,
    /// Another counted link leading it, and for how many readings in a row.
    link_lead: Option<(String, u32)>,
    /// Swap's counter and when it was read.
    swap: Option<(Stamp, u64)>,
    /// The start counter at the last reading that read one, and when ([`RowFacts::flow`]).
    starts: Option<(Stamp, crate::starts::StartCount)>,
    /// The heat counters' change the start counter's merge gave this reading, or why it gave none
    /// ([`crate::starts::Moved::heat`]); `None` where the reading's start counter carries no heat.
    heat_moved: Option<Result<crate::heat::HeatMove, &'static str>>,
    /// Per-id facts carried by that same accepted transition; consumed by the group feed.
    group_energy: Option<Result<Arc<crate::heat::EnergyFacts>, &'static str>>,
    group_starts: Vec<(u64, f64, u64)>,
    sessions: HashMap<String, Session>,
    /// Monotonic timestamp (ms) since which `rate60` disk filling has been positive.
    filling_since: Option<u64>,
    /// The GPU's reset count at the first reading, and when it first rose.
    gpu_recoveries: Option<u64>,
    gpu_rose_at: Option<i64>,
    /// Each process's GPU share from the client reads.
    gpu_clients: crate::gpu_clients::GpuShares,
    /// The units' 10 s readings and rates ([`crate::units`]).
    units: crate::units::UnitRings,
    /// The run queue's readings, P10's window ([`crate::runq`]).
    runq: crate::runq::RunQueueRing,
    /// Zombies and uninterruptible waits between readings ([`crate::blockage`]).
    blockage: crate::blockage::Book,
}

/// One scope's hour rings in [`Resource::ALL`] order; a row's ring exists once the scope read it.
#[derive(Debug, Default)]
struct ScopeRings([Option<BucketRing>; 7], [BucketRing; 2], BucketRing);

impl ScopeRings {
    fn push(&mut self, t: Taken, vals: [Option<f64>; 7]) {
        for (ring, v) in self.0.iter_mut().zip(vals) {
            if let Some(v) = v {
                let _ = ring
                    .get_or_insert_with(BucketRing::default)
                    .push(t, Some(v), false);
            }
        }
    }

    fn ring(&self, res: Resource) -> Option<&BucketRing> {
        self.0[res.index()].as_ref()
    }
}

/// The model's running state. One per host instance.
#[derive(Debug)]
pub struct Vitals {
    /// The sole in-memory group history; the GUI supplies this instance at construction.
    groups: crate::groups::GroupBook,
    self_cpu: crate::self_cpu::OwnCpuState,
    hardware_recorder: crate::hardware::Recorder,
    historical_cpu_report: Option<std::sync::Arc<crate::sys::json::Jv>>,
    authorized_cpu_policy: Option<std::sync::Arc<crate::sys::json::Jv>>,
    rows: Rows,
    seq: u64,
    last_cores_at: Option<u64>,
    last_gpu_at: Option<u64>,
    last_mem_at: Option<u64>,
    last_cores: Vec<Ticks>,
    core_trends: Vec<Trend>,
    core_histories: Vec<History>,
    busy: Trend,
    busy_history: History,
    gpu: Trend,
    gpu_history: History,
    mem: Trend,
    mem_history: History,
    last_counters: Option<(u64, MemCounters)>,
    last_sweep_at: Option<u64>,
    last_cpu: IdMap<u32, ProcessCpuCounter>,
    last_energy: IdMap<u32, ProcessCpuCounter>,
    last_runnable: IdMap<u32, u64>,
    last_read_at: IdMap<u32, u64>,
    global_foot: Trend,
    app_footprint: Trend,
    last_app_footprint_at: Option<u64>,
    aterm: ScopeState,
    shared: ScopeState,
    /// Live process origin keyed by PID and kernel start, retained across reparenting.
    origins: IdMap<crate::delegated::ProcessKey, Origin>,
    origins_incomplete: bool,
    tabs: HashMap<u64, ScopeState>,
    detector: crate::events::Detector,
    why: crate::explain::WhyState,
    pane_tokens: Arc<crate::pane_tokens::Panes>,
    pane_cpu: Arc<crate::pane_cpu::Panes>,
    pane_resources: crate::pane_resources::State,
    last_stamp: Option<Stamp>,
    restored_wall_ms: i64,
    last_pressure: Pressure,
    last_throttle: Throttle,
    last_gpu_resets: u64,
    time_gaps: VecDeque<TimeGap>,
    strip_events: VecDeque<StripEvent>,
    /// Accepted insertions/corrections awaiting the existing event publication path.
    strip_changes: VecDeque<(crate::events::StripChangeKind, StripEvent)>,
    strip_changes_lost: u64,
    /// The situation's stays of the last six hours, oldest first ([`crate::situation::Span`]).
    situation_spans: Vec<crate::situation::Span>,
    /// How many times the strip's held buckets changed other than by a bucket closing
    /// ([`Self::strip_rewrites`]).
    strip_rewrites: u64,
    /// The heat chain's state across readings: the usual Super clock per decile.
    heat: crate::heat::Engine,
    /// The Spotlight family's last split reading and its day ([`crate::spotlight`]).
    spotlight: crate::spotlight::Family,
}

impl Default for Vitals {
    fn default() -> Self {
        Self::with_group_book(crate::groups::GroupBook::default())
    }
}

/// The most gaps, the most events and the most situation spans a strip keeps.
pub const STRIP_HISTORY_CAP: usize = 128;
/// How far back the strip's gaps, events and situation spans are kept, ms: the Full deck's six
/// hours.
pub const STRIP_HORIZON_MS: i64 = 6 * 3_600_000;

/// How many rings a strip checkpoint holds, by slot ([`SavedStrip::ring_mut`]): the seven rows',
/// Network's send direction, memory pressure, aterm's seven, the process count, aterm's
/// memory/CPU coverage and raw CPU charge, and the instruments' four (A4): compute watts,
/// the thermal pressure level, process exits and blockage. A slot keeps its number for
/// good: a tail line names its rings by it.
pub const STRIP_RINGS: usize = 24;

/// Persisted strip state across restarts and crashes (the strip folder's base, schema 4): an
/// hour of 10 s buckets and seven of ten-minute ones, replayed forward by the tail's
/// [`StripLine`]s, with six hours of gaps, events and situation spans and the usual Super
/// clocks.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedStrip {
    /// Basis of persisted app CPU cells; absent legacy cells cannot enter the reported channel.
    pub scope_cpu_accounting: Option<crate::published_cpu::CpuAccounting>,
    /// Wall-clock timestamp (Unix ms) of the newest absorbed sample, or `0` when empty.
    pub last_wall_ms: i64,
    /// The newest ten-second epoch it holds closed: a tail line replays only past it
    /// ([`Self::replay`]). `i64::MIN` for a strip that has closed none.
    pub through_k: i64,
    /// True when written during a graceful probe-thread shutdown; false on periodic live checkpoints.
    pub clean_stop: bool,
    /// Fixed capacities learned for each [`Resource`] row.
    pub caps: [Option<f64>; 7],
    /// Global multi-scale rings (1-hour 10 s and 6-hour 10-minute) in [`Resource::ALL`] order.
    pub rings: [crate::ring::SavedRing; 7],
    /// Network transmit multi-scale ring (1-hour 10 s and 6-hour 10-minute).
    pub net_tx: crate::ring::SavedRing,
    /// How many processes ran (1-hour 10 s and 7-hour 10-minute), beside the starts
    /// `rings[Processes]` holds where a start counter reads.
    pub proc_count: crate::ring::SavedRing,
    /// Memory pressure rank multi-scale ring (1-hour 10 s and 6-hour 10-minute).
    pub pressure: crate::ring::SavedRing,
    /// aterm-scope multi-scale rings (1-hour 10 s and 6-hour 10-minute) in [`Resource::ALL`] order.
    pub aterm: [crate::ring::SavedRing; 7],
    /// Scope coverage in memory/CPU order; CPU gaps do not establish execution-window bounds.
    pub aterm_unread: [crate::ring::SavedRing; 2],
    /// Reconciled inherited charge, separate from published own counters.
    pub aterm_cpu_charge: crate::ring::SavedRing,
    /// Compute watts, CPU + GPU + Neural Engine (1-hour 10 s and 7-hour 10-minute).
    pub compute_w: crate::ring::SavedRing,
    /// The thermal pressure level, over while Moderate or more: a box's over time is its share
    /// at pressure and its largest reading its worst level ([`crate::heat::PressureBox`]).
    pub thermal: crate::ring::SavedRing,
    /// Process exits a second, beside the starts `rings[Processes]` holds.
    pub proc_exits: crate::ring::SavedRing,
    /// What blocks the processes' flow, over while anything does: a box's over time is its
    /// blocked share and its largest reading its worst kind ([`crate::blockage::BlockageBox`]).
    pub blockage: crate::ring::SavedRing,
    /// What the Processes rings hold: starts a second (`true`), the process count (`false`), or
    /// nothing yet.
    pub proc_starts: Option<bool>,
    /// Discontinuous wall-clock intervals in the last six hours ([`STRIP_HORIZON_MS`]).
    pub time_gaps: Vec<TimeGap>,
    /// Vertical event markers in the last six hours ([`STRIP_HORIZON_MS`]).
    pub strip_events: Vec<StripEvent>,
    /// The situation's stays in the last six hours, oldest first ([`STRIP_HORIZON_MS`]): each
    /// with netplace's kind for the place, the power source and the lid.
    pub situation: Vec<crate::situation::Span>,
    /// The usual Super clock per busy decile, as the heat chain had learned it.
    pub usual: crate::heat::UsualClocks,
}

impl Default for SavedStrip {
    fn default() -> Self {
        Self {
            scope_cpu_accounting: None,
            last_wall_ms: 0,
            through_k: i64::MIN,
            clean_stop: false,
            caps: [None; 7],
            rings: std::array::from_fn(|_| crate::ring::SavedRing::default()),
            net_tx: crate::ring::SavedRing::default(),
            proc_count: crate::ring::SavedRing::default(),
            pressure: crate::ring::SavedRing::default(),
            aterm: std::array::from_fn(|_| crate::ring::SavedRing::default()),
            aterm_unread: std::array::from_fn(|_| crate::ring::SavedRing::default()),
            aterm_cpu_charge: crate::ring::SavedRing::default(),
            compute_w: crate::ring::SavedRing::default(),
            thermal: crate::ring::SavedRing::default(),
            proc_exits: crate::ring::SavedRing::default(),
            blockage: crate::ring::SavedRing::default(),
            proc_starts: None,
            time_gaps: Vec::new(),
            strip_events: Vec::new(),
            situation: Vec::new(),
            usual: crate::heat::UsualClocks::default(),
        }
    }
}

impl SavedStrip {
    fn clear_scope_cpu(&mut self) {
        self.aterm[Resource::Cpu.index()] = crate::ring::SavedRing::default();
        self.aterm_unread[1] = crate::ring::SavedRing::default();
        self.aterm_cpu_charge = crate::ring::SavedRing::default();
    }
    /// Preserve a checkpoint's measured figures when its old format did not retain coverage.
    /// Every old CPU/memory scope reading stays explicitly incomplete at its original time.
    pub fn mark_scope_coverage_unknown(&mut self) {
        for (resource, coverage) in [(Resource::Memory, 0), (Resource::Cpu, 1)] {
            let mut ring = self.aterm[resource.index()].clone();
            for (_, agg) in ring.buckets.iter_mut().chain(&mut ring.long) {
                *agg = unknown_coverage(*agg);
            }
            for (_, value, over, _) in &mut ring.recent {
                *value = 1.0;
                *over = false;
            }
            self.aterm_unread[coverage] = ring;
        }
    }

    /// The ring in slot `slot` (`0..`[`STRIP_RINGS`]): the rows' in [`Resource::ALL`] order,
    /// then Network's send direction, memory pressure, aterm's in row order, the process
    /// count, aterm's memory/CPU coverage and its raw CPU charge, compute watts, the thermal
    /// pressure level, process exits and blockage. `None` past the last.
    pub fn ring_mut(&mut self, slot: usize) -> Option<&mut crate::ring::SavedRing> {
        match slot {
            0..7 => self.rings.get_mut(slot),
            7 => Some(&mut self.net_tx),
            8 => Some(&mut self.pressure),
            9..16 => self.aterm.get_mut(slot - 9),
            16 => Some(&mut self.proc_count),
            17..19 => self.aterm_unread.get_mut(slot - 17),
            19 => Some(&mut self.aterm_cpu_charge),
            20 => Some(&mut self.compute_w),
            21 => Some(&mut self.thermal),
            22 => Some(&mut self.proc_exits),
            23 => Some(&mut self.blockage),
            _ => None,
        }
    }

    /// The ring in slot `slot`, as [`Self::ring_mut`] numbers them.
    #[must_use]
    pub fn ring(&self, slot: usize) -> Option<&crate::ring::SavedRing> {
        match slot {
            0..7 => self.rings.get(slot),
            7 => Some(&self.net_tx),
            8 => Some(&self.pressure),
            9..16 => self.aterm.get(slot - 9),
            16 => Some(&self.proc_count),
            17..19 => self.aterm_unread.get(slot - 17),
            19 => Some(&self.aterm_cpu_charge),
            20 => Some(&self.compute_w),
            21 => Some(&self.thermal),
            22 => Some(&self.proc_exits),
            23 => Some(&self.blockage),
            _ => None,
        }
    }

    /// Replay `line`, a tail line written after this base: each ring takes its closed bucket
    /// ([`crate::ring::SavedRing::put_closed`]), the capacities it read, the gaps and events
    /// it opened, the situation spans it moved ([`crate::situation::Span::put`]) and the usual
    /// clocks it taught, and the strip's newest wall clock moves to the line's. A line at or
    /// before [`Self::through_k`] holds nothing the base does not, and changes nothing; the
    /// result says whether it was replayed.
    pub fn replay(&mut self, mut line: StripLine) -> bool {
        if line.k <= self.through_k {
            return false;
        }
        let reported = line.scope_cpu_accounting
            == Some(crate::published_cpu::CpuAccounting::PUBLISHED_COUNTERS);
        if reported {
            if self.scope_cpu_accounting != line.scope_cpu_accounting {
                self.clear_scope_cpu();
            }
            self.scope_cpu_accounting = line.scope_cpu_accounting;
        } else {
            line.rings.retain(|(slot, _)| !matches!(*slot, 9 | 18 | 19));
        }
        // Older tail lines retained scope figures but no coverage. Reading that absence as
        // zero would turn a historical lower bound into an exact figure after a restart.
        for (figure, coverage) in [(9, 18), (11, 17)] {
            if !line.rings.iter().any(|(slot, _)| *slot == coverage)
                && let Some((_, agg)) = line.rings.iter().find(|(slot, _)| *slot == figure)
            {
                line.rings.push((coverage, unknown_coverage(*agg)));
            }
        }
        for (slot, agg) in line.rings {
            if let Some(r) = self.ring_mut(usize::from(slot)) {
                r.put_closed(line.k, agg);
            }
        }
        for (cap, read) in self.caps.iter_mut().zip(line.caps) {
            if let Some(c) = read.filter(|c| c.is_finite() && *c > 0.0) {
                *cap = Some(c);
            }
        }
        if line.proc_starts.is_some() {
            self.proc_starts = line.proc_starts;
        }
        self.time_gaps.extend(line.time_gaps);
        self.strip_events.extend(line.strip_events);
        for span in line.spans {
            crate::situation::Span::put(&mut self.situation, span, STRIP_HISTORY_CAP);
        }
        self.usual.put(&line.usual);
        self.last_wall_ms = self.last_wall_ms.max(line.wall_ms);
        self.through_k = line.k;
        true
    }
}

fn unknown_coverage(agg: Agg) -> Agg {
    Agg {
        mean: 1.0,
        min: 1.0,
        max: 1.0,
        over_ms: 0,
        ..agg
    }
}

/// One closed ten-second epoch of every strip ring: a line of the checkpoint's append-only tail
/// (record §9), written at the first reading past the epoch's end ([`Vitals::strip_line`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StripLine {
    /// Basis for app CPU cells in this line; absent means those cells are unsupported legacy data.
    pub scope_cpu_accounting: Option<crate::published_cpu::CpuAccounting>,
    /// The epoch: unix seconds `[10 k, 10 k + 10)`.
    pub k: i64,
    /// The newest reading's wall clock when it was written, unix ms: a restore's
    /// [`SavedStrip::last_wall_ms`].
    pub wall_ms: i64,
    /// The capacities known when it was written.
    pub caps: [Option<f64>; 7],
    /// Each ring's bucket for the epoch, where it read anything, by slot
    /// ([`SavedStrip::ring_mut`]).
    pub rings: Vec<(u8, Agg)>,
    /// What the Processes rings held when it was written ([`SavedStrip::proc_starts`]): a base
    /// written before the run's first reading knows none.
    pub proc_starts: Option<bool>,
    /// Gaps opened or changed since the previous line.
    pub time_gaps: Vec<TimeGap>,
    /// Events marked since the previous line.
    pub strip_events: Vec<StripEvent>,
    /// Situation spans opened or moved since the previous line, oldest first.
    pub spans: Vec<crate::situation::Span>,
    /// The usual clocks' deciles taught since the previous line
    /// ([`crate::heat::UsualClocks::rows_unlike`]).
    pub usual: Vec<crate::heat::UsualRow>,
}

impl Vitals {
    /// Immutable zombie facts from this reading's supported process census. Missing or
    /// unsupported current input is unavailable even while an older display view is retained.
    #[must_use]
    pub fn current_zombie_facts(&self) -> Option<Arc<crate::blockage::ZombieFacts>> {
        self.rows.blockage.current_zombie_facts()
    }

    /// Published growth by its original observed assignment, including closed tabs.
    #[must_use]
    pub fn historical_reported(&self) -> crate::explain::HistoricalReported {
        self.why.historical_reported()
    }

    /// Whether native completion evidence needs another complete process reading before its
    /// delivered journal cursor can be considered adopted by the model.
    #[must_use]
    pub fn has_pending_native(&self) -> bool {
        self.why.has_pending_native()
    }
    /// A model that has read nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct the model around the supplied sole group book.
    #[must_use]
    pub fn with_group_book(groups: crate::groups::GroupBook) -> Self {
        Self {
            groups,
            self_cpu: Default::default(),
            hardware_recorder: Default::default(),
            historical_cpu_report: None,
            authorized_cpu_policy: None,
            rows: Default::default(),
            seq: 0,
            last_cores_at: None,
            last_gpu_at: None,
            last_mem_at: None,
            last_cores: Vec::new(),
            core_trends: Vec::new(),
            core_histories: Vec::new(),
            busy: Default::default(),
            busy_history: Default::default(),
            gpu: Default::default(),
            gpu_history: Default::default(),
            mem: Default::default(),
            mem_history: Default::default(),
            last_counters: None,
            last_sweep_at: None,
            last_cpu: IdMap::default(),
            last_energy: IdMap::default(),
            last_runnable: IdMap::default(),
            last_read_at: IdMap::default(),
            global_foot: Default::default(),
            app_footprint: Default::default(),
            last_app_footprint_at: None,
            aterm: Default::default(),
            shared: Default::default(),
            origins: IdMap::default(),
            origins_incomplete: false,
            tabs: HashMap::new(),
            detector: Default::default(),
            why: Default::default(),
            pane_tokens: Arc::default(),
            pane_cpu: Arc::default(),
            pane_resources: crate::pane_resources::State::default(),
            last_stamp: None,
            restored_wall_ms: 0,
            last_pressure: Default::default(),
            last_throttle: Default::default(),
            last_gpu_resets: 0,
            time_gaps: VecDeque::new(),
            strip_events: VecDeque::new(),
            strip_changes: VecDeque::new(),
            strip_changes_lost: 0,
            situation_spans: Vec::new(),
            strip_rewrites: 0,
            heat: Default::default(),
            spotlight: Default::default(),
        }
    }

    /// Record a wall-clock discontinuity (`Sleep`, `Crash`, or `Paused`) on the time-series strip,
    /// merging overlapping spans of the same kind (or upgrading overlapping spans) without bleeding
    /// a crash/sleep label into a merely touching earlier span of another kind.
    pub fn record_time_gap(&mut self, mut gap: TimeGap) {
        if gap.end_ms <= gap.start_ms {
            return;
        }
        loop {
            let before = self.time_gaps.len();
            self.time_gaps.retain(|g| {
                let merges = g.overlap_ms(gap.start_ms, gap.end_ms) > 0
                    || (g.kind == gap.kind
                        && (g.end_ms == gap.start_ms || gap.end_ms == g.start_ms));
                if merges {
                    gap.start_ms = gap.start_ms.min(g.start_ms);
                    gap.end_ms = gap.end_ms.max(g.end_ms);
                    gap.kind = gap.kind.max(g.kind);
                    false
                } else {
                    true
                }
            });
            if self.time_gaps.len() == before {
                break;
            }
        }
        let pos = self
            .time_gaps
            .partition_point(|g| (g.start_ms, g.end_ms) <= (gap.start_ms, gap.end_ms));
        self.time_gaps.insert(pos, gap);
        while self.time_gaps.len() > STRIP_HISTORY_CAP {
            self.time_gaps.pop_front();
        }
    }

    /// Record a vertical event marker (`Crash`, `Restart`, `SleepWake`, `PressureSpike`) on the
    /// multi-scale time-series strip, deduplicating identical adjacent markers and coalescing a
    /// checkpoint crash estimate with an exact crash-file timestamp (or a delayed post-wake sample
    /// with the kernel's `waketime` stamp) inside the same discontinuity span.
    pub fn record_strip_event(&mut self, event: StripEvent) {
        if event.kind == StripEventKind::Crash {
            let gap_covering = |t: i64| {
                self.time_gaps.iter().find(|g| {
                    g.kind == GapKind::Crash
                        && t >= g.start_ms.saturating_sub(2_000)
                        && t < g.end_ms
                })
            };
            let is_host_crash_detail =
                |s: &str| !s.is_empty() && s != "aterm crash" && !s.contains(" (SIG");
            let coalesces_checkpoint = |existing: &StripEvent| {
                if existing.kind != StripEventKind::Crash
                    || existing.scope != event.scope
                    || !((existing.label == "aterm crash" && is_host_crash_detail(&event.label))
                        || (is_host_crash_detail(&existing.label) && event.label == "aterm crash"))
                {
                    return false;
                }
                let (lo, hi) = (
                    existing.at_unix_ms.min(event.at_unix_ms),
                    existing.at_unix_ms.max(event.at_unix_ms),
                );
                if self.strip_events.iter().any(|r| {
                    r.kind == StripEventKind::Restart && r.at_unix_ms > lo && r.at_unix_ms <= hi
                }) {
                    return false;
                }
                match (
                    gap_covering(existing.at_unix_ms),
                    gap_covering(event.at_unix_ms),
                ) {
                    (Some(ga), Some(gb)) => ga == gb,
                    (None, None) => existing.at_unix_ms.abs_diff(event.at_unix_ms) <= 15_000,
                    (Some(_), None) | (None, Some(_)) => false,
                }
            };
            if let Some(idx) = self.strip_events.iter().position(coalesces_checkpoint) {
                let existing = &mut self.strip_events[idx];
                let accepted = if existing.label == "aterm crash" {
                    existing.at_unix_ms = event.at_unix_ms;
                    existing.label = event.label;
                    existing.span = event.span;
                    Some(existing.clone())
                } else {
                    None
                };
                self.strip_events
                    .make_contiguous()
                    .sort_by_key(|e| e.at_unix_ms);
                if let Some(accepted) = accepted {
                    self.note_strip_change(accepted, crate::events::StripChangeKind::Update);
                }
                return;
            }
        }
        if event.kind == StripEventKind::SleepWake {
            let same_wake_span = |a: i64, b: i64| {
                a.abs_diff(b) <= 15_000
                    || self.time_gaps.iter().any(|g| {
                        g.kind == GapKind::Sleep
                            && a >= g.start_ms.saturating_sub(2_000)
                            && a <= g.end_ms.saturating_add(15_000)
                            && b >= g.start_ms.saturating_sub(2_000)
                            && b <= g.end_ms.saturating_add(15_000)
                    })
            };
            if let Some(existing) = self.strip_events.iter_mut().find(|e| {
                e.kind == StripEventKind::SleepWake
                    && e.scope == event.scope
                    && same_wake_span(e.at_unix_ms, event.at_unix_ms)
            }) {
                let accepted = if event.at_unix_ms < existing.at_unix_ms {
                    existing.at_unix_ms = event.at_unix_ms;
                    existing.span = event.span.or(existing.span);
                    Some(existing.clone())
                } else {
                    None
                };
                self.strip_events
                    .make_contiguous()
                    .sort_by_key(|e| e.at_unix_ms);
                if let Some(accepted) = accepted {
                    self.note_strip_change(accepted, crate::events::StripChangeKind::Update);
                }
                return;
            }
        }
        let tol_ms: u64 = if event.kind == StripEventKind::Restart {
            5_000
        } else {
            1_500
        };
        let dup = self.strip_events.iter().any(|e| {
            e.kind == event.kind
                && e.scope == event.scope
                && e.at_unix_ms.abs_diff(event.at_unix_ms) <= tol_ms
                && e.label == event.label
        });
        if dup {
            return;
        }
        let pos = self
            .strip_events
            .partition_point(|e| e.at_unix_ms <= event.at_unix_ms);
        self.strip_events.insert(pos, event.clone());
        self.note_strip_change(event, crate::events::StripChangeKind::Insert);
        while self.strip_events.len() > STRIP_HISTORY_CAP {
            self.strip_events.pop_front();
        }
    }

    fn note_strip_change(&mut self, marker: StripEvent, change: crate::events::StripChangeKind) {
        if self.strip_changes.len() == STRIP_HISTORY_CAP {
            self.strip_changes.pop_front();
            self.strip_changes_lost = self.strip_changes_lost.saturating_add(1);
        }
        self.strip_changes.push_back((change, marker));
    }

    /// Publish each accepted marker change once, under this newly absorbed snapshot's sequence.
    /// Original marker wall times and authorities survive delayed correction and restoration.
    pub fn drain_strip_events(&mut self, snapshot: &mut Snapshot) {
        snapshot.events_lost = snapshot
            .events_lost
            .saturating_add(std::mem::take(&mut self.strip_changes_lost));
        let seq = snapshot.seq;
        let at_ns = snapshot.at_ns;
        snapshot
            .events
            .extend(
                self.strip_changes
                    .drain(..)
                    .map(|(change, marker)| crate::events::Event {
                        seq,
                        at_ns,
                        at_unix_ms: (marker.at_unix_ms != 0).then_some(marker.at_unix_ms),
                        scope: marker.scope.clone(),
                        kind: crate::events::EventKind::Strip { marker, change },
                    }),
            );
    }

    fn trim_strip_history(&mut self, now_unix_ms: i64) {
        if now_unix_ms <= 0 {
            return;
        }
        let cutoff = now_unix_ms.saturating_sub(STRIP_HORIZON_MS);
        self.time_gaps.retain(|g| g.end_ms >= cutoff);
        self.strip_events.retain(|e| e.at_unix_ms >= cutoff);
        self.situation_spans.retain(|s| s.end_ms >= cutoff);
    }

    /// Snapshot the global and `aterm` time-series rings, discontinuities, event markers,
    /// situation spans and usual clocks for atomic persistence (`clean_stop = true` on graceful
    /// probe shutdown).
    #[must_use]
    pub fn saved_strip(&self, clean_stop: bool) -> SavedStrip {
        SavedStrip {
            scope_cpu_accounting: Some(crate::published_cpu::CpuAccounting::PUBLISHED_COUNTERS),
            last_wall_ms: self.last_stamp.map_or(self.restored_wall_ms, |s| s.wall_ms),
            through_k: self.strip_open_k().map_or(i64::MIN, |k| k - 1),
            clean_stop,
            caps: self
                .rows
                .caps
                .map(|c| c.filter(|x| x.is_finite() && *x > 0.0)),
            rings: std::array::from_fn(|i| self.rows.rings[i].saved()),
            net_tx: self.rows.net_tx.saved(),
            proc_count: self.rows.proc_count.saved(),
            pressure: self.rows.pressure.saved(),
            aterm: std::array::from_fn(|i| {
                self.rows.aterm.0[i]
                    .as_ref()
                    .map_or_else(crate::ring::SavedRing::default, BucketRing::saved)
            }),
            aterm_unread: self.rows.aterm.1.each_ref().map(BucketRing::saved),
            aterm_cpu_charge: self.rows.aterm.2.saved(),
            compute_w: self.rows.compute_w.saved(),
            thermal: self.rows.thermal.saved(),
            proc_exits: self.rows.proc_exits.saved(),
            blockage: self.rows.blocked.saved(),
            proc_starts: self.rows.proc_starts,
            time_gaps: self.time_gaps.iter().copied().collect(),
            strip_events: self.strip_events.iter().cloned().collect(),
            situation: self.situation_spans.clone(),
            usual: self.heat.usual,
        }
    }

    /// Restore the time-series history persisted by an earlier run up to `now_unix_ms`.
    ///
    /// When the saved checkpoint came from an unclean exit (`!saved.clean_stop`), automatically
    /// records a [`GapKind::Crash`] discontinuity across `[saved.last_wall_ms, now_unix_ms)` plus
    /// [`StripEventKind::Crash`] and [`StripEventKind::Restart`] markers. When it came from a
    /// graceful stop at least one 10 s bucket ago, records a [`GapKind::Paused`] downtime span so
    /// pre-quit samples are never interpolated across the offline interval.
    ///
    /// The situation spans of the last six hours come back as they ended, none reaching past
    /// `now_unix_ms`: no reading of this run joins one. The usual clocks come back as they
    /// were learned.
    pub fn restore_strip_history(&mut self, saved: SavedStrip, now_unix_ms: i64) {
        if now_unix_ms <= 0 || saved.last_wall_ms <= 0 {
            return;
        }
        self.restored_wall_ms = saved.last_wall_ms.min(now_unix_ms);
        let reported = saved.scope_cpu_accounting
            == Some(crate::published_cpu::CpuAccounting::PUBLISHED_COUNTERS);
        for i in 0..7 {
            if let Some(c) = saved.caps[i].filter(|&x| x.is_finite() && x > 0.0) {
                self.rows.caps[i] = Some(c);
            }
            self.rows.rings[i] = BucketRing::restored(&saved.rings[i], now_unix_ms);
            let ar = BucketRing::restored(&saved.aterm[i], now_unix_ms);
            self.rows.aterm.0[i] = if i == Resource::Cpu.index() && !reported {
                None
            } else {
                (!ar.is_empty() || !saved.aterm[i].recent.is_empty()).then_some(ar)
            };
        }
        self.rows.net_tx = BucketRing::restored(&saved.net_tx, now_unix_ms);
        self.rows.proc_count = BucketRing::restored(&saved.proc_count, now_unix_ms);
        self.rows.pressure = BucketRing::restored(&saved.pressure, now_unix_ms);
        self.rows.aterm.1 = saved
            .aterm_unread
            .each_ref()
            .map(|ring| BucketRing::restored(ring, now_unix_ms));
        if !reported {
            self.rows.aterm.1[1] = BucketRing::default();
        }
        self.rows.aterm.2 = if reported {
            BucketRing::restored(&saved.aterm_cpu_charge, now_unix_ms)
        } else {
            BucketRing::default()
        };
        self.rows.compute_w = BucketRing::restored(&saved.compute_w, now_unix_ms);
        self.rows.thermal = BucketRing::restored(&saved.thermal, now_unix_ms);
        self.rows.proc_exits = BucketRing::restored(&saved.proc_exits, now_unix_ms);
        self.rows.blocked = BucketRing::restored(&saved.blockage, now_unix_ms);
        self.rows.proc_starts = saved.proc_starts;
        self.heat.usual = crate::heat::UsualClocks::restored(&saved.usual.saved());
        self.strip_rewrites += 1;

        let cutoff = now_unix_ms.saturating_sub(STRIP_HORIZON_MS);
        self.situation_spans.clear();
        for mut span in saved.situation {
            if span.end_ms >= cutoff && span.start_ms < now_unix_ms {
                span.end_ms = span.end_ms.min(now_unix_ms);
                crate::situation::Span::put(&mut self.situation_spans, span, STRIP_HISTORY_CAP);
            }
        }
        for mut gap in saved.time_gaps {
            if gap.end_ms >= cutoff && gap.start_ms < now_unix_ms {
                gap.end_ms = gap.end_ms.min(now_unix_ms);
                self.record_time_gap(gap);
            }
        }
        for ev in saved.strip_events {
            if ev.at_unix_ms >= cutoff && ev.at_unix_ms <= now_unix_ms {
                self.record_strip_event(ev);
            }
        }
        if saved.last_wall_ms >= cutoff && saved.last_wall_ms < now_unix_ms {
            if !saved.clean_stop {
                self.record_time_gap(TimeGap {
                    start_ms: saved.last_wall_ms,
                    end_ms: now_unix_ms,
                    kind: GapKind::Crash,
                });
                self.record_strip_event(StripEvent {
                    at_unix_ms: saved.last_wall_ms,
                    kind: StripEventKind::Crash,
                    label: "aterm crash".into(),
                    scope: crate::events::EventScope::Machine,
                    span: None,
                });
                self.record_strip_event(StripEvent {
                    at_unix_ms: now_unix_ms,
                    kind: StripEventKind::Restart,
                    label: "restarted".into(),
                    scope: crate::events::EventScope::Machine,
                    span: None,
                });
                self.restored_wall_ms = now_unix_ms;
            } else if now_unix_ms - saved.last_wall_ms >= BUCKET_MS {
                self.record_time_gap(TimeGap {
                    start_ms: saved.last_wall_ms,
                    end_ms: now_unix_ms,
                    kind: GapKind::Paused,
                });
                self.restored_wall_ms = now_unix_ms;
            }
        }
        self.trim_strip_history(now_unix_ms);
    }

    /// The strip's open ten-second epoch: the newest any row's ring holds. `None` before a
    /// reading.
    #[must_use]
    pub fn strip_open_k(&self) -> Option<i64> {
        self.rows.rings.iter().filter_map(BucketRing::open_k).max()
    }

    /// How many times the strip's held buckets changed other than by an epoch closing: a
    /// restore, a backward clock step that re-keyed any strip ring (aterm's re-key a reading
    /// after the rows' where the step left them no rate to read), or a Processes row whose
    /// meaning changed. An append-only checkpoint writes a whole base when it moves, since its
    /// tail can carry only closed epochs.
    #[must_use]
    pub fn strip_rewrites(&self) -> u64 {
        self.strip_rewrites
    }

    /// Every strip ring's open bucket, by slot ([`SavedStrip::ring`]).
    fn strip_opens(&self) -> [Option<i64>; STRIP_RINGS] {
        std::array::from_fn(|slot| self.strip_ring(slot).and_then(BucketRing::open_k))
    }

    fn strip_ring(&self, slot: usize) -> Option<&BucketRing> {
        match slot {
            0..7 => self.rows.rings.get(slot),
            7 => Some(&self.rows.net_tx),
            8 => Some(&self.rows.pressure),
            9..16 => self.rows.aterm.0.get(slot - 9).and_then(Option::as_ref),
            16 => Some(&self.rows.proc_count),
            17..19 => self.rows.aterm.1.get(slot - 17),
            19 => Some(&self.rows.aterm.2),
            20 => Some(&self.rows.compute_w),
            21 => Some(&self.rows.thermal),
            22 => Some(&self.rows.proc_exits),
            23 => Some(&self.rows.blocked),
            _ => None,
        }
    }

    /// Epoch `k` of every strip ring that read anything in it, for the checkpoint's tail; `None`
    /// when none did, or while `k` is still open ([`Self::strip_open_k`]). Its gaps, events,
    /// situation spans and usual clocks are left for the writer, which knows what it wrote
    /// before ([`Self::strip_marks`], [`Self::strip_spans`], [`Self::usual_clocks`]).
    #[must_use]
    pub fn strip_line(&self, k: i64) -> Option<StripLine> {
        if self.strip_open_k().is_none_or(|open| k >= open) {
            return None;
        }
        let rings: Vec<(u8, Agg)> = (0..STRIP_RINGS)
            .filter_map(|slot| {
                let agg = *self.strip_ring(slot)?.bucket(k)?;
                (agg.is_sound()).then(|| (u8::try_from(slot).unwrap_or(u8::MAX), agg))
            })
            .collect();
        (!rings.is_empty()).then(|| StripLine {
            scope_cpu_accounting: Some(crate::published_cpu::CpuAccounting::PUBLISHED_COUNTERS),
            k,
            wall_ms: self.last_stamp.map_or(self.restored_wall_ms, |s| s.wall_ms),
            caps: self
                .rows
                .caps
                .map(|c| c.filter(|x| x.is_finite() && *x > 0.0)),
            rings,
            proc_starts: self.rows.proc_starts,
            time_gaps: Vec::new(),
            strip_events: Vec::new(),
            spans: Vec::new(),
            usual: Vec::new(),
        })
    }

    /// The situation's stays of the last six hours, oldest first, as [`Self::saved_strip`]
    /// would keep them.
    #[must_use]
    pub fn strip_spans(&self) -> &[crate::situation::Span] {
        &self.situation_spans
    }

    /// The usual Super clock per busy decile, as the heat chain has learned it: the
    /// checkpoint keeps it with the strip.
    #[must_use]
    pub const fn usual_clocks(&self) -> &crate::heat::UsualClocks {
        &self.heat.usual
    }

    /// The strip's gaps and events, oldest first, as [`Self::saved_strip`] would keep them.
    #[must_use]
    pub fn strip_marks(&self) -> (Vec<TimeGap>, Vec<StripEvent>) {
        (
            self.time_gaps.iter().copied().collect(),
            self.strip_events.iter().cloned().collect(),
        )
    }

    /// The peaks learned so far, for the host to persist.
    #[must_use]
    pub fn peaks(&self) -> &Peaks {
        &self.rows.peaks
    }

    /// Take peaks the host persisted in an earlier run.
    pub fn restore_peaks(&mut self, peaks: Peaks) {
        self.rows.peaks = peaks;
    }

    /// The disk's free history, for the host to persist.
    #[must_use]
    pub fn free_history(&self) -> &FreeHistory {
        &self.rows.free
    }

    /// Take the free history the host persisted in an earlier run
    /// ([`history::FreeSeries::restored`]).
    pub fn restore_free_history(&mut self, history: FreeHistory) {
        self.rows.free = history;
    }

    /// The disk's three-week free history, for the host to persist.
    #[must_use]
    pub fn free_weeks(&self) -> &FreeWeeks {
        &self.rows.weeks
    }

    /// The Spotlight family's day, for the host to persist.
    #[must_use]
    pub fn spotlight_history(&self) -> &crate::history::SpotlightHistory {
        self.spotlight.history()
    }

    /// Take the Spotlight family's day the host persisted in an earlier run.
    pub fn restore_spotlight_history(&mut self, history: crate::history::SpotlightHistory) {
        self.spotlight.restore_history(history);
    }

    /// Take the Spotlight stores aterm's root LAN broker answered (SD-94): later readings carry
    /// the index's size while the answer is no older than [`crate::spotlight::STORE_STALE_MS`].
    pub fn note_spotlight_store(&mut self, store: crate::spotlight::StoreRead) {
        self.spotlight.note_store(store);
    }

    /// Whether the Spotlight family's last reading is at or over a bar: what the host's
    /// `mdutil` cadence reads ([`crate::spotlight::Cadence::step`]).
    #[must_use]
    pub fn spotlight_busy(&self) -> bool {
        self.spotlight.busy()
    }

    /// Take the three-week free history the host persisted in an earlier run
    /// ([`history::FreeSeries::restored`]).
    pub fn restore_free_weeks(&mut self, weeks: FreeWeeks) {
        self.rows.weeks = weeks;
    }

    /// Retain a prior process's diagnostic report for export, never as a live sample.
    pub fn restore_cpu_report(&mut self, report: Option<crate::sys::json::Jv>) {
        self.historical_cpu_report = report.map(std::sync::Arc::new);
    }

    /// Attach a bounded explicit collection without changing counters or anomaly state.
    pub fn record_cpu_policy(&mut self, report: crate::sys::json::Jv) {
        self.authorized_cpu_policy = Some(std::sync::Arc::new(report));
    }

    /// Whether the E-busy, P-idle recorder is pending: while it is, the host takes a CPU Stats
    /// sample with each reading (A2 §6.4).
    #[must_use]
    pub const fn cpu_recorder_pending(&self) -> bool {
        self.hardware_recorder.pending()
    }

    /// Whether a CPU report copies each worker's scheduling from the next reading: the recorder's
    /// frames while it is pending, and the report the host saves at each reading while a bundle
    /// is kept ([`Snapshot::cpu_diagnostics`]) or a CPU policy was recorded
    /// ([`Self::record_cpu_policy`]). While it holds, the host asks each reading's sweep for it.
    #[must_use]
    pub const fn cpu_reports_copy_scheduling(&self) -> bool {
        self.hardware_recorder.pending()
            || self.hardware_recorder.kept()
            || self.authorized_cpu_policy.is_some()
    }

    /// How many samples it has absorbed.
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Inject a fault into scope-balance attribution or the cause engine, for negative controls.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_fault(mut self, fault: crate::explain::Fault) -> Self {
        self.why.fault = Some(fault);
        self
    }

    /// Absorb one sample and return the snapshot it makes.
    pub fn absorb(&mut self, s: &Sample) -> Snapshot {
        let self_cpu = self.self_cpu.absorb(
            s.self_cpu
                .as_ref()
                .map(|cut| cut.as_ref().map_err(|why| *why)),
        );
        // A known PID with an unread start is neither a validated origin nor a disproved one.
        // Keep its proof dormant and mark every accounting consumer of this reading partial.
        let mut checked;
        let s = if !s.ownership_incomplete
            && (self.origins_incomplete
                || s.procs.as_ref().is_some_and(|procs| {
                    let unread: IdSet<_> = procs
                        .iter()
                        .filter(|p| p.start.is_none())
                        .map(|p| p.pid)
                        .collect();
                    self.origins.keys().any(|key| unread.contains(&key.pid))
                })) {
            checked = s.clone();
            checked.ownership_incomplete = true;
            &checked
        } else {
            s
        };
        self.seq += 1;
        let busy = s
            .gpu
            .as_ref()
            .and_then(|g| g.device_pct)
            .map(|p| f32::from(p.min(100)) / 100.0);
        self.rows
            .gpu_clients
            .absorb(s.gpu_clients.as_ref(), busy, s.at_ns);
        let stamp = Stamp::of(s);
        for &gap in &s.note_gaps {
            self.record_time_gap(gap);
        }
        for ev in &s.note_events {
            self.record_strip_event(ev.clone());
        }
        // The reading before this one where no gap lies between them: the stay that held
        // through it goes on through this one.
        let joins = self
            .last_stamp
            .filter(|prev| stamp.follows(*prev))
            .map(|prev| prev.wall_ms);
        crate::situation::Span::note(
            &mut self.situation_spans,
            stamp.wall_ms,
            crate::situation::Stay::of(s.place, &s.situation),
            joins,
            STRIP_HISTORY_CAP,
        );
        match self.last_stamp.replace(stamp) {
            Some(prev) if stamp.wall_ms > prev.wall_ms && stamp.mono_ms >= prev.mono_ms => {
                let wall_dt = stamp.wall_ms - prev.wall_ms;
                let mono_dt = i64::try_from(stamp.mono_ms - prev.mono_ms).unwrap_or(i64::MAX);
                let skew = wall_dt.saturating_sub(mono_dt);
                let pause_limit = i64::try_from(
                    GAP_CADENCES * u64::from(prev.cadence_ms.max(stamp.cadence_ms).max(1_000)),
                )
                .unwrap_or(i64::MAX);
                if skew > SLEEP_SKEW_MS {
                    self.record_time_gap(TimeGap {
                        start_ms: prev.wall_ms,
                        end_ms: stamp.wall_ms,
                        kind: GapKind::Sleep,
                    });
                    self.record_strip_event(StripEvent {
                        at_unix_ms: stamp.wall_ms,
                        kind: StripEventKind::SleepWake,
                        label: "woke from sleep".into(),
                        scope: crate::events::EventScope::Machine,
                        span: None,
                    });
                } else if mono_dt > pause_limit && wall_dt >= BUCKET_MS {
                    self.record_time_gap(TimeGap {
                        start_ms: prev.wall_ms,
                        end_ms: stamp.wall_ms,
                        kind: GapKind::Paused,
                    });
                }
            }
            None if self.restored_wall_ms > 0
                && stamp.wall_ms.saturating_sub(self.restored_wall_ms) >= BUCKET_MS =>
            {
                self.record_time_gap(TimeGap {
                    start_ms: self.restored_wall_ms,
                    end_ms: stamp.wall_ms,
                    kind: GapKind::Paused,
                });
            }
            _ => {}
        }
        if let Some(tl) = s.timeline.as_ref()
            && let (Some(sleep_ms), Some(wake_ms)) = (tl.sleep_ms, tl.wake_ms)
            && wake_ms > sleep_ms
            && wake_ms >= s.at_unix_ms.saturating_sub(STRIP_HORIZON_MS)
            && wake_ms <= s.at_unix_ms.saturating_add(2_000)
        {
            self.record_time_gap(TimeGap {
                start_ms: sleep_ms,
                end_ms: wake_ms,
                kind: GapKind::Sleep,
            });
            self.record_strip_event(StripEvent {
                at_unix_ms: wake_ms,
                kind: StripEventKind::SleepWake,
                label: "woke from sleep".into(),
                scope: crate::events::EventScope::Machine,
                span: Some(crate::events::EventSpan {
                    onset_unix_ms: sleep_ms,
                    end_unix_ms: wake_ms,
                }),
            });
        }
        let cur_pressure = s.mem.map_or(Pressure::Unknown, |m| m.pressure);
        if cur_pressure == Pressure::Critical && self.last_pressure != Pressure::Critical {
            self.record_strip_event(StripEvent {
                at_unix_ms: s.at_unix_ms,
                kind: StripEventKind::PressureSpike,
                label: "memory pressure critical".into(),
                scope: crate::events::EventScope::Machine,
                span: None,
            });
        }
        if cur_pressure != Pressure::Unknown {
            self.last_pressure = cur_pressure;
        }
        if let Some(t) = s.thermal {
            if t.level == Throttle::Severe && self.last_throttle != Throttle::Severe {
                self.record_strip_event(StripEvent {
                    at_unix_ms: s.at_unix_ms,
                    kind: StripEventKind::PressureSpike,
                    label: t.phrase(),
                    scope: crate::events::EventScope::Machine,
                    span: None,
                });
            }
            self.last_throttle = t.level;
        }
        let cores_dt = match s.cores.as_deref() {
            Some(c) if !c.is_empty() => stamp.rate_dt(self.rows.cores_at.replace(stamp)),
            _ => None,
        };
        let sweep_dt = s
            .procs
            .as_ref()
            .and_then(|_| stamp.rate_dt(self.rows.sweep_at.replace(stamp)));
        let mut cores_window_ns = None;
        let cpu = s
            .cores
            .as_deref()
            .filter(|cores| !cores.is_empty())
            .and_then(|cores| {
                let before = self.last_cores_at;
                let dt_s = interval_s(before, s.at_ns);
                self.last_cores_at = Some(s.at_ns);
                let view = self.absorb_cores(cores, &s.clusters, dt_s, s.at_ns);
                if view.is_some() {
                    cores_window_ns = before.filter(|&t| s.at_ns > t).map(|t| s.at_ns - t);
                }
                view
            });
        if cpu.is_none() {
            self.busy_history.push_at(None, s.at_ns);
        }

        let mut gpu = match s.gpu.as_ref() {
            Some(g) if g.device_pct.is_some() => {
                let pct = f32::from(g.device_pct.unwrap_or(0).min(100)) / 100.0;
                self.gpu.push(pct, interval_s(self.last_gpu_at, s.at_ns));
                self.last_gpu_at = Some(s.at_ns);
                self.gpu_history.push_at(Some(pct), s.at_ns);
                Some(GpuView {
                    load: self.gpu.view(),
                    cores: g.cores,
                    name: g.name.clone(),
                    history: self.gpu_history.to_vec(),
                    sample: g.clone(),
                    split: None,
                })
            }
            _ => {
                self.gpu_history.push_at(None, s.at_ns);
                None
            }
        };

        let mem_dt_s = interval_s(self.last_mem_at, s.at_ns);
        let mem = if let Some(m) = s.mem.filter(|m| m.total > 0) {
            let used = gib(m.used());
            self.mem.push(used, mem_dt_s);
            self.last_mem_at = Some(s.at_ns);
            self.mem_history.push_at(Some(used), s.at_ns);
            let rates = m.counters.and_then(|now| {
                let before = self.last_counters.replace((s.at_ns, now));
                let (then_ns, then) = before?;
                mem_rates(then, now, s.at_ns.checked_sub(then_ns)?)
            });
            if m.counters.is_none() {
                self.last_counters = None;
            }
            Some(MemView {
                sample: m,
                used_gib: self.mem.view(),
                history: self.mem_history.to_vec(),
                rates,
            })
        } else {
            self.mem_history.push_at(None, s.at_ns);
            None
        };

        let mut global = ScopeView {
            cores: cpu.as_ref().map(|c| c.busy).unwrap_or_default(),
            history: cpu.as_ref().map(|c| c.history.clone()).unwrap_or_default(),
            ..ScopeView::default()
        };
        if let Some(m) = mem.as_ref() {
            global.footprint = Some(m.sample.used());
            self.global_foot.push(gib(m.sample.used()), mem_dt_s);
            global.footprint_gib = self.global_foot.view();
        }

        let mut pass = match s.procs.as_deref() {
            Some(procs) => self.absorb_procs(procs, s, &mut global, sweep_dt),
            None => {
                self.pane_resources.missing(s);
                self.aterm.history.push_at(None, s.at_ns);
                for state in self.tabs.values_mut() {
                    state.history.push_at(None, s.at_ns);
                }
                ProcPass::default()
            }
        };
        let attribution = pass
            .attribution
            .as_ref()
            .map(|(m, o)| (m.as_slice(), o.as_slice()));
        let mut scoped_cpu = self.why.scope_cpu(s, attribution);
        if let Some(cpu) = &mut scoped_cpu
            && (sweep_dt.is_none() || pass.window_ns != Some(cpu.window_ns))
        {
            for pane in &mut cpu.panes {
                pane.complete = false;
            }
        }

        crate::pane_tokens::absorb(&mut self.pane_tokens, s);
        crate::pane_cpu::absorb(
            &mut self.pane_cpu,
            s,
            scoped_cpu.as_ref(),
            &self.pane_resources.panes,
        );
        let prepared_why = self.why.prepare(s, attribution);
        if let Some(cpu) = scoped_cpu.as_mut() {
            prepared_why.apply_app(s, cpu);
        }
        if let Some(owners) = prepared_why.memory_owners()
            && let Some(figs) = pass.figs.as_mut()
        {
            let charge = owners.aterm;
            let measured =
                (charge.members == 0 || charge.unread < charge.members).then_some(charge.bytes);
            let observed = figs.aterm.footprint;
            figs.aterm.footprint = plus(measured, observed, u64::max);
            figs.aterm.footprint_unread = u32::from(
                owners.ownership_incomplete
                    || owners.truncated
                    || charge.unread > 0
                    || owners.unknown.bytes > 0
                    || owners.unknown.unread > 0
                    || observed
                        .zip(measured)
                        .is_some_and(|(observed, measured)| observed > measured),
            );
            if let Some(aterm) = pass.aterm.as_mut() {
                aterm.footprint = figs.aterm.footprint;
                aterm.footprint_unread = figs.aterm.footprint_unread;
            }
        }
        if let Some(aterm) = pass.aterm.as_mut() {
            if let Some(bytes) = aterm.footprint {
                self.app_footprint
                    .push(gib(bytes), interval_s(self.last_app_footprint_at, s.at_ns));
                self.last_app_footprint_at = Some(s.at_ns);
            }
            aterm.footprint_gib = self.app_footprint.view();
        }
        if let Some(cpu) = scoped_cpu.as_ref()
            && let Some(figs) = pass.figs.as_mut()
        {
            if pass.window_ns != Some(cpu.window_ns) {
                figs.aterm.cpu_incomplete += 1;
                for tab in &mut figs.tabs {
                    tab.cpu_incomplete += 1;
                }
            } else {
                if cpu.missed || cpu.own_ambiguous {
                    figs.aterm.cpu_incomplete += 1;
                }
                let mut charge = cpu.own_charge;
                for tab in &cpu.tabs {
                    let indexes: Vec<_> = s
                        .tabs
                        .iter()
                        .enumerate()
                        .filter_map(|(i, root)| (root.tab == tab.key).then_some(i))
                        .collect();
                    let pane_reported: f64 = indexes
                        .iter()
                        .filter_map(|&index| {
                            cpu.panes.iter().find(|pane| pane.key == s.tabs[index].key)
                        })
                        .map(|pane| pane.reported)
                        .sum();
                    let pane_charge_known = (tab.charge - pane_reported).abs() < f64::EPSILON;
                    for &index in &indexes {
                        let reported = cpu
                            .panes
                            .iter()
                            .find(|pane| pane.key == s.tabs[index].key)
                            .map_or(0.0, |pane| pane.reported);
                        figs.tabs[index].cores = reported as f32;
                        figs.tabs[index].cpu_charge = pane_charge_known.then_some(reported);
                        if tab.missed || tab.ambiguous || tab.reported > pane_reported {
                            figs.tabs[index].cpu_incomplete += 1;
                        }
                    }
                    if tab.missed || tab.ambiguous {
                        figs.aterm.cpu_incomplete += 1;
                    }
                    charge += tab.charge;
                }
                figs.aterm.cores = cpu.process_reported() as f32;
                figs.aterm.cpu_charge = Some(charge);
            }
        }
        // Each core's fraction this reading, every core measured: the CPU row's reading is their
        // sum and each core's unit their own, so the tiles' mean is the row's %.
        let core_fracs: Option<Vec<f64>> = cpu
            .as_ref()
            .filter(|c| cores_dt.is_some() && !c.cores.is_empty())
            .and_then(|c| {
                c.cores
                    .iter()
                    .map(|core| core.current().map(f64::from))
                    .collect()
            });
        let busy = core_fracs.as_ref().map(|f| f.iter().sum::<f64>());
        let figs = pass.figs.as_ref().filter(|_| !s.machine_only);
        let opens_before = self.strip_opens();
        let meaning_before = self.rows.proc_starts;
        let (resources, mut facts, net) =
            self.rows
                .absorb(s, stamp, busy, figs, sweep_dt, scoped_cpu.as_ref());
        (facts.zombies, facts.dstate) = self.rows.blockage.absorb(
            self.seq,
            s,
            pass.attribution.as_ref().map(|(m, _)| m.as_slice()),
        );
        facts.blocked =
            crate::blockage::Blocked::judge(&facts.zombies, &facts.dstate, s.stuck_work.as_ref());
        let heat = self.heat.absorb(&crate::heat::Input {
            lane: s.coal_lane,
            pressure: s.thermal_pressure,
            moved: self.rows.heat_moved.take(),
            range: s.clock_range,
            gate: crate::heat::ClockGate::of_chip(self.rows.cpu_brand.as_deref()),
            cores: core_fracs.as_deref().zip(cores_window_ns),
            p_cores: cpu.as_ref().and_then(|c| super_cores(&c.clusters)),
            host: s.host_watts,
        });
        let groups = self.absorb_groups(s, &pass, &prepared_why, &heat);
        // The instruments' rings take the reading here, before the re-key is judged, so a step
        // back that re-keys one of them writes a whole base as the rows' does.
        self.rows.note_instruments(s, facts.blocked, &heat);
        // Any strip ring re-keyed back, not only the rows': aterm's, which read nothing at a step
        // a rate cannot cross, re-key at the reading after it.
        let rekeyed = opens_before
            .iter()
            .zip(self.strip_opens())
            .any(|(b, a)| b.zip(a).is_some_and(|(b, a)| a < b));
        if rekeyed || meaning_before.is_some_and(|m| self.rows.proc_starts != Some(m)) {
            self.strip_rewrites += 1;
        }
        let units = self.rows.units.absorb(
            &crate::units::UnitReading {
                now: s.at_unix_ms,
                period_ms: s.period_ms(),
                at_ns: s.at_ns,
                cores: core_fracs.as_deref(),
                n_cores: s.cores.as_ref().map(Vec::len).filter(|&n| n > 0),
                clusters: cpu.as_ref().map_or(&[], |c| c.clusters.as_slice()),
                queue: facts.queue,
                churn_bps: mem.as_ref().and_then(|m| m.rates).map(|r| r.churn_bps),
                churn_read: mem.as_ref().is_some_and(|m| m.sample.counters.is_some()),
                swap_bps: facts.swap_rate,
                swap_used: mem.as_ref().and_then(|m| m.sample.swap_used),
                block: &s.block,
                home_device_id: s.home_device_id.as_deref(),
            },
            &mut self.rows.peaks,
        );
        if let Some((resets, _)) = facts.gpu_resets
            && resets > self.last_gpu_resets
        {
            self.last_gpu_resets = resets;
            self.record_strip_event(StripEvent {
                at_unix_ms: s.at_unix_ms,
                kind: StripEventKind::PressureSpike,
                label: "GPU reset".into(),
                scope: crate::events::EventScope::Machine,
                span: None,
            });
        }
        self.trim_strip_history(s.at_unix_ms);
        if let (Some(g), Some((aterm, others, services)), Some(clients)) = (
            gpu.as_mut(),
            pass.gpu_parts,
            self.rows.gpu_clients.clients(),
        ) {
            g.split = Some(crate::gpu_clients::GpuSplit {
                clients,
                aterm,
                others,
                services: services.max(clients - aterm - others),
            });
        }
        let split = match (
            cpu.as_ref().and_then(CpuView::current_busy),
            pass.split_parts,
        ) {
            (Some(busy), Some((aterm, others, services, unknown_owner_reported, unread))) => {
                Some(Split {
                    global: busy,
                    aterm,
                    others,
                    services,
                    unknown_owner_reported,
                    unread,
                })
            }
            _ => None,
        };

        let mut snap = Snapshot {
            groups,
            boot_cost: None,
            self_cpu,
            self_cpu_read: s.self_cpu_read,
            native_terminals: s.native_terminals.clone(),
            cpu_counter_sample: s.cores.clone(),
            hardware: s.hardware.clone(),
            scheduling_read: s.scheduling_read,
            cpu_diagnostics: None,
            historical_cpu_report: self.historical_cpu_report.clone(),
            authorized_cpu_policy: self.authorized_cpu_policy.clone(),
            at_ns: s.at_ns,
            seq: self.seq,
            cpu,
            gpu,
            mem,
            volume: s.volume.clone(),
            net,
            // The host sets the place after the absorb, as it sets the slots.
            place: None,
            global,
            aterm: pass.aterm,
            shared: pass.shared,
            tabs: pass.tabs,
            top: pass.top,
            jobs: None,
            jobs_age_ms: 0,
            left: None,
            delegated: pass.delegated,
            machine_only: s.machine_only,
            at_unix_ms: s.at_unix_ms,
            slot_grid: SlotGrid::SECONDS,
            resources,
            free_history: self.rows.free.clone(),
            free_weeks: self.rows.weeks.clone(),
            facts,
            procs_window_ns: pass.window_ns,
            cores_window_ns,
            split,
            scoped_cpu,
            pane_tokens: Arc::clone(&self.pane_tokens),
            pane_cpu: Arc::clone(&self.pane_cpu),
            pane_resources: Arc::clone(&self.pane_resources.panes),
            procs: pass.rows,
            programs: pass.programs,
            memory_owners: pass.memory_owners,
            procs_truncated: s.procs_truncated,
            stalled: s.stalled.clone(),
            thermal: s.thermal,
            heat,
            ane: s.ane,
            load: s.load,
            slots: None,
            slots_age_ms: None,
            slot_tabs: None,
            self_pid: s.self_pid,
            events: Vec::new(),
            starving: None,
            events_lost: 0,
            why: None,
            coal_lane: s.coal_lane,
            services: None,
            services_filed: Vec::new(),
            time_gaps: self.time_gaps.iter().copied().collect(),
            strip_events: self.strip_events.iter().cloned().collect(),
            units,
            situation: s.situation.clone(),
            spotlight: None,
            spotlight_day: std::sync::Arc::default(),
        };
        snap.why = self.why.finish(s, &snap, prepared_why);
        snap.coal_lane = self.why.lane(s);
        let fresh = snap.why.as_deref().filter(|f| {
            f.at_ns == s.at_ns && f.lane.on() && f.split.via == crate::explain::Via::Coalitions
        });
        snap.spotlight = self
            .spotlight
            .absorb(fresh, s.spotlight_volumes.as_ref(), s.mono_ms);
        snap.spotlight_day = std::sync::Arc::clone(self.spotlight.history());
        snap.events = self.detector.detect(&snap, &s.event_sources);
        snap.starving = self.detector.starving().cloned().map(Box::new);
        self.drain_strip_events(&mut snap);
        snap.cpu_diagnostics = self.hardware_recorder.observe(&snap, s.cores.as_deref());
        snap
    }

    fn absorb_cores(
        &mut self,
        cores: &[Ticks],
        clusters: &[ClusterSpec],
        dt_s: f32,
        at_ns: u64,
    ) -> Option<CpuView> {
        if cores.is_empty() {
            return None;
        }
        if self.last_cores.len() != cores.len() {
            self.last_cores = cores.to_vec();
            self.core_trends = vec![Trend::new(); cores.len()];
            self.core_histories = vec![History::with_capacity(CORE_HISTORY_LEN); cores.len()];
            return None;
        }
        let mut busy_sum = 0.0f32;
        let mut views = Vec::with_capacity(cores.len());
        for (((now, before), trend), hist) in cores
            .iter()
            .zip(self.last_cores.iter())
            .zip(self.core_trends.iter_mut())
            .zip(self.core_histories.iter_mut())
        {
            let busy = now.busy.saturating_sub(before.busy);
            let total = now.total.saturating_sub(before.total);
            let measured = total > 0;
            if measured {
                let frac = (busy as f32 / total as f32).clamp(0.0, 1.0);
                trend.push(frac, dt_s);
                hist.push_at(Some(frac), at_ns);
                busy_sum += frac;
            } else {
                hist.push_at(None, at_ns);
            }
            views.push(CoreView {
                load: trend.view(),
                measured,
            });
        }
        self.last_cores = cores.to_vec();
        let measured = views.iter().all(|c| c.measured);
        if measured {
            self.busy.push(busy_sum, dt_s);
        }
        self.busy_history
            .push_at(measured.then_some(busy_sum), at_ns);
        Some(CpuView {
            clusters: cluster_views(clusters, views.len()),
            cores: views,
            busy: self.busy.view(),
            history: self.busy_history.to_vec(),
            core_history: self.core_histories.iter().map(History::to_vec).collect(),
        })
    }

    fn absorb_groups(
        &mut self,
        s: &Sample,
        pass: &ProcPass,
        prepared: &crate::explain::PreparedWhy,
        heat: &crate::heat::HeatView,
    ) -> crate::groups::View {
        use crate::groups::{
            CpuBasis, Energy, Frame, GroupKey, Observation, Rate, ReadWindow, Scope, Value,
        };
        let processes_complete = s.procs.is_some()
            && !s.machine_only
            && !s.procs_truncated
            && !s.ownership_incomplete
            && !s.native_incomplete
            && !s.native_source_lost
            && s.unproved_tabs.is_empty()
            && pass.attribution.as_ref().is_some_and(|(members, _)| {
                s.procs
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .zip(members)
                    .zip(&pass.group_keys)
                    .all(|((p, m), key)| {
                        !m.aterm || (key.is_some() && p.state != ProcState::Unknown)
                    })
            });
        let fresh = prepared
            .group_facts()
            .filter(|facts| facts.at_ns == s.at_ns);
        let coalitions_complete = processes_complete
            && fresh.is_some_and(|facts| {
                !facts.split.list_truncated && facts.split.rows.len() == facts.split.listed as usize
            });
        let energy = self
            .rows
            .group_energy
            .take()
            .and_then(Result::ok)
            .filter(|_| s.coal_lane.on() && heat.chain.is_ok());
        let starts = std::mem::take(&mut self.rows.group_starts);
        let rate = |value: Option<f64>, end_ns, window: Option<u64>| Rate {
            value: value.map_or_else(|| Value::not_sampled(1), Value::Read),
            window: window
                .filter(|&ns| ns > 0)
                .map_or(ReadWindow::Unread, |window_ns| ReadWindow::Reported {
                    end_ns,
                    window_ns,
                }),
        };
        let unsupported = || Rate {
            value: Value::Unread {
                why: Unread::Unsupported,
                unread: 1,
            },
            window: ReadWindow::Unread,
        };
        let procs = s.procs.as_deref().unwrap_or(&[]);
        // This index provides names only; it cannot establish group membership or custody.
        let mut names = IdMap::with_capacity_and_hasher(procs.len(), Default::default());
        for p in procs {
            if let Some(start) = p.start {
                names
                    .entry(crate::delegated::ProcessKey { pid: p.pid, start })
                    .and_modify(|name| *name = None)
                    .or_insert(Some(p.name.as_str()));
            }
        }
        let members = procs
            .iter()
            .zip(&pass.group_keys)
            .zip(&pass.rates)
            .filter_map(|((p, &key), r)| {
                let key = key?;
                let (scope, root) = match key {
                    GroupKey::Tab { pane, root } => (Scope::Tab(pane), root),
                    GroupKey::Aterm(root) => (Scope::Aterm, root),
                    GroupKey::Shared(root) => (Scope::Shared, root),
                    GroupKey::Coalition(_) => return None,
                };
                let name = names.get(&root).copied().flatten().unwrap_or("");
                let mut observation = Observation::new(key, scope, name, 1);
                observation.cpu = rate(r.cores.map(f64::from), r.read_at_ns, r.cpu_read_span_ns);
                observation.cpu_basis = CpuBasis::ProcessOwnCounters;
                observation.footprint = p
                    .footprint
                    .map_or_else(|| Value::not_sampled(1), Value::Read);
                observation.read_bps = rate(r.read_bps, s.at_ns, r.io_read_span_ns);
                observation.write_bps = rate(r.write_bps, s.at_ns, r.io_read_span_ns);
                observation.energy = Energy {
                    thread_mw: rate(r.energy_mw, r.read_at_ns, r.energy_read_span_ns),
                    cpu_mw: unsupported(),
                    gpu_mw: unsupported(),
                    ane_mw: unsupported(),
                };
                observation.starts_ps = unsupported();
                observation.membership_complete = processes_complete;
                Some(observation)
            });
        let coalitions = fresh.into_iter().flat_map(|facts| {
            facts.split.rows.iter().filter_map(|row| {
                if row.part == crate::explain::Part::Aterm
                    && !row.ownership_mixed
                    && row.footprint_aterm
                {
                    return None;
                }
                // `part` may retain an earlier owner or default to Services without a name.
                // Only the current complete foreign-member census proves this classification.
                let scope = if row.ownership_mixed || row.footprint_ownership_incomplete {
                    Scope::Unknown
                } else {
                    match row.footprint_outside {
                        Some(crate::explain::Part::Others) => Scope::Others,
                        Some(crate::explain::Part::Services) => Scope::Services,
                        _ => Scope::Unknown,
                    }
                };
                let name = row
                    .lead
                    .as_ref()
                    .and_then(|lead| lead.exe.as_deref())
                    .unwrap_or("");
                let mut observation =
                    Observation::new(GroupKey::Coalition(row.id), scope, name, row.live);
                observation.cpu_basis = CpuBasis::CoalitionCounters;
                observation.cpu = rate(
                    (row.measured && row.cpu_mc >= 0).then_some(row.cpu_mc as f64 / 1000.0),
                    facts.at_ns,
                    Some(facts.window_ns),
                );
                observation.footprint = row
                    .footprint
                    .map_or_else(|| Value::not_sampled(1), Value::Read);
                observation.read_bps = rate(
                    row.measured.then_some(row.read_bps as f64),
                    facts.at_ns,
                    Some(facts.window_ns),
                );
                observation.write_bps = unsupported();
                observation.energy = match energy
                    .as_ref()
                    .and_then(|energy| energy.get(row.id).map(|row| (energy, row)))
                {
                    Some((energy, row)) => Energy {
                        thread_mw: unsupported(),
                        cpu_mw: rate(Some(row.cpu_mw()), energy.at_ns(), Some(energy.window_ns())),
                        gpu_mw: rate(Some(row.gpu_mw()), energy.at_ns(), Some(energy.window_ns())),
                        ane_mw: rate(Some(row.ane_mw()), energy.at_ns(), Some(energy.window_ns())),
                    },
                    None => Energy {
                        thread_mw: unsupported(),
                        cpu_mw: Rate::default(),
                        gpu_mw: Rate::default(),
                        ane_mw: Rate::default(),
                    },
                };
                observation.starts_ps = starts
                    .binary_search_by_key(&row.id, |&(id, _, _)| id)
                    .ok()
                    .map_or_else(Rate::default, |i| {
                        rate(Some(starts[i].1), s.at_ns, Some(starts[i].2))
                    });
                observation.membership_complete =
                    coalitions_complete && row.memory_members.complete && scope != Scope::Unknown;
                Some(observation)
            })
        });
        self.groups.absorb(
            Frame {
                seq: self.seq,
                taken: self.rows.taken,
                mono_ms: s.mono_ms,
                at_ns: s.at_ns,
                processes_observed: s.procs.is_some() && !s.machine_only,
                coalitions_observed: s.coalitions.is_some() && s.coal_lane.on(),
                processes_complete,
                coalitions_complete,
            },
            members.chain(coalitions),
        );
        self.groups.view()
    }

    fn absorb_procs(
        &mut self,
        procs: &[ProcSample],
        s: &Sample,
        global: &mut ScopeView,
        sweep_dt: Option<f64>,
    ) -> ProcPass {
        let reads = window_counter(
            &self.rows.read_bytes,
            self.rows.sweep_whole,
            procs,
            sweep_dt,
            |p| p.disk_read,
        );
        let writes = window_writes(&self.rows.written, self.rows.sweep_whole, procs, sweep_dt);
        let writes_read = procs.iter().any(|p| p.disk_written.is_some());
        self.rows.read_bytes = procs
            .iter()
            .map(|p| (p.pid, (p.start, p.disk_read)))
            .collect();
        self.rows.written = procs
            .iter()
            .map(|p| (p.pid, (p.start, p.disk_written)))
            .collect();
        self.rows.sweep_whole = !s.procs_truncated;

        let wall_ns = self
            .last_sweep_at
            .filter(|&t| s.at_ns > t)
            .map(|t| s.at_ns - t);
        self.last_sweep_at = Some(s.at_ns);
        let mut next_cpu = IdMap::with_capacity_and_hasher(procs.len(), Default::default());
        let mut next_energy = IdMap::with_capacity_and_hasher(procs.len(), Default::default());
        let mut next_runnable = IdMap::with_capacity_and_hasher(procs.len(), Default::default());
        let mut next_read_at = IdMap::with_capacity_and_hasher(procs.len(), Default::default());
        let mut rates: Vec<ProcRates> = Vec::with_capacity(procs.len());
        let mut common_cores = Vec::with_capacity(procs.len());
        let mut pid_counts =
            IdMap::<u32, usize>::with_capacity_and_hasher(procs.len(), Default::default());
        for p in procs {
            *pid_counts.entry(p.pid).or_default() += 1;
        }
        for p in procs {
            common_cores.push(
                p.cpu_ns
                    .zip(self.last_cpu.get(&p.pid))
                    .zip(wall_ns)
                    .and_then(|((_, before), window)| {
                        (window > 0).then_some(())?;
                        Some(before.growth(p)? as f32 / window as f32)
                    }),
            );
            let mut r = ProcRates {
                read_at_ns: s.at_ns,
                io_read_span_ns: sweep_dt.map(|dt| (dt * 1e9) as u64),
                read_bps: sweep_dt
                    .zip(reads.get(&p.pid))
                    .map(|(dt, &bytes)| bytes as f64 / dt),
                write_bps: sweep_dt
                    .zip(writes.get(&p.pid))
                    .map(|(dt, &bytes)| bytes as f64 / dt),
                gpu: self.rows.gpu_clients.share(p.pid, &p.name),
                ..ProcRates::default()
            };
            let wall_ns = match (p.at_ns, self.last_read_at.get(&p.pid)) {
                (Some(now), Some(&then)) if now > then => {
                    r.read_at_ns = now;
                    Some(now - then)
                }
                _ => wall_ns,
            };
            if let Some(at) = p.at_ns {
                next_read_at.insert(p.pid, at);
            }
            if let Some(nj) = p.energy_nj.filter(|_| pid_counts[&p.pid] == 1) {
                next_energy.insert(
                    p.pid,
                    ProcessCpuCounter {
                        start: p.start,
                        version: p.version,
                        ns: nj,
                    },
                );
                if let Some((window, delta)) = sweep_dt
                    .and(wall_ns.filter(|&window| window > 0))
                    .zip(self.last_energy.get(&p.pid))
                    .and_then(|(window, before)| {
                        before.matches(p).then_some(())?;
                        Some((window, nj.checked_sub(before.ns)?))
                    })
                {
                    r.energy_mw = Some(delta as f64 * 1000.0 / window as f64);
                    r.energy_read_span_ns = Some(window);
                }
            }
            if let Some(ns) = p.cpu_ns {
                next_cpu.insert(
                    p.pid,
                    ProcessCpuCounter {
                        start: p.start,
                        version: p.version,
                        ns,
                    },
                );
                if let Some((wall, growth)) = wall_ns
                    .filter(|&wall| wall > 0)
                    .zip(self.last_cpu.get(&p.pid))
                    .and_then(|(wall, before)| Some((wall, before.growth(p)?)))
                {
                    r.cores = Some(growth as f32 / wall as f32);
                    r.cpu_read_span_ns = Some(wall);
                }
            }
            if let Some(run) = p.runnable_ns {
                next_runnable.insert(p.pid, run);
                if let (Some(wall), Some(&before), Some(cpu_before)) = (
                    wall_ns,
                    self.last_runnable.get(&p.pid),
                    self.last_cpu.get(&p.pid),
                ) && wall > 0
                    && let Some(d_cpu) = cpu_before.growth(p)
                    && let Some(d_run) = run.checked_sub(before)
                {
                    r.wait =
                        Some(((i128::from(d_run) - i128::from(d_cpu)) as f64 / wall as f64) as f32);
                }
            }
            rates.push(r);
        }
        self.last_cpu = next_cpu;
        self.last_energy = next_energy;
        self.last_runnable = next_runnable;
        self.last_read_at = next_read_at;
        global.procs = u32::try_from(procs.len()).unwrap_or(u32::MAX);
        let (membership, delegated_members) = self.attribute_owned(s, procs);
        self.pane_resources
            .observe(s, procs, &membership, &pid_counts, sweep_dt.is_some());
        let group_keys = self.process_groups(s, procs, &membership, &delegated_members);
        let Some(wall_ns) = wall_ns else {
            // No window yet, but the explanations' reap credit takes its first members here.
            return ProcPass {
                group_keys,
                rates,
                attribution: Some((membership, delegated_members)),
                ..ProcPass::default()
            };
        };
        let dt_s = wall_ns as f32 / 1e9;

        let mut aterm_acc = ScopeAcc::default();
        let mut shared_acc = ScopeAcc::default();
        let mut global_acc = ScopeAcc::default();
        let mut tab_acc: Vec<ScopeAcc> = vec![ScopeAcc::default(); s.tabs.len()];
        let incomplete = s.native_incomplete || s.ownership_incomplete || s.procs_truncated;
        if incomplete || !s.unproved_tabs.is_empty() {
            aterm_acc.cpu_incomplete += 1;
            aterm_acc.footprint_unread += 1;
        }
        if s.native_source_lost {
            aterm_acc.cpu_incomplete += 1;
        }
        for (root, acc) in s.tabs.iter().zip(&mut tab_acc) {
            if incomplete || s.unproved_tabs.contains(&root.key) {
                acc.cpu_incomplete = acc.cpu_incomplete.saturating_add(1);
                acc.footprint_unread = acc.footprint_unread.saturating_add(1);
            }
            if s.native_source_lost {
                acc.cpu_incomplete = acc.cpu_incomplete.saturating_add(1);
            }
        }
        let (mut others, mut services, mut unknown_owner_reported) = (0.0f32, 0.0f32, 0.0f32);
        let (mut others_gpu, mut services_gpu) = (0.0f32, 0.0f32);
        let mut rows: Vec<RowKey> = Vec::with_capacity(procs.len());
        let mut job_acc: Vec<ScopeAcc> = vec![ScopeAcc::default(); s.delegated.len()];
        let mut top: Vec<TopProc> = Vec::with_capacity(TOP_PROCS + 1);
        let mut writers: Vec<Writer> = Vec::new();
        let mut groups: Vec<Busiest> = Vec::new();
        let mut group_of: HashMap<&str, usize> = HashMap::new();
        let mut programs: Vec<ProgramGroup> = Vec::new();
        let mut program_of: HashMap<(ProcScope, &str), usize> = HashMap::new();
        for (index, ((p, m), r)) in procs
            .iter()
            .zip(membership.iter())
            .zip(rates.iter())
            .enumerate()
        {
            global_acc.add(p, r);
            let cores = r.cores.unwrap_or(0.0);
            let wrote = r.write_bps;
            if let Some(job) = delegated_members[index] {
                job_acc[job].add(p, r);
            }
            let scope = || match (m.tab, m.aterm, m.shared) {
                (Some(t), _, _) => s
                    .tabs
                    .get(t)
                    .map_or(ProcScope::Aterm, |r| ProcScope::Tab(r.tab)),
                (None, true, true) => ProcScope::Shared,
                (None, true, false) => ProcScope::Aterm,
                (None, false, _) => ProcScope::Other,
            };
            if cores >= 0.01
                && (top.len() < TOP_PROCS || top.last().is_some_and(|t| cores > t.cores))
            {
                let at = top.partition_point(|t| t.cores >= cores);
                top.insert(
                    at,
                    TopProc {
                        name: p.name.clone(),
                        cores,
                        footprint: p.footprint,
                        scope: scope(),
                    },
                );
                top.truncate(TOP_PROCS);
            }
            if let (Some(w), Some(&bytes)) = (wrote, writes.get(&p.pid))
                && bytes > 0
                && s.self_uid.is_none_or(|u| p.uid == Some(u))
            {
                let scope = scope();
                match writers
                    .iter_mut()
                    .find(|g| g.scope == scope && g.name == p.name)
                {
                    Some(g) => {
                        g.bytes_per_s += w;
                        g.n += 1;
                        g.bytes = g.bytes.saturating_add(bytes);
                    }
                    None => writers.push(Writer {
                        name: p.name.clone(),
                        bytes_per_s: w,
                        scope,
                        n: 1,
                        bytes,
                    }),
                }
            }
            if cores > 0.0 {
                let scope = scope();
                match group_of.get(p.name.as_str()) {
                    Some(&g) => {
                        let g = &mut groups[g];
                        g.cores += cores;
                        g.n += 1;
                        if g.scope != Some(scope) {
                            g.scope = None;
                        }
                        g.born_ms = earliest(g.born_ms, p.birth_ms);
                        g.unborn += usize::from(p.birth_ms.is_none());
                    }
                    None => {
                        group_of.insert(&p.name, groups.len());
                        groups.push(Busiest {
                            name: p.name.clone(),
                            cores,
                            n: 1,
                            scope: Some(scope),
                            born_ms: p.birth_ms,
                            unborn: usize::from(p.birth_ms.is_none()),
                        });
                    }
                }
            }
            let program = *program_of
                .entry((scope(), p.name.as_str()))
                .or_insert_with(|| {
                    programs.push(ProgramGroup::new(&p.name, scope()));
                    programs.len() - 1
                });
            programs[program].add(cores, p.footprint, p.birth_ms);
            if m.aterm {
                aterm_acc.add(p, r);
            }
            if m.shared {
                shared_acc.add(p, r);
            }
            if let Some(t) = m.tab {
                tab_acc[t].add(p, r);
            }
            let row_scope = if p.pid == s.self_pid {
                RowScope::SelfProc
            } else if let Some(t) = m.tab {
                RowScope::Tab(s.tabs[t].key)
            } else if m.shared {
                RowScope::Shared
            } else if m.aterm {
                RowScope::Helper
            } else {
                match (p.uid, s.self_uid) {
                    (Some(u), Some(me)) if u == me => RowScope::Others,
                    (Some(_), Some(_)) => RowScope::Services,
                    _ => RowScope::Unknown,
                }
            };
            match row_scope {
                RowScope::Others => {
                    others += common_cores[index].unwrap_or(0.0);
                    others_gpu += r.gpu.unwrap_or(0.0);
                }
                RowScope::Services => {
                    services += common_cores[index].unwrap_or(0.0);
                    services_gpu += r.gpu.unwrap_or(0.0);
                }
                RowScope::Unknown => {
                    unknown_owner_reported += common_cores[index].unwrap_or(0.0);
                    services_gpu += r.gpu.unwrap_or(0.0);
                }
                _ => {}
            }
            rows.push(RowKey {
                cores: r.cores,
                footprint: p.footprint,
                pid: p.pid,
                scope: row_scope,
                gpu: r.gpu,
            });
        }
        global.top = global_acc.top.clone();
        global.wait = global_acc.wait;
        global.read_bps = global_acc.reads;
        global.write_bps = global_acc.writes;
        global.refused = global_acc.refused;
        global.gpu = global_acc.gpu;
        writers.sort_by(|a, b| b.bytes_per_s.total_cmp(&a.bytes_per_s));
        let busiest = groups
            .into_iter()
            .reduce(|a, b| if b.cores > a.cores { b } else { a });

        let aterm_cores = aterm_acc.cores;
        let gpu_parts = aterm_acc.gpu.map(|aterm| (aterm, others_gpu, services_gpu));
        let aterm = self.aterm.absorb(&aterm_acc, dt_s, s.at_ns);
        let shared = self.shared.absorb(&shared_acc, dt_s, s.at_ns);
        let named = front_names(procs, &s.tabs);
        let mut live = std::collections::HashSet::with_capacity(s.tabs.len());
        let tabs = s
            .tabs
            .iter()
            .zip(tab_acc.iter())
            .zip(named)
            .map(|((root, acc), named)| {
                live.insert(root.key);
                let state = self.tabs.entry(root.key).or_default();
                TabView {
                    key: root.key,
                    tab: root.tab,
                    label: root.label.clone(),
                    shell_pid: root.shell_pid,
                    program: if root.program.is_empty() {
                        named
                    } else {
                        root.program.clone()
                    },
                    scope: state.absorb(acc, dt_s, s.at_ns),
                }
            })
            .collect();
        self.tabs.retain(|k, _| live.contains(k));
        let delegated = s
            .delegated
            .iter()
            .zip(job_acc)
            // This legacy receipt has no coverage flag. Retain partial process/scope evidence,
            // but do not publish a reduced job total as a new complete receipt.
            .filter(|(_, acc)| {
                !incomplete
                    && acc.procs > 0
                    && acc.cpu_incomplete == 0
                    && acc.refused == 0
                    && acc.footprint_unread == 0
            })
            .map(|(job, acc)| DelegatedUsage {
                root: job.root,
                cores: acc.cores,
                footprint: acc.footprint,
                procs: acc.procs,
            })
            .collect();
        let kept = keep_rows(&rows)
            .into_iter()
            .map(|index| {
                proc_view(
                    &procs[index],
                    &rates[index],
                    group_keys[index],
                    rows[index].scope,
                )
            })
            .collect();
        ProcPass {
            group_keys,
            rates,
            aterm: Some(aterm),
            shared: Some(shared),
            tabs,
            top,
            delegated,
            figs: Some(SweepFigs {
                aterm: aterm_acc,
                tabs: tab_acc,
                writes_read,
                writers,
                busiest,
            }),
            memory_owners: Some(MemoryOwners::of(
                &rows,
                s.procs_truncated,
                s.native_incomplete || s.ownership_incomplete || !s.unproved_tabs.is_empty(),
            )),
            rows: kept,
            programs: keep_programs(programs),
            split_parts: Some((
                aterm_cores,
                others,
                services,
                unknown_owner_reported,
                global_acc.refused,
            )),
            gpu_parts,
            window_ns: Some(wall_ns),
            attribution: Some((membership, delegated_members)),
        }
    }
}

/// Each process's bytes written over the sweep window, by pid, where a window formed a rate
/// (`sweep_dt`): its counter's rise since `last` read it under the same start (by pid alone where
/// neither read knew a start), or, for one born inside the window, its whole counter. A process
/// present in the last sweep under another known start was born inside it (its pid was reused
/// after that read). One absent from a whole last sweep (`last_whole`) was born inside it only
/// when its start is later than every start that sweep read: an older one was alive then and
/// missed (a read that changed under the sweep, a pid list cut short), and its whole counter is
/// its life's writes, not the window's. One the last sweep read with no counter, or whose start
/// only one of the two reads knew, counts nothing until the next.
fn window_writes(
    last: &IdMap<u32, (Option<u64>, Option<u64>)>,
    last_whole: bool,
    procs: &[ProcSample],
    sweep_dt: Option<f64>,
) -> IdMap<u32, u64> {
    window_counter(last, last_whole, procs, sweep_dt, |p| p.disk_written)
}

fn window_counter(
    last: &IdMap<u32, (Option<u64>, Option<u64>)>,
    last_whole: bool,
    procs: &[ProcSample],
    sweep_dt: Option<f64>,
    field: impl Fn(&ProcSample) -> Option<u64>,
) -> IdMap<u32, u64> {
    if sweep_dt.is_none() {
        return IdMap::default();
    }
    let newest = last.values().filter_map(|r| r.0).max();
    procs
        .iter()
        .filter_map(|p| {
            let now = field(p)?;
            let bytes = match last.get(&p.pid) {
                Some(&(start, Some(before))) if start == p.start => now.saturating_sub(before),
                Some(&(Some(then), _)) if p.start.is_some_and(|s| s != then) => now,
                None if last_whole && p.start.zip(newest).is_some_and(|(s, n)| s > n) => now,
                _ => return None,
            };
            Some((p.pid, bytes))
        })
        .collect()
}

fn mem_rates(then: MemCounters, now: MemCounters, window_ns: u64) -> Option<MemRates> {
    if window_ns == 0 {
        return None;
    }
    let d = |a: u64, b: u64| b.checked_sub(a);
    let per_s = |v: u64| v as f64 * 1e9 / window_ns as f64;
    let decompress_ps = per_s(d(then.decompressions, now.decompressions)?);
    let compress_ps = per_s(d(then.compressions, now.compressions)?);
    let page_bps = |a: Option<u64>, b: Option<u64>| {
        a.zip(b)
            .and_then(|(a, b)| d(a, b))
            .map(|pages| per_s(pages) * now.page_bytes as f64)
    };
    let window_us = window_ns as f64 / 1e3;
    let stall = then.stall_us.zip(now.stall_us).and_then(|(a, b)| {
        let share = |us: u64| (us as f64 / window_us).min(1.0);
        Some((share(d(a.0, b.0)?), share(d(a.1, b.1)?)))
    });
    let swapin_bps = per_s(d(then.swapin_bytes, now.swapin_bytes)?);
    Some(MemRates {
        anon_back_bps: decompress_ps * now.page_bytes as f64 + swapin_bps,
        swapin_bps,
        swapout_bps: per_s(d(then.swapout_bytes, now.swapout_bytes)?),
        decompress_ps,
        compress_ps,
        pagein_ps: per_s(d(then.pageins, now.pageins)?),
        churn_bps: (decompress_ps + compress_ps) * now.page_bytes as f64,
        push_bps: page_bps(then.pushed, now.pushed),
        refault_bps: page_bps(then.refaulted, now.refaulted),
        stall,
        window_ns,
    })
}

/// What [`keep_rows`] and [`MemoryOwners::of`] read of one swept process's row: its order's
/// keys, its scope and its GPU share. A reading takes one for every process and builds the whole
/// row ([`proc_view`]) only for those [`keep_rows`] keeps.
#[derive(Clone, Copy, Debug)]
struct RowKey {
    cores: Option<f32>,
    footprint: Option<u64>,
    pid: u32,
    scope: RowScope,
    gpu: Option<f32>,
}

/// `p`'s row in [`Snapshot::procs`], at its rates `r`, its group and its scope.
fn proc_view(
    p: &ProcSample,
    r: &ProcRates,
    group: Option<crate::groups::GroupKey>,
    scope: RowScope,
) -> ProcView {
    #[cfg(test)]
    ROWS_BUILT.with(|n| n.set(n.get() + 1));
    ProcView {
        group,
        energy_mw: r.energy_mw,
        energy_read_span_ns: r.energy_read_span_ns,
        scheduling: p.scheduling,
        cpu_accounting: CpuAccounting::from(p),
        pid: p.pid,
        ppid: p.ppid,
        pgid: p.pgid,
        name: p.name.clone(),
        cores: r.cores,
        cpu_read_span_ns: r.cpu_read_span_ns,
        wait: r.wait,
        footprint: p.footprint,
        peak: p.peak,
        read_bps: r.read_bps,
        write_bps: r.write_bps,
        state: p.state,
        scope,
        refused: p.cpu_ns.is_none(),
        gpu: r.gpu,
        birth_ms: p.birth_ms,
    }
}

fn busiest_first(a: &RowKey, b: &RowKey) -> std::cmp::Ordering {
    #[cfg(test)]
    ROW_COMPARES.with(|n| n.set(n.get() + 1));
    b.cores
        .unwrap_or(-1.0)
        .total_cmp(&a.cores.unwrap_or(-1.0))
        .then(b.footprint.unwrap_or(0).cmp(&a.footprint.unwrap_or(0)))
        .then(a.pid.cmp(&b.pid))
}

#[cfg(test)]
thread_local! {
    /// [`busiest_first`]'s calls on this thread: the row comparisons [`keep_rows`] makes.
    static ROW_COMPARES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// [`proc_view`]'s calls on this thread: the rows a reading builds.
    static ROWS_BUILT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// [`Snapshot::procs`]' bound, as indices into `rows` in its order, busiest first
/// ([`busiest_first`]): every aterm and tab row up to [`ATERM_ROWS`] (half busiest and half
/// largest when over the cap), and of the machine's others the [`TOP_ROWS`] busiest, the
/// [`TOP_ROWS`] largest of the rest and every other one that holds a GPU share.
///
/// Of the others it selects the busiest and the largest instead of sorting them all: each order
/// is a stable sort's, spelled as a total order on its key and then the row's index, so the
/// first [`TOP_ROWS`] it selects and sorts are the rows, in the order, that a stable sort of
/// every row gives.
fn keep_rows(rows: &[RowKey]) -> Vec<usize> {
    let busiest = |a: &usize, b: &usize| busiest_first(&rows[*a], &rows[*b]).then(a.cmp(b));
    let largest = |a: &usize, b: &usize| {
        rows[*b]
            .footprint
            .unwrap_or(0)
            .cmp(&rows[*a].footprint.unwrap_or(0))
            .then_with(|| busiest(a, b))
    };
    let (mut kept, mut rest): (Vec<usize>, Vec<usize>) =
        (0..rows.len()).partition(|&i| rows[i].scope.in_aterm());
    kept.sort_unstable_by(busiest);
    if kept.len() > ATERM_ROWS {
        let mut by_foot = kept.split_off(ATERM_ROWS / 2);
        by_foot.sort_unstable_by(largest);
        by_foot.truncate(ATERM_ROWS - kept.len());
        kept.extend(by_foot);
    }
    let busy = first_by(&mut rest, TOP_ROWS, busiest);
    kept.extend_from_slice(&rest[..busy]);
    let rest = &mut rest[busy..];
    let large = first_by(rest, TOP_ROWS, largest);
    kept.extend_from_slice(&rest[..large]);
    let mut gpu: Vec<usize> = rest[large..]
        .iter()
        .copied()
        .filter(|&i| rows[i].gpu.is_some_and(|g| g > 0.0))
        .collect();
    gpu.sort_unstable_by(largest);
    kept.extend(gpu);
    kept.sort_by(|a, b| busiest_first(&rows[*a], &rows[*b]));
    kept
}

/// Puts the first `n` of `v` under `order`, a total order, at its front, sorted, and the rest
/// after them in no order; returns how many it put there (`n`, or all of `v` where it is
/// shorter).
fn first_by(
    v: &mut [usize],
    n: usize,
    mut order: impl FnMut(&usize, &usize) -> std::cmp::Ordering,
) -> usize {
    if v.len() > n {
        v.select_nth_unstable_by(n, &mut order);
    }
    let n = n.min(v.len());
    v[..n].sort_unstable_by(order);
    n
}

fn busiest_program_first(a: &ProgramGroup, b: &ProgramGroup) -> std::cmp::Ordering {
    #[cfg(test)]
    PROGRAM_COMPARES.with(|n| n.set(n.get() + 1));
    b.cores
        .total_cmp(&a.cores)
        .then(b.footprint.unwrap_or(0).cmp(&a.footprint.unwrap_or(0)))
        .then(a.name.cmp(&b.name))
}

#[cfg(test)]
thread_local! {
    /// The comparisons [`keep_programs`] makes on this thread: [`busiest_program_first`]'s calls
    /// and its footprint order's (a footprint tie counts twice, as that order then calls
    /// [`busiest_program_first`]).
    static PROGRAM_COMPARES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// [`Snapshot::programs`]' bound, as [`keep_rows`] bounds the rows: every aterm and tab group
/// up to [`ATERM_ROWS`] (half busiest and half largest when over the cap), and of the machine's
/// others the [`TOP_ROWS`] busiest and the [`TOP_ROWS`] largest of the rest, busiest first
/// ([`busiest_program_first`]).
///
/// As [`keep_rows`] does, it orders indices, never groups, and of the others selects the
/// busiest and the largest instead of sorting them all: each order is a stable sort's, spelled
/// as a total order on its key and then the group's index (busiest: [`busiest_program_first`]
/// then index; largest: footprint descending, then busiest), so the groups it keeps, and the
/// sequence its last stable sort orders, are those of a stable sort of every group.
fn keep_programs(groups: Vec<ProgramGroup>) -> Vec<ProgramGroup> {
    let busiest =
        |a: &usize, b: &usize| busiest_program_first(&groups[*a], &groups[*b]).then(a.cmp(b));
    let largest = |a: &usize, b: &usize| {
        #[cfg(test)]
        PROGRAM_COMPARES.with(|n| n.set(n.get() + 1));
        groups[*b]
            .footprint
            .unwrap_or(0)
            .cmp(&groups[*a].footprint.unwrap_or(0))
            .then_with(|| busiest(a, b))
    };
    let (mut kept, mut rest): (Vec<usize>, Vec<usize>) =
        (0..groups.len()).partition(|&i| groups[i].scope != ProcScope::Other);
    kept.sort_unstable_by(busiest);
    if kept.len() > ATERM_ROWS {
        let mut by_foot = kept.split_off(ATERM_ROWS / 2);
        by_foot.sort_unstable_by(largest);
        by_foot.truncate(ATERM_ROWS - kept.len());
        kept.extend(by_foot);
    }
    let busy = first_by(&mut rest, TOP_ROWS, busiest);
    kept.extend_from_slice(&rest[..busy]);
    let rest = &mut rest[busy..];
    let large = first_by(rest, TOP_ROWS, largest);
    kept.extend_from_slice(&rest[..large]);
    kept.sort_by(|a, b| busiest_program_first(&groups[*a], &groups[*b]));
    let mut groups: Vec<Option<ProgramGroup>> = groups.into_iter().map(Some).collect();
    kept.into_iter()
        .filter_map(|index| groups[index].take())
        .collect()
}

impl Rows {
    fn absorb(
        &mut self,
        s: &Sample,
        stamp: Stamp,
        busy: Option<f64>,
        figs: Option<&SweepFigs>,
        sweep_dt: Option<f64>,
        scoped_cpu: Option<&crate::explain::ScopeCpu>,
    ) -> ([ResourceView; 7], RowFacts, Option<NetView>) {
        use Resource::{Cpu, Disk, Gpu, Memory, Network, Processes, Tokens};
        let now = s.at_unix_ms;
        let t = Taken::at(now, s.period_ms());
        self.taken = t;
        if let Some(cpu) = scoped_cpu
            .filter(|cpu| sweep_dt.is_some_and(|dt| (dt * 1e9 - cpu.window_ns as f64).abs() < 1.0))
        {
            self.logical_cpu
                .retain(|id, _| cpu.tabs.iter().any(|tab| tab.id == *id));
            for tab in &cpu.tabs {
                let rings = self.logical_cpu.entry(tab.id).or_default();
                let mut values = [None; 7];
                values[Cpu.index()] = Some(tab.reported);
                rings.push(t, values);
                let _ = rings.2.push(t, Some(tab.charge), false);
                let incomplete = tab.missed
                    || tab.ambiguous
                    || cpu.missed
                    || s.native_incomplete
                    || s.native_source_lost
                    || s.ownership_incomplete
                    || s.procs_truncated
                    || s.tabs.iter().enumerate().any(|(index, root)| {
                        root.tab == tab.key
                            && (s.unproved_tabs.contains(&root.key)
                                || figs.is_some_and(|figs| figs.tabs[index].refused > 0))
                    });
                let _ = rings.1[1].push(t, Some(f64::from(u32::from(incomplete))), false);
            }
        }
        if let Some(brand) = s.cpu_brand.as_ref().filter(|b| !b.is_empty()) {
            self.cpu_brand = Some(brand.clone());
        }
        let mut facts = RowFacts {
            cpu_brand: self.cpu_brand.clone(),
            load1: s.load1,
            gpu: s.gpu.clone(),
            disk: s.disk.clone(),
            plan: s.plan.clone(),
            room: s.room,
            process_room: self.room.absorb(s.room),
            ..RowFacts::default()
        };

        facts.runnable = self.runq.absorb(t, s.run_queue);
        facts.queue = crate::runq::RunQueue::of(facts.runnable, s.load1);
        let n_cores = s.cores.as_ref().map(Vec::len).filter(|&n| n > 0);
        let cgroup_cores = s
            .cpu_millicores
            .filter(|&m| m > 0)
            .map(|m| f64::from(m) / 1000.0);
        let cpu_cap = match (n_cores.map(|n| n as f64), cgroup_cores) {
            (Some(h), Some(c)) => Some(h.min(c)),
            (h, c) => h.or(c),
        };
        if let Some(c) = cpu_cap {
            self.caps[Cpu.index()] = Some(c);
        }
        let cpu_over = crate::units::cpu_over(busy, n_cores, facts.queue);
        self.rings[Cpu.index()].push(t, busy, cpu_over);

        let gpu_pct = s
            .gpu
            .as_ref()
            .and_then(|g| g.device_pct)
            .map(|p| f64::from(p.min(100)));
        self.caps[Gpu.index()] = Some(100.0);
        self.rings[Gpu.index()].push(t, gpu_pct, false);
        if let Some(r) = s.gpu.as_ref().and_then(|g| g.recoveries) {
            let first = *self.gpu_recoveries.get_or_insert(r);
            if r > first {
                let since = *self.gpu_rose_at.get_or_insert(now);
                facts.gpu_resets = Some((r - first, since));
            }
        }

        let mem = s.mem.filter(|m| m.total > 0);
        let cgroup_mem = s.mem_bytes.filter(|&b| b > 0);
        let mem_cap = match (mem.map(|m| m.total), cgroup_mem) {
            (Some(h), Some(c)) => Some(h.min(c)),
            (h, c) => h.or(c),
        };
        if let Some(b) = mem_cap {
            self.caps[Memory.index()] = Some(b as f64);
        }
        if let Some(kib) = s.swapped_kib {
            facts.swap_rate = self.swap.and_then(|(then, before)| {
                let dt = stamp.rate_dt(Some(then))?;
                Some(kib.checked_sub(before)? as f64 * 1024.0 / dt)
            });
            self.swap = Some((stamp, kib));
        }
        let mem_over = facts.swap_rate.is_some_and(|r| r >= SWAP_OVER_BPS);
        self.rings[Memory.index()].push(t, mem.map(|m| m.used() as f64), mem_over);
        let pressure = mem.and_then(|m| m.pressure.rank()).map(f64::from);
        self.pressure.push(t, pressure, false);

        let vol = s.volume.as_ref().filter(|v| v.total > 0);
        if let Some(v) = vol {
            self.caps[Disk.index()] = Some(v.total as f64);
            self.free.note_on(&v.mount, v.total, t, v.free);
            self.weeks.note_on(&v.mount, v.total, t, v.free);
        }
        self.rings[Disk.index()].push(t, vol.map(|v| v.used() as f64), false);
        if let Some(v) = vol {
            facts.disk_rates = history::rates(&self.rings[Disk.index()]);
            match facts.disk_rates.rate60 {
                Some(r) if r > 0.0 => {
                    let _ = self.filling_since.get_or_insert(stamp.mono_ms);
                }
                Some(_) => self.filling_since = None,
                None => {}
            }
            let filling_for = self.filling_since.map_or(0, |t| {
                i64::try_from(stamp.mono_ms.saturating_sub(t)).unwrap_or(i64::MAX)
            });
            let ttf = history::time_to_full(v.free, facts.disk_rates.rate60, filling_for);
            facts.filling = history::filling(v.free, &facts.disk_rates, ttf);
        }

        let (rx, tx, net) = self.absorb_links(s, stamp, &mut facts);
        let closed_rx = self.rings[Network.index()].push(t, rx, false);
        let closed_tx = self.net_tx.push(t, tx, false);
        note_peak(
            &self.rings[Network.index()],
            closed_rx,
            &mut self.peaks.net_rx,
        );
        note_peak(&self.net_tx, closed_tx, &mut self.peaks.net_tx);

        let tokens = self.absorb_tokens(&s.tokens, stamp);
        let closed = self.rings[Tokens.index()].push(t, tokens.out, false);
        note_peak(&self.rings[Tokens.index()], closed, &mut self.peaks.tokens);
        if let Some(input) = tokens.input {
            self.tokens_in.push(now, input);
        }
        facts.tokens_in = self.tokens_in.mean(now, TOKENS_WINDOW_MS);
        facts.agents = tokens.agents;

        facts.flow = match s.starts.as_ref() {
            Some(count) => {
                let moved = match self.starts.as_ref() {
                    None => Err("warming"),
                    Some((then, before)) => match stamp.rate_dt(Some(*then)) {
                        None => Err("gap"),
                        Some(dt) => crate::starts::moved(before, count)
                            .map(|m| (m, dt))
                            .ok_or("start_counter"),
                    },
                };
                let carries_heat = matches!(
                    count,
                    crate::starts::StartCount::Coalitions { read, .. } if read.heat().is_some()
                );
                self.heat_moved = match &moved {
                    Ok((m, _)) => m.heat,
                    Err(why) => carries_heat.then_some(Err(*why)),
                };
                self.group_energy = moved.as_ref().ok().and_then(|(m, _)| m.energy.clone());
                self.group_starts.clear();
                if let (
                    Ok((_, dt)),
                    Some((then, crate::starts::StartCount::Coalitions { read: before, .. })),
                    crate::starts::StartCount::Coalitions { read: now, .. },
                ) = (&moved, self.starts.as_ref(), count)
                    && let Some(window) = stamp
                        .mono_ms
                        .checked_sub(then.mono_ms)
                        .and_then(|ms| ms.checked_mul(1_000_000))
                        .filter(|&ns| ns > 0)
                {
                    // `moved` has already admitted this exact merge, including new ids,
                    // missing bases, resets and list coverage. Do not run it a second time.
                    self.group_starts
                        .extend(now.rows().iter().filter_map(|row| {
                            let starts = match before.get(row.id) {
                                Some(old) => row.started.checked_sub(old.started)?,
                                None => row.started,
                            };
                            Some((row.id, starts as f64 / dt, window))
                        }));
                }
                self.starts = Some((stamp, count.clone()));
                moved
                    .ok()
                    .and_then(|(m, dt)| crate::starts::TaskFlow::of(m, dt))
            }
            None => {
                self.starts = None;
                self.heat_moved = Some(Err("coalitions_unread"));
                self.group_energy = None;
                self.group_starts.clear();
                None
            }
        };
        let starts = self.proc_meaning(s.starts.is_some());
        let proc_v = if starts {
            facts.flow.as_ref().map(|f| f.starts_ps)
        } else {
            s.room.procs.map(|n| n as f64)
        };
        let closed = self.rings[Processes.index()].push(t, proc_v, false);
        if starts {
            note_peak(
                &self.rings[Processes.index()],
                closed,
                &mut self.peaks.proc_starts,
            );
        }
        let _ = self
            .proc_count
            .push(t, s.room.procs.map(|n| n as f64), false);
        let _ = self
            .proc_exits
            .push(t, facts.flow.as_ref().and_then(|f| f.exits_ps), false);
        facts.proc_read_s = self.peaks.proc_starts.read_s;
        // aterm's figure is its starts where the row is starts (its own coalition's); a tab's
        // share of them is F14's estimate, so a tab reads none until then.
        let aterm_procs = |acc: &ScopeAcc| {
            if starts {
                facts.flow.as_ref().and_then(|f| f.aterm_starts_ps)
            } else {
                Some(f64::from(acc.procs))
            }
        };

        let today = day_of(now);
        let link_cap = facts
            .link
            .as_ref()
            .and_then(|l| l.rate_bps)
            .map(|bps| bps as f64 / 8.0);
        let (net_basis, rx_cap, tx_cap) = net_capacity(&self.peaks, today, link_cap);
        facts.net_read_s = self.peaks.net_rx.read_s.min(self.peaks.net_tx.read_s);
        let (tok_basis, tok_cap) = if self.peaks.tokens.learned(TOKENS_LEARN_S) {
            (CapBasis::Peak, self.peaks.tokens.peak(today))
        } else {
            (CapBasis::Learning, None)
        };
        let (proc_basis, proc_cap) = match () {
            () if !starts => (CapBasis::Fixed, s.room.procs_max.map(|n| n as f64)),
            () if self.peaks.proc_starts.learned(PROC_LEARN_S) => {
                (CapBasis::Peak, self.peaks.proc_starts.peak(today))
            }
            () => (CapBasis::Learning, None),
        };
        facts.tokens_read_s = self.peaks.tokens.read_s;

        let live: HashSet<u64> = s.tabs.iter().map(|t| t.key).collect();
        self.tabs.retain(|k, _| live.contains(k));
        // A program's tab is its `TabRoot::tab` (`ProcScope::Tab`), not a pane's key.
        self.write_hour
            .fold_closed(&s.tabs.iter().map(|t| t.tab).collect());
        match (figs.filter(|f| f.writes_read), sweep_dt) {
            (Some(f), Some(dt)) => {
                self.write_hour
                    .note(now, (dt * 1000.0).round() as u64, &f.writers);
            }
            _ => {
                let _ = self.write_hour.at(now);
            }
        }
        facts.hour_writers = self.write_hour.top();
        facts.writes_covered_s = self.write_hour.covered_s();
        if let Some(f) = figs {
            let kappa = self.gpu_clients.kappa().unwrap_or(1.0);
            let vals = |acc: &ScopeAcc,
                        tokens: Option<f64>,
                        sockets: Option<u32>,
                        procs: Option<f64>|
             -> [Option<f64>; 7] {
                let mut v = [None; 7];
                v[Cpu.index()] = sweep_dt.map(|_| f64::from(acc.cores));
                // The scope's client time times κ, its share of the device's busy time over
                // the client window (`gpu_clients::kappa`), held at this reading's device
                // busy so each 10 s cell keeps tab <= all tabs <= Mac.
                v[Gpu.index()] = acc.gpu.map(|g| {
                    let share = f64::from(g * kappa) * 100.0;
                    gpu_pct.map_or(share, |mac| share.min(mac))
                });
                v[Memory.index()] = acc.footprint.map(|b| b as f64);
                v[Disk.index()] = sweep_dt.and(acc.writes);
                v[Network.index()] = sockets.map(f64::from);
                v[Tokens.index()] = tokens;
                v[Processes.index()] = procs;
                v
            };
            let aterm_v = vals(&f.aterm, tokens.out, s.aterm_sockets, aterm_procs(&f.aterm));
            self.aterm.push(t, aterm_v);
            let _ = self.aterm.2.push(t, f.aterm.cpu_charge, false);
            let _ = self.aterm.1[0].push(t, Some(f64::from(f.aterm.footprint_unread)), false);
            let _ = self.aterm.1[1].push(
                t,
                Some(f64::from(
                    f.aterm.refused.saturating_add(f.aterm.cpu_incomplete),
                )),
                false,
            );
            for (root, acc) in s.tabs.iter().zip(&f.tabs) {
                let rings = self.tabs.entry(root.key).or_default();
                let tab_sock = s
                    .tab_sockets
                    .iter()
                    .find(|(k, _)| *k == root.key)
                    .map(|(_, c)| *c);
                let tab_procs = (!starts).then(|| f64::from(acc.procs));
                rings.push(
                    t,
                    vals(
                        acc,
                        tokens.tabs.get(&root.key).copied(),
                        tab_sock,
                        tab_procs,
                    ),
                );
                let _ = rings.1[0].push(t, Some(f64::from(acc.footprint_unread)), false);
                let _ = rings.2.push(t, acc.cpu_charge, false);
                let _ = rings.1[1].push(
                    t,
                    Some(f64::from(acc.refused.saturating_add(acc.cpu_incomplete))),
                    false,
                );
            }
            facts.writers = crate::writes::top_per_scope(f.writers.iter().cloned(), |w| w.scope);
            facts.busiest = f.busiest.clone();
        } else {
            let mut v = [None; 7];
            v[Network.index()] = s.aterm_sockets.map(f64::from);
            if starts {
                v[Processes.index()] = facts.flow.as_ref().and_then(|f| f.aterm_starts_ps);
            }
            self.aterm.push(t, v);
            if s.aterm_sockets.is_some() {
                for &(key, count) in &s.tab_sockets {
                    let mut tv = [None; 7];
                    tv[Network.index()] = Some(f64::from(count));
                    self.tabs.entry(key).or_default().push(t, tv);
                }
            }
        }
        let gpu_held = self.gpu_clients.held();
        let net_has_sockets = self.aterm.ring(Network).is_some() || s.aterm_sockets.is_some();
        let missing = |res: Resource, agent: bool| -> Option<NoFigure> {
            match res {
                Gpu if !gpu_held => Some(NoFigure::NotPerApp),
                Network if !net_has_sockets => Some(NoFigure::NotPerApp),
                Disk if figs.is_some_and(|f| !f.writes_read) => Some(NoFigure::NotReadHere),
                Tokens if !agent => Some(NoFigure::NoAgent),
                _ => None,
            }
        };
        let fig = |rings: Option<&ScopeRings>, res: Resource, agent: bool| {
            let ring = rings.and_then(|r| r.ring(res));
            let has_scope = figs.is_some() || (res == Network && s.aterm_sockets.is_some());
            ScopeFig {
                cpu_accounting: (res == Cpu)
                    .then_some(crate::published_cpu::CpuAccounting::PUBLISHED_COUNTERS),
                now: has_scope
                    .then_some(())
                    .and(ring)
                    .and_then(|r| r.now_mean(now, window_of(res))),
                cpu_charge: if res == Cpu {
                    rings.and_then(|r| r.2.now_mean(now, window_of(res)))
                } else {
                    None
                },
                unread: match res {
                    Memory | Cpu => {
                        let i = usize::from(res == Cpu);
                        rings
                            .and_then(|r| r.1[i].now_max(now, window_of(res)))
                            .unwrap_or(0.0) as u32
                    }
                    _ => 0,
                },
                missing: missing(res, agent),
                run: ring.map_or_else(|| Arc::from([]), |r| r.tail_cells(SCOPE_RUN_BUCKETS).into()),
                run_k: ring.and_then(BucketRing::open_k).unwrap_or(0),
            }
        };

        let disk_state = disk_state(s.disk.as_ref(), facts.filling.as_ref());
        let mem_state = mem.and_then(|m| m.pressure.state());
        let gpu_state = facts.gpu_resets.map(|_| State::Warn);
        let cpu_state = s.thermal.and_then(Thermal::state);
        let resources = Resource::ALL.map(|res| {
            let i = res.index();
            let ring = &self.rings[i];
            let (capacity, basis) = match res {
                Network => (rx_cap, net_basis),
                Tokens => (tok_cap, tok_basis),
                Processes => (proc_cap, proc_basis),
                _ => (self.caps[i], CapBasis::Fixed),
            };
            let second = match res {
                Network => Some(series(&self.net_tx, now, NOW_WINDOW_MS, tx_cap)),
                _ => None,
            };
            let mut view = ResourceView::of(
                series(ring, now, window_of(res), capacity),
                second,
                ring.open_k().unwrap_or(0),
                ring.open_over(),
                basis,
            );
            view.long_k = ring.long_k().unwrap_or(0);
            view.state = match res {
                Cpu => cpu_state,
                Gpu => gpu_state,
                Memory => mem_state,
                Disk => disk_state,
                Processes => processes_state(facts.process_room, None),
                _ => None,
            };
            if res == Memory {
                let hour: Vec<Cell> = ring
                    .iter()
                    .map(|(k, _)| self.pressure.bucket(k).map(Agg::cell).unwrap_or_default())
                    .collect();
                view.pressure = hour.iter().map(|c| c.max.map(Pressure::of_rank)).collect();
                view.pressure_ring = hour.into();
                // The pressure ring's ten-minute cells, by key with Memory's own: the pressure
                // ring's long ring may have started later (no pressure read at first).
                let (cells, open) = (self.pressure.long_cells(), self.pressure.long_k());
                let first = view.long_k + 1 - view.long.len() as i64;
                view.pressure_long = (first..first + view.long.len() as i64)
                    .map(|k| {
                        let back = usize::try_from(open? - k).ok()?;
                        cells.get(cells.len().checked_sub(1 + back)?).copied()
                    })
                    .map(Option::unwrap_or_default)
                    .collect();
                view.pressure_recent = self.pressure.readings().into();
            }
            view.aterm = fig(Some(&self.aterm), res, tokens.agents > 0);
            view.tab = fig(None, res, false);
            view.tabs = s
                .tabs
                .iter()
                .map(|t| {
                    let agent = tokens.with_agent.contains(&t.key);
                    (t.key, fig(self.tabs.get(&t.key), res, agent))
                })
                .collect();
            if res == Cpu
                && let Some(cpu) = scoped_cpu
            {
                view.logical_cpu = cpu
                    .tabs
                    .iter()
                    .map(|tab| {
                        let keys = s
                            .tabs
                            .iter()
                            .filter(|root| root.tab == tab.key)
                            .map(|root| root.key)
                            .collect();
                        (keys, fig(self.logical_cpu.get(&tab.id), Cpu, false))
                    })
                    .collect();
            }
            if res == Gpu
                && let Some(mac) = view.now
            {
                for f in
                    std::iter::once(&mut view.aterm).chain(view.tabs.iter_mut().map(|t| &mut t.1))
                {
                    f.now = f.now.map(|v| v.min(mac));
                }
            }
            view
        });
        (resources, facts, net)
    }

    /// Whether this reading's Processes figures are starts a second (`true`) or the count against
    /// its limit: starts where the reading carries a start counter (record §8.2), and still starts
    /// for a reading that missed one in a run that read one, which reads as unread. A run that
    /// reads no counter counts, over restored starts too. Where the meaning moves, the rings that
    /// held the other are cleared, so no box mixes the two.
    fn proc_meaning(&mut self, has_counter: bool) -> bool {
        self.starts_read |= has_counter;
        let starts = has_counter || (self.starts_read && self.proc_starts == Some(true));
        if self.proc_starts.is_some_and(|was| was != starts) {
            let i = Resource::Processes.index();
            self.rings[i] = BucketRing::default();
            self.aterm.0[i] = None;
            for rings in self.tabs.values_mut() {
                rings.0[i] = None;
            }
        }
        self.proc_starts = Some(starts);
        starts
    }

    /// Takes a reading into the instruments' rings (A4 §4, §6.2): compute watts, with the peak
    /// its tiles draw against; the thermal pressure level, over while Moderate or more; what
    /// blocks the processes' flow, over while anything does; and work per joule, for its peak
    /// alone. A value its reader did not give adds no time to its ring.
    fn note_instruments(
        &mut self,
        s: &Sample,
        blocked: crate::blockage::Blocked,
        heat: &crate::heat::HeatView,
    ) {
        let t = Taken::at(s.at_unix_ms, s.period_ms());
        let closed = self.compute_w.push(t, heat.watts.compute().ok(), false);
        note_peak(&self.compute_w, closed, &mut self.peaks.compute_w);
        let level = heat.pressure.ok();
        let _ = self.thermal.push(
            t,
            level.map(|p| f64::from(p.level())),
            level.is_some_and(|p| p >= crate::heat::ThermalPressure::Moderate),
        );
        let (kind, held) = blocked.reading().unzip();
        let _ = self.blocked.push(t, kind, held.unwrap_or(false));
        let work = heat.chain.ok().and_then(|c| c.judged.ipj);
        let closed = self.work.push(t, work, false);
        note_peak(&self.work, closed, &mut self.peaks.work_per_joule);
    }

    fn absorb_links(
        &mut self,
        s: &Sample,
        stamp: Stamp,
        facts: &mut RowFacts,
    ) -> (Option<f64>, Option<f64>, Option<NetView>) {
        let links = s.links.as_deref();
        let tcp = s.tcp;
        let aterm_sockets = s.aterm_sockets;
        let tab_sockets = &s.tab_sockets;
        let srtt = s.tcp_srtt_ms.filter(|x| x.is_finite() && *x > 0.0);
        let Some(links) = links else {
            let any_net = tcp.is_some() || srtt.is_some() || aterm_sockets.is_some();
            let state = if srtt.is_some_and(|ms| ms >= 250.0) {
                NetState::Degraded
            } else if tcp.is_some_and(|t| t.established > 0) {
                NetState::Online
            } else {
                NetState::Idle
            };
            let net = any_net.then(|| NetView {
                tcp,
                srtt_ms: srtt,
                state,
                aterm_sockets,
                tab_sockets: tab_sockets.to_vec(),
                ..NetView::default()
            });
            return (None, None, net);
        };
        let dt = stamp.rate_dt(self.links_at.replace(stamp));
        let (mut rx, mut tx) = (0u64, 0u64);
        let (mut rx_pkts, mut tx_pkts) = (0u64, 0u64);
        let (mut rx_err, mut tx_err) = (0u64, 0u64);
        let (mut rx_drop, mut tx_drop) = (0u64, 0u64);
        let mut any_tx_drop = false;
        let mut measured = false;
        let mut rates_why = None;
        let mut packets_moved_without_rate = false;
        let mut any_up = false;
        let mut moved_by: Vec<(&LinkSample, u64)> = Vec::new();
        let mut link_views: Vec<LinkView> = Vec::with_capacity(links.len());
        for l in links {
            let prev = self.links.get(&l.name);
            let result = prev.ok_or(DeltaError::Warming).and_then(|before| {
                dt.ok_or(DeltaError::InvalidInterval)
                    .and_then(|d| link_rates(before, l, d))
            });
            let rates = result.as_ref().ok().map(|(rates, _)| *rates);
            let why = result.as_ref().err().copied();
            if !l.loopback && l.up {
                any_up = true;
            }
            if l.counts {
                if rates.is_none()
                    && prev.is_some_and(|before| {
                        before.index == l.index
                            && before.byte_counter == l.byte_counter
                            && l.rx_packets >= before.rx_packets
                            && l.tx_packets >= before.tx_packets
                            && (l.rx_packets > before.rx_packets
                                || l.tx_packets > before.tx_packets)
                    })
                {
                    // Packet activity is known even when byte-wrap count is not. An active
                    // link must not be labelled Idle merely because its rate is unknown.
                    packets_moved_without_rate = true;
                }
                let moved = if let Ok((_, bytes)) = result
                    && let Some(b) = prev
                {
                    let d_rx = bytes.rx;
                    let d_tx = bytes.tx;
                    rx = rx.saturating_add(d_rx);
                    tx = tx.saturating_add(d_tx);
                    rx_pkts = rx_pkts.saturating_add(l.rx_packets - b.rx_packets);
                    tx_pkts = tx_pkts.saturating_add(l.tx_packets - b.tx_packets);
                    rx_err = rx_err.saturating_add(l.rx_errors - b.rx_errors);
                    tx_err = tx_err.saturating_add(l.tx_errors - b.tx_errors);
                    rx_drop = rx_drop.saturating_add(l.rx_drops - b.rx_drops);
                    if let (Some(bt), Some(nt)) = (b.tx_drops, l.tx_drops) {
                        tx_drop = tx_drop.saturating_add(nt - bt);
                        any_tx_drop = true;
                    }
                    measured = true;
                    d_rx.saturating_add(d_tx)
                } else {
                    rates_why = rates_why.or(why);
                    0
                };
                moved_by.push((l, moved));
            }
            link_views.push(LinkView {
                sample: l.clone(),
                rates,
                rates_why: why,
            });
        }
        facts.link = self.pick_link(&moved_by, dt).cloned();
        self.links = links.iter().map(|l| (l.name.clone(), l.clone())).collect();
        let rates = dt
            .filter(|_| measured && rates_why.is_none())
            .map(|d| LinkRates {
                rx_bps: rx as f64 / d,
                tx_bps: tx as f64 / d,
                rx_pps: rx_pkts as f64 / d,
                tx_pps: tx_pkts as f64 / d,
                rx_err_ps: rx_err as f64 / d,
                tx_err_ps: tx_err as f64 / d,
                rx_drop_ps: rx_drop as f64 / d,
                tx_drop_ps: any_tx_drop.then_some(tx_drop as f64 / d),
            });
        let window_ns = rates.and(dt).map(|d| (d * 1e9).round() as u64);
        let state = if !any_up && links.iter().any(|l| !l.loopback) {
            NetState::Offline
        } else if rates.is_some_and(|r| {
            r.rx_err_ps + r.tx_err_ps + r.rx_drop_ps + r.tx_drop_ps.unwrap_or(0.0) >= 2.0
        }) || srtt.is_some_and(|ms| ms >= 250.0)
        {
            NetState::Degraded
        } else if packets_moved_without_rate
            || rates.is_some_and(|r| r.rx_bps + r.tx_bps >= 1_024.0)
            || tcp.is_some_and(|t| t.established > 0)
        {
            NetState::Online
        } else {
            NetState::Idle
        };
        let has_net =
            !link_views.is_empty() || tcp.is_some() || srtt.is_some() || aterm_sockets.is_some();
        let net = has_net.then_some(NetView {
            links: link_views,
            rates,
            rates_why,
            window_ns,
            tcp,
            srtt_ms: srtt,
            state,
            aterm_sockets,
            tab_sockets: tab_sockets.to_vec(),
        });
        (rates.map(|r| r.rx_bps), rates.map(|r| r.tx_bps), net)
    }

    fn pick_link<'a>(
        &mut self,
        moved: &[(&'a LinkSample, u64)],
        dt: Option<f64>,
    ) -> Option<&'a LinkSample> {
        let key = |&(l, m): &(&LinkSample, u64)| (m, l.rate_bps.unwrap_or(0));
        let busiest = moved
            .iter()
            .copied()
            .reduce(|best, b| if key(&b) > key(&best) { b } else { best });
        let Some(busiest) = busiest else {
            self.link_pick = None;
            self.link_lead = None;
            return None;
        };
        let held = self
            .link_pick
            .as_deref()
            .and_then(|name| moved.iter().copied().find(|(l, _)| l.name == name));
        let Some(held) = held else {
            self.link_pick = Some(busiest.0.name.clone());
            self.link_lead = None;
            return Some(busiest.0);
        };
        let leads = busiest.0.name != held.0.name
            && dt.is_some_and(|dt| busiest.1 as f64 >= LINK_LEAD_BPS * dt)
            && busiest.1 > held.1.saturating_mul(2);
        if !leads {
            self.link_lead = None;
            return Some(held.0);
        }
        let n = match &self.link_lead {
            Some((name, n)) if *name == busiest.0.name => n + 1,
            _ => 1,
        };
        if n >= LINK_LEAD_READINGS {
            self.link_pick = Some(busiest.0.name.clone());
            self.link_lead = None;
            Some(busiest.0)
        } else {
            self.link_lead = Some((busiest.0.name.clone(), n));
            Some(held.0)
        }
    }

    fn absorb_tokens(&mut self, tokens: &[TabTokens], stamp: Stamp) -> TokenPass {
        let mut pass = TokenPass::default();
        let mut seen: HashSet<&str> = HashSet::with_capacity(tokens.len());
        for t in tokens {
            if !seen.insert(t.session.as_str()) {
                continue;
            }
            pass.agents += 1;
            pass.with_agent.insert(t.key);
            let now_ms = i64::try_from(stamp.mono_ms).unwrap_or(i64::MAX);
            let read_ms = t.read_ms.unwrap_or(now_ms);
            let rate = match self.sessions.get_mut(t.session.as_str()) {
                Some(b) if stamp.rate_dt(Some(b.seen)).is_some() => {
                    if read_ms == b.read_ms && b.same_fold(t) {
                        b.seen = stamp;
                        b.held
                            .filter(|_| now_ms.saturating_sub(read_ms) <= TOKENS_HOLD_MS)
                    } else {
                        let rate = b.rates_to(t, read_ms);
                        *b = Session::of(t, stamp, read_ms, rate);
                        rate
                    }
                }
                Some(b) => {
                    let baseline = read_ms != b.read_ms;
                    *b = Session::of(t, stamp, read_ms, None);
                    b.baseline = baseline;
                    None
                }
                None => {
                    self.sessions
                        .insert(t.session.clone(), Session::of(t, stamp, read_ms, None));
                    None
                }
            };
            if let Some((out, input)) = rate {
                pass.out = Some(pass.out.unwrap_or(0.0) + out);
                pass.input = Some(pass.input.unwrap_or(0.0) + input);
                *pass.tabs.entry(t.key).or_default() += out;
            }
        }
        self.sessions.retain(|k, _| seen.contains(k.as_str()));
        pass
    }
}

fn window_of(res: Resource) -> i64 {
    if res == Resource::Tokens {
        TOKENS_WINDOW_MS
    } else {
        NOW_WINDOW_MS
    }
}

fn link_rates(
    before: &LinkSample,
    now: &LinkSample,
    dt: f64,
) -> Result<(LinkRates, ByteDelta), DeltaError> {
    if !dt.is_finite() || dt <= 0.0 || dt > 90.0 {
        return Err(DeltaError::InvalidInterval);
    }
    if before.up != now.up || before.counts != now.counts {
        return Err(DeltaError::SourceChanged);
    }
    let bytes = byte_delta(
        before.byte_sample(),
        now.byte_sample(),
        (dt * 1e9).round() as u64,
    )?;
    let subtract = |now: u64, before: u64| now.checked_sub(before).ok_or(DeltaError::Discontinuity);
    let d_rx_pkts = subtract(now.rx_packets, before.rx_packets)?;
    let d_tx_pkts = subtract(now.tx_packets, before.tx_packets)?;
    let d_rx_err = subtract(now.rx_errors, before.rx_errors)?;
    let d_tx_err = subtract(now.tx_errors, before.tx_errors)?;
    let d_rx_drop = subtract(now.rx_drops, before.rx_drops)?;
    let tx_drop_ps = match (before.tx_drops, now.tx_drops) {
        (Some(b), Some(n)) => Some(subtract(n, b)? as f64 / dt),
        _ => None,
    };
    Ok((
        LinkRates {
            rx_bps: bytes.rx as f64 / dt,
            tx_bps: bytes.tx as f64 / dt,
            rx_pps: d_rx_pkts as f64 / dt,
            tx_pps: d_tx_pkts as f64 / dt,
            rx_err_ps: d_rx_err as f64 / dt,
            tx_err_ps: d_tx_err as f64 / dt,
            rx_drop_ps: d_rx_drop as f64 / dt,
            tx_drop_ps,
        },
        bytes,
    ))
}

fn series(ring: &BucketRing, now: i64, window_ms: i64, capacity: Option<f64>) -> Series {
    Series {
        now: ring.now_mean(now, window_ms),
        capacity,
        ring: ring.cells().into(),
        long: ring.long_cells().into(),
        recent: ring.readings().into(),
        binds: false,
    }
}

/// Notes in `peak` the sustained-30 s value ending at bucket `closed` of `ring`.
pub(crate) fn note_peak(ring: &BucketRing, closed: Option<i64>, peak: &mut DailyMax) {
    if let Some(k) = closed
        && let Some(v) = ring.sustained(k)
    {
        peak.note_bucket(day_of(k * BUCKET_MS), v);
    }
}

fn net_capacity(
    peaks: &Peaks,
    today: u32,
    link: Option<f64>,
) -> (CapBasis, Option<f64>, Option<f64>) {
    let clamp = |p: f64| link.map_or(p, |l| p.min(l));
    let learned = peaks.net_rx.learned(NET_LEARN_S) && peaks.net_tx.learned(NET_LEARN_S);
    match (peaks.net_rx.peak(today), peaks.net_tx.peak(today)) {
        (Some(rx), Some(tx)) if learned => (CapBasis::Peak, Some(clamp(rx)), Some(clamp(tx))),
        _ if link.is_some() => (CapBasis::Link, link, link),
        _ if learned => (CapBasis::Peak, None, None),
        _ => (CapBasis::Learning, None, None),
    }
}

fn disk_state(facts: Option<&DiskFacts>, filling: Option<&Filling>) -> Option<State> {
    let mut states = Vec::new();
    if let Some(f) = facts.filter(|f| f.watch) {
        states.extend(f.level.and_then(DiskLevel::state));
        if f.stuck > 0 {
            states.push(State::Critical);
        }
        if f.stalled_since.is_some() {
            states.push(State::Warn);
        }
        if f.others.iter().any(|(_, level, _)| *level != DiskLevel::Ok) {
            states.push(State::Warn);
        }
    }
    if filling.is_some_and(Filling::outranks_level) {
        states.push(State::Warn);
    }
    states.into_iter().max()
}

/// The UTC Unix day of `unix_ms`.
pub(crate) fn day_of(unix_ms: i64) -> u32 {
    u32::try_from(unix_ms.div_euclid(86_400_000)).unwrap_or(0)
}

fn interval_s(before: Option<u64>, now: u64) -> f32 {
    before
        .filter(|&then| now > then)
        .map_or(1.0, |then| (now - then) as f32 / 1e9)
}

fn front_names(procs: &[ProcSample], tabs: &[TabRoot]) -> Vec<String> {
    tabs.iter()
        .map(|t| {
            if t.shell_pid == 0 {
                return String::new();
            }
            procs
                .iter()
                .filter(|p| p.ppid == t.shell_pid)
                .max_by_key(|p| p.footprint.unwrap_or(0))
                .or_else(|| procs.iter().find(|p| p.pid == t.shell_pid))
                .map(|p| p.name.clone())
                .unwrap_or_default()
        })
        .collect()
}

/// Which scopes one process belongs to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Membership {
    /// In aterm's scope.
    pub aterm: bool,
    /// Shared app work outside this instance and its tab trees (also in `aterm`).
    pub shared: bool,
    /// In this tab (an index into the sample's tabs).
    pub tab: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Origin {
    tab: Option<u64>,
    shared: bool,
    // Additional in-memory metadata on the existing custody inventory, not part of the
    // bounded GroupBook budget or the public handoff. It never authorizes ownership.
    group: Option<crate::groups::GroupKey>,
}

impl Origin {
    fn same_custody(self, other: Self) -> bool {
        self.tab == other.tab && self.shared == other.shared
    }
}

/// The largest natural-origin inventory transferred through one private GUI handoff.
pub const PROCESS_ORIGIN_CAP: usize = 65_536;

/// Natural app custody previously established from a process tree or app-service proof.
/// The boot containing `process` is supplied and checked by the private handoff owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessOrigin {
    /// Kernel process generation; never a PID alone.
    pub process: crate::delegated::ProcessKey,
    /// The source's local pane key, translated through a stable session ID by the host.
    pub tab: Option<u64>,
    /// App-service custody outside a tab or this GUI's own process tree.
    pub shared: bool,
}

impl Vitals {
    /// Snapshot natural origins, plus whether the bounded inventory omitted any proof.
    /// Explicit delegated claims are excluded and remain in their authenticated ledger.
    #[must_use]
    pub fn process_origins(&self) -> (Vec<ProcessOrigin>, bool) {
        let mut origins: Vec<_> = self
            .origins
            .iter()
            .map(|(&process, origin)| ProcessOrigin {
                process,
                tab: origin.tab,
                shared: origin.shared,
            })
            .collect();
        let truncated = origins.len() > PROCESS_ORIGIN_CAP;
        if truncated {
            origins.select_nth_unstable_by_key(PROCESS_ORIGIN_CAP, |origin| origin.process);
        }
        origins.truncate(PROCESS_ORIGIN_CAP);
        (origins, truncated || self.origins_incomplete)
    }

    /// Restore dormant same-boot origin proofs. Only a matching process generation in a later
    /// sweep can use one; absent rows in a truncated sweep do not discard the proof.
    pub fn restore_process_origins(&mut self, origins: &[ProcessOrigin]) {
        self.origins_incomplete |= origins.len() > PROCESS_ORIGIN_CAP;
        let mut rejected = IdSet::default();
        for origin in origins.iter().take(PROCESS_ORIGIN_CAP) {
            if rejected.contains(&origin.process) {
                continue;
            }
            if origin.process.pid > 1 && !(origin.shared && origin.tab.is_some()) {
                let claim = Origin {
                    tab: origin.tab,
                    shared: origin.shared,
                    group: None,
                };
                if self
                    .origins
                    .get(&origin.process)
                    .is_some_and(|previous| !previous.same_custody(claim))
                {
                    self.origins.remove(&origin.process);
                    rejected.insert(origin.process);
                    self.origins_incomplete = true;
                } else {
                    self.origins.entry(origin.process).or_insert(claim);
                }
            } else {
                self.origins_incomplete = true;
                self.origins.remove(&origin.process);
                rejected.insert(origin.process);
            }
        }
    }

    fn attribute_owned(
        &mut self,
        s: &Sample,
        procs: &[ProcSample],
    ) -> (Vec<Membership>, Vec<Option<usize>>) {
        let current = attribute(procs, s.self_pid, &s.tabs);
        let mut roots = IdMap::default();
        let mut native = IdMap::default();
        for root in &s.native_roots {
            native
                .entry(root.process)
                .and_modify(|tab| {
                    if *tab != Some(root.tab) {
                        *tab = None;
                    }
                })
                .or_insert(Some(root.tab));
        }
        for (process, member) in procs.iter().zip(&current) {
            let natural = process
                .start
                .and_then(|start| {
                    native.get(&crate::delegated::ProcessKey {
                        pid: process.pid,
                        start,
                    })
                })
                .copied()
                .flatten();
            if let Some(tab) = natural {
                roots.insert(
                    process.pid,
                    Membership {
                        aterm: true,
                        shared: false,
                        tab: tab.and_then(|key| s.tabs.iter().position(|tab| tab.key == key)),
                    },
                );
                continue;
            }
            if member.aterm {
                continue;
            }
            // A proved tab origin survives setsid/reparenting. Merely loading this app's
            // executable does not turn a tab's orphaned helper into shared app overhead.
            let origin = process
                .start
                .and_then(|start| {
                    self.origins.get(&crate::delegated::ProcessKey {
                        pid: process.pid,
                        start,
                    })
                })
                .copied()
                .or_else(|| {
                    s.shared_roots
                        .iter()
                        .any(|root| root.matches(process))
                        .then_some(Origin {
                            tab: None,
                            shared: true,
                            group: None,
                        })
                });
            if let Some(origin) = origin {
                roots.insert(
                    process.pid,
                    Membership {
                        aterm: true,
                        shared: origin.shared,
                        tab: origin
                            .tab
                            .and_then(|key| s.tabs.iter().position(|tab| tab.key == key)),
                    },
                );
            }
        }
        let membership = attribute_roots(procs, s.self_pid, &s.tabs, roots);
        let origins = membership.clone();
        let (membership, owners) = attribute_delegated(procs, &s.tabs, &s.delegated, membership);
        if !s.procs_truncated {
            let present: IdMap<_, _> = procs.iter().map(|p| (p.pid, p.start)).collect();
            self.origins.retain(|key, _| {
                present
                    .get(&key.pid)
                    .is_some_and(|start| start.is_none() || *start == Some(key.start))
            });
        }
        // Explicit delegated custody belongs to its own ledger. Remembering those claims here
        // would keep charging a foreign process after that job deliberately let it go.
        for (process, member) in procs.iter().zip(&origins) {
            if member.aterm
                && let Some(start) = process.start
            {
                let key = crate::delegated::ProcessKey {
                    pid: process.pid,
                    start,
                };
                let mut origin = Origin {
                    tab: member.tab.map(|index| s.tabs[index].key),
                    shared: member.shared,
                    group: None,
                };
                if let Some(before) = self
                    .origins
                    .get(&key)
                    .filter(|before| before.same_custody(origin))
                {
                    origin.group = before.group;
                }
                self.origins.insert(key, origin);
            }
        }
        (membership, owners)
    }

    /// Resolve shell-child anchors over this exact attributed census. The existing
    /// birth-bound Origin retains an established anchor across exec and reparenting.
    fn process_groups(
        &mut self,
        s: &Sample,
        procs: &[ProcSample],
        membership: &[Membership],
        owners: &[Option<usize>],
    ) -> Vec<Option<crate::groups::GroupKey>> {
        use crate::delegated::ProcessKey;
        use crate::groups::GroupKey;
        let key = |p: &ProcSample| {
            Some(ProcessKey {
                pid: p.pid,
                start: p.start?,
            })
        };
        let mut by_pid = IdMap::with_capacity_and_hasher(procs.len(), Default::default());
        for (index, p) in procs.iter().enumerate() {
            by_pid
                .entry(p.pid)
                .and_modify(|entry| *entry = None)
                .or_insert(Some(index));
        }
        let mut resolved = vec![false; procs.len()];
        let mut groups = vec![None; procs.len()];
        let coalitions: IdSet<_> = s
            .coalitions
            .as_ref()
            .into_iter()
            .flat_map(|read| read.rows.iter().map(|row| row.id))
            .collect();
        for (index, (p, m)) in procs.iter().zip(membership).enumerate() {
            if !m.aterm || by_pid.get(&p.pid) != Some(&Some(index)) {
                if !m.aterm && by_pid.get(&p.pid) == Some(&Some(index)) {
                    groups[index] = p
                        .coalition
                        .filter(|id| coalitions.contains(id))
                        .map(GroupKey::Coalition);
                }
                resolved[index] = true;
                continue;
            }
            if let Some(job) = owners[index] {
                let root = s.delegated[job].root;
                groups[index] = Some(m.tab.map_or(GroupKey::Aterm(root), |tab| GroupKey::Tab {
                    pane: s.tabs[tab].key,
                    root,
                }));
                resolved[index] = true;
                continue;
            }
            if let Some(origin) = key(p)
                .and_then(|key| self.origins.get(&key))
                .filter(|origin| {
                    origin.tab == m.tab.map(|i| s.tabs[i].key) && origin.shared == m.shared
                })
                && origin.group.is_some()
            {
                groups[index] = origin.group;
                resolved[index] = true;
            }
        }
        for index in 0..procs.len() {
            if resolved[index] {
                continue;
            }
            let mut path = Vec::new();
            let mut visiting = HashSet::new();
            let mut current = index;
            let group = loop {
                if resolved[current] {
                    break groups[current];
                }
                if !visiting.insert(current) {
                    break None;
                }
                path.push(current);
                let p = &procs[current];
                let m = membership[current];
                let Some(own) = key(p) else {
                    break None;
                };
                if let Some(tab) = m.tab {
                    let root = &s.tabs[tab];
                    if p.pid == root.shell_pid || p.ppid == root.shell_pid {
                        break Some(GroupKey::Tab {
                            pane: root.key,
                            root: own,
                        });
                    }
                } else if p.pid == s.self_pid {
                    break Some(GroupKey::Aterm(own));
                }
                let parent = by_pid.get(&p.ppid).copied().flatten().filter(|&parent| {
                    membership[parent] == m
                        && procs[parent].start.is_some_and(|start| start <= own.start)
                });
                if let Some(parent) = parent {
                    current = parent;
                } else {
                    break if m.tab.is_some() {
                        None
                    } else if m.shared {
                        Some(GroupKey::Shared(own))
                    } else {
                        Some(GroupKey::Aterm(own))
                    };
                }
            };
            for index in path {
                groups[index] = group;
                resolved[index] = true;
            }
        }
        for (index, ((p, m), &group)) in procs.iter().zip(membership).zip(&groups).enumerate() {
            // Explicit jobs supply a current group through their own existing ledger. Do not
            // remember that claim as a natural origin after the job releases it.
            if owners[index].is_some() {
                continue;
            }
            if let Some(origin) =
                key(p)
                    .and_then(|key| self.origins.get_mut(&key))
                    .filter(|origin| {
                        origin.tab == m.tab.map(|i| s.tabs[i].key) && origin.shared == m.shared
                    })
            {
                origin.group = group;
            }
        }
        groups
    }
}

/// Attribute each process in `procs` to `aterm` (`self_pid` tree + tab trees) and its nearest tab.
#[must_use]
pub fn attribute(procs: &[ProcSample], self_pid: u32, tabs: &[TabRoot]) -> Vec<Membership> {
    attribute_roots(procs, self_pid, tabs, IdMap::default())
}

fn attribute_roots(
    procs: &[ProcSample],
    self_pid: u32,
    tabs: &[TabRoot],
    mut memo: IdMap<u32, Membership>,
) -> Vec<Membership> {
    let starts: IdMap<_, _> = procs.iter().map(|p| (p.pid, p.start)).collect();
    let parent: IdMap<u32, u32> = procs
        .iter()
        .map(|p| {
            // A sweep is not atomic: an old parent may exit and its PID name a newer process by
            // the time that row is read. Such a row cannot be this child's parent.
            let reused = p
                .start
                .zip(starts.get(&p.ppid).copied().flatten())
                .is_some_and(|(child, parent)| parent > child);
            (p.pid, if reused { 1 } else { p.ppid })
        })
        .collect();
    let shells: IdMap<u32, usize> = tabs
        .iter()
        .enumerate()
        .filter(|(_, t)| t.shell_pid != 0)
        .map(|(i, t)| (t.shell_pid, i))
        .collect();
    // Current lineage overrides remembered origins, including a process moved to a new tab.
    memo.remove(&self_pid);
    for shell in shells.keys() {
        memo.remove(shell);
    }
    procs
        .iter()
        .map(|p| resolve(p.pid, self_pid, &parent, &shells, &mut memo))
        .collect()
}

fn attribute_delegated(
    procs: &[ProcSample],
    tabs: &[TabRoot],
    jobs: &[DelegatedJob],
    mut membership: Vec<Membership>,
) -> (Vec<Membership>, Vec<Option<usize>>) {
    let mut claims = IdMap::default();
    for (index, job) in jobs.iter().enumerate() {
        for &key in &job.members {
            claims
                .entry(key)
                .and_modify(|claim| *claim = None)
                .or_insert(Some(index));
        }
    }
    let owners: Vec<_> = procs
        .iter()
        .enumerate()
        .map(|(index, process)| {
            let key = crate::delegated::ProcessKey {
                pid: process.pid,
                start: process.start?,
            };
            let job = claims.get(&key).copied().flatten()?;
            membership[index] = Membership {
                aterm: true,
                shared: false,
                tab: jobs[job]
                    .tab
                    .and_then(|key| tabs.iter().position(|tab| tab.key == key)),
            };
            Some(job)
        })
        .collect();
    (membership, owners)
}

fn resolve(
    pid: u32,
    self_pid: u32,
    parent: &IdMap<u32, u32>,
    shells: &IdMap<u32, usize>,
    memo: &mut IdMap<u32, Membership>,
) -> Membership {
    let mut path = Vec::new();
    let mut cur = pid;
    let decided = loop {
        if let Some(m) = memo.get(&cur) {
            break *m;
        }
        let here = Membership {
            aterm: cur == self_pid,
            shared: false,
            tab: shells.get(&cur).copied(),
        };
        if here.aterm || here.tab.is_some() {
            path.push(cur);
            break Membership::default();
        }
        path.push(cur);
        match parent.get(&cur) {
            Some(&pp) if pp != cur && pp > 1 && path.len() < 4096 && !path.contains(&pp) => {
                cur = pp;
            }
            _ => break Membership::default(),
        }
    };
    let mut m = decided;
    for &p in path.iter().rev() {
        if p == self_pid {
            m.aterm = true;
        }
        if let Some(&t) = shells.get(&p) {
            m.aterm = true;
            if m.tab.is_none() {
                m.tab = Some(t);
            }
        }
        memo.insert(p, m);
    }
    memo.get(&pid).copied().unwrap_or_default()
}

/// The Super (performance) cores' indices: the fastest cluster's, where the machine names more than
/// one kind of core; `None` with one kind, whose Super time no core count checks.
fn super_cores(clusters: &[ClusterView]) -> Option<&[usize]> {
    let kinds = clusters.iter().filter(|c| c.name != "CPU").count();
    (kinds > 1).then(|| clusters[0].cores.as_slice())
}

fn cluster_views(specs: &[ClusterSpec], n: usize) -> Vec<ClusterView> {
    let mut seen = vec![false; n];
    let mut out = Vec::new();
    for c in specs {
        let mut cores = Vec::with_capacity(c.logical.len());
        for i in c.logical.iter().map(|&i| usize::from(i)) {
            if i < n && !seen[i] {
                seen[i] = true;
                cores.push(i);
            }
        }
        if cores.is_empty() {
            continue;
        }
        out.push(ClusterView {
            short: short_name(&c.name),
            name: c.name.clone(),
            cores,
        });
    }
    let rest: Vec<usize> = (0..n).filter(|&i| !seen[i]).collect();
    if !rest.is_empty() {
        out.push(ClusterView {
            name: "CPU".into(),
            short: "CPU".into(),
            cores: rest,
        });
    }
    out
}

fn short_name(name: &str) -> String {
    match name.chars().next() {
        Some(c) if name != "CPU" => {
            let digits = name.len() - name.trim_end_matches(|d: char| d.is_ascii_digit()).len();
            let mut short: String = c.to_uppercase().collect();
            if digits < name.len() {
                short.push_str(&name[name.len() - digits..]);
            }
            short
        }
        _ => "CPU".into(),
    }
}

/// Bytes as `f32` GiB.
#[must_use]
pub fn gib(bytes: u64) -> f32 {
    (bytes as f64 / f64::from(1u32 << 30)) as f32
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
