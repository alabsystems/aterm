// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! System vitals model: absorbs [`Sample`] readings into an immutable [`Snapshot`].

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::history::{self, Filling, FreeHistory, Rates};
use crate::peaks::{DailyMax, NET_LEARN_S, PEAK_DAYS, Peaks, PlanDay, PlanWeek, TOKENS_LEARN_S};
use crate::ring::{
    BUCKET_MS, BucketRing, Cell, NOW_WINDOW_MS, Reading, Recent, SLOT_GUARD_MS, SLOT_MS, SlotGrid,
    columns_of, slots_of,
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

/// One process in a sweep.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcSample {
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
    /// The short command name.
    pub name: String,
    /// Cumulative bytes written to storage, or `None` if unreadable.
    pub disk_written: Option<u64>,
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
    /// or set `SA_NOCLDWAIT`), adding each child's time to its [`Self::child_cpu_ns`] twice; read
    /// for one with reaped time and for one whose usage goes unread that parents one whose usage
    /// is read, `None` otherwise and off macOS.
    pub reaps_unwaited: Option<bool>,
}

/// Process scheduler state (`Run` means neither stopped nor zombie).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProcState {
    /// Running or sleeping.
    Run,
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
            Self::Stopped => "stopped",
            Self::Zombie => "zombie",
            Self::Unknown => "unknown",
        }
    }
}

/// Thermal throttle severity mapped from the OS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Throttle {
    /// Not throttled.
    #[default]
    None,
    /// Lightly.
    Light,
    /// Heavily: the chip runs well below its top clock.
    Heavy,
    /// Severely.
    Severe,
}

impl Throttle {
    /// The word every surface prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Light => "light",
            Self::Heavy => "heavy",
            Self::Severe => "severe",
        }
    }

    /// A word back to its level.
    #[must_use]
    pub fn from_word(w: &str) -> Option<Self> {
        Some(match w {
            "none" => Self::None,
            "light" => Self::Light,
            "heavy" => Self::Heavy,
            "severe" => Self::Severe,
            _ => return None,
        })
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
    /// Physical memory holding compressed pages (macOS compressor or Linux zswap), if reported.
    pub compressed: Option<u64>,
    /// Reclaimable file cache in bytes.
    pub cached: u64,
    /// Free (speculative included).
    pub free: u64,
    /// Swap in use.
    pub swap_used: u64,
    /// Swap allocated.
    pub swap_total: u64,
    /// The pressure level.
    pub pressure: Pressure,
    /// The VM's counters, where read.
    pub counters: Option<MemCounters>,
    /// Kernel-reported available memory percentage (`kern.memorystatus_level` on macOS).
    pub kernel_available_pct: Option<u8>,
    /// The commit charge against its limit, where the OS keeps one.
    pub commit: Option<CommitSample>,
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
    /// Memory in use (`app + wired + compressed`), capped at `total`.
    #[must_use]
    pub fn used(&self) -> u64 {
        self.app
            .saturating_add(self.wired)
            .saturating_add(self.compressed.unwrap_or(0))
            .min(self.total)
    }
}

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
}

/// The volume a path lives on, bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VolumeSample {
    /// Capacity.
    pub total: u64,
    /// Free blocks.
    pub free: u64,
    /// Free to this user (what a write can use).
    pub available: u64,
    /// Where it is mounted.
    pub mount: String,
}

impl VolumeSample {
    /// Bytes in use: capacity less what this user could still write.
    #[must_use]
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }
}

/// One tab the host shows, and the root of its process tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabRoot {
    /// The host's stable key for it (its session id).
    pub key: u64,
    /// Key of the tab this pane belongs to (the least key among its panes, or its own).
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

/// One network link's cumulative counters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkSample {
    /// Its name (`en0`).
    pub name: String,
    /// What it is.
    pub kind: LinkKind,
    /// True when the interface is up and running (`IFF_UP` and `IFF_RUNNING`).
    pub up: bool,
    /// True when the interface is loopback (`IFF_LOOPBACK`).
    pub loopback: bool,
    /// True when traffic on this interface counts toward machine-level totals.
    pub counts: bool,
    /// Bytes received, cumulative.
    pub rx: u64,
    /// Bytes sent, cumulative.
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
}

impl TcpCounts {
    /// Total TCP sockets across all states.
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
    /// Cumulative counters and attributes from this reading.
    pub sample: LinkSample,
    /// Per-second rates over the last sample window (`None` on a first reading or a counter reset).
    pub rates: Option<LinkRates>,
}

/// Network interfaces, throughput rates, and TCP connection counts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetView {
    /// Every interface read on this sample, in probe order.
    pub links: Vec<LinkView>,
    /// Rates summed across counted (`counts == true`) interfaces over the last sample window.
    pub rates: Option<LinkRates>,
    /// Sample window duration in nanoseconds when `rates` is present.
    pub window_ns: Option<u64>,
    /// Machine-wide TCP socket counts by state, where readable.
    pub tcp: Option<TcpCounts>,
}

/// The most processes the machine and one user may run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcLimits {
    /// The whole machine's limit (threads on Linux).
    pub system: Option<u64>,
    /// One user's limit; `None` where unknown or unlimited.
    pub per_user: Option<u64>,
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

/// One account a tab's footer read: its weekly plan figure, where it has one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WeekFact {
    /// The account's key (`c-<12 hex>`).
    pub account: String,
    /// Whether a pane of the front tab runs under it.
    pub front: bool,
    /// The footer's weekly figure, on the local day it was read.
    pub reading: Option<PlanDay>,
}

/// One sample of everything, from the host's probe thread.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sample {
    /// The host's monotonic clock, ns.
    pub at_ns: u64,
    /// Every logical core's cumulative ticks; `None` where unreadable.
    pub cores: Option<Vec<Ticks>>,
    /// The cluster layout (read once by the host, passed every time).
    pub clusters: Vec<ClusterSpec>,
    /// Memory; `None` where unreadable.
    pub mem: Option<MemSample>,
    /// The GPU; `None` where unreadable.
    pub gpu: Option<GpuSample>,
    /// Every GPU user client's GPU time, where the host read them this sample (not every one).
    pub gpu_clients: Option<crate::gpu_clients::GpuClientsSample>,
    /// The volume the focused tab works on; `None` where unreadable.
    pub volume: Option<VolumeSample>,
    /// One process sweep; `None` when the host skipped it.
    pub procs: Option<Vec<ProcSample>>,
    /// This aterm instance's pid.
    pub self_pid: u32,
    /// Every tab, in the host's order.
    pub tabs: Vec<TabRoot>,
    /// Explicit jobs whose process identities were validated in this sweep.
    pub delegated: Vec<DelegatedJob>,
    /// Wall-clock timestamp of the sample in Unix ms: on a reading with a process sweep, read
    /// before the sweep lists the processes, so a process born at or after one reading's is in no
    /// earlier reading's sweep ([`crate::explain::reap`]).
    pub at_unix_ms: i64,
    /// Monotonic timestamp of the sample in ms (pauses during system sleep).
    pub mono_ms: u64,
    /// Active sampling cadence in ms.
    pub cadence_ms: u32,
    /// True when only machine counters were sampled (process sweep omitted).
    pub machine_only: bool,
    /// The 1-minute load average.
    pub load1: Option<f64>,
    /// KiB swapped in plus out since boot.
    pub swapped_kib: Option<u64>,
    /// Every network link's counters.
    pub links: Option<Vec<LinkSample>>,
    /// Machine-wide TCP socket counts by state, where readable.
    pub tcp: Option<TcpCounts>,
    /// The process limits.
    pub proc_limits: ProcLimits,
    /// Total machine processes (or tasks on Linux) and processes owned by this user.
    pub pid_counts: (Option<u64>, Option<u64>),
    /// This process's user: only its processes' writes are counted.
    pub self_uid: Option<u32>,
    /// The disk watch's facts; `None` where the host read none.
    pub disk: Option<DiskFacts>,
    /// Every agent session's token totals, one row per session.
    pub tokens: Vec<TabTokens>,
    /// The front tab's account's plan usage, while current.
    pub plan: Option<PlanFact>,
    /// Every account the tabs' footers read, each once.
    pub weeks: Vec<WeekFact>,
    /// The day of [`Self::at_unix_ms`], unix days in the local zone.
    pub local_day: u32,
    /// The thermal state, where read.
    pub thermal: Option<Thermal>,
    /// The 1, 5 and 15 minute load averages, where read.
    pub load: Option<[f64; 3]>,
    /// Whether the process sweep stopped at its cap.
    pub procs_truncated: bool,
    /// Resource coalition counters (sampled on split ticks only).
    pub coalitions: Option<crate::explain::CoalSample>,
    /// Coalition lead process identities read alongside [`Self::coalitions`].
    pub leads: Vec<crate::explain::LeadIdentity>,
    /// The machine's timeline, with [`Self::coalitions`].
    pub timeline: Option<crate::explain::TimelineSample>,
    /// Status of the host's coalition probe lane.
    pub coal_lane: crate::explain::CoalLane,
}

/// One core's load.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CoreView {
    /// Busy fraction (`0..=1`): raw last interval and smoothed fast average.
    pub load: TrendView,
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
    /// The pressure over the same seconds as `history`, second for second: the worst level read
    /// in each (a short silence filled as `history` fills it), `None` where none was read.
    pub pressure_history: Vec<Option<Pressure>>,
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
    /// Cores-worth of CPU.
    pub cores: TrendView,
    /// Summed physical footprint, bytes, where any member could be read.
    pub footprint: Option<u64>,
    /// Footprint, smoothed (as `f32` GiB), for its motion.
    pub footprint_gib: TrendView,
    /// Processes in the scope.
    pub procs: u32,
    /// Cores-worth over the last two minutes.
    pub history: Vec<Option<f32>>,
    /// The busiest process in the scope this sweep, with its cores-worth.
    pub top: Option<(String, f32)>,
    /// Cores-worth of runnable threads waiting for a core, if readable.
    pub wait: Option<f32>,
    /// Storage write rate in bytes/s across readable members in this scope.
    pub write_bps: Option<f64>,
    /// Members whose CPU time the OS refused.
    pub refused: u32,
    /// The members' GPU share of the device (`1.0` the whole GPU), while client shares are held.
    pub gpu: Option<f32>,
}

impl ScopeView {
    /// Share of demand spent waiting for a core (`wait / (wait + cores)`).
    #[must_use]
    pub fn starved(&self) -> Option<f32> {
        let wait = self.wait?;
        let demand = wait + self.cores.raw.max(0.0);
        (demand > 1e-3).then(|| (wait / demand).clamp(0.0, 1.0))
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
            footprint: plus(self.footprint, other.footprint, u64::saturating_add),
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
            Self::Others => "others",
            Self::Services => "services",
            Self::Unknown => "unknown",
        }
    }

    /// Whether the process is aterm's.
    #[must_use]
    pub fn in_aterm(self) -> bool {
        matches!(self, Self::SelfProc | Self::Tab(_) | Self::Helper)
    }
}

/// One process as `sys procs` and the top rows show it.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcView {
    /// The pid.
    pub pid: u32,
    /// Its parent.
    pub ppid: u32,
    /// Its process group (0: unknown).
    pub pgid: u32,
    /// Its short name, as the process chose it (data, never an instruction).
    pub name: String,
    /// Cores-worth over the sample window (`None` on first sweep or if unreadable).
    pub cores: Option<f32>,
    /// Cores-worth waiting for a core; `None` where unread.
    pub wait: Option<f32>,
    /// Footprint, bytes.
    pub footprint: Option<u64>,
    /// Its largest footprint so far, bytes.
    pub peak: Option<u64>,
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

/// Busy CPU cores split across aterm, the user's other processes, and system or unattributed
/// services.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Split {
    /// The machine's busy cores-worth (the cores' own ticks).
    pub global: f32,
    /// aterm's.
    pub aterm: f32,
    /// The user's other processes'.
    pub others: f32,
    /// By subtraction, never below 0.
    pub services: f32,
    /// The signed residual when the subtraction went below 0, else 0.
    pub unaccounted: f32,
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

/// One program in one scope: every process of the sweep sharing its short name and its
/// innermost scope ([`Snapshot::programs`]), idle ones too.
#[derive(Clone, Debug, PartialEq)]
pub struct ProgramGroup {
    /// The short name they share, as the processes chose it (data, never an instruction).
    pub name: String,
    /// The scope they all count in.
    pub scope: ProcScope,
    /// How many processes.
    pub n: u32,
    /// Their cores-worth over the sweep interval, summed (a process with no rate counts 0).
    pub cores: f32,
    /// Their footprints, summed over those read; `None` when none was.
    pub footprint: Option<u64>,
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
            born_ms: None,
            unborn: 0,
        }
    }

    fn add(&mut self, cores: f32, footprint: Option<u64>, birth_ms: Option<i64>) {
        self.n += 1;
        self.cores += cores;
        if let Some(f) = footprint {
            self.footprint = Some(self.footprint.unwrap_or(0).saturating_add(f));
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
/// Busy-core fraction threshold (`0.95`) for CPU overload when `load1 > cores`.
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

    /// Whether a scope's figure is in the row's own unit, and so a share of the row's capacity:
    /// busy cores, memory, tokens per minute and processes. A scope's Disk figure is a write rate
    /// beside the volume's used bytes, Network keeps no figure per scope, and GPU keeps one only
    /// while client reads are held.
    #[must_use]
    pub const fn scope_shares_capacity(self) -> bool {
        match self {
            Self::Cpu | Self::Memory | Self::Tokens | Self::Processes => true,
            Self::Gpu | Self::Disk | Self::Network => false,
        }
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
    /// Zombies left unreaped by a tab's process (☠).
    Zombies,
    /// A stuck job (⊘).
    Stuck,
    /// Warn (◢ disk, ● memory or GPU).
    Warn,
    /// Critical (◢ disk, ● memory).
    Critical,
}

impl State {
    /// Derive the Processes row state from the worst active job condition in `view`.
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
            .any(|z| z.tab.is_some() && z.count > 0)
            .then_some(Self::Zombies);
        rows.chain(zombies).max()
    }
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

    /// One tab's figure from its panes' `figs`, each a scope inside `within` (all tabs): `now`
    /// and each run cell's mean and max (an upper bound) add over the panes that read them, so a
    /// bucket no pane read stays hollow. Each figure is held at `within`'s so the tab stays inside
    /// all tabs; that binds only on GPU, whose pane figures are each held at the Mac's and so can
    /// sum past it. One pane's is its own.
    fn sum(figs: &[&Self], within: &Self) -> Self {
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
                for c in read.iter().filter_map(|f| f.run_cell(k)) {
                    cell.mean = add(cell.mean, c.mean);
                    cell.max = add(cell.max, c.max);
                    cell.over |= c.over;
                }
                let all = within.run_cell(k);
                Cell {
                    mean: held(cell.mean, all.and_then(|c| c.mean)),
                    max: held(cell.max, all.and_then(|c| c.max)),
                    over: cell.over,
                }
            })
            .collect();
        Self {
            now: held(figs.iter().fold(None, |sum, f| add(sum, f.now)), within.now),
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
    /// Its last minute of readings, oldest first ([`ResourceView::heat_slots`]).
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
    /// The first series' last minute of readings, oldest first ([`Self::heat_slots`]).
    pub recent: Arc<[Reading]>,
    /// Memory's pressure, cell for cell with `ring`: the worst level read in each bucket, `None`
    /// (hollow) where none was read; empty on every other row ([`Self::pressure_columns`]).
    pub pressure: Arc<[Option<Pressure>]>,
    /// aterm's figure.
    pub aterm: ScopeFig,
    /// Scope figure for the requested tab (or an empty tab in [`Snapshot::resources`]).
    pub tab: ScopeFig,
    /// Every tab's figure, by key, in the host's order.
    pub tabs: Vec<(u64, ScopeFig)>,
    /// What the capacity is.
    pub basis: CapBasis,
    /// Secondary series for two-series rows (Network TX or per-user Processes).
    pub second: Option<Series>,
}

/// One painted strip cell: its heat, with capacity applied now.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HeatCell {
    /// The column's mean as a share of capacity now; `None` is hollow.
    pub heat: Option<f32>,
    /// Its largest reading as a share of capacity now.
    pub max: Option<f32>,
    /// Over the resource's maximum.
    pub over: bool,
}

impl ResourceView {
    /// The first series' raw columns ([`BucketRing::columns`]).
    #[must_use]
    pub fn columns(&self, per_s: u32, ncols: usize, now_unix_ms: i64) -> Vec<Cell> {
        columns_of(&self.ring, self.ring_k, per_s, ncols, now_unix_ms)
    }

    /// Memory's pressure in [`Self::columns`]' columns, column for column: the worst level read
    /// in each column's buckets, `None` (hollow) where none was read or the row keeps none.
    #[must_use]
    pub fn pressure_columns(
        &self,
        per_s: u32,
        ncols: usize,
        now_unix_ms: i64,
    ) -> Vec<Option<Pressure>> {
        let cells: Vec<Cell> = self
            .pressure
            .iter()
            .map(|p| {
                let r = p.and_then(Pressure::rank).map(f64::from);
                Cell {
                    mean: r,
                    max: r,
                    over: false,
                }
            })
            .collect();
        columns_of(&cells, self.ring_k, per_s, ncols, now_unix_ms)
            .into_iter()
            .map(|c| c.max.map(Pressure::of_rank))
            .collect()
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
                a.over |= b.over;
            }
        }
        out
    }

    /// The last `n` slots of `grid` as heat, ending at the one holding `at_ms` ([`slots_of`]
    /// over each series' readings, an empty slot spoken for by a reading at most `cover_ms` after
    /// the one before it), the second series merged in as in [`Self::heat_columns`]: a strip
    /// drawn reading by reading, on the snapshot's [`Snapshot::slot_grid`].
    #[must_use]
    pub fn heat_slots(&self, grid: SlotGrid, n: usize, at_ms: i64, cover_ms: i64) -> Vec<HeatCell> {
        let cells = |s: &Series| {
            self.heat_of_columns(slots_of(&s.recent, at_ms, grid, n, cover_ms), s.capacity)
        };
        let mut out = cells(&self.first());
        if let Some(second) = self.second.as_ref() {
            for (a, b) in out.iter_mut().zip(cells(second)) {
                a.heat = larger(a.heat, b.heat);
                a.max = larger(a.max, b.max);
                a.over |= b.over;
            }
        }
        out
    }

    fn heat_of_columns(&self, cols: Vec<Cell>, capacity: Option<f64>) -> Vec<HeatCell> {
        let ceiling = self.basis.heat_ceiling();
        cols.into_iter()
            .map(|c| HeatCell {
                heat: heat_of(c.mean, capacity).map(|h| h.min(ceiling)),
                max: heat_of(c.max, capacity).map(|h| h.min(ceiling)),
                over: c.over,
            })
            .collect()
    }

    /// The first series, as [`Series`].
    #[must_use]
    pub fn first(&self) -> Series {
        Series {
            now: self.now,
            capacity: self.capacity,
            ring: Arc::clone(&self.ring),
            recent: Arc::clone(&self.recent),
            binds: self.second.as_ref().is_none_or(|s| !s.binds),
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

    fn of(first: Series, second: Option<Series>, ring_k: i64, over: bool, basis: CapBasis) -> Self {
        let a = heat_of(first.now, first.capacity);
        let b = second.as_ref().and_then(|s| heat_of(s.now, s.capacity));
        let second_binds = match (a, b) {
            (Some(a), Some(b)) => b > a,
            (None, Some(_)) => true,
            _ => false,
        };
        Self {
            now: first.now,
            capacity: first.capacity,
            heat_now: larger(a, b).map(|h| h.min(basis.heat_ceiling())),
            over,
            basis,
            ring: first.ring,
            ring_k,
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
    let (v, cap) = (v?, cap?);
    Some(if cap > 0.0 {
        (v / cap).max(0.0) as f32
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
    /// CPU: the 1-minute load average.
    pub load1: Option<f64>,
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
    /// Tokens: the weekly plan figure's run, where an account's week is kept.
    pub plan_week: Option<PlanRun>,
    /// Processes: running on the machine and as this user.
    pub pid_counts: (Option<u64>, Option<u64>),
    /// Processes: the limits.
    pub proc_limits: ProcLimits,
}

/// One account's weekly plan figure over the [`PEAK_DAYS`] days ending today, a point a day:
/// the front tab's account's where a pane of it runs under one, else the account read most
/// recently. A day with no row was not read; the day the week reset has two rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanRun {
    /// The account's rows of those days ([`PlanWeek`]).
    pub week: PlanWeek,
    /// Whether it is the front tab's account.
    pub front: bool,
    /// Today, unix days in the local zone: the run's right end.
    pub today: u32,
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

/// Everything at one moment. Immutable once made; the host shares it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
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
    /// The machine.
    pub global: ScopeView,
    /// This instance and its tabs; `None` until two sweeps exist.
    pub aterm: Option<ScopeView>,
    /// Every tab, in the host's order; empty until two sweeps exist.
    pub tabs: Vec<TabView>,
    /// Busiest processes this sweep (at most [`TOP_PROCS`]) with attribution scope.
    pub top: Vec<TopProc>,
    /// Latest job-progress view from the host's stuck-job engine.
    pub jobs: Option<std::sync::Arc<crate::jobs::JobsView>>,
    /// Delegated-job totals, from exactly the same process rates as the scopes.
    pub delegated: Vec<DelegatedUsage>,
    /// Whether this reading omitted the process sweep and read machine counters only.
    pub machine_only: bool,
    /// The sample's wall clock, unix ms.
    pub at_unix_ms: i64,
    /// Where a strip drawn reading by reading puts its slots: a second each, their edges between
    /// the recent readings ([`SlotGrid::clear_of`]), so each reading a second stands alone in
    /// its own slot.
    pub slot_grid: SlotGrid,
    /// The deck's rows, in [`Resource::ALL`] order.
    pub resources: [ResourceView; 7],
    /// The home volume's free space over the last 24 hours.
    pub free_history: FreeHistory,
    /// What each row's Details and lens say beside its heat.
    pub facts: RowFacts,
    /// The process sweep's window, ns (`None` until two sweeps exist).
    pub procs_window_ns: Option<u64>,
    /// The cores' window, ns (`None` until two readings exist).
    pub cores_window_ns: Option<u64>,
    /// Busy CPU breakdown across scopes, once two full sweeps and core ticks are available.
    pub split: Option<Split>,
    /// Process rows: aterm's (up to [`ATERM_ROWS`]), the top [`TOP_ROWS`] by CPU and by
    /// footprint, and every other process with a GPU share.
    pub procs: Vec<ProcView>,
    /// Programs by scope, busiest first: every aterm and tab group (up to [`ATERM_ROWS`]) plus
    /// the machine's top [`TOP_ROWS`] by CPU and, separately, by footprint.
    pub programs: Vec<ProgramGroup>,
    /// Whether the process sweep stopped at its cap.
    pub procs_truncated: bool,
    /// The thermal state, where read.
    pub thermal: Option<Thermal>,
    /// The 1, 5 and 15 minute load averages, where read.
    pub load: Option<[f64; 3]>,
    /// Immediately available memory in bytes (free + inactive + purgeable), set by the host.
    pub avail: Option<u64>,
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
    /// Most recent split-tick attribution facts ([`crate::explain`]).
    pub why: Option<Arc<crate::explain::WhyFacts>>,
    /// Whether the coalition lane reads (`Off` once a read misread the layout).
    pub coal_lane: crate::explain::CoalLane,
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
        let figs: Vec<&ScopeFig> = self.resources[res.index()]
            .tabs
            .iter()
            .filter(|(k, _)| panes.contains(k))
            .map(|(_, f)| f)
            .collect();
        if !figs.is_empty() {
            v.tab = ScopeFig::sum(&figs, &v.aterm);
        }
        v
    }

    /// Resample `fig`, a scope's figure in row `res`, into heat columns against the row's
    /// binding capacity, the basis of the scope's share of the row; `None` where a scope's figure
    /// is no share of the row's capacity ([`Resource::scope_shares_capacity`]).
    #[must_use]
    pub fn scope_heat_columns(
        &self,
        res: Resource,
        fig: &ScopeFig,
        per_s: u32,
        ncols: usize,
        now_unix_ms: i64,
    ) -> Option<Vec<HeatCell>> {
        let view = &self.resources[res.index()];
        res.scope_shares_capacity().then(|| {
            view.heat_of_columns(
                fig.columns(per_s, ncols, now_unix_ms),
                view.binding().capacity,
            )
        })
    }

    /// Take the stuck engine's view, and with it the Processes row's state.
    pub fn set_jobs(&mut self, jobs: Option<std::sync::Arc<crate::jobs::JobsView>>) {
        self.resources[Resource::Processes.index()].state =
            jobs.as_deref().and_then(State::of_jobs);
        self.jobs = jobs;
    }

    /// Logical cores, where known.
    #[must_use]
    pub fn total_cores(&self) -> Option<usize> {
        self.cpu.as_ref().map(CpuView::total)
    }
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
            footprint: acc.footprint,
            footprint_gib: self.footprint.view(),
            procs: acc.procs,
            history: self.history.to_vec(),
            top: acc.top.clone(),
            wait: acc.wait,
            write_bps: acc.writes,
            refused: acc.refused,
            gpu: acc.gpu,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct ScopeAcc {
    cores: f32,
    footprint: Option<u64>,
    procs: u32,
    top: Option<(String, f32)>,
    /// Bytes written per second, where any member's counter gave a delta.
    writes: Option<f64>,
    /// Cores-worth waiting for a core across readable members.
    wait: Option<f32>,
    /// Members whose CPU time the OS refused.
    refused: u32,
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
        }
        if let Some(w) = r.write_bps {
            self.writes = Some(self.writes.unwrap_or(0.0) + w);
        }
        if let Some(w) = r.wait {
            self.wait = Some(self.wait.unwrap_or(0.0) + w);
        }
        if p.cpu_ns.is_none() {
            self.refused += 1;
        }
        self.procs += 1;
        if cores > 0.0 && self.top.as_ref().is_none_or(|(_, c)| cores > *c) {
            self.top = Some((p.name.clone(), cores));
        }
    }
}

/// Per-process CPU, wait, and storage write rates over the sample interval.
#[derive(Clone, Copy, Debug, Default)]
struct ProcRates {
    cores: Option<f32>,
    wait: Option<f32>,
    write_bps: Option<f64>,
    gpu: Option<f32>,
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
            .then(|| dt as f64 / 1000.0)
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
        (out <= TOKENS_CAP_PER_S * dt).then(|| (out / dt * 60.0, input / dt * 60.0))
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
    aterm: Option<ScopeView>,
    tabs: Vec<TabView>,
    top: Vec<TopProc>,
    delegated: Vec<DelegatedUsage>,
    figs: Option<SweepFigs>,
    /// `sys procs`' rows ([`keep_rows`]).
    rows: Vec<ProcView>,
    /// [`Snapshot::programs`] ([`keep_programs`]).
    programs: Vec<ProgramGroup>,
    /// `(aterm_cores, user_other_cores, cpu_unreadable_count)` for [`Split`].
    split_parts: Option<(f32, f32, u32)>,
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
    /// Each row's hour ring, in [`Resource::ALL`] order: the first series.
    rings: [BucketRing; 7],
    /// Network's send direction.
    net_tx: BucketRing,
    /// Processes run by this user.
    yours: BucketRing,
    /// Memory pressure's rank ([`Pressure::rank`]), pushed beside Memory's ring: a bucket's
    /// largest reading is the worst level read in it.
    pressure: BucketRing,
    /// Last known fixed capacity per row.
    caps: [Option<f64>; 7],
    /// aterm's figures' hour, by row.
    aterm: ScopeRings,
    /// Each tab's, by key.
    tabs: HashMap<u64, ScopeRings>,
    /// aterm's input tokens per minute.
    tokens_in: Recent,
    peaks: Peaks,
    free: FreeHistory,
    cores_at: Option<Stamp>,
    sweep_at: Option<Stamp>,
    /// Each process of the last sweep: its start and cumulative bytes written (`None` unread).
    written: HashMap<u32, (Option<u64>, Option<u64>)>,
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
    sessions: HashMap<String, Session>,
    /// Monotonic timestamp (ms) since which `rate60` disk filling has been positive.
    filling_since: Option<u64>,
    /// The GPU's reset count at the first reading, and when it first rose.
    gpu_recoveries: Option<u64>,
    gpu_rose_at: Option<i64>,
    /// Each process's GPU share from the client reads.
    gpu_clients: crate::gpu_clients::GpuShares,
}

/// One scope's hour rings in [`Resource::ALL`] order; a row's ring exists once the scope read it.
#[derive(Debug, Default)]
struct ScopeRings([Option<BucketRing>; 7]);

impl ScopeRings {
    fn push(&mut self, now: i64, vals: [Option<f64>; 7]) {
        for (ring, v) in self.0.iter_mut().zip(vals) {
            if let Some(v) = v {
                let _ = ring
                    .get_or_insert_with(BucketRing::default)
                    .push(now, Some(v), false);
            }
        }
    }

    fn ring(&self, res: Resource) -> Option<&BucketRing> {
        self.0[res.index()].as_ref()
    }
}

/// The model's running state. One per host instance.
#[derive(Debug, Default)]
pub struct Vitals {
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
    /// The pressure's rank each second, beside `mem_history` ([`History::push_worst_at`]).
    pressure_history: History,
    last_counters: Option<(u64, MemCounters)>,
    last_sweep_at: Option<u64>,
    last_cpu: HashMap<u32, (Option<u64>, u64)>,
    last_runnable: HashMap<u32, u64>,
    last_read_at: HashMap<u32, u64>,
    global_foot: Trend,
    aterm: ScopeState,
    tabs: HashMap<u64, ScopeState>,
    detector: crate::events::Detector,
    why: crate::explain::WhyState,
    /// The last [`SLOT_TIMES`] readings' wall clocks, unix ms, oldest first.
    slot_times: VecDeque<i64>,
    /// Where the strips' reading slots stand ([`Snapshot::slot_grid`]).
    slot_grid: SlotGrid,
}

/// The readings [`Vitals`] places its slot grid's edges between: eight seconds at the focused
/// cadence, four at the lens's.
const SLOT_TIMES: usize = 8;

impl Vitals {
    /// A model that has read nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The slot grid with a reading at `at_ms` in it: the one before while that reading stands
    /// clear of its slot's edges, else the grid clear of the last [`SLOT_TIMES`] readings. Its
    /// edges then move only when the readings' place in the second has drifted near one, and
    /// move to the middle of the widest gap between them, so a strip keeps each reading in a
    /// slot of its own.
    fn slot_grid(&mut self, at_ms: i64) -> SlotGrid {
        self.slot_times.push_back(at_ms);
        while self.slot_times.len() > SLOT_TIMES {
            self.slot_times.pop_front();
        }
        if !self.slot_grid.clear(at_ms, SLOT_GUARD_MS) {
            self.slot_grid = SlotGrid::clear_of(self.slot_times.make_contiguous(), SLOT_MS);
        }
        self.slot_grid
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

    /// Take the free history the host persisted in an earlier run ([`FreeHistory::restored`]).
    pub fn restore_free_history(&mut self, history: FreeHistory) {
        self.rows.free = history;
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
        self.seq += 1;
        self.rows
            .gpu_clients
            .absorb(s.gpu_clients.as_ref(), s.at_ns);
        let stamp = Stamp::of(s);
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
            self.pressure_history
                .push_worst_at(m.pressure.rank().map(f32::from), s.at_ns);
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
                pressure_history: self
                    .pressure_history
                    .to_vec()
                    .into_iter()
                    .map(|r| r.map(|r| Pressure::of_rank(f64::from(r))))
                    .collect(),
                rates,
            })
        } else {
            self.mem_history.push_at(None, s.at_ns);
            self.pressure_history.push_worst_at(None, s.at_ns);
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

        let pass = match s.procs.as_deref() {
            Some(procs) => self.absorb_procs(procs, s, &mut global, sweep_dt),
            None => {
                self.aterm.history.push_at(None, s.at_ns);
                for state in self.tabs.values_mut() {
                    state.history.push_at(None, s.at_ns);
                }
                ProcPass::default()
            }
        };
        let busy = cpu
            .as_ref()
            .filter(|_| cores_dt.is_some())
            .map(|c| f64::from(c.busy.raw));
        let figs = pass.figs.as_ref().filter(|_| !s.machine_only);
        let (resources, facts, net) = self.rows.absorb(s, stamp, busy, figs, sweep_dt);
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
        let split = match (cpu.as_ref(), pass.split_parts) {
            (Some(c), Some((aterm, others, unread))) => {
                let rest = c.busy.raw - aterm - others;
                Some(Split {
                    global: c.busy.raw,
                    aterm,
                    others,
                    services: rest.max(0.0),
                    unaccounted: rest.min(0.0),
                    unread,
                })
            }
            _ => None,
        };

        let mut snap = Snapshot {
            at_ns: s.at_ns,
            seq: self.seq,
            cpu,
            gpu,
            mem,
            volume: s.volume.clone(),
            net,
            global,
            aterm: pass.aterm,
            tabs: pass.tabs,
            top: pass.top,
            jobs: None,
            delegated: pass.delegated,
            machine_only: s.machine_only,
            at_unix_ms: s.at_unix_ms,
            slot_grid: self.slot_grid(s.at_unix_ms),
            resources,
            free_history: self.rows.free.clone(),
            facts,
            procs_window_ns: pass.window_ns,
            cores_window_ns,
            split,
            procs: pass.rows,
            programs: pass.programs,
            procs_truncated: s.procs_truncated,
            thermal: s.thermal,
            load: s.load,
            avail: None,
            slots: None,
            slots_age_ms: None,
            slot_tabs: None,
            self_pid: s.self_pid,
            events: Vec::new(),
            why: None,
            coal_lane: s.coal_lane,
        };
        let attribution = pass
            .attribution
            .as_ref()
            .map(|(m, o)| (m.as_slice(), o.as_slice()));
        snap.why = self.why.absorb(s, &snap, attribution);
        snap.coal_lane = self.why.lane(s);
        snap.events = self.detector.detect(&snap);
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
            let frac = if total == 0 {
                trend.view().raw
            } else {
                (busy as f32 / total as f32).clamp(0.0, 1.0)
            };
            trend.push(frac, dt_s);
            hist.push_at(Some(frac), at_ns);
            busy_sum += frac;
            views.push(CoreView { load: trend.view() });
        }
        self.last_cores = cores.to_vec();
        self.busy.push(busy_sum, dt_s);
        self.busy_history.push_at(Some(busy_sum), at_ns);
        Some(CpuView {
            clusters: cluster_views(clusters, views.len()),
            cores: views,
            busy: self.busy.view(),
            history: self.busy_history.to_vec(),
            core_history: self.core_histories.iter().map(History::to_vec).collect(),
        })
    }

    fn absorb_procs(
        &mut self,
        procs: &[ProcSample],
        s: &Sample,
        global: &mut ScopeView,
        sweep_dt: Option<f64>,
    ) -> ProcPass {
        let writes = window_writes(&self.rows.written, self.rows.sweep_whole, procs, sweep_dt);
        let writes_read = procs.iter().any(|p| p.disk_written.is_some());
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
        let mut next_cpu = HashMap::with_capacity(procs.len());
        let mut next_runnable = HashMap::with_capacity(procs.len());
        let mut next_read_at = HashMap::with_capacity(procs.len());
        let mut rates: Vec<ProcRates> = Vec::with_capacity(procs.len());
        for p in procs {
            let mut r = ProcRates {
                write_bps: sweep_dt
                    .zip(writes.get(&p.pid))
                    .map(|(dt, &bytes)| bytes as f64 / dt),
                gpu: self.rows.gpu_clients.share(p.pid, &p.name),
                ..ProcRates::default()
            };
            let wall_ns = match (p.at_ns, self.last_read_at.get(&p.pid)) {
                (Some(now), Some(&then)) if now > then => Some(now - then),
                _ => wall_ns,
            };
            if let Some(at) = p.at_ns {
                next_read_at.insert(p.pid, at);
            }
            if let Some(ns) = p.cpu_ns {
                next_cpu.insert(p.pid, (p.start, ns));
                if let (Some(wall), Some(&(birth, before))) = (wall_ns, self.last_cpu.get(&p.pid))
                    && wall > 0
                    && birth == p.start
                {
                    r.cores = Some(ns.saturating_sub(before) as f32 / wall as f32);
                }
            }
            if let Some(run) = p.runnable_ns {
                next_runnable.insert(p.pid, run);
                if let (Some(wall), Some(&before), Some(ns), Some(&(birth, cpu_before))) = (
                    wall_ns,
                    self.last_runnable.get(&p.pid),
                    p.cpu_ns,
                    self.last_cpu.get(&p.pid),
                ) && wall > 0
                    && birth == p.start
                {
                    let d_run = run.saturating_sub(before);
                    let d_cpu = ns.saturating_sub(cpu_before);
                    r.wait = Some(d_run.saturating_sub(d_cpu) as f32 / wall as f32);
                }
            }
            rates.push(r);
        }
        self.last_cpu = next_cpu;
        self.last_runnable = next_runnable;
        self.last_read_at = next_read_at;
        global.procs = u32::try_from(procs.len()).unwrap_or(u32::MAX);
        let Some(wall_ns) = wall_ns else {
            // No window yet, but the explanations' reap credit takes its first members here.
            return ProcPass {
                attribution: Some(attribute_delegated(
                    procs,
                    s.self_pid,
                    &s.tabs,
                    &s.delegated,
                )),
                ..ProcPass::default()
            };
        };
        let dt_s = wall_ns as f32 / 1e9;

        let (membership, delegated_members) =
            attribute_delegated(procs, s.self_pid, &s.tabs, &s.delegated);
        let mut aterm_acc = ScopeAcc::default();
        let mut global_acc = ScopeAcc::default();
        let mut tab_acc: Vec<ScopeAcc> = vec![ScopeAcc::default(); s.tabs.len()];
        let mut others = 0.0f32;
        let (mut others_gpu, mut services_gpu) = (0.0f32, 0.0f32);
        let mut ownership_known = s.self_uid.is_some();
        let mut rows: Vec<ProcView> = Vec::with_capacity(procs.len());
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
            let scope = || match (m.tab, m.aterm) {
                (Some(t), _) => s
                    .tabs
                    .get(t)
                    .map_or(ProcScope::Aterm, |r| ProcScope::Tab(r.tab)),
                (None, true) => ProcScope::Aterm,
                (None, false) => ProcScope::Other,
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
            if let Some(t) = m.tab {
                tab_acc[t].add(p, r);
            }
            let row_scope = if p.pid == s.self_pid {
                RowScope::SelfProc
            } else if let Some(t) = m.tab {
                RowScope::Tab(s.tabs[t].key)
            } else if m.aterm {
                RowScope::Helper
            } else {
                match (p.uid, s.self_uid) {
                    (Some(u), Some(me)) if u == me => RowScope::Others,
                    (Some(_), Some(_)) => RowScope::Services,
                    _ => {
                        ownership_known = false;
                        RowScope::Unknown
                    }
                }
            };
            match row_scope {
                RowScope::Others => {
                    others += cores;
                    others_gpu += r.gpu.unwrap_or(0.0);
                }
                RowScope::Services | RowScope::Unknown => services_gpu += r.gpu.unwrap_or(0.0),
                _ => {}
            }
            rows.push(ProcView {
                pid: p.pid,
                ppid: p.ppid,
                pgid: p.pgid,
                name: p.name.clone(),
                cores: r.cores,
                wait: r.wait,
                footprint: p.footprint,
                peak: p.peak,
                write_bps: r.write_bps,
                state: p.state,
                scope: row_scope,
                refused: p.cpu_ns.is_none(),
                gpu: r.gpu,
                birth_ms: p.birth_ms,
            });
        }
        global.top = global_acc.top.clone();
        global.wait = global_acc.wait;
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
            .filter(|(_, acc)| acc.procs > 0)
            .map(|(job, acc)| DelegatedUsage {
                root: job.root,
                cores: acc.cores,
                footprint: acc.footprint,
                procs: acc.procs,
            })
            .collect();
        ProcPass {
            aterm: Some(aterm),
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
            rows: keep_rows(rows),
            programs: keep_programs(programs),
            split_parts: ownership_known.then_some((aterm_cores, others, global_acc.refused)),
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
    last: &HashMap<u32, (Option<u64>, Option<u64>)>,
    last_whole: bool,
    procs: &[ProcSample],
    sweep_dt: Option<f64>,
) -> HashMap<u32, u64> {
    if sweep_dt.is_none() {
        return HashMap::new();
    }
    let newest = last.values().filter_map(|r| r.0).max();
    procs
        .iter()
        .filter_map(|p| {
            let now = p.disk_written?;
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
    Some(MemRates {
        swapin_bps: per_s(d(then.swapin_bytes, now.swapin_bytes)?),
        swapout_bps: per_s(d(then.swapout_bytes, now.swapout_bytes)?),
        decompress_ps: per_s(d(then.decompressions, now.decompressions)?),
        compress_ps: per_s(d(then.compressions, now.compressions)?),
        pagein_ps: per_s(d(then.pageins, now.pageins)?),
        window_ns,
    })
}

fn busiest_first(a: &ProcView, b: &ProcView) -> std::cmp::Ordering {
    b.cores
        .unwrap_or(-1.0)
        .total_cmp(&a.cores.unwrap_or(-1.0))
        .then(b.footprint.unwrap_or(0).cmp(&a.footprint.unwrap_or(0)))
        .then(a.pid.cmp(&b.pid))
}

fn keep_rows(rows: Vec<ProcView>) -> Vec<ProcView> {
    let (mut mine, mut rest): (Vec<ProcView>, Vec<ProcView>) =
        rows.into_iter().partition(|r| r.scope.in_aterm());
    mine.sort_by(busiest_first);
    mine.truncate(ATERM_ROWS);
    rest.sort_by(busiest_first);
    let mut kept: Vec<ProcView> = rest.iter().take(TOP_ROWS).cloned().collect();
    let mut by_foot: Vec<&ProcView> = rest.iter().skip(TOP_ROWS).collect();
    by_foot.sort_by_key(|p| std::cmp::Reverse(p.footprint.unwrap_or(0)));
    let mut by_foot = by_foot.into_iter();
    kept.extend(by_foot.by_ref().take(TOP_ROWS).cloned());
    kept.extend(by_foot.filter(|p| p.gpu.is_some_and(|g| g > 0.0)).cloned());
    mine.extend(kept);
    mine.sort_by(busiest_first);
    mine
}

fn busiest_program_first(a: &ProgramGroup, b: &ProgramGroup) -> std::cmp::Ordering {
    b.cores
        .total_cmp(&a.cores)
        .then(b.footprint.unwrap_or(0).cmp(&a.footprint.unwrap_or(0)))
        .then(a.name.cmp(&b.name))
}

/// [`Snapshot::programs`]' bound, as [`keep_rows`] bounds the rows: every aterm and tab group
/// up to [`ATERM_ROWS`], and of the machine's others the [`TOP_ROWS`] busiest and the
/// [`TOP_ROWS`] largest of the rest.
fn keep_programs(groups: Vec<ProgramGroup>) -> Vec<ProgramGroup> {
    let (mut kept, mut rest): (Vec<ProgramGroup>, Vec<ProgramGroup>) = groups
        .into_iter()
        .partition(|g| g.scope != ProcScope::Other);
    kept.sort_by(busiest_program_first);
    kept.truncate(ATERM_ROWS);
    rest.sort_by(busiest_program_first);
    let mut by_foot = rest.split_off(rest.len().min(TOP_ROWS));
    by_foot.sort_by_key(|g| std::cmp::Reverse(g.footprint.unwrap_or(0)));
    by_foot.truncate(TOP_ROWS);
    kept.extend(rest);
    kept.extend(by_foot);
    kept.sort_by(busiest_program_first);
    kept
}

impl Rows {
    fn absorb(
        &mut self,
        s: &Sample,
        stamp: Stamp,
        busy: Option<f64>,
        figs: Option<&SweepFigs>,
        sweep_dt: Option<f64>,
    ) -> ([ResourceView; 7], RowFacts, Option<NetView>) {
        use Resource::{Cpu, Disk, Gpu, Memory, Network, Processes, Tokens};
        let now = s.at_unix_ms;
        let mut facts = RowFacts {
            load1: s.load1,
            gpu: s.gpu.clone(),
            disk: s.disk.clone(),
            plan: s.plan.clone(),
            pid_counts: s.pid_counts,
            proc_limits: s.proc_limits,
            ..RowFacts::default()
        };

        let n_cores = s.cores.as_ref().map(Vec::len).filter(|&n| n > 0);
        if let Some(n) = n_cores {
            self.caps[Cpu.index()] = Some(n as f64);
        }
        let cpu_over = match (busy, n_cores, s.load1) {
            (Some(b), Some(n), Some(load)) => load > n as f64 && b >= CPU_OVER_BUSY * n as f64,
            _ => false,
        };
        self.rings[Cpu.index()].push(now, busy, cpu_over);

        let gpu_pct = s
            .gpu
            .as_ref()
            .and_then(|g| g.device_pct)
            .map(|p| f64::from(p.min(100)));
        self.caps[Gpu.index()] = Some(100.0);
        self.rings[Gpu.index()].push(now, gpu_pct, false);
        if let Some(r) = s.gpu.as_ref().and_then(|g| g.recoveries) {
            let first = *self.gpu_recoveries.get_or_insert(r);
            if r > first {
                let since = *self.gpu_rose_at.get_or_insert(now);
                facts.gpu_resets = Some((r - first, since));
            }
        }

        let mem = s.mem.filter(|m| m.total > 0);
        if let Some(m) = mem {
            self.caps[Memory.index()] = Some(m.total as f64);
        }
        if let Some(kib) = s.swapped_kib {
            facts.swap_rate = self.swap.and_then(|(then, before)| {
                let dt = stamp.rate_dt(Some(then))?;
                Some(kib.checked_sub(before)? as f64 * 1024.0 / dt)
            });
            self.swap = Some((stamp, kib));
        }
        let mem_over = facts.swap_rate.is_some_and(|r| r >= SWAP_OVER_BPS);
        self.rings[Memory.index()].push(now, mem.map(|m| m.used() as f64), mem_over);
        let pressure = mem.and_then(|m| m.pressure.rank()).map(f64::from);
        self.pressure.push(now, pressure, false);

        let vol = s.volume.as_ref().filter(|v| v.total > 0);
        if let Some(v) = vol {
            self.caps[Disk.index()] = Some(v.total as f64);
            self.free.note_on(&v.mount, v.total, now, v.available);
        }
        self.rings[Disk.index()].push(now, vol.map(|v| v.used() as f64), false);
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
            let ttf = history::time_to_full(v.available, facts.disk_rates.rate60, filling_for);
            facts.filling = history::filling(v.available, &facts.disk_rates, ttf);
        }

        let (rx, tx, net) = self.absorb_links(s.links.as_deref(), s.tcp, stamp, &mut facts);
        let closed_rx = self.rings[Network.index()].push(now, rx, false);
        let closed_tx = self.net_tx.push(now, tx, false);
        note_peak(
            &self.rings[Network.index()],
            closed_rx,
            &mut self.peaks.net_rx,
        );
        note_peak(&self.net_tx, closed_tx, &mut self.peaks.net_tx);

        for week in &s.weeks {
            if let Some(reading) = week.reading {
                self.peaks.note_plan(&week.account, reading);
            }
        }
        facts.plan_week = plan_run(&self.peaks, &s.weeks, s.local_day);

        let tokens = self.absorb_tokens(&s.tokens, stamp);
        let closed = self.rings[Tokens.index()].push(now, tokens.out, false);
        note_peak(&self.rings[Tokens.index()], closed, &mut self.peaks.tokens);
        if let Some(input) = tokens.input {
            self.tokens_in.push(now, input);
        }
        facts.tokens_in = self.tokens_in.mean(now, TOKENS_WINDOW_MS);
        facts.agents = tokens.agents;

        let (all, yours) = s.pid_counts;
        self.rings[Processes.index()].push(now, all.map(|n| n as f64), false);
        self.yours.push(now, yours.map(|n| n as f64), false);

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
            let vals = |acc: &ScopeAcc, tokens: Option<f64>| -> [Option<f64>; 7] {
                let mut v = [None; 7];
                v[Cpu.index()] = sweep_dt.map(|_| f64::from(acc.cores));
                // Held at the device's reading so each 10 s cell keeps tab <= all tabs <= Mac.
                v[Gpu.index()] = acc.gpu.map(|g| {
                    gpu_pct.map_or(f64::from(g) * 100.0, |mac| (f64::from(g) * 100.0).min(mac))
                });
                v[Memory.index()] = acc.footprint.map(|b| b as f64);
                v[Disk.index()] = sweep_dt.and(acc.writes);
                v[Tokens.index()] = tokens;
                v[Processes.index()] = Some(f64::from(acc.procs));
                v
            };
            self.aterm.push(now, vals(&f.aterm, tokens.out));
            for (root, acc) in s.tabs.iter().zip(&f.tabs) {
                let rings = self.tabs.entry(root.key).or_default();
                rings.push(now, vals(acc, tokens.tabs.get(&root.key).copied()));
            }
            facts.writers = crate::writes::top_per_scope(f.writers.iter().cloned(), |w| w.scope);
            facts.busiest = f.busiest.clone();
        }
        let gpu_held = self.gpu_clients.held();
        let missing = |res: Resource, agent: bool| -> Option<NoFigure> {
            match res {
                Gpu if !gpu_held => Some(NoFigure::NotPerApp),
                Network => Some(NoFigure::NotPerApp),
                Disk if figs.is_some_and(|f| !f.writes_read) => Some(NoFigure::NotReadHere),
                Tokens if !agent => Some(NoFigure::NoAgent),
                _ => None,
            }
        };
        let fig = |rings: Option<&ScopeRings>, res: Resource, agent: bool| {
            let ring = rings.and_then(|r| r.ring(res));
            ScopeFig {
                now: figs.and(ring).and_then(|r| r.now_mean(now, window_of(res))),
                missing: missing(res, agent),
                run: ring.map_or_else(|| Arc::from([]), |r| r.tail_cells(SCOPE_RUN_BUCKETS).into()),
                run_k: ring.and_then(BucketRing::open_k).unwrap_or(0),
            }
        };

        let disk_state = disk_state(s.disk.as_ref(), facts.filling.as_ref());
        let mem_state = match mem.map(|m| m.pressure) {
            Some(Pressure::Warn) => Some(State::Warn),
            Some(Pressure::Critical) => Some(State::Critical),
            _ => None,
        };
        let gpu_state = facts.gpu_resets.map(|_| State::Warn);
        let resources = Resource::ALL.map(|res| {
            let i = res.index();
            let ring = &self.rings[i];
            let (capacity, basis) = match res {
                Network => (rx_cap, net_basis),
                Tokens => (tok_cap, tok_basis),
                Processes => (s.proc_limits.system.map(|n| n as f64), CapBasis::Fixed),
                _ => (self.caps[i], CapBasis::Fixed),
            };
            let second = match res {
                Network => Some(series(&self.net_tx, now, NOW_WINDOW_MS, tx_cap)),
                Processes => Some(series(
                    &self.yours,
                    now,
                    NOW_WINDOW_MS,
                    s.proc_limits.per_user.map(|n| n as f64),
                )),
                _ => None,
            };
            let mut view = ResourceView::of(
                series(ring, now, window_of(res), capacity),
                second,
                ring.open_k().unwrap_or(0),
                ring.open_over(),
                basis,
            );
            view.state = match res {
                Gpu => gpu_state,
                Memory => mem_state,
                Disk => disk_state,
                _ => None,
            };
            if res == Memory {
                view.pressure = ring
                    .iter()
                    .map(|(k, _)| {
                        let worst = self.pressure.bucket(k).and_then(|b| b.cell().max);
                        worst.map(Pressure::of_rank)
                    })
                    .collect();
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

    fn absorb_links(
        &mut self,
        links: Option<&[LinkSample]>,
        tcp: Option<TcpCounts>,
        stamp: Stamp,
        facts: &mut RowFacts,
    ) -> (Option<f64>, Option<f64>, Option<NetView>) {
        let Some(links) = links else {
            let net = tcp.map(|tcp| NetView {
                tcp: Some(tcp),
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
        let mut moved_by: Vec<(&LinkSample, u64)> = Vec::new();
        let mut link_views: Vec<LinkView> = Vec::with_capacity(links.len());
        for l in links {
            let prev = self.links.get(&l.name);
            let rates = dt.and_then(|d| prev.and_then(|b| link_rates(b, l, d)));
            if l.counts {
                let moved = if rates.is_some()
                    && let Some(b) = prev
                {
                    let d_rx = l.rx - b.rx;
                    let d_tx = l.tx - b.tx;
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
                    0
                };
                moved_by.push((l, moved));
            }
            link_views.push(LinkView {
                sample: l.clone(),
                rates,
            });
        }
        facts.link = self.pick_link(&moved_by, dt).cloned();
        self.links = links.iter().map(|l| (l.name.clone(), l.clone())).collect();
        let rates = dt.filter(|_| measured).map(|d| LinkRates {
            rx_bps: rx as f64 / d,
            tx_bps: tx as f64 / d,
            rx_pps: rx_pkts as f64 / d,
            tx_pps: tx_pkts as f64 / d,
            rx_err_ps: rx_err as f64 / d,
            tx_err_ps: tx_err as f64 / d,
            rx_drop_ps: rx_drop as f64 / d,
            tx_drop_ps: any_tx_drop.then(|| tx_drop as f64 / d),
        });
        let window_ns = dt.filter(|_| measured).map(|d| (d * 1e9).round() as u64);
        let net = (!link_views.is_empty() || tcp.is_some()).then_some(NetView {
            links: link_views,
            rates,
            window_ns,
            tcp,
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

fn link_rates(before: &LinkSample, now: &LinkSample, dt: f64) -> Option<LinkRates> {
    if dt <= 0.0 {
        return None;
    }
    let d_rx = now.rx.checked_sub(before.rx)?;
    let d_tx = now.tx.checked_sub(before.tx)?;
    let d_rx_pkts = now.rx_packets.checked_sub(before.rx_packets)?;
    let d_tx_pkts = now.tx_packets.checked_sub(before.tx_packets)?;
    let d_rx_err = now.rx_errors.checked_sub(before.rx_errors)?;
    let d_tx_err = now.tx_errors.checked_sub(before.tx_errors)?;
    let d_rx_drop = now.rx_drops.checked_sub(before.rx_drops)?;
    let tx_drop_ps = match (before.tx_drops, now.tx_drops) {
        (Some(b), Some(n)) => Some(n.checked_sub(b)? as f64 / dt),
        _ => None,
    };
    Some(LinkRates {
        rx_bps: d_rx as f64 / dt,
        tx_bps: d_tx as f64 / dt,
        rx_pps: d_rx_pkts as f64 / dt,
        tx_pps: d_tx_pkts as f64 / dt,
        rx_err_ps: d_rx_err as f64 / dt,
        tx_err_ps: d_tx_err as f64 / dt,
        rx_drop_ps: d_rx_drop as f64 / dt,
        tx_drop_ps,
    })
}

fn series(ring: &BucketRing, now: i64, window_ms: i64, capacity: Option<f64>) -> Series {
    Series {
        now: ring.now_mean(now, window_ms),
        capacity,
        ring: ring.cells().into(),
        recent: ring.readings().into(),
        binds: false,
    }
}

fn note_peak(ring: &BucketRing, closed: Option<i64>, peak: &mut DailyMax) {
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

/// The [`PlanRun`] of `peaks`' weeks on `today`.
fn plan_run(peaks: &Peaks, weeks: &[WeekFact], today: u32) -> Option<PlanRun> {
    let front = weeks
        .iter()
        .filter(|w| w.front)
        .find_map(|w| peaks.plan.iter().find(|kept| kept.account == w.account));
    let (week, front) = match front {
        Some(week) => (week, true),
        None => (peaks.plan.iter().max_by_key(|week| week.read_at())?, false),
    };
    let days: Vec<PlanDay> = week
        .days
        .iter()
        .filter(|d| today.checked_sub(d.day).is_some_and(|age| age < PEAK_DAYS))
        .copied()
        .collect();
    (!days.is_empty()).then(|| PlanRun {
        week: PlanWeek {
            account: week.account.clone(),
            days,
        },
        front,
        today,
    })
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

fn day_of(unix_ms: i64) -> u32 {
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
    /// In this tab (an index into the sample's tabs).
    pub tab: Option<usize>,
}

/// Attribute each process in `procs` to `aterm` (`self_pid` tree + tab trees) and its nearest tab.
#[must_use]
pub fn attribute(procs: &[ProcSample], self_pid: u32, tabs: &[TabRoot]) -> Vec<Membership> {
    let parent: HashMap<u32, u32> = procs.iter().map(|p| (p.pid, p.ppid)).collect();
    let shells: HashMap<u32, usize> = tabs
        .iter()
        .enumerate()
        .filter(|(_, t)| t.shell_pid != 0)
        .map(|(i, t)| (t.shell_pid, i))
        .collect();
    let mut memo: HashMap<u32, Membership> = HashMap::with_capacity(procs.len());
    procs
        .iter()
        .map(|p| resolve(p.pid, self_pid, &parent, &shells, &mut memo))
        .collect()
}

fn attribute_delegated(
    procs: &[ProcSample],
    self_pid: u32,
    tabs: &[TabRoot],
    jobs: &[DelegatedJob],
) -> (Vec<Membership>, Vec<Option<usize>>) {
    let mut membership = attribute(procs, self_pid, tabs);
    let mut claims = HashMap::new();
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
    parent: &HashMap<u32, u32>,
    shells: &HashMap<u32, usize>,
    memo: &mut HashMap<u32, Membership>,
) -> Membership {
    let mut path = Vec::new();
    let mut cur = pid;
    let decided = loop {
        if let Some(m) = memo.get(&cur) {
            break *m;
        }
        let here = Membership {
            aterm: cur == self_pid,
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
mod tests {
    use super::*;

    fn proc(pid: u32, ppid: u32, cpu_ms: u64, foot_mib: u64, name: &str) -> ProcSample {
        ProcSample {
            pid,
            start: Some(1),
            ppid,
            cpu_ns: Some(cpu_ms * 1_000_000),
            footprint: Some(foot_mib << 20),
            name: name.into(),
            ..ProcSample::default()
        }
    }

    fn tab(key: u64, shell: u32, label: &str, program: &str) -> TabRoot {
        TabRoot {
            key,
            tab: key,
            shell_pid: shell,
            label: label.into(),
            program: program.into(),
            tab_id: Some(key),
        }
    }

    #[test]
    fn delegated_jobs_charge_two_tabs_once_without_their_shared_daemon() {
        use crate::delegated::ProcessKey;
        let member = |pid| ProcessKey {
            pid,
            start: u64::from(pid),
        };
        let processes = |second: bool| {
            [
                (900, 1, 800, 300),
                (901, 900, 50, 10),
                (902, 901, 200, 20),
                (903, 900, 100, 30),
                (904, 903, 400, 40),
            ]
            .into_iter()
            .map(|(pid, ppid, cpu, mem)| {
                let mut sample = proc(pid, ppid, if second { cpu } else { 0 }, mem, "work");
                sample.start = Some(u64::from(pid));
                sample
            })
            .collect()
        };
        let mut sample = Sample {
            at_ns: 1_000_000_000,
            at_unix_ms: 1000,
            mono_ms: 1000,
            cadence_ms: 1000,
            self_pid: 100,
            tabs: vec![
                tab(1, 200, "Tab 1", "codex"),
                tab(2, 300, "Tab 2", "claude"),
            ],
            procs: Some(processes(false)),
            delegated: vec![
                DelegatedJob {
                    root: member(901),
                    tab: Some(1),
                    members: vec![member(901), member(902)],
                },
                DelegatedJob {
                    root: member(903),
                    tab: Some(2),
                    members: vec![member(903), member(904)],
                },
            ],
            ..Sample::default()
        };
        let mut vitals = Vitals::new();
        vitals.absorb(&sample);
        sample.at_ns += 1_000_000_000;
        sample.at_unix_ms += 1000;
        sample.mono_ms += 1000;
        sample.procs = Some(processes(true));
        let snapshot = vitals.absorb(&sample);
        assert_eq!(snapshot.aterm.as_ref().unwrap().cores.raw, 0.75);
        assert_eq!(snapshot.aterm.as_ref().unwrap().footprint, Some(100 << 20));
        assert_eq!(snapshot.tab(1).unwrap().scope.cores.raw, 0.25);
        assert_eq!(snapshot.tab(2).unwrap().scope.cores.raw, 0.5);
        let cpu = &snapshot.resources[Resource::Cpu.index()];
        assert_eq!(cpu.aterm.now, Some(0.75));
        assert_eq!(snapshot.resource(Resource::Cpu, &[1]).tab.now, Some(0.25));
        assert_eq!(snapshot.resource(Resource::Cpu, &[2]).tab.now, Some(0.5));
        assert_eq!(
            snapshot.resources[Resource::Memory.index()].aterm.now,
            Some((100 << 20) as f64)
        );
        assert_eq!(
            snapshot.resources[Resource::Processes.index()].aterm.now,
            Some(4.0)
        );
        assert_eq!(
            snapshot.delegated.iter().map(|job| job.procs).sum::<u32>(),
            4
        );
        assert_eq!(
            snapshot.delegated.iter().map(|job| job.cores).sum::<f32>(),
            0.75
        );
    }

    #[test]
    fn reused_pid_inherits_neither_delegation_nor_previous_cpu() {
        use crate::delegated::ProcessKey;
        let mut process = proc(900, 1, 1000, 10, "old");
        let root = ProcessKey { pid: 900, start: 1 };
        let mut sample = Sample {
            at_ns: 1,
            self_pid: 100,
            procs: Some(vec![process.clone()]),
            delegated: vec![DelegatedJob {
                root,
                tab: None,
                members: vec![root],
            }],
            ..Sample::default()
        };
        let mut vitals = Vitals::new();
        vitals.absorb(&sample);
        process.start = Some(2);
        process.cpu_ns = Some(2_000_000_000);
        sample.at_ns += 1_000_000_000;
        sample.procs = Some(vec![process]);
        let snapshot = vitals.absorb(&sample);
        assert_eq!(snapshot.aterm.unwrap().cores.raw, 0.0);
        assert!(
            snapshot.delegated.is_empty(),
            "reused PID is not a zero reading of the old job"
        );
        assert!(snapshot.top.is_empty());
    }

    #[test]
    fn conflicting_explicit_claims_are_not_counted_twice() {
        use crate::delegated::ProcessKey;
        let process = proc(900, 1, 1, 1, "shared");
        let key = ProcessKey { pid: 900, start: 1 };
        let jobs = vec![
            DelegatedJob {
                root: key,
                tab: Some(1),
                members: vec![key],
            },
            DelegatedJob {
                root: key,
                tab: Some(2),
                members: vec![key],
            },
        ];
        let (membership, jobs) = attribute_delegated(&[process], 100, &[], &jobs);
        assert_eq!(membership, vec![Membership::default()]);
        assert_eq!(jobs, vec![None]);
    }

    #[test]
    fn attribution_follows_parent_chains_to_aterm_and_the_nearest_tab() {
        let procs = vec![
            proc(1, 0, 0, 0, "launchd"),
            proc(100, 1, 0, 0, "aterm"),
            proc(200, 100, 0, 0, "zsh"),
            proc(201, 200, 0, 0, "cargo"),
            proc(202, 201, 0, 0, "rustc"),
            proc(300, 100, 0, 0, "zsh"),
            proc(301, 300, 0, 0, "claude"),
            proc(50, 1, 0, 0, "keeper"),
            proc(400, 50, 0, 0, "zsh"),
            proc(900, 1, 0, 0, "Safari"),
        ];
        let tabs = vec![
            tab(7, 200, "Tab 1", "cargo"),
            tab(8, 300, "Tab 2", "claude"),
            tab(9, 400, "Tab 3", "zsh"),
        ];
        let m = attribute(&procs, 100, &tabs);
        let by_pid: HashMap<u32, Membership> =
            procs.iter().map(|p| p.pid).zip(m.iter().copied()).collect();
        assert_eq!(by_pid[&1], Membership::default());
        assert_eq!(
            by_pid[&100],
            Membership {
                aterm: true,
                tab: None
            }
        );
        assert_eq!(
            by_pid[&202],
            Membership {
                aterm: true,
                tab: Some(0)
            }
        );
        assert_eq!(
            by_pid[&301],
            Membership {
                aterm: true,
                tab: Some(1)
            }
        );
        assert_eq!(
            by_pid[&400],
            Membership {
                aterm: true,
                tab: Some(2)
            }
        );
        assert_eq!(
            by_pid[&50],
            Membership::default(),
            "the keeper is not a tab"
        );
        assert_eq!(by_pid[&900], Membership::default());
    }

    #[test]
    fn a_parent_cycle_from_pid_reuse_terminates() {
        let procs = vec![proc(10, 11, 0, 0, "a"), proc(11, 10, 0, 0, "b")];
        let m = attribute(&procs, 100, &[]);
        assert_eq!(m, vec![Membership::default(); 2]);
    }

    fn sample(at_s: u64, busy: &[u64], procs: Vec<ProcSample>) -> Sample {
        Sample {
            at_ns: at_s * 1_000_000_000,
            cores: Some(
                busy.iter()
                    .map(|&b| Ticks {
                        busy: b,
                        total: at_s * 100,
                    })
                    .collect(),
            ),
            clusters: vec![
                ClusterSpec {
                    name: "Performance".into(),
                    logical: vec![2, 3],
                },
                ClusterSpec {
                    name: "Efficiency".into(),
                    logical: vec![0, 1],
                },
            ],
            mem: Some(MemSample {
                total: 32 << 30,
                app: 10 << 30,
                wired: 3 << 30,
                compressed: Some(1 << 30),
                cached: 8 << 30,
                free: 10 << 30,
                swap_used: 0,
                swap_total: 0,
                pressure: Pressure::Normal,
                commit: None,
                ..MemSample::default()
            }),
            gpu: Some(GpuSample {
                device_pct: Some(4),
                cores: Some(10),
                ..GpuSample::default()
            }),
            volume: None,
            procs: Some(procs),
            self_pid: 100,
            tabs: vec![tab(7, 200, "Tab 1", "cargo")],
            ..Sample::default()
        }
    }

    #[test]
    fn each_process_is_rated_over_its_own_reads() {
        let stamped = |cpu_ms, at_ms: u64| ProcSample {
            at_ns: Some(at_ms * 1_000_000),
            ..proc(201, 200, cpu_ms, 300, "cargo")
        };
        let mut v = Vitals::new();
        v.absorb(&sample(
            10,
            &[0, 0, 0, 0],
            vec![stamped(0, 9_000), proc(900, 1, 0, 900, "Safari")],
        ));
        let s = v.absorb(&sample(
            11,
            &[0, 0, 100, 100],
            vec![stamped(1_900, 10_900), proc(900, 1, 1_900, 900, "Safari")],
        ));
        let cores = |pid| {
            s.procs
                .iter()
                .find(|p| p.pid == pid)
                .and_then(|p| p.cores)
                .expect("a rate")
        };
        assert!((cores(201) - 1.0).abs() < 1e-6, "{}", cores(201));
        assert!((cores(900) - 1.9).abs() < 1e-6, "{}", cores(900));
    }

    #[test]
    fn the_busiest_processes_carry_their_scope_and_cores_keep_a_minute() {
        let mut v = Vitals::new();
        let p0 = vec![
            proc(100, 1, 1000, 200, "aterm"),
            proc(200, 100, 0, 5, "zsh"),
            proc(201, 200, 5000, 300, "cargo"),
            proc(900, 1, 0, 900, "Safari"),
            proc(901, 1, 0, 9, "idle"),
        ];
        let _ = v.absorb(&sample(10, &[0, 0, 0, 0], p0));
        let p1 = vec![
            proc(100, 1, 1200, 200, "aterm"),
            proc(200, 100, 0, 5, "zsh"),
            proc(201, 200, 5900, 300, "cargo"),
            proc(900, 1, 300, 900, "Safari"),
            proc(901, 1, 0, 9, "idle"),
        ];
        let s = v.absorb(&sample(11, &[0, 0, 100, 50], p1));
        let names: Vec<&str> = s.top.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            ["cargo", "Safari", "aterm"],
            "busiest first, idle left out"
        );
        assert_eq!(s.top[0].scope, ProcScope::Tab(7));
        assert_eq!(s.top[1].scope, ProcScope::Other);
        assert_eq!(s.top[2].scope, ProcScope::Aterm);
        assert_eq!(s.top[0].footprint, Some(300 << 20));
        let cpu = s.cpu.as_ref().expect("an interval");
        assert_eq!(cpu.core_history.len(), 4);
        assert_eq!(cpu.core_history[2].last().copied().flatten(), Some(1.0));
        for at in 12..100 {
            let _ = v.absorb(&sample(
                at,
                &[0, 0, 100 * (at - 10), 50 * (at - 10)],
                Vec::new(),
            ));
        }
        let s = v.absorb(&sample(100, &[0, 0, 9000, 4500], Vec::new()));
        let cpu = s.cpu.as_ref().expect("an interval");
        assert!(cpu.core_history.iter().all(|h| h.len() <= CORE_HISTORY_LEN));
    }

    #[test]
    fn the_busiest_program_counts_every_process_of_its_name() {
        let procs = |rustc_ms: u64, cargo_ms: u64| {
            let mut p = vec![
                proc(100, 1, 0, 200, "aterm"),
                proc(200, 100, 0, 5, "zsh"),
                proc(201, 200, cargo_ms, 300, "cargo"),
            ];
            p.extend((0..9).map(|i| proc(300 + i, 201, rustc_ms, 100, "rustc")));
            p
        };
        let mut v = Vitals::new();
        let _ = v.absorb(&sample(10, &[0, 0, 0, 0], procs(0, 0)));
        let s = v.absorb(&sample(11, &[0, 0, 100, 100], procs(600, 2_000)));
        assert_eq!(s.top.len(), TOP_PROCS);
        assert_eq!(s.top[0].name, "cargo");
        let b = s.facts.busiest.expect("a busy program");
        assert_eq!(
            (b.name.as_str(), b.n, b.scope),
            ("rustc", 9, Some(ProcScope::Tab(7)))
        );
        assert_eq!((b.born_ms, b.unborn), (None, 9), "no birth read, never 0");
        assert!((b.cores - 5.4).abs() < 1e-4, "{b:?}");
        let mut mixed = procs(1_200, 4_000);
        mixed[3].ppid = 1;
        let s = v.absorb(&sample(12, &[0, 0, 200, 200], mixed));
        let b = s.facts.busiest.expect("a busy program");
        assert_eq!((b.n, b.scope), (9, None));
    }

    /// A PROGRAM'S AGE IS ITS OLDEST MEMBER'S, WITHIN ITS SCOPE: nine rustc in Tab 1 born a
    /// minute apart and two older rustc outside aterm are two groups, and the tab's age is its
    /// own oldest member's though that one sits idle. The busiest program's birth is its busy
    /// members' earliest. NEGATIVE CONTROLS: a member with no birth read counts as unborn and
    /// leaves the others' oldest; a group with none read has no age, never 0; a birth after the
    /// reading (a wall-clock step) reads 0, not a wrap.
    #[test]
    fn a_program_groups_age_is_its_oldest_members_within_its_scope() {
        const T0: i64 = 1_790_000_000_000;
        const MIN: i64 = 60_000;
        let born = |p: ProcSample, birth_ms: Option<i64>| ProcSample { birth_ms, ..p };
        let procs = |rustc_ms: u64, other_ms: u64, unread_ms: u64| {
            let mut p = vec![
                proc(100, 1, 0, 200, "aterm"),
                proc(200, 100, 0, 5, "zsh"),
                born(proc(201, 200, 0, 300, "cargo"), Some(T0 - MIN)),
            ];
            p.extend((0..9u32).map(|i| {
                let cpu = if i == 0 { 0 } else { rustc_ms };
                let p = proc(300 + i, 201, cpu, 100, "rustc");
                born(p, Some(T0 + i64::from(i) * MIN))
            }));
            p.push(born(
                proc(900, 1, other_ms, 50, "rustc"),
                Some(T0 - 60 * MIN),
            ));
            p.push(born(proc(901, 1, unread_ms, 50, "rustc"), None));
            p.push(born(proc(950, 1, 0, 10, "mds"), None));
            p
        };
        let at = T0 + 47 * MIN;
        let reading = |at_s: u64, busy: &[u64], procs: Vec<ProcSample>, at_unix_ms: i64| Sample {
            at_unix_ms,
            ..sample(at_s, busy, procs)
        };
        let mut v = Vitals::new();
        let _ = v.absorb(&reading(10, &[0, 0, 0, 0], procs(0, 0, 0), at - 1000));
        let s = v.absorb(&reading(11, &[0, 0, 100, 100], procs(600, 600, 0), at));
        let group = |s: &Snapshot, name: &str, scope: ProcScope| {
            s.programs
                .iter()
                .find(|g| g.name == name && g.scope == scope)
                .cloned()
                .unwrap_or_else(|| panic!("{name} in {scope:?}: {:?}", s.programs))
        };
        let tab = group(&s, "rustc", ProcScope::Tab(7));
        assert_eq!((tab.n, tab.born_ms, tab.unborn), (9, Some(T0), 0));
        assert_eq!(tab.age_ms(at), Some(47 * 60_000), "its idle oldest counts");
        assert!((tab.cores - 4.8).abs() < 1e-4, "{tab:?}");
        assert_eq!(tab.footprint, Some(900 << 20));
        let other = group(&s, "rustc", ProcScope::Other);
        assert_eq!(
            (other.n, other.born_ms, other.unborn),
            (2, Some(T0 - 60 * MIN), 1),
            "its own scope; the unread member is unborn"
        );
        let mds = group(&s, "mds", ProcScope::Other);
        assert_eq!((mds.born_ms, mds.unborn, mds.age_ms(at)), (None, 1, None));
        assert_eq!(tab.age_ms(T0 - 1), Some(0), "a birth after the reading");
        assert_eq!(
            group(&s, "cargo", ProcScope::Tab(7)).born_ms,
            Some(T0 - MIN)
        );
        assert_eq!(
            s.programs[0].name, "rustc",
            "busiest first: {:?}",
            s.programs
        );
        assert_eq!(
            s.procs
                .iter()
                .find(|p| p.pid == 305)
                .and_then(|p| p.age_ms(at)),
            Some(42 * 60_000),
            "a row carries its birth"
        );
        let b = s.facts.busiest.clone().expect("a busy program");
        assert_eq!(
            (b.name.as_str(), b.n, b.scope, b.born_ms, b.unborn),
            ("rustc", 9, None, Some(T0 - 60 * MIN), 0)
        );
        let s = v.absorb(&reading(
            12,
            &[0, 0, 200, 200],
            procs(1_200, 600, 0),
            at + 1000,
        ));
        let b = s.facts.busiest.expect("a busy program");
        assert_eq!(
            (b.n, b.scope, b.born_ms, b.unborn, b.age_ms(at + 1000)),
            (
                8,
                Some(ProcScope::Tab(7)),
                Some(T0 + MIN),
                0,
                Some(46 * 60_000 + 1000)
            ),
            "its busy members' earliest: the idle oldest and the idle outsider are not busy"
        );
        let s = v.absorb(&reading(
            13,
            &[0, 0, 300, 300],
            procs(1_800, 600, 600),
            at + 2000,
        ));
        let b = s.facts.busiest.expect("a busy program");
        assert_eq!(
            (b.n, b.scope, b.born_ms, b.unborn),
            (9, None, Some(T0 + MIN), 1),
            "a busy member whose birth went unread is counted unborn"
        );
    }

    #[test]
    fn the_programs_kept_are_every_aterm_group_and_the_machines_busiest_and_largest() {
        let group = |name: String, scope, cores: f32, footprint: usize| ProgramGroup {
            name,
            scope,
            n: 1,
            cores,
            footprint: Some(footprint as u64),
            born_ms: None,
            unborn: 1,
        };
        let mut all: Vec<ProgramGroup> = (0..3 * TOP_ROWS)
            .map(|i| {
                let cores = i as f32 / 100.0;
                group(
                    format!("other{i}"),
                    ProcScope::Other,
                    cores,
                    3 * TOP_ROWS - i,
                )
            })
            .collect();
        all.extend((0..5).map(|i| group(format!("tab{i}"), ProcScope::Tab(7), 0.0, 1)));
        all.push(group("aterm".into(), ProcScope::Aterm, 0.0, 0));
        let kept = keep_programs(all);
        let ours = kept.iter().filter(|g| g.scope != ProcScope::Other).count();
        assert_eq!(ours, 6, "every aterm and tab group");
        let others: Vec<&str> = kept
            .iter()
            .filter(|g| g.scope == ProcScope::Other)
            .map(|g| g.name.as_str())
            .collect();
        assert_eq!(others.len(), 2 * TOP_ROWS);
        let busiest = format!("other{}", 3 * TOP_ROWS - 1);
        assert_eq!(kept[0].name, busiest, "busiest first");
        assert!(others.contains(&"other0"), "the largest of the rest");
        let neither = format!("other{}", TOP_ROWS + 5);
        assert!(
            !others.contains(&neither.as_str()),
            "neither busy nor large"
        );
    }

    #[test]
    fn cores_and_scopes_come_from_deltas_over_the_interval() {
        let mut v = Vitals::new();
        let p0 = vec![
            proc(100, 1, 1000, 200, "aterm"),
            proc(200, 100, 0, 5, "zsh"),
            proc(201, 200, 5000, 300, "cargo"),
            proc(900, 1, 0, 900, "Safari"),
        ];
        let first = v.absorb(&sample(10, &[0, 0, 0, 0], p0));
        assert!(first.cpu.is_none(), "one reading has no interval");
        assert!(first.aterm.is_none());
        assert!(first.mem.is_some() && first.gpu.is_some());

        let p1 = vec![
            proc(100, 1, 1200, 200, "aterm"),
            proc(200, 100, 0, 5, "zsh"),
            proc(201, 200, 5900, 300, "cargo"),
            proc(900, 1, 300, 900, "Safari"),
        ];
        let s = v.absorb(&sample(11, &[0, 0, 100, 50], p1.clone()));
        let cpu = s.cpu.as_ref().expect("an interval");
        assert_eq!(cpu.total(), 4);
        assert!((cpu.busy.raw - 1.5).abs() < 1e-6, "{:?}", cpu.busy);
        assert_eq!(cpu.cores[2].load.raw, 1.0);
        assert_eq!(cpu.clusters[0].short, "P");
        assert_eq!(cpu.clusters[0].cores, vec![2, 3]);
        assert_eq!(cpu.clusters[1].short, "E");

        let aterm = s.aterm.as_ref().expect("two sweeps");
        assert!((aterm.cores.raw - 1.1).abs() < 1e-5, "{aterm:?}");
        assert_eq!(aterm.procs, 3);
        assert_eq!(aterm.footprint, Some(505 << 20));
        assert_eq!(aterm.top.as_ref().map(|t| t.0.as_str()), Some("cargo"));
        let t = s.tab(7).expect("tab 1");
        assert!((t.scope.cores.raw - 0.9).abs() < 1e-5, "{t:?}");
        assert_eq!(t.scope.procs, 2);
        assert_eq!(s.global.procs, 4);
        assert_eq!(s.global.footprint, Some(14 << 30));

        for at in [12, 13] {
            let mut missing = sample(at, &[0; 4], Vec::new());
            missing.cores = None;
            missing.gpu = None;
            missing.mem = None;
            missing.procs = None;
            if at == 13 {
                missing.cores = Some(Vec::new());
                missing.gpu = Some(GpuSample::default());
                missing.mem = Some(MemSample::default());
            }
            let gap = v.absorb(&missing);
            assert!(gap.cpu.is_none() && gap.gpu.is_none() && gap.mem.is_none());
            assert!(gap.aterm.is_none() && gap.tabs.is_empty());
        }
        let mut recovered = sample(15, &[0, 0, 100, 50], p1.clone());
        recovered.gpu.as_mut().expect("GPU").device_pct = Some(84);
        recovered.mem.as_mut().expect("memory").app = 18 << 30;
        let recovered = v.absorb(&recovered);
        let decay = (-4.0 / crate::trend::FAST_TAU_S).exp();
        let cpu = recovered.cpu.as_ref().expect("CPU recovered");
        assert_eq!(cpu.busy.raw, 0.0, "no new busy ticks");
        assert!((cpu.busy.level - 1.5 * decay).abs() < 1e-5, "{cpu:?}");
        assert!((cpu.cores[2].load.level - decay).abs() < 1e-5);
        let gpu = recovered.gpu.as_ref().expect("GPU recovered");
        let gpu_level = 0.84 + (0.04 - 0.84) * decay;
        assert!((gpu.load.level - gpu_level).abs() < 1e-5, "{gpu:?}");
        let mem = recovered.mem.as_ref().expect("memory recovered");
        let mem_level = 22.0 + (14.0 - 22.0) * decay;
        assert!((mem.used_gib.level - mem_level).abs() < 1e-5, "{mem:?}");
        assert!((recovered.global.footprint_gib.level - mem_level).abs() < 1e-5);
        for history in [&cpu.history, &gpu.history, &mem.history] {
            assert_eq!(history.len(), 6);
            assert_eq!(&history[2..4], &[None, None]);
            assert_eq!(history[4], history[5], "14 holds the reading that spans it");
        }
        let aterm = recovered.aterm.as_ref().expect("aterm recovered");
        let tab = recovered.tab(7).expect("tab recovered");
        assert!((aterm.cores.level - 1.1 * decay).abs() < 1e-5);
        assert!((tab.scope.cores.level - 0.9 * decay).abs() < 1e-5);
        for history in [&aterm.history, &tab.scope.history] {
            assert_eq!(history.len(), 5);
            assert_eq!(&history[1..], &[None, None, Some(0.0), Some(0.0)]);
        }

        for at in [16, 17] {
            let mut unreadable = p1.clone();
            for process in &mut unreadable {
                process.footprint = None;
            }
            let unknown = v.absorb(&sample(at, &[0, 0, 100, 50], unreadable));
            assert_eq!(unknown.aterm.as_ref().expect("aterm sweep").footprint, None);
            assert_eq!(unknown.tab(7).expect("tab sweep").scope.footprint, None);
        }
        let mut footprints = p1;
        for process in &mut footprints {
            process.footprint = process.footprint.map(|bytes| bytes * 2);
        }
        let recovered = v.absorb(&sample(19, &[0, 0, 100, 50], footprints));
        let aterm = recovered.aterm.as_ref().expect("aterm footprint");
        let tab = recovered.tab(7).expect("tab footprint");
        let aterm_level = (1010.0 + (505.0 - 1010.0) * decay) / 1024.0;
        let tab_level = (610.0 + (305.0 - 610.0) * decay) / 1024.0;
        assert_eq!(aterm.footprint, Some(1010 << 20));
        assert_eq!(tab.scope.footprint, Some(610 << 20));
        assert!(
            (aterm.footprint_gib.level - aterm_level).abs() < 1e-5,
            "{aterm:?}"
        );
        assert!(
            (tab.scope.footprint_gib.level - tab_level).abs() < 1e-5,
            "{tab:?}"
        );
    }

    #[test]
    fn a_tab_with_no_program_word_is_named_from_its_shells_biggest_child() {
        let mut v = Vitals::new();
        let procs = || {
            vec![
                proc(200, 100, 0, 5, "zsh"),
                proc(201, 200, 0, 400, "claude"),
                proc(202, 200, 0, 2, "less"),
            ]
        };
        let mut s0 = sample(1, &[0; 4], procs());
        s0.tabs[0].program.clear();
        let _ = v.absorb(&s0);
        let mut s1 = sample(2, &[0; 4], procs());
        s1.tabs[0].program.clear();
        let snap = v.absorb(&s1);
        assert_eq!(snap.tab(7).map(|t| t.program.as_str()), Some("claude"));
        let mut s2 = sample(3, &[0; 4], vec![proc(200, 100, 0, 5, "zsh")]);
        s2.tabs[0].program.clear();
        let snap = v.absorb(&s2);
        assert_eq!(snap.tab(7).map(|t| t.program.as_str()), Some("zsh"));
    }

    #[test]
    fn a_process_seen_once_counts_from_its_second_sweep() {
        let mut v = Vitals::new();
        let _ = v.absorb(&sample(1, &[0; 4], vec![proc(100, 1, 0, 1, "aterm")]));
        let s = v.absorb(&sample(
            2,
            &[0; 4],
            vec![
                proc(100, 1, 0, 1, "aterm"),
                proc(201, 100, 99_000, 1, "new"),
            ],
        ));
        assert_eq!(s.aterm.as_ref().map(|a| a.cores.raw), Some(0.0));
    }

    #[test]
    fn a_closed_tab_drops_its_state() {
        let mut v = Vitals::new();
        let _ = v.absorb(&sample(1, &[0; 4], vec![proc(200, 100, 0, 1, "zsh")]));
        let _ = v.absorb(&sample(2, &[0; 4], vec![proc(200, 100, 0, 1, "zsh")]));
        assert_eq!(v.tabs.len(), 1);
        let mut s = sample(3, &[0; 4], vec![]);
        s.tabs.clear();
        let snap = v.absorb(&s);
        assert!(snap.tabs.is_empty());
        assert!(v.tabs.is_empty());
    }

    #[test]
    fn a_core_count_change_restarts_the_cores_instead_of_misreading_them() {
        let mut v = Vitals::new();
        let _ = v.absorb(&sample(1, &[0; 4], vec![]));
        assert!(v.absorb(&sample(2, &[0; 4], vec![])).cpu.is_some());
        assert!(v.absorb(&sample(3, &[0; 6], vec![])).cpu.is_none());
        assert!(v.absorb(&sample(4, &[0; 6], vec![])).cpu.is_some());
    }

    #[test]
    fn cluster_views_cover_every_core_once() {
        let specs = vec![
            ClusterSpec {
                name: "Super".into(),
                logical: vec![0, 1, 9, 1],
            },
            ClusterSpec {
                name: "Efficiency".into(),
                logical: vec![1, 2],
            },
        ];
        let v = cluster_views(&specs, 4);
        assert_eq!(v[0].cores, vec![0, 1]);
        assert_eq!(v[0].short, "S");
        assert_eq!(v[1].cores, vec![2]);
        assert_eq!(v[2].name, "CPU");
        assert_eq!(v[2].cores, vec![3]);
    }

    #[test]
    fn memory_used_is_app_wired_and_compressed_capped_at_physical() {
        let m = MemSample {
            total: 10,
            app: 6,
            wired: 3,
            compressed: Some(4),
            ..MemSample::default()
        };
        assert_eq!(m.used(), 10);
        let unread = MemSample {
            compressed: None,
            ..m
        };
        assert_eq!(unread.used(), 9);
        assert_eq!(gib(1 << 30), 1.0);
    }

    #[test]
    fn short_labels_keep_numbered_levels_apart() {
        let spec = |name: &str, logical: Vec<u16>| ClusterSpec {
            name: name.into(),
            logical,
        };
        let v = cluster_views(
            &[
                spec("Performance", vec![0]),
                spec("Mid 1", vec![1]),
                spec("Mid 2", vec![2]),
                spec("Efficiency", vec![3]),
            ],
            4,
        );
        let shorts: Vec<&str> = v.iter().map(|c| c.short.as_str()).collect();
        assert_eq!(shorts, ["P", "M1", "M2", "E"]);
        assert_eq!(cluster_views(&[spec("CPU", vec![0, 1])], 2)[0].short, "CPU");
        assert_eq!(cluster_views(&[spec("7", vec![0])], 1)[0].short, "7");
    }

    #[test]
    fn commit_is_near_its_limit_only_where_the_limit_is_enforced() {
        let c = |charge, limit, exact| CommitSample {
            charge,
            limit,
            exact,
        };
        assert_eq!(c(93, 100, true).near_limit(), Some(93));
        assert_eq!(c(90, 100, true).near_limit(), Some(90));
        assert_eq!(c(899, 1000, true).near_limit(), None);
        assert_eq!(c(150, 100, false).near_limit(), None);
        assert_eq!(c(150, 100, false).pct(), Some(150));
        assert_eq!(c(5, 0, true).pct(), None);
        assert_eq!(c(5, 0, true).near_limit(), None);
    }

    use Resource::{Cpu, Disk, Gpu, Memory, Network, Processes, Tokens};

    const T0: i64 = 1_792_000_800_000;

    fn rig(mono_ms: u64, wall_ms: i64, cadence_ms: u32) -> Sample {
        let ticks = mono_ms / 10;
        let mine = |p: ProcSample, written: u64| ProcSample {
            disk_written: Some(written),
            uid: Some(501),
            ..p
        };
        Sample {
            at_ns: mono_ms * 1_000_000,
            cores: Some(vec![
                Ticks {
                    busy: ticks,
                    total: ticks,
                },
                Ticks {
                    busy: ticks / 2,
                    total: ticks,
                },
            ]),
            mem: Some(MemSample {
                total: 32 << 30,
                app: 8 << 30,
                ..MemSample::default()
            }),
            gpu: Some(GpuSample {
                device_pct: Some(12),
                ..GpuSample::default()
            }),
            volume: Some(VolumeSample {
                total: 2_000_000_000_000,
                free: 1_000_000_000_000,
                available: 1_000_000_000_000,
                mount: "/".into(),
            }),
            procs: Some(vec![
                mine(proc(100, 1, mono_ms / 10, 200, "aterm"), 0),
                mine(proc(200, 100, 0, 5, "zsh"), 0),
                mine(proc(201, 200, mono_ms / 2, 300, "cargo"), 2_000 * mono_ms),
            ]),
            self_pid: 100,
            tabs: vec![tab(7, 200, "Tab 1", "claude")],
            at_unix_ms: T0 + wall_ms,
            mono_ms,
            cadence_ms,
            load1: Some(0.5),
            links: Some(vec![LinkSample {
                name: "en0".into(),
                kind: LinkKind::Wifi,
                up: true,
                counts: true,
                rx: 1_000 * mono_ms,
                tx: 250 * mono_ms,
                rate_bps: Some(100_000_000),
                ..LinkSample::default()
            }]),
            proc_limits: ProcLimits {
                system: Some(16_000),
                per_user: Some(10_666),
            },
            pid_counts: (Some(900), Some(500)),
            self_uid: Some(501),
            tokens: vec![TabTokens {
                key: 7,
                session: "s1".into(),
                out_total: 600 * mono_ms / 1000,
                in_total: 6_000 * mono_ms / 1000,
                complete: true,
                ..TabTokens::default()
            }],
            ..Sample::default()
        }
    }

    fn at(t_ms: u64, cadence_ms: u32) -> Sample {
        rig(t_ms, t_ms as i64, cadence_ms)
    }

    fn open(s: &Snapshot, r: Resource) -> Option<f64> {
        s.resources[r.index()].ring.last().and_then(|c| c.mean)
    }

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() <= 1e-6 * b.abs().max(1.0))
    }

    #[test]
    fn unknown_is_never_zero() {
        let mut v = Vitals::new();
        let blank = |t: i64| Sample {
            at_unix_ms: T0 + t * 1000,
            mono_ms: t as u64 * 1000,
            cadence_ms: 1000,
            tabs: vec![tab(7, 200, "Tab 1", "zsh")],
            ..Sample::default()
        };
        let mut s = Snapshot::default();
        for t in 0..25 {
            s = v.absorb(&blank(t));
        }
        for res in Resource::ALL {
            let r = s.resource(res, &[7]);
            assert_eq!((r.now, r.heat_now), (None, None), "{res:?}");
            assert!(r.ring.iter().all(|c| c.mean.is_none()), "{res:?}");
            for per in [10, 20, 30] {
                assert!(
                    r.heat_columns(per, 20, s.at_unix_ms)
                        .iter()
                        .all(|c| c.heat.is_none() && c.max.is_none()),
                    "{res:?}"
                );
            }
            assert_eq!(r.aterm.now, None);
            assert_eq!(r.tab.now, None);
        }
        let mut idle = blank(25);
        idle.gpu = Some(GpuSample {
            device_pct: Some(0),
            ..GpuSample::default()
        });
        let s = v.absorb(&idle);
        let gpu = &s.resources[Gpu.index()];
        assert_eq!((gpu.now, gpu.heat_now), (Some(0.0), Some(0.0)));
    }

    #[test]
    fn a_gap_is_a_baseline_not_a_rate() {
        let baseline = Stamp {
            wall_ms: 0,
            mono_ms: 0,
            cadence_ms: 1,
        };
        let rate_at = |wall_ms| {
            Stamp {
                wall_ms,
                mono_ms: 1,
                cadence_ms: 1,
            }
            .rate_dt(Some(baseline))
        };
        for wall in [i64::MIN, i64::MIN + 1, i64::MAX] {
            assert_eq!(rate_at(wall), None, "extreme clock jump {wall}");
        }
        for wall in [-999, 1001] {
            assert_eq!(rate_at(wall), Some(0.001));
        }
        for wall in [-1000, 1002] {
            assert_eq!(rate_at(wall), None);
        }

        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for t in 0..5 {
            s = v.absorb(&at(t * 1000, 1000));
        }
        assert!(close(open(&s, Cpu), 1.5));
        assert!(close(open(&s, Network), 1e6));
        assert!(close(open(&s, Tokens), 36_000.0));
        assert!(close(s.resources[Disk.index()].aterm.now, 2e6));

        let s = v.absorb(&rig(14_000, 604_000, 1000));
        for res in [Cpu, Network, Tokens] {
            assert_eq!(open(&s, res), None, "{res:?}");
            assert_eq!(s.resources[res.index()].now, None, "{res:?}");
        }
        let tx = s.resources[Network.index()].second.as_ref().expect("send");
        assert_eq!(tx.ring.last().and_then(|c| c.mean), None);
        assert_eq!(s.resources[Disk.index()].aterm.now, None, "bytes written");
        assert_eq!(s.resource(Disk, &[7]).tab.now, None);
        assert!(close(open(&s, Memory), (8u64 << 30) as f64));
        assert!(close(open(&s, Disk), 1e12));

        let s = v.absorb(&rig(15_000, 605_000, 1000));
        assert!(close(open(&s, Cpu), 1.5));
        assert!(close(open(&s, Network), 1e6));
        assert!(close(open(&s, Tokens), 36_000.0));
        assert!(close(s.resources[Disk.index()].aterm.now, 2e6));
        assert!(close(s.resource(Disk, &[7]).tab.now, 2e6));

        let renamed = |mono_ms: u64| {
            let mut r = rig(mono_ms, mono_ms as i64 + 590_000, 10_000);
            if let Some(l) = r.links.as_mut().and_then(|l| l.first_mut()) {
                l.name = "en1".into();
            }
            r
        };
        let s = v.absorb(&renamed(25_000));
        assert!(close(open(&s, Cpu), 1.5), "the interval is admitted");
        assert_eq!(open(&s, Network), None);
        let tx = s.resources[Network.index()].second.as_ref().expect("send");
        assert_eq!(tx.ring.last().and_then(|c| c.mean), None);
        let s = v.absorb(&renamed(35_000));
        assert!(close(open(&s, Network), 1e6));
    }

    #[test]
    fn heat_formulas() {
        let day = day_of(T0);
        let learned = |peak: f64, read_s: u32| DailyMax {
            days: vec![(day, peak)],
            read_s,
        };
        let mut v = Vitals::new();
        v.restore_peaks(Peaks {
            net_rx: learned(50e6, NET_LEARN_S),
            net_tx: learned(1e6, NET_LEARN_S),
            tokens: learned(72_000.0, TOKENS_LEARN_S),
            ..Peaks::default()
        });
        let mut s = Snapshot::default();
        for t in 0..6 {
            s = v.absorb(&at(t * 1000, 1000));
        }
        let heat = |s: &Snapshot, r: Resource| s.resources[r.index()].heat_now.map(f64::from);
        assert_eq!(s.resources[Cpu.index()].capacity, Some(2.0));
        assert!(close(heat(&s, Cpu), 0.75));
        assert!(close(heat(&s, Gpu), 0.12));
        let mem = &s.resources[Memory.index()];
        assert!(close(heat(&s, Memory), 0.25));
        assert!(close(
            heat(&s, Memory),
            mem.now.expect("read") / mem.capacity.expect("read")
        ));
        assert!(close(heat(&s, Disk), 0.5));
        let net = &s.resources[Network.index()];
        assert_eq!(net.basis, CapBasis::Peak);
        assert_eq!(net.capacity, Some(12.5e6), "50 MB/s clamped to the link");
        let tx = net.second.as_ref().expect("send");
        assert_eq!(tx.capacity, Some(1e6));
        assert!(close(heat(&s, Network), 0.25), "{net:?}");
        assert!(tx.binds && !net.first().binds);
        assert_eq!(net.binding().now, tx.now);
        assert_eq!(s.resources[Tokens.index()].basis, CapBasis::Peak);
        assert!(close(heat(&s, Tokens), 0.5));
        assert!(close(heat(&s, Processes), 900.0 / 16_000.0));
        assert!(
            !s.resources[Processes.index()]
                .second
                .as_ref()
                .expect("yours")
                .binds
        );
        let mut crowded = at(6000, 1000);
        crowded.pid_counts = (Some(900), Some(9_000));
        let s = v.absorb(&crowded);
        let yours = s.resources[Processes.index()]
            .second
            .clone()
            .expect("yours");
        assert!(yours.binds);
        assert!(close(
            heat(&s, Processes),
            (6.0 * 500.0 + 9_000.0) / 7.0 / 10_666.0
        ));

        let col = |s: &Snapshot| {
            s.resources[Tokens.index()].heat_columns(10, 1, T0 + 6_000)[0]
                .heat
                .map(f64::from)
        };
        assert!(close(col(&s), 0.5));
        let mut raised = v.peaks().clone();
        raised.tokens = learned(144_000.0, TOKENS_LEARN_S);
        v.restore_peaks(raised);
        let s = v.absorb(&at(7000, 1000));
        assert!(close(col(&s), 0.25));

        let mut burst = at(8000, 1000);
        if let Some(l) = burst.links.as_mut().and_then(|l| l.first_mut()) {
            l.rx += 1_000_000_000;
        }
        let s = v.absorb(&burst);
        let net = &s.resources[Network.index()];
        assert_eq!(net.heat_now, Some(1.0));
        assert_eq!(net.capacity, Some(12.5e6));
        assert!(net.first().binds);
        let cells = net.heat_columns(10, 60, T0 + 8000);
        assert!(
            cells
                .iter()
                .all(|c| c.heat.is_none_or(|h| h <= 1.0) && c.max.is_none_or(|m| m <= 1.0)),
            "{cells:?}"
        );
        assert_eq!(cells.last().and_then(|c| c.max), Some(1.0));

        let mut fresh = Vitals::new();
        let mut s = Snapshot::default();
        for t in 0..3 {
            s = fresh.absorb(&at(t * 1000, 1000));
        }
        let net = &s.resources[Network.index()];
        assert_eq!((net.basis, net.capacity), (CapBasis::Link, Some(12.5e6)));
        assert!(close(heat(&s, Network), 0.08));
        let tok = &s.resources[Tokens.index()];
        assert_eq!((tok.basis, tok.capacity), (CapBasis::Learning, None));
        assert_eq!(tok.heat_now, None);
        assert!(
            tok.now.is_some(),
            "the rate is read while the peak is learned"
        );
    }

    #[test]
    fn gap_rule_across_cadence_switches() {
        let mut readings: Vec<(u64, u32, bool)> =
            (0..10).map(|t| (t * 1000, 1000, false)).collect();
        readings.push((14_000, 5_000, true));
        readings.push((19_000, 5_000, false));
        readings.push((20_000, 500, true));
        readings.extend((1..20).map(|i| (20_000 + i * 500, 500, false)));
        readings.push((39_500, 10_000, true));
        readings.push((49_500, 10_000, false));
        readings.push((58_500, 1_000, true));
        readings.push((59_500, 1_000, false));
        let mut v = Vitals::new();
        for (t, cadence, first) in readings {
            let s = v.absorb(&at(t, cadence));
            if first {
                for res in [Cpu, Network, Tokens] {
                    assert!(
                        open(&s, res).is_some(),
                        "{res:?} after the switch at {t} ms"
                    );
                }
                assert!(s.resources[Disk.index()].aterm.now.is_some(), "{t} ms");
            }
        }
        let s = v.absorb(&at(69_500, 1_000));
        for res in [Cpu, Network, Tokens] {
            assert_eq!(open(&s, res), None, "{res:?} after missed readings");
        }
        let s = v.absorb(&at(79_500, 10_000));
        assert!(open(&s, Cpu).is_some());
        let s = v.absorb(&rig(89_500, 669_500, 10_000));
        for res in [Cpu, Network, Tokens] {
            assert_eq!(open(&s, res), None, "{res:?} across a sleep");
        }
    }

    #[test]
    fn now_is_one_value() {
        let mut v = Vitals::new();
        let used = |i: u64| (8u64 << 30) + i * (64 << 20);
        let mut s = Snapshot::default();
        for i in 0..75u64 {
            let mut r = at(i * 1000, 1000);
            r.mem.as_mut().expect("memory").app = used(i);
            if let Some(p) = r.procs.as_mut().and_then(|p| p.first_mut()) {
                p.footprint = Some((200 + 10 * i) << 20);
            }
            s = v.absorb(&r);
        }
        let want = (65..75).map(|i| used(i) as f64).sum::<f64>() / 10.0;
        let mem = s.resource(Memory, &[7]);
        assert!(close(mem.now, want), "{:?} vs {want}", mem.now);
        assert!(close(
            mem.heat_now.map(f64::from),
            want / (32u64 << 30) as f64
        ));
        let want_aterm = (65..75u64)
            .map(|i| ((200 + 10 * i + 5 + 300) << 20) as f64)
            .sum::<f64>()
            / 10.0;
        assert!(close(mem.aterm.now, want_aterm));
        assert!(close(mem.tab.now, (305u64 << 20) as f64));
        let opens: Vec<Option<f64>> = [(10, 60), (20, 30), (30, 20)]
            .into_iter()
            .map(|(per, n)| {
                mem.columns(per, n, s.at_unix_ms)
                    .last()
                    .and_then(|c| c.mean)
            })
            .collect();
        for o in &opens {
            assert!(o.is_some_and(|o| (o - want).abs() > 1.0), "{opens:?}");
        }
        assert_ne!(opens[0], opens[1]);
        assert_eq!(s.resource(Memory, &[]).now, mem.now);
        assert_eq!(s.resource(Memory, &[99]).aterm, mem.aterm);
    }

    /// A reading at `t_ms` whose memory is `(GiB in use, pressure)`; `None` reads no memory.
    fn pressed(t_ms: u64, cadence_ms: u32, reading: Option<(u64, Pressure)>) -> Sample {
        let mut s = at(t_ms, cadence_ms);
        match reading {
            Some((used_gib, pressure)) => {
                let m = s.mem.as_mut().expect("the rig reads memory");
                m.app = used_gib << 30;
                m.pressure = pressure;
            }
            None => s.mem = None,
        }
        s
    }

    #[test]
    fn memory_keeps_the_worst_pressure_of_each_bucket_and_an_unread_bucket_hollow() {
        use Pressure::{Critical, Normal, Unknown, Warn};
        let reading = |t: u64| match t {
            3 => Some(Critical),
            5 | 40 => Some(Warn),
            30..40 => Some(Unknown),
            41 => None,
            _ => Some(Normal),
        };
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for t in (0..43u64).filter(|t| !(20..30).contains(t)) {
            s = v.absorb(&pressed(t * 1000, 1000, reading(t).map(|p| (8, p))));
        }
        let mem = &s.resources[Memory.index()];
        assert_eq!(
            means(&mem.ring)
                .iter()
                .map(Option::is_some)
                .collect::<Vec<_>>(),
            [true, true, false, true, true],
            "the run behind it"
        );
        assert_eq!(
            *mem.pressure,
            [Some(Critical), Some(Normal), None, None, Some(Warn)],
            "the worst of 0-9 s is its one critical reading; 20-29 s read nothing; 30-39 s read \
             memory but no pressure; 40-42 s keeps its warn past an unread and a normal reading"
        );
    }

    #[test]
    fn memory_pressure_keeps_two_minutes_second_for_second_beside_used() {
        use Pressure::{Critical, Normal, Unknown};
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for i in 0..40u64 {
            let p = match i {
                5 => Some(Critical),
                8 | 9 => Some(Unknown),
                12..32 | 34 | 35 => None,
                _ => Some(Normal),
            };
            s = v.absorb(&pressed(i * 500, 500, p.map(|p| (8, p))));
        }
        let m = s.mem.as_ref().expect("memory read last");
        assert_eq!(
            m.pressure_history.len(),
            m.history.len(),
            "the same seconds"
        );
        assert_eq!(m.pressure_history.len(), 20);
        assert_eq!(
            m.pressure_history[2],
            Some(Critical),
            "a second keeps its worst"
        );
        assert_eq!(
            m.pressure_history[4], None,
            "memory but no pressure is hollow"
        );
        assert!(
            m.pressure_history[6..16].iter().all(Option::is_none),
            "a long silence is hollow, never normal"
        );
        assert_eq!(
            (m.history[17], m.pressure_history[17]),
            (None, None),
            "a second read without memory between two normal ones is hollow, not filled"
        );
        assert_eq!(m.pressure_history[19], Some(Normal));
    }

    #[test]
    fn memory_pressure_columns_align_with_the_runs_columns() {
        use Pressure::{Critical, Normal, Warn};
        let level = |t: u64| match t / 10 {
            7 => Critical,
            18 | 19 => Warn,
            _ => Normal,
        };
        // Used memory rises with the level, so the largest reading in a run's column names the
        // worst level its buckets read: a column-for-column oracle that knows no column edge.
        let used = |p: Pressure| match p {
            Critical => 16,
            Warn => 12,
            _ => 8,
        };
        let worst_of = |max: Option<f64>| {
            max.map(|b| {
                [Critical, Warn, Normal]
                    .into_iter()
                    .find(|&p| b >= (used(p) << 30) as f64)
                    .unwrap_or(Normal)
            })
        };
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for t in (0..400u64).filter(|t| !(250..275).contains(t)) {
            s = v.absorb(&pressed(t * 1000, 1000, Some((used(level(t)), level(t)))));
        }
        let mem = &s.resources[Memory.index()];
        assert_eq!(mem.pressure.len(), mem.ring.len(), "one level per run cell");
        assert_eq!(mem.ring.len(), 40);
        for (i, (cell, p)) in mem.ring.iter().zip(mem.pressure.iter()).enumerate() {
            assert_eq!(*p, worst_of(cell.max), "cell {i}");
        }
        for (per, n) in [(10, 60), (20, 30), (30, 20), (60, 10), (60, 60)] {
            let cols = mem.columns(per, n, s.at_unix_ms);
            let worst = mem.pressure_columns(per, n, s.at_unix_ms);
            let want: Vec<_> = cols.iter().map(|c| worst_of(c.max)).collect();
            assert_eq!(worst, want, "{per} s columns");
            for p in [Some(Critical), Some(Warn), Some(Normal), None] {
                assert!(worst.contains(&p), "{per} s: {p:?} drawn");
            }
        }
        assert_eq!(
            mem.pressure_columns(10, 60, s.at_unix_ms)[45..47],
            [None, None],
            "250-269 s read nothing"
        );
    }

    #[test]
    fn without_a_pressure_reading_the_pressure_run_is_hollow_never_normal() {
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for t in 0..95u64 {
            s = v.absorb(&pressed(t * 1000, 1000, Some((8, Pressure::Unknown))));
        }
        let mem = &s.resources[Memory.index()];
        assert!(mem.ring.iter().all(|c| c.mean.is_some()), "memory is read");
        assert_eq!(mem.pressure.len(), mem.ring.len());
        assert!(mem.pressure.iter().all(Option::is_none));
        let m = s.mem.as_ref().expect("memory");
        assert_eq!(m.pressure_history.len(), m.history.len());
        assert!(m.pressure_history.iter().all(Option::is_none));
        for res in Resource::ALL {
            let r = &s.resources[res.index()];
            assert!(res == Memory || r.pressure.is_empty(), "{res:?} keeps none");
            assert!(
                r.pressure_columns(10, 60, s.at_unix_ms)
                    .iter()
                    .all(Option::is_none),
                "{res:?}"
            );
        }
    }

    #[test]
    fn hidden_reads_fill_global_rows() {
        let mut v = Vitals::new();
        let hidden = |t_s: u64| Sample {
            procs: None,
            machine_only: true,
            ..at(t_s * 1000, 10_000)
        };
        let mut s = Snapshot::default();
        for i in 0..=60 {
            s = v.absorb(&hidden(10 * i));
        }
        for res in Resource::ALL {
            let r = s.resource(res, &[7]);
            let read: Vec<bool> = r.ring.iter().map(|c| c.mean.is_some()).collect();
            assert_eq!(read.len(), 61, "{res:?}: one bucket per 10 s");
            let counter = matches!(res, Cpu | Network | Tokens);
            assert_eq!(read[0], !counter, "{res:?}");
            assert!(read[1..].iter().all(|&r| r), "{res:?}: {read:?}");
            assert!(r.now.is_some(), "{res:?}");
            assert_eq!((r.aterm.now, r.tab.now), (None, None), "{res:?}");
        }
        let _ = v.absorb(&at(601_000, 1000));
        let s = v.absorb(&at(602_000, 1000));
        for res in [Cpu, Memory, Disk, Tokens, Processes] {
            let r = s.resource(res, &[7]);
            assert!(r.aterm.now.is_some() && r.tab.now.is_some(), "{res:?}");
        }
        for res in [Gpu, Network] {
            let r = s.resource(res, &[7]);
            assert_eq!(r.aterm.missing, Some(NoFigure::NotPerApp));
            assert_eq!(r.tab.missing, Some(NoFigure::NotPerApp));
        }
    }

    /// GPU per scope from two client reads: each client counts in its process's scope, a reused
    /// pid's time stays with services as unclaimed, the rows hold this tab ≤ all tabs ≤ the Mac's
    /// (a device reading of 0 included), and "not per app" shows only while no client read is held.
    #[test]
    fn gpu_time_per_scope_comes_from_the_client_reads() {
        use crate::gpu_clients::{GpuClientRow, GpuClientsSample};
        const MS: u64 = 1_000_000;
        let row = |entry: u64, pid: u32, name: &str, gpu_ms: u64| GpuClientRow {
            entry,
            pid,
            name: name.into(),
            gpu_ns: gpu_ms * MS,
        };
        let reading = |t_ms: u64, device: u8, ran: Option<u64>| {
            let mut s = at(t_ms, 1000);
            s.gpu.as_mut().expect("GPU").device_pct = Some(device);
            s.procs.as_mut().expect("sweep").push(ProcSample {
                uid: Some(88),
                ..proc(601, 1, 0, 400, "WindowServer")
            });
            s.gpu_clients = ran.map(|k| GpuClientsSample {
                at_ns: s.at_ns,
                complete: true,
                entries: vec![1, 2, 3, 4],
                rows: vec![
                    row(1, 100, "aterm", 200 * k),
                    row(2, 201, "cargo", 100 * k),
                    row(3, 601, "WindowServer", 40 * k),
                    row(4, 200, "bash", 1000 * k),
                ],
            });
            s
        };
        for device in [12u8, 0] {
            let mut v = Vitals::new();
            let first = v.absorb(&reading(1000, device, Some(0)));
            let r = first.resource(Gpu, &[7]);
            assert_eq!(
                r.aterm.missing,
                Some(NoFigure::NotPerApp),
                "a baseline only"
            );
            assert_eq!(first.aterm.as_ref().and_then(|a| a.gpu), None);
            let _ = v.absorb(&reading(2000, device, None));
            let s = v.absorb(&reading(3000, device, Some(1)));
            let gpu_of = |pid: u32| s.procs.iter().find(|p| p.pid == pid).and_then(|p| p.gpu);
            let near = |a: Option<f32>, b: f32| a.is_some_and(|a| (a - b).abs() < 1e-5);
            assert!(near(gpu_of(100), 0.10), "{:?}", s.procs);
            assert!(near(gpu_of(201), 0.05));
            assert!(near(gpu_of(601), 0.02));
            assert_eq!(gpu_of(200), Some(0.0), "a reused pid takes no GPU time");
            assert!(near(s.aterm.as_ref().and_then(|a| a.gpu), 0.15));
            assert!(near(s.tab(7).and_then(|t| t.scope.gpu), 0.05));
            let split = s.gpu.as_ref().and_then(|g| g.split).expect("a split");
            assert!((split.clients - 0.67).abs() < 1e-5, "{split:?}");
            assert!((split.aterm - 0.15).abs() < 1e-5, "{split:?}");
            // WindowServer's 0.02 and the reused pid's unclaimed 0.5.
            assert!(split.others.abs() < 1e-6 && (split.services - 0.52).abs() < 1e-5);

            let r = s.resource(Gpu, &[7]);
            assert_eq!((r.aterm.missing, r.tab.missing), (None, None));
            let (mac, all, tab) = (r.now.expect("Mac"), r.aterm.now.expect("all"), r.tab.now);
            let tab = tab.expect("tab");
            assert!(tab <= all && all <= mac, "{tab} ≤ {all} ≤ {mac}");
            assert!(close(Some(all), f64::from(device).min(15.0)), "{all}");
            assert!(close(Some(tab), f64::from(device).min(5.0)), "{tab}");

            let last = |run: &[Cell]| run.last().and_then(|c| c.mean).expect("a cell");
            let (mac_run, all_run) = (last(&r.ring), last(&r.aterm.run));
            let tab_run = last(&r.tab.run);
            assert!(
                tab_run <= all_run && all_run <= mac_run,
                "each scope's 10 s cell: {tab_run} ≤ {all_run} ≤ {mac_run}"
            );

            let max_ms = crate::explain::WINDOW_MAX_NS / MS;
            let later = v.absorb(&reading(3000 + max_ms + 1000, device, None));
            let r = later.resource(Gpu, &[7]);
            assert_eq!(r.aterm.missing, Some(NoFigure::NotPerApp), "expired");
            assert_eq!(later.aterm.as_ref().and_then(|a| a.gpu), None);
        }
    }

    use crate::ring::RING_BUCKETS;

    const MIB: f64 = (1u64 << 20) as f64;

    fn scope_ring(v: &Vitals, key: Option<u64>, res: Resource) -> Option<&BucketRing> {
        match key {
            Some(key) => v.rows.tabs.get(&key)?.ring(res),
            None => v.rows.aterm.ring(res),
        }
    }

    fn means(cells: &[Cell]) -> Vec<Option<f64>> {
        cells.iter().map(|c| c.mean).collect()
    }

    fn whole(t_s: u64) -> Sample {
        at(t_s * 1000, 10_000)
    }

    #[test]
    fn each_scope_keeps_the_hour_and_a_snapshot_carries_its_last_ten_minutes() {
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for i in 0..=420 {
            s = v.absorb(&whole(10 * i));
        }
        for (res, tab, aterm) in [
            (Cpu, 0.5, 0.6),
            (Memory, 305.0 * MIB, 505.0 * MIB),
            (Processes, 2.0, 3.0),
            (Disk, 2e6, 2e6),
        ] {
            let r = s.resource(res, &[7]);
            for (key, fig, want) in [(Some(7), &r.tab, tab), (None, &r.aterm, aterm)] {
                assert_eq!(
                    scope_ring(&v, key, res).map(BucketRing::len),
                    Some(RING_BUCKETS),
                    "{res:?} {key:?}: the model keeps the hour"
                );
                assert_eq!(fig.run.len(), SCOPE_RUN_BUCKETS, "{res:?} {key:?}");
                assert_eq!(fig.run_k, r.ring_k, "{res:?} {key:?}: ends with the Mac's");
                assert!(
                    fig.run.iter().all(|c| close(c.mean, want)),
                    "{res:?} {key:?}: {:?}",
                    means(&fig.run)
                );
                assert!(close(fig.now, want), "{res:?} {key:?}: {:?}", fig.now);
            }
        }
    }

    #[test]
    fn a_scope_run_never_exceeds_its_container() {
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        let (mut cargo_ms, mut helper_ms, mut child_ms) = (0, 0, 0);
        for i in 0..90u64 {
            cargo_ms += (i * 7 % 5) * 800;
            helper_ms += (i * 3 % 4) * 500;
            child_ms += i % 2 * 1_500;
            let mut r = at(i * 2_000, 2_000);
            let procs = r.procs.as_mut().expect("swept");
            if let Some(cargo) = procs.iter_mut().find(|p| p.pid == 201) {
                cargo.cpu_ns = Some(cargo_ms * 1_000_000);
                cargo.footprint = Some((100 + i * 13 % 9 * 50) << 20);
            }
            if i % 3 != 0 {
                procs.push(ProcSample {
                    disk_written: Some(i * 4_000_000),
                    uid: Some(501),
                    ..proc(202, 201, child_ms, 40 + i % 5 * 20, "rustc")
                });
            }
            if i % 4 != 1 {
                procs.push(ProcSample {
                    disk_written: Some(i * 1_000_000),
                    uid: Some(501),
                    ..proc(101, 100, helper_ms, 70 + i % 7 * 10, "helper")
                });
            }
            s = v.absorb(&r);
        }
        for res in [Cpu, Memory, Processes, Disk] {
            let r = s.resource(res, &[7]);
            let tab = r.tab.columns(10, 60, s.at_unix_ms);
            let aterm = r.aterm.columns(10, 60, s.at_unix_ms);
            let both: Vec<(f64, f64)> = tab
                .iter()
                .zip(&aterm)
                .filter_map(|(t, a)| t.mean.zip(a.mean))
                .collect();
            assert!(both.len() >= 15, "{res:?}: {both:?}");
            let mut seen: Vec<f64> = both.iter().map(|&(t, _)| t).collect();
            seen.dedup();
            assert!(seen.len() > 1, "{res:?}: the load varies: {both:?}");
            for (t, a) in both {
                assert!(
                    t <= a + 1e-6 * a.abs().max(1.0),
                    "{res:?}: tab {t} > all tabs {a}"
                );
            }
        }
    }

    #[test]
    fn hidden_time_is_hollow_in_scope_runs_never_zero() {
        let mut v = Vitals::new();
        let hidden = |t_s: u64| Sample {
            procs: None,
            machine_only: true,
            ..whole(t_s)
        };
        let mut s = Snapshot::default();
        for i in 0..60 {
            let t_s = 10 * i;
            s = v.absorb(&if (30..48).contains(&i) {
                hidden(t_s)
            } else {
                whole(t_s)
            });
            if i == 40 {
                let r = s.resource(Memory, &[7]);
                assert_eq!(r.tab.now, None, "no figure while hidden");
                assert_eq!(r.tab.run.len(), 29, "the run is carried while hidden");
            }
        }
        let now = s.at_unix_ms;
        assert!(
            s.resources[Memory.index()]
                .columns(10, 60, now)
                .iter()
                .all(|c| c.mean.is_some()),
            "the Mac reads throughout"
        );
        for res in [Cpu, Memory, Disk, Processes] {
            let r = s.resource(res, &[7]);
            let back = if matches!(res, Cpu | Disk) { 49 } else { 48 };
            let hidden_span: Vec<usize> = std::iter::once(0).chain(30..back).collect();
            for fig in [&r.tab, &r.aterm] {
                let cols = fig.columns(10, 60, now);
                let hollow: Vec<usize> = (0..cols.len())
                    .filter(|&j| cols[j].mean.is_none())
                    .collect();
                assert_eq!(
                    hollow, hidden_span,
                    "{res:?}: a scope reads from its second sweep, unread is hollow, and a \
                     rate's first sweep back is a baseline"
                );
                assert!(
                    cols.iter().all(|c| c.mean != Some(0.0)),
                    "{res:?}: {cols:?}"
                );
            }
        }
    }

    #[test]
    fn a_closed_tab_takes_its_run_with_it() {
        let mut v = Vitals::new();
        for i in 0..6 {
            let _ = v.absorb(&whole(10 * i));
        }
        assert!(v.rows.tabs.contains_key(&7));
        let mut gone = whole(60);
        gone.tabs.clear();
        let s = v.absorb(&gone);
        assert!(!v.rows.tabs.contains_key(&7));
        assert!(s.resources.iter().all(|r| r.tabs.is_empty()));
        assert!(s.resource(Memory, &[7]).tab.run.is_empty());
        let s = v.absorb(&whole(70));
        let back = s.resource(Memory, &[7]);
        assert_eq!(
            means(&back.tab.run),
            [Some(305.0 * MIB)],
            "a returning key starts hollow"
        );
        assert_eq!(back.aterm.run.len(), 7, "aterm's run goes on");
    }

    /// Tab 1 split in two panes, one opened 200 s in, with a 50 s gap in the sweeps: "this tab"
    /// is both panes summed cell by cell, hollow only where no pane read, never past all tabs.
    #[test]
    fn a_split_tab_reads_its_panes_summed() {
        let split = |i: u64| {
            let mut s = whole(10 * i);
            if i >= 20 {
                let ms = 10_000 * i;
                s.tabs.push(TabRoot {
                    tab: 7,
                    ..tab(8, 300, "Tab 1", "")
                });
                let procs = s.procs.as_mut().expect("swept");
                for (pid, ppid, cpu_ms, mib, name, wrote) in [
                    (300, 100, 0, 5, "zsh", 0),
                    (301, 300, ms / 4, 100, "node", 500 * ms),
                ] {
                    procs.push(ProcSample {
                        disk_written: Some(wrote),
                        uid: Some(501),
                        ..proc(pid, ppid, cpu_ms, mib, name)
                    });
                }
            }
            if (40..45).contains(&i) {
                s.procs = None;
                s.machine_only = true;
            }
            s
        };
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for i in 0..60 {
            s = v.absorb(&split(i));
        }
        let now = s.at_unix_ms;
        let same = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(a), Some(b)) => close(Some(a), b),
            (a, b) => a == b,
        };
        for (res, focused, other) in [
            (Cpu, 0.5, 0.25),
            (Memory, 305.0 * MIB, 105.0 * MIB),
            (Processes, 2.0, 2.0),
            (Disk, 2e6, 0.5e6),
        ] {
            let own = |key: u64| {
                s.resources[res.index()]
                    .tabs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, f)| f.clone())
            };
            let (seven, eight) = (s.resource(res, &[7]).tab, s.resource(res, &[8]).tab);
            assert_eq!(
                Some(&seven),
                own(7).as_ref(),
                "{res:?}: one pane is its own"
            );
            assert_eq!(Some(&eight), own(8).as_ref(), "{res:?}");
            assert!(close(seven.now, focused), "{res:?}: {:?}", seven.now);
            assert!(close(eight.now, other), "{res:?}: {:?}", eight.now);
            let r = s.resource(res, &[7, 8]);
            assert!(
                close(r.tab.now, focused + other),
                "{res:?}: {:?}",
                r.tab.now
            );
            assert_eq!(s.resource(res, &[8, 7]).tab, r.tab, "{res:?}: in any order");
            assert!(r.tab.now <= r.aterm.now, "{res:?}: this tab ≤ all tabs");

            let cols = |f: &ScopeFig| f.columns(10, 60, now);
            let (a, b, tab, all) = (cols(&seven), cols(&eight), cols(&r.tab), cols(&r.aterm));
            let back = if matches!(res, Cpu | Disk) { 46 } else { 45 };
            let hollow: Vec<usize> = (0..60).filter(|&j| tab[j].mean.is_none()).collect();
            let unread: Vec<usize> = std::iter::once(0).chain(40..back).collect();
            assert_eq!(hollow, unread, "{res:?}: hollow only where no pane read");
            for j in 0..60 {
                let sum = plus(a[j].mean, b[j].mean, |x, y| x + y);
                assert!(
                    same(tab[j].mean, sum),
                    "{res:?} {j}: {:?} vs {sum:?}",
                    tab[j]
                );
                if let (Some(t), Some(all)) = (tab[j].max, all[j].max) {
                    assert!(t <= all, "{res:?} {j}: tab {t} > all tabs {all}");
                }
            }
            assert!(
                (1..20).all(|j| tab[j] == a[j]),
                "{res:?}: before pane 8 opened"
            );
            assert!(
                (21..40).all(|j| tab[j].mean > a[j].mean),
                "{res:?}: the focused pane alone reads less: {:?} vs {:?}",
                means(&tab[21..40]),
                means(&a[21..40])
            );
        }

        let tokens = s.resource(Tokens, &[7, 8]).tab;
        assert_eq!(
            s.resource(Tokens, &[8]).tab.missing,
            Some(NoFigure::NoAgent)
        );
        assert_eq!(tokens.missing, None, "an agent in either pane is the tab's");
        assert!(tokens.now.is_some() && tokens.now == s.resource(Tokens, &[7]).tab.now);
        let net = s.resource(Network, &[7, 8]).tab;
        assert_eq!(net.missing, Some(NoFigure::NotPerApp));

        assert_eq!(
            [s.tab(7), s.tab(8)].map(|t| t.map(|t| t.tab)),
            [Some(7), Some(7)],
            "each pane carries its tab's key"
        );
        let t = s.tab_of(&[7, 8]).expect("the tab");
        assert_eq!((t.key, t.label.as_str()), (7, "Tab 1"));
        assert_eq!(t.scope.procs, 4);
        assert!(
            (t.scope.cores.raw - 0.75).abs() < 1e-6,
            "{:?}",
            t.scope.cores
        );
        assert_eq!(t.scope.footprint, Some(410 << 20));
        assert!(close(t.scope.write_bps, 2.5e6), "{:?}", t.scope.write_bps);
        assert_eq!(t.scope.top.as_ref().map(|t| t.0.as_str()), Some("cargo"));
        let aterm = s.aterm.as_ref().expect("aterm");
        assert!(t.scope.cores.raw <= aterm.cores.raw && t.scope.procs <= aterm.procs);
        assert_eq!(s.tab_of(&[7]), s.tab(7).cloned(), "one pane is itself");
        assert_eq!(
            s.tab_of(&[8, 7]).map(|t| (t.key, t.scope.procs)),
            Some((8, 4)),
            "the focused pane names it"
        );
        assert_eq!(s.tab_of(&[99]), None);
    }

    /// Two panes at 10% each, each within the Mac's 12%, sum past it, so the split tab's GPU
    /// figure is held at all tabs'.
    #[test]
    fn a_split_tabs_gpu_stays_inside_all_tabs() {
        use crate::gpu_clients::{GpuClientRow, GpuClientsSample};
        let reading = |t_ms: u64, ran: Option<u64>| {
            let mut s = at(t_ms, 1000);
            s.gpu.as_mut().expect("GPU").device_pct = Some(12);
            s.tabs.push(TabRoot {
                tab: 7,
                ..tab(8, 300, "Tab 1", "")
            });
            let procs = s.procs.as_mut().expect("sweep");
            procs.push(proc(300, 100, 0, 5, "zsh"));
            procs.push(proc(301, 300, 0, 50, "node"));
            s.gpu_clients = ran.map(|k| GpuClientsSample {
                at_ns: s.at_ns,
                complete: true,
                entries: vec![1, 2],
                rows: [(1, 201, "cargo"), (2, 301, "node")]
                    .into_iter()
                    .map(|(entry, pid, name)| GpuClientRow {
                        entry,
                        pid,
                        name: name.into(),
                        gpu_ns: 200_000_000 * k,
                    })
                    .collect(),
            });
            s
        };
        let mut v = Vitals::new();
        let _ = v.absorb(&reading(1000, Some(0)));
        let _ = v.absorb(&reading(2000, None));
        let s = v.absorb(&reading(3000, Some(1)));
        for pane in [7, 8] {
            let fig = s.resource(Gpu, &[pane]).tab;
            assert!(close(fig.now, 10.0), "pane {pane}: {:?}", fig.now);
        }
        let r = s.resource(Gpu, &[7, 8]);
        let (mac, all) = (r.now.expect("Mac"), r.aterm.now.expect("all tabs"));
        assert!(close(Some(all), 12.0), "{all}");
        assert!(close(r.tab.now, all), "held at all tabs': {:?}", r.tab.now);
        assert!(all <= mac);
        let last = |run: &[Cell]| run.last().and_then(|c| c.mean).expect("a cell");
        assert!(
            last(&r.tab.run) <= last(&r.aterm.run),
            "{:?}",
            r.tab.run.last()
        );
    }

    /// rustc in both panes of a split tab is one program of Tab 1 among the busiest, the top
    /// processes and the writers; the same panes as two tabs (the control) leave its scope unnamed.
    #[test]
    fn a_split_tabs_processes_are_the_tabs() {
        let read = |tab_of_8: u64| {
            let procs = |t: u64| {
                let wrote = |p: ProcSample, mb: u64| ProcSample {
                    disk_written: Some(t * mb * 1_000_000),
                    ..p
                };
                vec![
                    proc(100, 1, 0, 200, "aterm"),
                    proc(200, 100, 0, 5, "zsh"),
                    wrote(proc(201, 200, t * 500, 100, "rustc"), 1),
                    proc(300, 100, 0, 5, "zsh"),
                    wrote(proc(301, 300, t * 1000, 100, "rustc"), 2),
                ]
            };
            let mut v = Vitals::new();
            let mut s = Snapshot::default();
            for t in [10, 11] {
                let mut swept = Sample {
                    mono_ms: t * 1000,
                    at_unix_ms: T0 + t as i64 * 1000,
                    cadence_ms: 1000,
                    ..sample(t, &[0, 0, 0, 0], procs(t))
                };
                swept.tabs.push(TabRoot {
                    tab: tab_of_8,
                    ..tab(8, 300, "Tab 1", "")
                });
                s = v.absorb(&swept);
            }
            s
        };
        let s = read(7);
        let b = s.facts.busiest.as_ref().expect("a busy program");
        assert_eq!(
            (b.name.as_str(), b.n, b.scope),
            ("rustc", 2, Some(ProcScope::Tab(7)))
        );
        assert!((b.cores - 1.5).abs() < 1e-4, "{b:?}");
        let scopes = |names: Vec<(&str, ProcScope)>| -> Vec<ProcScope> {
            names
                .into_iter()
                .filter(|(n, _)| *n == "rustc")
                .map(|(_, s)| s)
                .collect()
        };
        let top = scopes(s.top.iter().map(|t| (t.name.as_str(), t.scope)).collect());
        assert_eq!(top, [ProcScope::Tab(7); 2], "{:?}", s.top);
        let writers: Vec<_> = s
            .facts
            .writers
            .iter()
            .filter(|w| w.name == "rustc")
            .map(|w| (w.scope, w.n))
            .collect();
        assert_eq!(writers, [(ProcScope::Tab(7), 2)], "one program of Tab 1");
        assert_eq!(s.tab(7).map(|t| t.label.as_str()), Some("Tab 1"));

        let apart = read(8);
        let b = apart.facts.busiest.as_ref().expect("a busy program");
        assert_eq!((b.n, b.scope), (2, None), "CONTROL: two tabs");
    }

    #[test]
    fn gpu_and_network_scope_runs_stay_empty() {
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for i in 0..6 {
            s = v.absorb(&whole(10 * i));
        }
        for res in [Gpu, Network] {
            let r = s.resource(res, &[7]);
            for fig in [&r.aterm, &r.tab] {
                assert_eq!(fig.missing, Some(NoFigure::NotPerApp), "{res:?}");
                assert!(fig.run.is_empty() && fig.now.is_none(), "{res:?}");
            }
            assert!(scope_ring(&v, None, res).is_none(), "{res:?}");
            assert!(scope_ring(&v, Some(7), res).is_none(), "{res:?}");
        }
        assert_eq!(s.resource(Memory, &[7]).tab.run.len(), 5);
    }

    #[test]
    fn a_scope_run_and_its_share_agree() {
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for i in 0..12 {
            let mut r = whole(10 * i);
            r.proc_limits.per_user = Some(1_000);
            s = v.absorb(&r);
        }
        assert_eq!(
            s.resources[Processes.index()].binding().capacity,
            Some(1_000.0),
            "the per-user limit binds, not the first series' system limit"
        );
        for res in [Disk, Gpu, Network] {
            let r = s.resource(res, &[7]);
            assert_eq!(
                s.scope_heat_columns(res, &r.tab, 10, 60, s.at_unix_ms),
                None,
                "{res:?}: a scope's figure is no share of this row"
            );
        }
        assert!(
            s.resource(Disk, &[7])
                .tab
                .run
                .iter()
                .any(|c| c.mean > Some(0.0)),
            "the writes run itself is carried raw"
        );
        for res in [Cpu, Memory, Processes] {
            let r = s.resource(res, &[7]);
            let cap = r.binding().capacity.expect("a capacity");
            for fig in [&r.aterm, &r.tab] {
                let cols = s
                    .scope_heat_columns(res, fig, 10, 60, s.at_unix_ms)
                    .expect("a share of the row");
                assert_eq!(cols.len(), 60);
                let share = fig.now.expect("read") / cap;
                let last = cols.last().and_then(|c| c.heat).map(f64::from);
                assert!(close(last, share), "{res:?}: {last:?} vs {share}");
                assert!(cols[..48].iter().all(|c| c.heat.is_none()), "{res:?}");
            }
        }
    }

    #[test]
    fn token_rate_hygiene() {
        let mut v = Vitals::new();
        let tok = |i: i64, out: u64, unread: usize, more: bool, complete: bool| Sample {
            at_unix_ms: T0 + i * 10_000,
            mono_ms: i as u64 * 10_000,
            cadence_ms: 10_000,
            tabs: vec![tab(7, 200, "Tab 1", "claude"), tab(8, 300, "Tab 2", "zsh")],
            tokens: vec![TabTokens {
                key: 7,
                session: "s".into(),
                out_total: out,
                in_total: out * 10,
                unread,
                unread_more: more,
                complete,
                read_ms: None,
            }],
            ..Sample::default()
        };
        let steps: [(u64, usize, bool, bool, Option<f64>); 11] = [
            (0, 0, false, true, None),
            (6_000, 0, false, true, Some(36_000.0)),
            (12_000, 1, false, true, None),
            (18_000, 1, false, true, Some(36_000.0)),
            (24_000, 1, true, true, None),
            (30_000, 1, true, true, Some(36_000.0)),
            (36_000, 1, true, false, None),
            (42_000, 1, true, true, None),
            (48_000, 1, true, true, Some(36_000.0)),
            (108_000, 1, true, true, None),
            (114_000, 1, true, true, Some(36_000.0)),
        ];
        for (i, (out, unread, more, complete, want)) in steps.into_iter().enumerate() {
            let s = v.absorb(&tok(i as i64, out, unread, more, complete));
            match want {
                Some(w) => assert!(
                    close(open(&s, Tokens), w),
                    "step {i}: {:?}",
                    open(&s, Tokens)
                ),
                None => assert_eq!(open(&s, Tokens), None, "step {i}"),
            }
            assert_eq!(s.facts.agents, 1);
            let t2 = s.resource(Tokens, &[8]).tab;
            assert_eq!(t2.missing, Some(NoFigure::NoAgent), "Tab 2 runs no agent");
        }
        let mut out = 114_000;
        for i in 11..22 {
            out += if i == 16 { 30_000 } else { 6_000 };
            let _ = v.absorb(&tok(i, out, 1, true, true));
        }
        assert_eq!(v.peaks().tokens.peak(day_of(T0)), Some(36_000.0));
    }

    #[test]
    fn token_rates_span_folds_not_readings() {
        let mut v = Vitals::new();
        let reading = |i: u64, k: u64, fold_s: i64, slept_ms: i64| Sample {
            at_unix_ms: T0 + i as i64 * 1000 + slept_ms,
            mono_ms: i * 1000,
            cadence_ms: 1000,
            tabs: vec![tab(7, 200, "Tab 1", "claude")],
            tokens: vec![TabTokens {
                key: 7,
                session: "s".into(),
                out_total: 45_000 * k,
                in_total: 450_000 * k,
                complete: true,
                read_ms: Some(fold_s * 1000),
                ..TabTokens::default()
            }],
            ..Sample::default()
        };
        let per_min = 3_000.0 * 60.0;
        for i in 0..=100u64 {
            let k = (i / 15).min(2);
            let s = v.absorb(&reading(i, k, k as i64 * 15, 0));
            let now = s.resource(Tokens, &[]).now;
            match i {
                0..15 => assert_eq!(open(&s, Tokens), None, "{i}: one fold"),
                15..=90 => {
                    assert!(
                        close(open(&s, Tokens), per_min),
                        "{i}: {:?}",
                        open(&s, Tokens)
                    );
                    assert!(close(now, per_min), "{i}: now {now:?}");
                }
                100 => assert_eq!(open(&s, Tokens), None, "past the hold"),
                _ => {}
            }
        }
        let since = 45_000.0 / 71.0 * 60.0;
        let s = v.absorb(&reading(101, 3, 101, 0));
        assert!(close(open(&s, Tokens), since), "{:?}", open(&s, Tokens));
        let s = v.absorb(&reading(102, 3, 101, 0));
        assert!(close(open(&s, Tokens), since), "held");
        let s = v.absorb(&reading(103, 3, 101, 120_000));
        assert_eq!(open(&s, Tokens), None, "a sleep drops the rate");
        let s = v.absorb(&reading(104, 3, 101, 120_000));
        assert_eq!(open(&s, Tokens), None, "no new fold since the sleep");
        let spaced = |i, k, fold_s, slept_ms| Sample {
            cadence_ms: 10_000,
            ..reading(i, k, fold_s, slept_ms)
        };
        let s = v.absorb(&spaced(111, 4, 111, 120_000));
        assert_eq!(open(&s, Tokens), None, "the first fresh post-sleep fold");
        let s = v.absorb(&spaced(121, 5, 121, 120_000));
        assert!(close(open(&s, Tokens), 4_500.0 * 60.0), "new interval");

        let mut v = Vitals::new();
        let _ = v.absorb(&spaced(0, 0, 0, 0));
        let s = v.absorb(&spaced(10, 1, 10, 0));
        assert!(close(open(&s, Tokens), 4_500.0 * 60.0));
        let s = v.absorb(&spaced(20, 2, 10, 0));
        assert_eq!(open(&s, Tokens), None, "changed totals at the same ms");
        let s = v.absorb(&spaced(30, 3, 30, 0));
        assert!(
            close(open(&s, Tokens), 2_250.0 * 60.0),
            "new totals baseline"
        );
        let s = v.absorb(&spaced(40, 3, 30, 0));
        assert!(
            close(open(&s, Tokens), 2_250.0 * 60.0),
            "identical fold held"
        );
        let mut incomplete = spaced(50, 3, 30, 0);
        incomplete.tokens[0].complete = false;
        let s = v.absorb(&incomplete);
        assert_eq!(open(&s, Tokens), None, "incomplete at the same ms");
        let mut repeated = spaced(60, 3, 30, 0);
        repeated.tokens[0].complete = false;
        let s = v.absorb(&repeated);
        assert_eq!(open(&s, Tokens), None, "an incomplete fold holds no rate");
        let s = v.absorb(&spaced(70, 4, 70, 0));
        assert_eq!(open(&s, Tokens), None, "both folds must be complete");
        let s = v.absorb(&spaced(80, 5, 80, 0));
        assert!(close(open(&s, Tokens), 4_500.0 * 60.0), "complete again");
    }

    fn disk_reading(i: i64, available: u64, facts: Option<DiskFacts>) -> Sample {
        Sample {
            at_unix_ms: T0 + i * 10_000,
            mono_ms: i as u64 * 10_000,
            cadence_ms: 10_000,
            volume: Some(VolumeSample {
                total: 2_000_000_000_000,
                free: available,
                available,
                mount: "/".into(),
            }),
            disk: facts,
            ..Sample::default()
        }
    }

    fn watched(level: DiskLevel) -> Option<DiskFacts> {
        Some(DiskFacts {
            level: Some(level),
            watch: true,
            ..DiskFacts::default()
        })
    }

    #[test]
    fn disk_history_and_rates() {
        let mut v = Vitals::new();
        let mut free = 500_000_000_000u64;
        let mut s = Snapshot::default();
        for i in 0..420 {
            s = v.absorb(&disk_reading(i, free, watched(DiskLevel::Ok)));
            match i {
                356 => assert_eq!(s.facts.disk_rates.rate60, None),
                357 => {
                    assert!(close(s.facts.disk_rates.rate60, 1e7));
                    assert!(close(s.facts.disk_rates.rate10, 1e7));
                    assert!(close(s.facts.disk_rates.rate5, 1e7));
                    assert_eq!(s.facts.filling, None);
                }
                416 => assert_eq!(s.facts.filling, None),
                _ => {}
            }
            free -= 100_000_000;
        }
        let f = s.facts.filling.expect("time to full under a day");
        assert!(!f.burst && !f.outranks_level());
        assert!((f.secs - 458.1e9 / 1e7).abs() < 1.0, "{f:?}");
        assert_eq!(s.resources[Disk.index()].state, None);
        for i in 420..450 {
            free -= 2_000_000_000;
            s = v.absorb(&disk_reading(i, free, None));
        }
        let f = s.facts.filling.expect("a burst");
        assert!(f.burst && f.outranks_level(), "{f:?}");
        assert_eq!(s.resources[Disk.index()].state, Some(State::Warn));
        let w = s.free_history.window(s.at_unix_ms);
        let read: Vec<u64> = w.iter().flatten().copied().collect();
        assert_eq!(read.len(), 8, "75 minutes span 8 ten-minute buckets");
        assert_eq!(read.last(), Some(&free), "the least free space");
        assert!(read.windows(2).all(|p| p[1] < p[0]));
        for i in 450..480 {
            s = v.absorb(&disk_reading(i, free, None));
        }
        let f = s.facts.filling.expect("full within a day");
        assert!(!f.burst && !f.outranks_level(), "{f:?}");
        assert_eq!(s.resources[Disk.index()].state, None);
        let off = DiskFacts {
            level: Some(DiskLevel::Low),
            stuck: 2,
            watch: false,
            ..DiskFacts::default()
        };
        let s = v.absorb(&disk_reading(480, free, Some(off)));
        assert_eq!(s.resources[Disk.index()].state, None);
        let s = v.absorb(&disk_reading(481, free, watched(DiskLevel::Low)));
        assert_eq!(s.resources[Disk.index()].state, Some(State::Warn));
        let s = v.absorb(&disk_reading(482, free, watched(DiskLevel::EmergencyFiles)));
        assert_eq!(s.resources[Disk.index()].state, Some(State::Critical));
    }

    #[test]
    fn missed_readings_and_clock_steps_keep_the_filling_clause() {
        let mut v = Vitals::new();
        let free = |i: i64| 500_000_000_000 - i as u64 * 100_000_000;
        let missed = |i: i64| i == 100 || (200..204).contains(&i);
        for i in (0..640).filter(|&i| !missed(i)) {
            let s = v.absorb(&disk_reading(i, free(i), None));
            if i < 417 {
                continue;
            }
            let unknown = (559..561).contains(&i);
            assert_eq!(s.facts.disk_rates.rate60.is_none(), unknown, "{i}");
            assert_eq!(s.facts.filling.is_some(), !unknown, "{i}");
        }
        let mut back = disk_reading(640, free(640), None);
        back.at_unix_ms -= 7_200_000;
        let s = v.absorb(&back);
        assert!(close(s.facts.disk_rates.rate60, 1e7));
        assert!(s.facts.filling.is_some());
        assert_eq!(
            s.free_history
                .window(s.at_unix_ms)
                .last()
                .copied()
                .flatten(),
            Some(free(640))
        );
    }

    #[test]
    fn a_restored_free_history_reads_on_only_from_its_volume() {
        let mut before = Vitals::new();
        for i in 0..120 {
            let _ = before.absorb(&disk_reading(i, 400_000_000_000 - i as u64, None));
        }
        let saved = before.free_history().saved().expect("20 minutes read");
        assert_eq!(saved.volume.mount, "/");
        let after = disk_reading(200, 1, None);
        let read = |s: &Snapshot| {
            s.free_history
                .window(s.at_unix_ms)
                .into_iter()
                .flatten()
                .count()
        };

        let mut v = Vitals::new();
        v.restore_free_history(FreeHistory::restored(&saved, after.at_unix_ms));
        assert_eq!(
            read(&v.absorb(&after)),
            3,
            "two kept ten minutes and the new one"
        );
        let mut moved = after.clone();
        if let Some(vol) = moved.volume.as_mut() {
            vol.mount = "/Volumes/Backup".into();
        }
        let mut v = Vitals::new();
        v.restore_free_history(FreeHistory::restored(&saved, after.at_unix_ms));
        assert_eq!(read(&v.absorb(&moved)), 1, "another volume starts afresh");
    }

    #[test]
    fn over_is_a_queue_on_the_cores_or_heavy_swapping() {
        let run = |load1: f64, busy_half: bool, swap_kib_per_s: u64| {
            let mut v = Vitals::new();
            let mut s = Snapshot::default();
            for t in 0..5u64 {
                let mut r = at(t * 1000, 1000);
                r.load1 = Some(load1);
                if !busy_half && let Some(c) = r.cores.as_mut() {
                    c[1].busy = c[1].total;
                }
                r.swapped_kib = Some(swap_kib_per_s * t);
                s = v.absorb(&r);
            }
            (
                s.resources[Cpu.index()].over,
                s.resources[Memory.index()].over,
            )
        };
        assert_eq!(run(2.5, false, 0), (true, false));
        assert_eq!(run(1.5, false, 0), (false, false), "no queue");
        assert_eq!(run(2.5, true, 0), (false, false), "1.5 of 2 cores busy");
        assert_eq!(run(0.5, true, 4 * 1024), (false, true));
        assert_eq!(run(0.5, true, 3 * 1024), (false, false));
    }

    #[test]
    fn the_processes_state_follows_the_jobs_well() {
        use crate::jobs::{Findings, JobsView, ZombieParent};
        let zombie = |tab| ZombieParent {
            ppid: 9,
            name: "server".into(),
            count: 3,
            per_min_deci: 0,
            tab,
        };
        let mut s = Snapshot::default();
        s.set_jobs(Some(Arc::new(JobsView::default())));
        assert_eq!(s.resources[Processes.index()].state, None, "a calm machine");
        let view = |tab| JobsView {
            findings: Findings {
                zombies: vec![zombie(tab)],
                ..Findings::default()
            },
            ..JobsView::default()
        };
        s.set_jobs(Some(Arc::new(view(Some(7)))));
        assert_eq!(s.resources[Processes.index()].state, Some(State::Zombies));
        s.set_jobs(Some(Arc::new(view(None))));
        assert_eq!(s.resources[Processes.index()].state, None);
        assert!(State::Critical > State::Warn && State::Warn > State::Stuck);
        assert!(State::Stuck > State::Zombies && State::Quiet > State::Waiting);
        assert!(State::Waiting > State::Stopped);
    }

    #[test]
    fn the_measured_link_holds_while_the_mix_holds() {
        let link = |name: &str, kind, total: u64, rate_bps| LinkSample {
            name: name.into(),
            kind,
            up: true,
            counts: true,
            rx: total,
            tx: 0,
            rate_bps: Some(rate_bps),
            ..LinkSample::default()
        };
        let wifi = |total| link("en0", LinkKind::Wifi, total, 54_000_000);
        let wired = |total| link("en7", LinkKind::Ethernet, total, 1_000_000_000);
        let mut v = Vitals::new();
        let mut read = |t: u64, links: Vec<LinkSample>| {
            let mut s = at(t * 1000, 1000);
            s.links = Some(links);
            let snap = v.absorb(&s);
            let l = snap.facts.link.expect("a counted link");
            let cap = snap.resources[Network.index()].capacity;
            (l.name, cap)
        };
        let (mut w, mut e) = (1_000u64, 1_000u64);
        assert_eq!(
            read(0, vec![wifi(w), wired(e)]),
            ("en7".into(), Some(125e6))
        );
        for t in 1..40 {
            if t % 3 == 0 {
                w += 300;
            } else {
                e += 200;
            }
            assert_eq!(
                read(t, vec![wifi(w), wired(e)]).0,
                "en7",
                "idle reading {t}"
            );
        }
        for t in 40..40 + u64::from(LINK_LEAD_READINGS) {
            w += 2_000_000;
            let want = if t + 1 < 40 + u64::from(LINK_LEAD_READINGS) {
                "en7"
            } else {
                "en0"
            };
            assert_eq!(read(t, vec![wifi(w), wired(e)]).0, want, "busy reading {t}");
        }
        assert_eq!(
            read(45, vec![wifi(w + 2_000_000), wired(e)]),
            ("en0".into(), Some(6.75e6))
        );
        let mut t = 46;
        for burst in [true, true, false, true, true, true, true] {
            if burst {
                e += 10_000_000;
            }
            assert_eq!(read(t, vec![wifi(w + 2_000_000), wired(e)]).0, "en0");
            t += 1;
        }
        assert_eq!(read(t, vec![wired(e + 10_000_000)]).0, "en7");
    }

    #[test]
    fn the_plan_week_survives_a_restart() {
        const DAY: u32 = 20_740;
        let at_s = |day: u32, hour: i64| i64::from(day) * 86_400 + hour * 3_600;
        let reading = |day: u32, hour: i64, used_x10: u16| {
            Some(PlanDay {
                day,
                used_x10,
                resets_at: at_s(DAY + 4, 0),
                at: at_s(day, hour),
            })
        };
        let week = |account: &str, front: bool, reading: Option<PlanDay>| WeekFact {
            account: account.into(),
            front,
            reading,
        };
        let sample = |i: i64, today: u32, weeks: Vec<WeekFact>| Sample {
            at_unix_ms: T0 + i * 1000,
            mono_ms: u64::try_from(i).expect("a reading") * 1000,
            cadence_ms: 1000,
            local_day: today,
            weeks,
            ..Sample::default()
        };
        let used = |run: &PlanRun| -> (String, bool, Vec<u16>) {
            let used = run.week.days.iter().map(|d| d.used_x10).collect();
            (run.week.account.clone(), run.front, used)
        };

        let mut a = Vitals::new();
        let first = a.absorb(&sample(
            0,
            DAY,
            vec![week("c-b", false, reading(DAY, 10, 500))],
        ));
        assert_eq!(
            first.facts.plan_week.as_ref().map(used),
            Some(("c-b".into(), false, vec![500])),
            "no front account: the one read last"
        );
        let _ = a.absorb(&sample(
            1,
            DAY,
            vec![
                week("c-a", true, reading(DAY, 11, 100)),
                week("c-b", false, reading(DAY, 10, 500)),
            ],
        ));
        let s = a.absorb(&sample(
            2,
            DAY + 1,
            vec![
                week("c-b", false, reading(DAY + 1, 10, 600)),
                week("c-a", true, reading(DAY + 1, 11, 150)),
            ],
        ));
        let run = s.facts.plan_week.expect("a run");
        assert_eq!(used(&run), ("c-a".into(), true, vec![100, 150]));
        assert_eq!(run.today, DAY + 1);
        let held = a.peaks().clone();
        let _ = a.absorb(&sample(
            3,
            DAY + 1,
            vec![week("c-a", true, reading(DAY + 1, 11, 150))],
        ));
        assert_eq!(
            *a.peaks(),
            held,
            "the same figure again leaves nothing to write"
        );

        let mut b = Vitals::new();
        b.restore_peaks(a.peaks().clone());
        let s = b.absorb(&sample(0, DAY + 1, Vec::new()));
        assert_eq!(
            s.facts.plan_week.as_ref().map(used),
            Some(("c-a".into(), false, vec![100, 150])),
            "after a restart, the kept week of the account read last"
        );
        let s = b.absorb(&sample(
            1,
            DAY + 1,
            vec![week("c-a", false, None), week("c-b", true, None)],
        ));
        assert_eq!(
            s.facts.plan_week.as_ref().map(used),
            Some(("c-b".into(), true, vec![500, 600])),
            "the front tab's account, though its footer has no figure now"
        );
        let s = b.absorb(&sample(2, DAY + 7, vec![week("c-b", true, None)]));
        assert_eq!(
            s.facts.plan_week.as_ref().map(used),
            Some(("c-b".into(), true, vec![600])),
            "the run is the 7 days ending today"
        );
        let s = b.absorb(&sample(3, DAY + 8, vec![week("c-b", true, None)]));
        assert_eq!(s.facts.plan_week, None, "nothing read in the last 7 days");
        assert_eq!(*b.peaks(), held, "a reading with no figure keeps nothing");
    }

    /// A process of tab 7's `cargo` (pid 201), own user's, with its own start.
    fn child(pid: u32, start: u64, name: &str, written: u64) -> ProcSample {
        ProcSample {
            start: Some(start),
            disk_written: Some(written),
            uid: Some(501),
            ..proc(pid, 201, 10, 20, name)
        }
    }

    fn write_bps(s: &Snapshot, pid: u32) -> Option<f64> {
        s.procs
            .iter()
            .find(|p| p.pid == pid)
            .and_then(|p| p.write_bps)
    }

    fn writer<'a>(s: &'a Snapshot, name: &str) -> Option<&'a Writer> {
        s.facts.writers.iter().find(|w| w.name == name)
    }

    #[test]
    fn a_process_born_between_sweeps_counts_what_it_wrote_since_birth() {
        let mut v = Vitals::new();
        let _ = v.absorb(&at(0, 1000));
        let _ = v.absorb(&at(1000, 1000));
        let mut r = at(2000, 1000);
        r.procs
            .as_mut()
            .expect("swept")
            .push(child(300, 300, "rustc", 5_000_000));
        let s = v.absorb(&r);
        assert!(close(write_bps(&s, 300), 5e6), "{:?}", write_bps(&s, 300));
        let w = writer(&s, "rustc").expect("a writer");
        assert_eq!((w.scope, w.n, w.bytes), (ProcScope::Tab(7), 1, 5_000_000));
        assert!(
            close(s.tab(7).and_then(|t| t.scope.write_bps), 7e6),
            "cargo's and rustc's"
        );
    }

    /// NEGATIVE CONTROL: across a gap no window forms, so even a newborn counts nothing.
    #[test]
    fn the_first_sweep_after_a_gap_counts_nothing() {
        let mut v = Vitals::new();
        let _ = v.absorb(&at(0, 1000));
        let _ = v.absorb(&at(1000, 1000));
        let mut r = at(14_000, 1000);
        r.procs
            .as_mut()
            .expect("swept")
            .push(child(300, 300, "rustc", 5_000_000));
        let s = v.absorb(&r);
        assert_eq!(write_bps(&s, 300), None);
        assert_eq!(write_bps(&s, 201), None, "nor the old cargo");
        assert!(s.facts.writers.is_empty(), "{:?}", s.facts.writers);
        assert!(
            (s.facts.writes_covered_s - 1.0).abs() < 1e-9,
            "only the first window"
        );
        let s = v.absorb(&at(15_000, 1000));
        assert!(close(write_bps(&s, 201), 2e6));
        assert!((s.facts.writes_covered_s - 2.0).abs() < 1e-9);
    }

    /// NEGATIVE CONTROL: a sweep that stopped at its cap may have missed any process, so the
    /// next counts none as born since it.
    #[test]
    fn after_a_truncated_sweep_no_process_counts_as_newborn() {
        let mut v = Vitals::new();
        let _ = v.absorb(&at(0, 1000));
        let mut cut = at(1000, 1000);
        cut.procs_truncated = true;
        let _ = v.absorb(&cut);
        let mut r = at(2000, 1000);
        r.procs
            .as_mut()
            .expect("swept")
            .push(child(300, 300, "rustc", 5_000_000));
        let s = v.absorb(&r);
        assert_eq!(write_bps(&s, 300), None);
        assert!(
            close(write_bps(&s, 201), 2e6),
            "one it read last time still counts"
        );
        let mut r = at(3000, 1000);
        r.procs
            .as_mut()
            .expect("swept")
            .push(child(300, 300, "rustc", 6_000_000));
        let s = v.absorb(&r);
        assert!(close(write_bps(&s, 300), 1e6), "from the next sweep on");
    }

    /// A LIVE PROCESS ONE SWEEP MISSED IS NO NEWBORN: a long-lived writer (its start older than
    /// the newest the last sweep read) that one whole sweep skipped reads, on its return, nothing
    /// for that window, never its life's 50 GB as one second's writes. NEGATIVE CONTROL: a process
    /// truly born in the same window (its start the newest) counts its whole counter.
    #[test]
    fn a_live_process_one_sweep_missed_never_counts_its_life_as_one_window() {
        let mut v = Vitals::new();
        let old = |written| child(500, 5, "postgres", written);
        let mut r = at(0, 1000);
        let procs = r.procs.as_mut().expect("swept");
        procs.push(old(50_000_000_000));
        procs.push(child(300, 300, "rustc", 1_000_000));
        let _ = v.absorb(&r);
        let mut r = at(1000, 1000);
        r.procs
            .as_mut()
            .expect("swept")
            .push(child(300, 300, "rustc", 2_000_000));
        let _ = v.absorb(&r);
        let mut r = at(2000, 1000);
        let procs = r.procs.as_mut().expect("swept");
        procs.push(old(50_000_001_000));
        procs.push(child(300, 300, "rustc", 3_000_000));
        procs.push(child(600, 600, "cc", 3_000_000));
        let s = v.absorb(&r);
        assert_eq!(write_bps(&s, 500), None, "{:?}", write_bps(&s, 500));
        assert!(writer(&s, "postgres").is_none(), "{:?}", s.facts.writers);
        assert!(
            s.facts.hour_writers.iter().all(|p| p.name != "postgres"),
            "{:?}",
            s.facts.hour_writers
        );
        assert!(close(write_bps(&s, 600), 3e6), "{:?}", write_bps(&s, 600));
        assert_eq!(writer(&s, "cc").map(|w| w.bytes), Some(3_000_000));
        let mut r = at(3000, 1000);
        let procs = r.procs.as_mut().expect("swept");
        procs.push(old(50_000_003_000));
        let s = v.absorb(&r);
        assert!(
            close(write_bps(&s, 500), 2_000.0),
            "from the next sweep on: {:?}",
            write_bps(&s, 500)
        );
    }

    #[test]
    fn a_reused_pid_never_reads_as_the_old_processs_delta() {
        let mut v = Vitals::new();
        for t in [0, 1000] {
            let mut r = at(t, 1000);
            r.procs
                .as_mut()
                .expect("swept")
                .push(child(300, 300, "rustc", 400_000_000));
            let _ = v.absorb(&r);
        }
        let mut r = at(2000, 1000);
        r.procs
            .as_mut()
            .expect("swept")
            .push(child(300, 301, "cp", 500_000_000));
        let s = v.absorb(&r);
        assert!(
            close(write_bps(&s, 300), 5e8),
            "the new process's own bytes, not 100 MB over the old one's: {:?}",
            write_bps(&s, 300)
        );
        assert!(writer(&s, "rustc").is_none());
        assert_eq!(writer(&s, "cp").map(|w| w.bytes), Some(500_000_000));
    }

    #[test]
    fn a_zombie_row_gives_the_writes_after_its_last_live_sweep() {
        let mut v = Vitals::new();
        for (t, written) in [(0, 10_000_000), (1000, 20_000_000)] {
            let mut r = at(t, 1000);
            r.procs
                .as_mut()
                .expect("swept")
                .push(child(300, 300, "rustc", written));
            let _ = v.absorb(&r);
        }
        let mut r = at(2000, 1000);
        r.procs.as_mut().expect("swept").push(ProcSample {
            cpu_ns: None,
            footprint: None,
            state: ProcState::Zombie,
            ..child(300, 300, "rustc", 68_000_000)
        });
        let s = v.absorb(&r);
        assert!(close(write_bps(&s, 300), 48e6), "{:?}", write_bps(&s, 300));
        let s = v.absorb(&at(3000, 1000));
        assert_eq!(write_bps(&s, 300), None, "reaped");
    }

    #[test]
    fn a_program_split_over_many_processes_is_one_writer() {
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for t in [0u64, 1000] {
            let mut r = at(t, 1000);
            let procs = r.procs.as_mut().expect("swept");
            for i in 0..9 {
                procs.push(child(400 + i, 400 + u64::from(i), "rustc", 3_400 * t));
            }
            procs.push(ProcSample {
                start: Some(900),
                disk_written: Some(10_000 * t),
                uid: Some(501),
                ..proc(900, 1, 10, 20, "cp")
            });
            s = v.absorb(&r);
        }
        let rustc = &s.facts.writers[0];
        assert_eq!(
            (rustc.name.as_str(), rustc.scope, rustc.n),
            ("rustc", ProcScope::Tab(7), 9),
            "{:?}",
            s.facts.writers
        );
        assert!(close(Some(rustc.bytes_per_s), 30.6e6), "{rustc:?}");
        assert_eq!(rustc.bytes, 30_600_000);
        let cp = writer(&s, "cp").expect("cp");
        assert_eq!((cp.scope, cp.n), (ProcScope::Other, 1));
        // NEGATIVE CONTROL: process by process, cp outranks every rustc.
        assert!(
            (400..409).all(|pid| write_bps(&s, pid).is_some_and(|r| r < cp.bytes_per_s)),
            "{:?}",
            s.procs
        );
    }

    /// EACH SCOPE KEEPS ITS OWN FIRST WRITERS THIS SWEEP: five programs of the rest of the Mac,
    /// each writing more than tab 7's cargo, leave cargo among the writers (the lens's "this
    /// tab" finds it), and the rest of the Mac keeps its first four. NEGATIVE CONTROL: by rate
    /// alone cargo is sixth, past a single first four.
    #[test]
    fn each_scope_keeps_its_own_first_writers_this_sweep() {
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for t in [0u64, 1000] {
            let mut r = at(t, 1000);
            let procs = r.procs.as_mut().expect("swept");
            for i in 0..5u32 {
                procs.push(ProcSample {
                    start: Some(900 + u64::from(i)),
                    disk_written: Some((10_000 + u64::from(i)) * t),
                    uid: Some(501),
                    ..proc(900 + i, 1, 10, 20, &format!("cp{i}"))
                });
            }
            s = v.absorb(&r);
        }
        let others: Vec<&str> = s
            .facts
            .writers
            .iter()
            .filter(|w| w.scope == ProcScope::Other)
            .map(|w| w.name.as_str())
            .collect();
        assert_eq!(
            others,
            ["cp4", "cp3", "cp2", "cp1"],
            "{:?}",
            s.facts.writers
        );
        let cargo = writer(&s, "cargo").expect("tab 7's writer");
        assert_eq!(cargo.scope, ProcScope::Tab(7));
        assert!(
            (900..905).all(|pid| write_bps(&s, pid).is_some_and(|r| r > cargo.bytes_per_s)),
            "{:?}",
            s.procs
        );
    }

    #[test]
    fn a_tabs_hour_names_its_top_writer_with_its_coverage() {
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for t in 0..=30u64 {
            s = v.absorb(&at(t * 1000, 1000));
        }
        assert!((s.facts.writes_covered_s - 30.0).abs() < 1e-9);
        let cargo = &s.facts.hour_writers[0];
        assert_eq!(
            (cargo.name.as_str(), cargo.scope),
            ("cargo", ProcScope::Tab(7))
        );
        assert_eq!(
            cargo.per_hour(s.facts.writes_covered_s),
            None,
            "half a minute read"
        );
        for t in 31..=90u64 {
            s = v.absorb(&at(t * 1000, 1000));
        }
        let cargo = &s.facts.hour_writers[0];
        assert_eq!((cargo.bytes, cargo.n_max), (180_000_000, 1));
        let per_hour = cargo.per_hour(s.facts.writes_covered_s);
        assert!(close(per_hour, 7.2e9), "{per_hour:?}");
        // Hidden for ten minutes (no sweep): the hour keeps what it read and its coverage.
        for t in 91..=690u64 {
            let mut r = at(t * 1000, 1000);
            r.procs = None;
            r.machine_only = true;
            s = v.absorb(&r);
        }
        assert!((s.facts.writes_covered_s - 90.0).abs() < 1e-9);
        assert!(close(s.facts.hour_writers[0].per_hour(90.0), 7.2e9));
    }

    #[test]
    fn no_disk_counter_means_no_coverage() {
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for t in 0..90u64 {
            let mut r = at(t * 1000, 1000);
            for p in r.procs.as_mut().expect("swept") {
                p.disk_written = None;
            }
            s = v.absorb(&r);
        }
        assert_eq!(s.facts.writes_covered_s, 0.0);
        assert!(s.facts.hour_writers.is_empty());
        assert!(s.facts.writers.is_empty());
        assert_eq!(
            s.resources[Disk.index()].aterm.missing,
            Some(NoFigure::NotReadHere)
        );
    }

    /// Readings paced from the moment each was asked drift through the second. The snapshot's
    /// slot grid keeps its edges between them, so each reading of a second's cadence opens one
    /// slot, and each pair of the lens's half-second cadence one, even as their place in the
    /// second drifts past the wall clock's edge and jitters by tens of ms; on the wall clock's
    /// own seconds the same readings open none and then two.
    #[test]
    fn the_slot_grid_keeps_a_reading_a_slot_as_readings_drift() {
        let blank = |at_ms: i64| Sample {
            at_unix_ms: at_ms,
            cadence_ms: 1000,
            ..Sample::default()
        };
        // A small fixed jitter, ±20 ms.
        let jitter = |i: i64| ((i * 7_919) % 41) - 20;
        for (cadence, per_slot) in [(1_003_i64, 1_i64), (501, 2)] {
            let mut v = Vitals::new();
            let times: Vec<i64> = (0..400)
                .map(|i| T0 + 900 + i * cadence + jitter(i))
                .collect();
            let (mut opened, mut seconds) = (Vec::new(), Vec::new());
            let mut prev: Option<i64> = None;
            for &t in &times {
                let snap = v.absorb(&blank(t));
                assert!(snap.slot_grid.clear(t, SLOT_GUARD_MS) || prev.is_none());
                if let Some(p) = prev {
                    opened.push(snap.slot_grid.opened(p, t));
                    seconds.push(SlotGrid::SECONDS.opened(p, t));
                }
                prev = Some(t);
            }
            let at = format!("cadence {cadence} ms");
            // At a second's cadence every reading opens a slot; at the lens's, never two readings
            // in a row open none (the strip never stands a second).
            let steady = |opened: &[i64]| {
                opened.iter().all(|&o| o == 0 || o == 1)
                    && if per_slot == 1 {
                        opened.iter().all(|&o| o == 1)
                    } else {
                        !opened.windows(2).any(|w| w == [0, 0])
                    }
            };
            assert!(steady(&opened), "{at}: {opened:?}");
            let span_s = (times[times.len() - 1] - times[0]) / SLOT_MS;
            let sum = opened.iter().sum::<i64>();
            assert!(
                (sum - span_s).abs() <= 1,
                "{at}: {sum} slots over {span_s} s"
            );
            assert!(
                !steady(&seconds),
                "CONTROL {at}: on the wall clock's seconds the strip stutters: {seconds:?}"
            );
        }
    }

    /// A strip drawn reading by reading reads the snapshot's last minute of readings: each 1 s
    /// slot holds its second's reading, the second series merges in as in the bucketed strip,
    /// and a series not read at the snapshot's moment leaves its last slot unread.
    #[test]
    fn a_snapshot_carries_its_readings_for_a_strip_slot_by_slot() {
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for t in 0..12 {
            s = v.absorb(&at(t * 1000, 1000));
        }
        let gpu = &s.resources[Gpu.index()];
        assert_eq!(gpu.recent.len(), 12, "a reading a second, all kept");
        assert!(gpu.recent.windows(2).all(|w| w[0].at_ms < w[1].at_ms));
        let slots = gpu.heat_slots(s.slot_grid, 5, s.at_unix_ms, 15_000);
        assert!(
            slots
                .iter()
                .all(|c| c.heat.is_some_and(|h| (h - 0.12).abs() < 1e-6)),
            "{slots:?}"
        );
        // Network: the larger of receive and send, slot by slot, as the bucketed strip says.
        let net = &s.resources[Network.index()];
        let by_slot = net.heat_slots(s.slot_grid, 3, s.at_unix_ms, 15_000);
        let by_col = net.heat_columns(10, 1, s.at_unix_ms);
        let last = by_slot.last().and_then(|c| c.heat);
        assert!(
            last.zip(by_col[0].heat)
                .is_some_and(|(a, b)| (a - b).abs() < 1e-6),
            "{by_slot:?} {by_col:?}"
        );
        // The GPU unread at the next reading: its last slot is unread, never zero; the slots
        // before it keep their readings.
        let mut blind = at(12_000, 1000);
        blind.gpu = None;
        let s = v.absorb(&blind);
        let slots = s.resources[Gpu.index()].heat_slots(s.slot_grid, 3, s.at_unix_ms, 15_000);
        assert_eq!(slots[2].heat, None, "{slots:?}");
        assert!(slots[..2].iter().all(|c| c.heat.is_some()), "{slots:?}");
    }
}
