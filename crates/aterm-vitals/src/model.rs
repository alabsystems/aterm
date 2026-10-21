// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The model: plain samples in ([`Sample`]), one immutable [`Snapshot`] out.
//!
//! [`Vitals::absorb`] turns cumulative counters (core ticks, per-process CPU
//! time) into rates, smooths each quantity ([`crate::trend`]) and attributes
//! processes to the three scopes the owner asked for:
//!
//! * **Global**: the whole machine, from the cores' own ticks.
//! * **aterm**: this instance and everything it started: its own process,
//!   every descendant, and every tab's shell tree (a shell kept alive by the
//!   PTY keeper is not aterm's child, but its tab is aterm's work).
//! * **Tab**: one tab's shell and every descendant of it.
//!
//! Attribution walks the parent chain of every process in one sweep, memoized,
//! so a sweep of N processes costs O(N). A process seen for the first time has
//! no earlier CPU time to subtract, so it counts from its second sweep: the
//! figures are "live" (exited work between sweeps is not credited back).

use std::collections::HashMap;

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcSample {
    /// The pid.
    pub pid: u32,
    /// Its parent's pid.
    pub ppid: u32,
    /// Cumulative CPU time, ns; `None` where the OS refused it.
    pub cpu_ns: Option<u64>,
    /// Physical footprint, bytes; `None` where unreadable.
    pub footprint: Option<u64>,
    /// The short command name.
    pub name: String,
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
}

/// The machine's memory by the OS's classes, bytes (design §9.1 macOS,
/// §9.6 Linux and Windows). Only macOS partitions physical memory exactly;
/// elsewhere `total` less the classes is memory no class names, and no
/// class takes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemSample {
    /// Physical memory.
    pub total: u64,
    /// In use by processes: macOS app memory (anonymous pages less
    /// purgeable); Linux anonymous pages plus shared memory (tmpfs and
    /// shared anonymous memory, not reclaimable without swap, so never
    /// cache); Windows in use less the kernel's pools.
    pub app: u64,
    /// The kernel's own: macOS wired; Linux unreclaimable slab, kernel
    /// stacks and page tables; Windows the paged and nonpaged pools.
    pub wired: u64,
    /// Physical memory holding compressed pages (macOS's compressor, Linux's
    /// zswap pool, 0 with zswap off); `None` where the OS does not say
    /// (Windows' compression store, which this build does not read), so a
    /// surface leaves the class out rather than show a false zero.
    pub compressed: Option<u64>,
    /// Reclaimable file cache: macOS file-backed and purgeable; Linux
    /// `Cached − Shmem + Buffers + SReclaimable`; Windows `SystemCache` (the
    /// standby list and the system cache's working set), at most what is
    /// available.
    pub cached: u64,
    /// Free (speculative included).
    pub free: u64,
    /// Swap in use.
    pub swap_used: u64,
    /// Swap allocated.
    pub swap_total: u64,
    /// The pressure level.
    pub pressure: Pressure,
    /// The commit charge against its limit, where the OS keeps one.
    pub commit: Option<CommitSample>,
}

/// Memory promised to processes against what the OS can back (design
/// §9.6): exact on Windows, where a process fails to allocate at the limit
/// while RAM is free; a heuristic on Linux, where overcommit makes the
/// limit advisory. macOS keeps none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommitSample {
    /// Committed bytes.
    pub charge: u64,
    /// The limit, bytes.
    pub limit: u64,
    /// Whether the OS enforces the limit (Windows), so the share is a fact.
    pub exact: bool,
}

/// An exact commit charge at or past this share of its limit is worth
/// saying: design §9.6's Windows warn line.
pub const COMMIT_NEAR_PCT: u64 = 90;

impl CommitSample {
    /// The charge as a whole percent of the limit (rounded down); `None`
    /// for a zero limit.
    #[must_use]
    pub fn pct(&self) -> Option<u64> {
        (self.limit > 0).then(|| {
            u64::try_from(u128::from(self.charge) * 100 / u128::from(self.limit))
                .unwrap_or(u64::MAX)
        })
    }

    /// The percent when the limit is enforced and the charge is at or past
    /// [`COMMIT_NEAR_PCT`] of it; `None` otherwise, including every
    /// heuristic (Linux) reading.
    #[must_use]
    pub fn near_limit(&self) -> Option<u64> {
        self.pct().filter(|p| self.exact && *p >= COMMIT_NEAR_PCT)
    }
}

impl MemSample {
    /// Memory in use, as Activity Monitor counts it: app + wired + compressed,
    /// never more than physical memory.
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
    /// Its shell's pid; 0 when it has none (a native tab).
    pub shell_pid: u32,
    /// Its label, `Tab 2`.
    pub label: String,
    /// The program in front, as the host names it (`claude`, `zsh`); empty
    /// when unknown.
    pub program: String,
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
    /// The volume the focused tab works on; `None` where unreadable.
    pub volume: Option<VolumeSample>,
    /// One process sweep; `None` when the host skipped it.
    pub procs: Option<Vec<ProcSample>>,
    /// This aterm instance's pid.
    pub self_pid: u32,
    /// Every tab, in the host's order.
    pub tabs: Vec<TabRoot>,
}

/// One core's load.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CoreView {
    /// Busy fraction, `0..=1`: raw is the last interval, level the fast
    /// average.
    pub load: TrendView,
}

/// One cluster's cores.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClusterView {
    /// The OS's name.
    pub name: String,
    /// A short label: the name's first letter and any number it ends in
    /// (`P`, `E`, `S`, `M2` for "Mid 2"), or `CPU`.
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
    /// Each core's busy fraction over the last minute, oldest first, by
    /// logical id (the lens's core × time strip).
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
}

/// One tab's scope.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabView {
    /// The host's key for it.
    pub key: u64,
    /// `Tab 2`.
    pub label: String,
    /// The program in front.
    pub program: String,
    /// Its load.
    pub scope: ScopeView,
}

/// Which scope a busy process is attributed to ([`Snapshot::top`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcScope {
    /// One of this instance's tabs, by its key.
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

/// Processes [`Snapshot::top`] keeps.
pub const TOP_PROCS: usize = 6;
/// Seconds each core's history keeps ([`CpuView::core_history`]): a minute.
pub const CORE_HISTORY_LEN: usize = 60;

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
    /// The machine.
    pub global: ScopeView,
    /// This instance and its tabs; `None` until two sweeps exist.
    pub aterm: Option<ScopeView>,
    /// Every tab, in the host's order; empty until two sweeps exist.
    pub tabs: Vec<TabView>,
    /// The busiest processes this sweep, busiest first, at most
    /// [`TOP_PROCS`], each with the scope it is attributed to.
    pub top: Vec<TopProc>,
    /// The stuck engine's view of every tab's jobs (§11), as the host last
    /// took it: the host samples jobs at the engine's own cadence, so this
    /// may be an earlier reading's view. `None` until the first, and where
    /// the host reads no jobs. [`Vitals::absorb`] leaves it `None`.
    pub jobs: Option<std::sync::Arc<crate::jobs::JobsView>>,
}

impl Snapshot {
    /// The tab with key `key`.
    #[must_use]
    pub fn tab(&self, key: u64) -> Option<&TabView> {
        self.tabs.iter().find(|t| t.key == key)
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
        }
    }
}

#[derive(Clone, Debug, Default)]
struct ScopeAcc {
    cores: f32,
    footprint: Option<u64>,
    procs: u32,
    top: Option<(String, f32)>,
}

impl ScopeAcc {
    fn add(&mut self, p: &ProcSample, cores: f32) {
        self.cores += cores;
        if let Some(f) = p.footprint {
            self.footprint = Some(self.footprint.unwrap_or(0).saturating_add(f));
        }
        self.procs += 1;
        if cores > 0.0 && self.top.as_ref().is_none_or(|(_, c)| cores > *c) {
            self.top = Some((p.name.clone(), cores));
        }
    }
}

/// The model's running state. One per host instance.
#[derive(Debug, Default)]
pub struct Vitals {
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
    last_sweep_at: Option<u64>,
    last_cpu: HashMap<u32, u64>,
    global_foot: Trend,
    aterm: ScopeState,
    tabs: HashMap<u64, ScopeState>,
}

impl Vitals {
    /// A model that has read nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Absorb one sample and return the snapshot it makes.
    pub fn absorb(&mut self, s: &Sample) -> Snapshot {
        self.seq += 1;
        let cpu = s
            .cores
            .as_deref()
            .filter(|cores| !cores.is_empty())
            .and_then(|cores| {
                let dt_s = interval_s(self.last_cores_at, s.at_ns);
                self.last_cores_at = Some(s.at_ns);
                self.absorb_cores(cores, &s.clusters, dt_s, s.at_ns)
            });
        if cpu.is_none() {
            self.busy_history.push_at(None, s.at_ns);
        }

        let gpu = match s.gpu.as_ref() {
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
            Some(MemView {
                sample: m,
                used_gib: self.mem.view(),
                history: self.mem_history.to_vec(),
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

        let (aterm, tabs, top) = match s.procs.as_deref() {
            Some(procs) => self.absorb_procs(procs, s, &mut global),
            None => {
                self.aterm.history.push_at(None, s.at_ns);
                for state in self.tabs.values_mut() {
                    state.history.push_at(None, s.at_ns);
                }
                (None, Vec::new(), Vec::new())
            }
        };

        Snapshot {
            at_ns: s.at_ns,
            seq: self.seq,
            cpu,
            gpu,
            mem,
            volume: s.volume.clone(),
            global,
            aterm,
            tabs,
            top,
            jobs: None,
        }
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
            // First reading, or the OS changed the core count: start over.
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
    ) -> (Option<ScopeView>, Vec<TabView>, Vec<TopProc>) {
        let wall_ns = self
            .last_sweep_at
            .filter(|&t| s.at_ns > t)
            .map(|t| s.at_ns - t);
        self.last_sweep_at = Some(s.at_ns);
        let mut next_cpu = HashMap::with_capacity(procs.len());
        let mut rate = HashMap::with_capacity(procs.len());
        for p in procs {
            if let Some(ns) = p.cpu_ns {
                next_cpu.insert(p.pid, ns);
                let cores = match (wall_ns, self.last_cpu.get(&p.pid)) {
                    (Some(wall), Some(&before)) if wall > 0 => {
                        ns.saturating_sub(before) as f32 / wall as f32
                    }
                    _ => 0.0,
                };
                rate.insert(p.pid, cores);
            }
        }
        self.last_cpu = next_cpu;
        global.procs = u32::try_from(procs.len()).unwrap_or(u32::MAX);
        let Some(wall_ns) = wall_ns else {
            // The first sweep has nothing to subtract.
            return (None, Vec::new(), Vec::new());
        };
        let dt_s = wall_ns as f32 / 1e9;

        let membership = attribute(procs, s.self_pid, &s.tabs);
        let mut aterm_acc = ScopeAcc::default();
        let mut tab_acc: Vec<ScopeAcc> = vec![ScopeAcc::default(); s.tabs.len()];
        let mut global_top: Option<(String, f32)> = None;
        let mut top: Vec<TopProc> = Vec::with_capacity(TOP_PROCS + 1);
        for (p, m) in procs.iter().zip(membership.iter()) {
            let cores = rate.get(&p.pid).copied().unwrap_or(0.0);
            if cores >= 0.01
                && (top.len() < TOP_PROCS || top.last().is_some_and(|t| cores > t.cores))
            {
                let scope = match (m.tab, m.aterm) {
                    (Some(t), _) => s
                        .tabs
                        .get(t)
                        .map_or(ProcScope::Aterm, |r| ProcScope::Tab(r.key)),
                    (None, true) => ProcScope::Aterm,
                    (None, false) => ProcScope::Other,
                };
                let at = top.partition_point(|t| t.cores >= cores);
                top.insert(
                    at,
                    TopProc {
                        name: p.name.clone(),
                        cores,
                        footprint: p.footprint,
                        scope,
                    },
                );
                top.truncate(TOP_PROCS);
            }
            if cores > 0.0 && global_top.as_ref().is_none_or(|(_, c)| cores > *c) {
                global_top = Some((p.name.clone(), cores));
            }
            if m.aterm {
                aterm_acc.add(p, cores);
            }
            if let Some(t) = m.tab {
                tab_acc[t].add(p, cores);
            }
        }
        global.top = global_top;

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
                    label: root.label.clone(),
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
        (Some(aterm), tabs, top)
    }
}

/// Seconds since this quantity's last usable reading, with the existing
/// first/non-increasing timestamp fallback.
fn interval_s(before: Option<u64>, now: u64) -> f32 {
    before
        .filter(|&then| now > then)
        .map_or(1.0, |then| (now - then) as f32 / 1e9)
}

/// A name for what each tab runs, from the process table, for a tab whose
/// host gave none: the shell's direct child with the largest footprint (an
/// agent, an editor, a build), else the shell's own name, else empty.
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

/// Attribute every process in `procs` to aterm (`self_pid` and its
/// descendants, plus every tab's tree) and to at most one tab (the nearest
/// tab shell on its parent chain). One entry per process, in order.
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

fn resolve(
    pid: u32,
    self_pid: u32,
    parent: &HashMap<u32, u32>,
    shells: &HashMap<u32, usize>,
    memo: &mut HashMap<u32, Membership>,
) -> Membership {
    // Walk up until a decided ancestor, a root, or a cycle (pid reuse can
    // briefly make one); then decide every pid on the path at once.
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
    // Decide from the top of the path down, so a tab shell's membership flows
    // to its descendants and aterm's flows to everything under it.
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
            ppid,
            cpu_ns: Some(cpu_ms * 1_000_000),
            footprint: Some(foot_mib << 20),
            name: name.into(),
        }
    }

    fn tab(key: u64, shell: u32, label: &str, program: &str) -> TabRoot {
        TabRoot {
            key,
            shell_pid: shell,
            label: label.into(),
            program: program.into(),
        }
    }

    #[test]
    fn attribution_follows_parent_chains_to_aterm_and_the_nearest_tab() {
        // launchd 1 → aterm 100 → zsh 200 (tab A) → cargo 201 → rustc 202
        //                       → zsh 300 (tab B) → claude 301
        // keeper 50 → zsh 400 (tab C, kept alive outside aterm's tree)
        // launchd 1 → Safari 900 (others)
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
            }),
            gpu: Some(GpuSample {
                device_pct: Some(4),
                cores: Some(10),
                name: None,
            }),
            volume: None,
            procs: Some(procs),
            self_pid: 100,
            tabs: vec![tab(7, 200, "Tab 1", "cargo")],
        }
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
        // A minute at most per core.
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

        // One second later: core 2 fully busy, core 3 half; aterm used 0.2 s,
        // cargo 0.9 s, Safari 0.3 s.
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

        // Other metrics may arrive while these readers are unavailable.
        // Their valid timestamps, and the gaps, belong to each series.
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
        // One entry a second: 12 and 13 read nothing, and the reading at 15
        // spans the silent 14 too (`History::push_at`).
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

        // A valid process sweep can still contain no readable footprint.
        // Its CPU interval must not advance the footprint's own age.
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
        // An unread class adds nothing, and is not a zero to show.
        let unread = MemSample {
            compressed: None,
            ..m
        };
        assert_eq!(unread.used(), 9);
        assert_eq!(gib(1 << 30), 1.0);
    }

    /// Unnamed levels (Linux `cpu_capacity`, Windows efficiency classes)
    /// keep distinct short labels: "Mid 1" and "Mid 2" are `M1` and `M2`,
    /// and one machine-wide "CPU" cluster stays `CPU`.
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

    /// The commit share: said only where the limit is enforced (Windows)
    /// and the charge is at 90 % of it or past. NEGATIVE CONTROLS: Linux's
    /// heuristic charge past its advisory limit, and a zero limit.
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
}
