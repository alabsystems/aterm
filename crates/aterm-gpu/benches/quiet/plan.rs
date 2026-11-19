// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The quiet-commit grid's pure parts. `benches/quiet_commit.rs` mounts this file; the library
//! mounts it too, only under `cfg(test)`, beside its tests (`plan_tests.rs`, which the library
//! mounts itself: cargo builds a harness-less bench with `--cfg test`, so a test module this
//! file mounted would build into the bench too), and no build without tests compiles it into
//! aterm-gpu. It holds which takes a run makes and the order it makes them in,
//! one take's window arithmetic (other-thread CPU, and a row per thread that ran with its name
//! and the dispatch queue it drained), the GPU time other processes ran over the quiet, the
//! footprint's release steps, one take's record as a `TAKE` line and as one JSON object, which
//! takes the decisions admit (a take after a quiet by its foreign duty: other processes' GPU
//! time over the stretch it was read across), the per-shape `ARM` summaries, the foreign split
//! (other-thread CPU against foreign duty within each shape's re-takes and held takes, one
//! placement at a time, the control judging the quiet-end one) and the run's validity
//! controls. Nothing here opens a device or reads a counter.
//!
//! The grid asks what the ~4.2 ms submit burst of a resting window's reading frame is. The
//! 2026-11-17 profile fits its largest single-thread burst at 2.94 ms plus 6.9 us per MiB the
//! driver takes back: about 1.2 ms tracks the freed block, and about 3 ms grows with how long
//! the GPU sat idle, whatever the submit holds. Block 1 is the curve over the quiet, block 2
//! the content arms, block 3 the level (queue, process, device) that holds the idle state and
//! block 4 a per-process residency restore.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use aterm_sysprobe::GpuClients;
use aterm_sysprobe::self_cpu::Queue;
use aterm_time::Duration;

use crate::{QuietArm, QuietKeep, QuietTake};

/// Block 1's quiets, ms: back to back, then past the driver's release, then the 10 s cadence.
pub const CURVE_QUIETS_MS: [u64; 8] = [0, 500, 1_000, 1_500, 2_000, 3_000, 5_000, 10_000];
/// Block 1's arms.
pub const CURVE_ARMS: [QuietArm; 5] = [
    QuietArm::P,
    QuietArm::Clear,
    QuietArm::Dot,
    QuietArm::TwoCb,
    QuietArm::EmptyCb,
];
/// Block 2's arms.
pub const CONTENT_ARMS: [QuietArm; 2] = [QuietArm::Blit, QuietArm::Compute];
/// Block 2's quiets, ms.
pub const CONTENT_QUIETS_MS: [u64; 2] = [1_000, 5_000];
/// Blocks 3 and 4: [`QuietArm::P`] after this quiet, ms.
pub const HOLD_QUIET_MS: u64 = 5_000;
/// Block 3's keeps: every level at one period, 500 ms, and the other process at 100 ms too, so
/// a difference between levels is not a difference of cadence, and the cadence's own effect is
/// read at one level.
pub const KEEPS: [Option<QuietKeep>; 6] = [
    None,
    Some(QuietKeep::SameQueueDot(Duration::from_millis(500))),
    Some(QuietKeep::OtherQueueCompute(Duration::from_millis(500))),
    Some(QuietKeep::OtherQueueDot(Duration::from_millis(500))),
    Some(QuietKeep::OtherProcessDot(Duration::from_millis(500))),
    Some(QuietKeep::OtherProcessDot(Duration::from_millis(100))),
];
/// Block 4's ballasts, MiB.
pub const BALLASTS_MIB: [u32; 2] = [0, 512];

/// The default shuffle seed, the existing bench's; `QUIET_COMMIT_SEED` sets another, and the
/// run object records the one used.
pub const SEED: u64 = 0x9e37_79b9_7f4a_7c15;

/// One repetition's takes in block order, before any shuffle: 40 in block 1, 4 in block 2, 6
/// in block 3 and 2 in block 4, each with its block's number.
#[must_use]
pub fn repetition() -> Vec<(u8, QuietTake)> {
    let mut takes = Vec::with_capacity(52);
    for arm in CURVE_ARMS {
        for q in CURVE_QUIETS_MS {
            takes.push((1, QuietTake::bare(arm, q)));
        }
    }
    for arm in CONTENT_ARMS {
        for q in CONTENT_QUIETS_MS {
            takes.push((2, QuietTake::bare(arm, q)));
        }
    }
    for keep in KEEPS {
        takes.push((
            3,
            QuietTake {
                keep,
                ..QuietTake::bare(QuietArm::P, HOLD_QUIET_MS)
            },
        ));
    }
    for ballast_mib in BALLASTS_MIB {
        takes.push((
            4,
            QuietTake {
                ballast_mib,
                ..QuietTake::bare(QuietArm::P, HOLD_QUIET_MS)
            },
        ));
    }
    takes
}

/// Shuffle `order` in place with the existing bench's xorshift, advancing `seed`.
pub fn shuffle<T>(order: &mut [T], seed: &mut u64) {
    for i in (1..order.len()).rev() {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        let j = usize::try_from(*seed % (i as u64 + 1)).unwrap_or(0);
        order.swap(i, j);
    }
}

/// One planned take: its repetition, its block and its plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Planned {
    /// Which repetition, from 0.
    pub rep: usize,
    /// Which block of [`repetition`].
    pub block: u8,
    /// The take.
    pub take: QuietTake,
}

/// `reps` repetitions of the takes of [`repetition`] that `admit` keeps, each repetition in its
/// own order: one xorshift stream from `seed` across the whole plan.
#[must_use]
pub fn plan(reps: usize, seed: u64, admit: impl Fn(&QuietTake) -> bool) -> Vec<Planned> {
    let base: Vec<(u8, QuietTake)> = repetition().into_iter().filter(|(_, t)| admit(t)).collect();
    let mut seed = seed;
    let mut out = Vec::with_capacity(base.len() * reps);
    for rep in 0..reps {
        let mut order = base.clone();
        shuffle(&mut order, &mut seed);
        out.extend(
            order
                .into_iter()
                .map(|(block, take)| Planned { rep, block, take }),
        );
    }
    out
}

/// Where a take's quiet-end-side heavy reads (the GPU's use and its clients' GPU time from
/// IOKit, and the purgeable walk of this process's map) are made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Placement {
    /// Between the quiet's end and the submit.
    QuietEnd,
    /// Just after the take's completion, on the caller's thread inside the CPU window, so
    /// nothing is read between the quiet and the submit; the caller's CPU then holds the reads.
    Completed,
}

impl Placement {
    /// Both, in the order the output gives them.
    pub const ALL: [Self; 2] = [Self::QuietEnd, Self::Completed];

    /// The placement's name in every output line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::QuietEnd => "quiet_end",
            Self::Completed => "completed",
        }
    }
}

/// `QUIET_COMMIT_IOKIT_AT`: one [`Placement`] for every take, or the two by repetition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlacementMode {
    /// Every take's reads at this placement.
    Every(Placement),
    /// Even repetitions [`Placement::QuietEnd`], odd ones [`Placement::Completed`], so one run
    /// compares the two over the same shapes.
    Alternate,
}

impl PlacementMode {
    /// The mode named `word`: `quiet_end`, `completed` or `alternate`.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "quiet_end" => Some(Self::Every(Placement::QuietEnd)),
            "completed" => Some(Self::Every(Placement::Completed)),
            "alternate" => Some(Self::Alternate),
            _ => None,
        }
    }

    /// The mode's name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Every(p) => p.name(),
            Self::Alternate => "alternate",
        }
    }

    /// Repetition `rep`'s placement.
    #[must_use]
    pub const fn of(self, rep: usize) -> Placement {
        match self {
            Self::Every(p) => p,
            Self::Alternate if rep % 2 == 0 => Placement::QuietEnd,
            Self::Alternate => Placement::Completed,
        }
    }
}

/// Whose thread a counter row is, within one take's window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadRole {
    /// The thread that encoded, committed and waited.
    Caller,
    /// The bench's own sampler.
    Sampler,
    /// Every other thread: Metal's submission and completion work among them.
    Other,
}

impl ThreadRole {
    /// The role's name in the JSON record.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Caller => "caller",
            Self::Sampler => "sampler",
            Self::Other => "other",
        }
    }
}

/// One thread's counters at one cut of a window, each `None` where unread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadSample {
    /// The thread's system-wide unique id.
    pub id: u64,
    /// Whose it is.
    pub role: ThreadRole,
    /// Its pthread name, where it has one.
    pub name: Option<String>,
    /// The dispatch queue it was draining, where the cut asked; `None` where it did not.
    pub queue: Option<Queue>,
    /// User plus system CPU, ns.
    pub cpu_ns: Option<u64>,
    /// User CPU, ns.
    pub user_ns: Option<u64>,
    /// System CPU, ns.
    pub system_ns: Option<u64>,
    /// Instructions retired, every performance level.
    pub instructions: Option<u64>,
    /// Cycles run, every performance level.
    pub cycles: Option<u64>,
    /// Energy charged, nJ, every performance level.
    pub energy_nj: Option<u64>,
}

/// One cut of a take's window: when it was taken, the process's CPU then, and its threads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowCut {
    /// Ns since the run's epoch.
    pub at_ns: u64,
    /// The whole process's published CPU, ns: every thread it ever had, exited ones included.
    pub process_ns: u64,
    /// Whether every listed thread was read (none past the cap, none unread).
    pub complete: bool,
    /// The threads read.
    pub threads: Vec<ThreadSample>,
}

/// One thread that ran in a take's window: its growth from the first cut to the end cut.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadRow {
    /// Its system-wide unique id, which holds across takes for the thread's life in the run.
    pub id: u64,
    /// Whose it is (`None` only in a default row).
    pub role: Option<ThreadRole>,
    /// Its pthread name at the end cut.
    pub name: Option<String>,
    /// CPU, ns.
    pub cpu_ns: u64,
    /// User CPU, ns.
    pub user_ns: Option<u64>,
    /// System CPU, ns.
    pub system_ns: Option<u64>,
    /// Instructions, from the counts at both ends.
    pub instructions: Option<u64>,
    /// Cycles.
    pub cycles: Option<u64>,
    /// Energy, nJ.
    pub energy_nj: Option<u64>,
    /// When the first cut that saw it move came, ns after the window's first cut.
    pub moved_ns: u64,
    /// The label it was draining at the most cuts that read one for it (the earliest of a tie),
    /// `None` where none did.
    pub queue: Option<String>,
    /// The cuts that asked for its queue.
    pub queue_asked: u32,
    /// Of those, the ones that read a label.
    pub queue_labelled: u32,
    /// Of those, the ones whose read failed ([`Queue::Unread`]).
    pub queue_unread: u32,
}

/// What one take's window spent, and on which threads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowCpu {
    /// The caller's CPU over the window, ns.
    pub caller_ns: Option<u64>,
    /// Its user part.
    pub caller_user_ns: Option<u64>,
    /// Its system part.
    pub caller_system_ns: Option<u64>,
    /// The process's CPU growth less the caller's and the sampler's, ns: it counts threads
    /// that exited inside the window, which a sum over live threads loses.
    pub other_ns: Option<u64>,
    /// Other live threads' user CPU growth, ns.
    pub other_user_ns: u64,
    /// Other live threads' system CPU growth, ns.
    pub other_system_ns: u64,
    /// Other live threads' instructions, from the counts at both ends.
    pub other_instructions: u64,
    /// Other live threads' cycles.
    pub other_cycles: u64,
    /// Other live threads' energy, nJ.
    pub other_energy_nj: u64,
    /// The other thread that grew most, where one grew: its id, its growth and how long after
    /// the first cut the first cut that saw it move came, ns.
    pub burst: Option<(u64, u64, u64)>,
    /// Other threads at the end whose CPU growth could not be read: absent from an incomplete
    /// first cut, or with no CPU counter at either end.
    pub unread: usize,
    /// Other threads whose CPU growth was read but whose instructions, cycles or energy were
    /// missing at either end: those counters add nothing to the `other_*` sums.
    pub counts_unread: usize,
    /// Every thread whose CPU growth was read and is above zero, the caller and the sampler
    /// among them, in the end cut's order.
    pub threads: Vec<ThreadRow>,
}

impl WindowCpu {
    /// The row of thread `id`.
    #[must_use]
    pub fn row(&self, id: u64) -> Option<&ThreadRow> {
        self.threads.iter().find(|t| t.id == id)
    }
}

/// The window from `first` (taken by the caller just before it submits) through `series` (the
/// sampler's cuts, in order), which must reach `end_ns`: `None` where the series is empty or its
/// last cut came before `end_ns` (the sampler's last read failed, or it stopped short), so a
/// window that was not read is never a window that grew nothing.
#[must_use]
pub fn window_cpu(first: &WindowCut, series: &[WindowCut], end_ns: u64) -> Option<WindowCpu> {
    let end = series.last()?;
    if end.at_ns < end_ns {
        return None;
    }
    /// `cut`'s one thread of `role`; none where it holds none or several.
    fn only(cut: &WindowCut, role: ThreadRole) -> Option<&ThreadSample> {
        let mut rows = cut.threads.iter().filter(|t| t.role == role);
        let row = rows.next()?;
        rows.next().is_none().then_some(row)
    }
    let pair = |role: ThreadRole| -> Option<(&ThreadSample, &ThreadSample)> {
        let (a, b) = (only(first, role)?, only(end, role)?);
        (a.id == b.id).then_some((a, b))
    };
    let delta = |b: Option<u64>, a: Option<u64>| b?.checked_sub(a?);
    let caller = pair(ThreadRole::Caller);
    let caller_ns = caller.and_then(|(a, b)| delta(b.cpu_ns, a.cpu_ns));
    let sampler_ns = pair(ThreadRole::Sampler).and_then(|(a, b)| delta(b.cpu_ns, a.cpu_ns));
    let other_ns = end
        .process_ns
        .checked_sub(first.process_ns)
        .zip(caller_ns)
        .zip(sampler_ns)
        .map(|((process, caller), sampler)| process.saturating_sub(caller + sampler));
    let mut out = WindowCpu {
        caller_ns,
        caller_user_ns: caller.and_then(|(a, b)| delta(b.user_ns, a.user_ns)),
        caller_system_ns: caller.and_then(|(a, b)| delta(b.system_ns, a.system_ns)),
        other_ns,
        ..WindowCpu::default()
    };
    let zero = |t: &ThreadSample| ThreadSample {
        cpu_ns: Some(0),
        user_ns: Some(0),
        system_ns: Some(0),
        instructions: Some(0),
        cycles: Some(0),
        energy_nj: Some(0),
        ..t.clone()
    };
    for t in &end.threads {
        let base = match first.threads.iter().find(|f| f.id == t.id) {
            Some(f) => f.clone(),
            None if first.complete => zero(t),
            None => {
                out.unread += usize::from(t.role == ThreadRole::Other);
                continue;
            }
        };
        let Some(cpu) = delta(t.cpu_ns, base.cpu_ns) else {
            out.unread += usize::from(t.role == ThreadRole::Other);
            continue;
        };
        let instructions = delta(t.instructions, base.instructions);
        let cycles = delta(t.cycles, base.cycles);
        let energy_nj = delta(t.energy_nj, base.energy_nj);
        let moved_ns = series
            .iter()
            .find(|c| {
                c.threads
                    .iter()
                    .any(|s| s.id == t.id && s.cpu_ns.zip(base.cpu_ns).is_some_and(|(n, b)| n > b))
            })
            .map_or(0, |c| c.at_ns.saturating_sub(first.at_ns));
        if t.role == ThreadRole::Other {
            out.other_user_ns += delta(t.user_ns, base.user_ns).unwrap_or(0);
            out.other_system_ns += delta(t.system_ns, base.system_ns).unwrap_or(0);
            out.other_instructions += instructions.unwrap_or(0);
            out.other_cycles += cycles.unwrap_or(0);
            out.other_energy_nj += energy_nj.unwrap_or(0);
            out.counts_unread +=
                usize::from(instructions.is_none() || cycles.is_none() || energy_nj.is_none());
            if cpu > 0 && out.burst.is_none_or(|(_, ns, _)| cpu > ns) {
                out.burst = Some((t.id, cpu, moved_ns));
            }
        }
        if cpu == 0 {
            continue;
        }
        let mut row = ThreadRow {
            id: t.id,
            role: Some(t.role),
            name: t.name.clone(),
            cpu_ns: cpu,
            user_ns: delta(t.user_ns, base.user_ns),
            system_ns: delta(t.system_ns, base.system_ns),
            instructions,
            cycles,
            energy_nj,
            moved_ns,
            ..ThreadRow::default()
        };
        let mut seen: Vec<(&str, u32)> = Vec::new();
        for q in series
            .iter()
            .filter_map(|c| c.threads.iter().find(|s| s.id == t.id)?.queue.as_ref())
        {
            row.queue_asked += 1;
            match q {
                Queue::Label(label) => {
                    row.queue_labelled += 1;
                    match seen.iter_mut().find(|(l, _)| *l == label.as_str()) {
                        Some((_, n)) => *n += 1,
                        None => seen.push((label.as_str(), 1)),
                    }
                }
                Queue::Unread => row.queue_unread += 1,
                Queue::None | Queue::Unlabelled => {}
            }
        }
        let mut most: Option<(&str, u32)> = None;
        for &(label, n) in &seen {
            if most.is_none_or(|(_, m)| n > m) {
                most = Some((label, n));
            }
        }
        row.queue = most.map(|(label, _)| label.to_owned());
        out.threads.push(row);
    }
    Some(out)
}

/// What other processes' GPU clients ran between two reads of every client.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Foreign {
    /// Their summed `accumulatedGPUTime` growth, ns.
    pub ns: u64,
    /// The processes whose clients grew.
    pub pids: usize,
    /// Clients whose growth could not be read: born after the first read where that read was
    /// incomplete (it may have missed them), or whose counter went back.
    pub unread: usize,
    /// Clients in the first read missing from a complete second one: whatever they ran after
    /// the first read is lost.
    pub gone: usize,
    /// The process whose clients grew most: its short name and its growth, ns.
    pub top: Option<(String, u64)>,
}

/// What the clients of every process outside `mine` ran from `start` to `end`. A client is
/// one registry entry, unique for the boot; one born after `start` grew by its whole counter
/// where `start` was complete, so it cannot predate that read.
#[must_use]
pub fn foreign(start: &GpuClients, end: &GpuClients, mine: &[u32]) -> Foreign {
    let mut out = Foreign::default();
    let mut by_pid: BTreeMap<u32, (u64, &str)> = BTreeMap::new();
    for row in end.rows.iter().filter(|r| !mine.contains(&r.pid)) {
        let grew = match start.rows.iter().find(|s| s.entry == row.entry) {
            Some(before) => row.gpu_ns.checked_sub(before.gpu_ns),
            None if start.complete && !start.entries.contains(&row.entry) => Some(row.gpu_ns),
            None => None,
        };
        match grew {
            Some(ns) => {
                out.ns = out.ns.saturating_add(ns);
                let slot = by_pid.entry(row.pid).or_insert((0, row.name.as_str()));
                slot.0 = slot.0.saturating_add(ns);
            }
            None => out.unread += 1,
        }
    }
    if end.complete {
        out.gone = start
            .rows
            .iter()
            .filter(|s| !mine.contains(&s.pid) && !end.entries.contains(&s.entry))
            .count();
    }
    out.pids = by_pid.values().filter(|(ns, _)| *ns > 0).count();
    out.top = by_pid
        .values()
        .filter(|(ns, _)| *ns > 0)
        .max_by_key(|(ns, _)| *ns)
        .map(|(ns, name)| ((*name).to_owned(), *ns));
    out
}

/// One reading of the process's footprint in a series.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VmPoint {
    /// Ns since the run's epoch.
    pub at_ns: u64,
    /// `phys_footprint`, bytes.
    pub phys: u64,
    /// `ledger_tag_graphics_footprint`, bytes, if read.
    pub gfx: Option<i64>,
}

/// Which stretch of a take a release step fell in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleasePhase {
    /// From the end of the previous take's teardown to this take's quiet's end: this take's
    /// ballast clear and keeper start, then its quiet.
    Quiet,
    /// After this take settled: its keeper child's exit and its ballast's release, with their
    /// settle.
    Teardown,
}

impl ReleasePhase {
    /// The phase's name in every output line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Quiet => "quiet",
            Self::Teardown => "teardown",
        }
    }
}

/// One step down of the footprint between two readings of a series.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReleaseStep {
    /// Which stretch it fell in.
    pub phase: ReleasePhase,
    /// When the lower reading came, ms after the instant the steps are timed from (negative
    /// before it).
    pub at_ms: f64,
    /// The footprint before, MiB.
    pub from_mib: f64,
    /// The footprint after, MiB.
    pub to_mib: f64,
    /// The graphics footprint before, MiB.
    pub gfx_from_mib: Option<f64>,
    /// The graphics footprint after, MiB.
    pub gfx_to_mib: Option<f64>,
}

/// The smallest footprint drop between two readings that counts as a release step: 4 MiB,
/// above the allocator's own churn and far below the driver's 50-180 MiB steps.
pub const RELEASE_STEP_MIN: u64 = 4 << 20;

/// Bytes as MiB.
#[must_use]
pub fn mib(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

/// Every step down of at least [`RELEASE_STEP_MIN`] in `points`, each compared with the reading
/// before it (`prev` for the first), timed from `from_ns` and tagged `phase`.
#[must_use]
pub fn releases(
    prev: Option<VmPoint>,
    points: &[VmPoint],
    from_ns: u64,
    phase: ReleasePhase,
) -> Vec<ReleaseStep> {
    let mut out = Vec::new();
    let mut last = prev;
    for &p in points {
        if let Some(l) = last
            && l.phys >= p.phys + RELEASE_STEP_MIN
        {
            out.push(ReleaseStep {
                phase,
                at_ms: (p.at_ns as f64 - from_ns as f64) / 1e6,
                from_mib: mib(l.phys),
                to_mib: mib(p.phys),
                gfx_from_mib: l.gfx.map(|g| g as f64 / 1_048_576.0),
                gfx_to_mib: p.gfx.map(|g| g as f64 / 1_048_576.0),
            });
        }
        last = Some(p);
    }
    out
}

/// Everything one take measured. Spans are µs and sizes MiB; `None` is unread.
#[derive(Clone, Debug, PartialEq)]
pub struct TakeRecord {
    /// Its place in the run, from 0.
    pub index: usize,
    /// Its repetition.
    pub rep: usize,
    /// Its block.
    pub block: u8,
    /// Its plan.
    pub take: QuietTake,
    /// The take before it in the run: its index and arm.
    pub prev: Option<(usize, QuietArm)>,
    /// Where its quiet-end-side heavy reads were made.
    pub iokit_at: Placement,
    /// When its first encode call began, ns since the run's epoch.
    pub at_ns: Option<u64>,
    /// When its quiet began, ns since the run's epoch.
    pub quiet_from_ns: Option<u64>,
    /// From this process's last completed work (keeps aside) to the take's first encode.
    pub idle_us: Option<f64>,
    /// From the last completion on the take's own queue to its first encode.
    pub queue_idle_us: Option<f64>,
    /// Encode through commit (P: the renderer's own encode span).
    pub submit_us: Option<f64>,
    /// Commit to completion as the waiting thread saw it (P: its present's work span less the
    /// encode).
    pub complete_us: Option<f64>,
    /// `GPUEndTime - GPUStartTime` of the work's command buffer (unread for P).
    pub gpu_us: Option<f64>,
    /// The window, `None` where the sampler's window was not read ([`window_cpu`]).
    pub window: Option<WindowCpu>,
    /// The sampler's cuts that were read, whether or not the window was.
    pub cuts: usize,
    /// The sampler's reads that failed.
    pub cut_fails: u32,
    /// The sampler's last cut less the completion, µs: negative where the completion fell past
    /// the window.
    pub last_cut_after_done_us: Option<f64>,
    /// `phys_footprint` at the quiet's end.
    pub fp_quiet: Option<f64>,
    /// Its highest reading from the submit through the settle.
    pub fp_peak: Option<f64>,
    /// At the settle.
    pub fp_settled: Option<f64>,
    /// `ledger_tag_graphics_footprint` at the quiet's end.
    pub gfx_fp: Option<f64>,
    /// Its highest reading from the submit through the settle.
    pub gfx_fp_peak: Option<f64>,
    /// `ledger_tag_graphics_nofootprint` at the quiet's end.
    pub gfx_nofp: Option<f64>,
    /// `ledger_purgeable_volatile` at the quiet's end.
    pub purg_vol: Option<f64>,
    /// `purgeable_volatile_resident` from the purgeable walk at [`Self::iokit_at`].
    pub purg_vol_res: Option<f64>,
    /// The same at the settle.
    pub purg_vol_res_settled: Option<f64>,
    /// The GPU's device utilization at [`Self::iokit_at`], recorded and never used to exclude.
    pub device_pct: Option<u8>,
    /// At the settle.
    pub device_pct_settled: Option<u8>,
    /// This process's `accumulatedGPUTime` growth from the take's `QuietStart` mark to
    /// [`Self::iokit_at`], ns. The mark comes at or after the quiet's start
    /// ([`Self::quiet_from_ns`]): the previous take's record is written and this take's ballast
    /// and keeper child are made between them.
    pub gpu_quiet_ns: Option<u64>,
    /// Its growth from [`Self::iokit_at`] to the settle, ns.
    pub gpu_ns: Option<u64>,
    /// Other processes' GPU time from the take's `QuietStart` mark to [`Self::iokit_at`] (its
    /// keeper child aside), so not what they ran between the quiet's start and that mark.
    pub foreign: Option<Foreign>,
    /// From the return of the `QuietStart` mark's walk of every GPU client to the start of the
    /// walk at [`Self::iokit_at`], ns: the stretch between the two walks, which each client's
    /// growth in [`Self::foreign`] spans, since each counter is read inside its walk.
    pub foreign_span_ns: Option<u64>,
    /// Other processes' GPU time from [`Self::iokit_at`] to the settle.
    pub foreign_take: Option<Foreign>,
    /// In-process keep commits during the quiet.
    pub keeps: u32,
    /// From the last keep's completion (in-process, or the keeper child's last clear) to the
    /// first encode, µs.
    pub last_keep_to_submit_us: Option<f64>,
    /// The keeper child's clears, all of them.
    pub keeper_commits: Option<u64>,
    /// From the keeper child's spawn to its first clear's completion, ms.
    pub keeper_first_commit_ms: Option<f64>,
    /// Whether the keeper child exited cleanly (`None`: no child).
    pub keep_ok: Option<bool>,
    /// The footprint's steps down: in this take's quiet (timed from its start) and in its
    /// teardown (timed from its completion).
    pub releases: Vec<ReleaseStep>,
}

impl TakeRecord {
    /// A record of `take` with nothing measured yet.
    #[must_use]
    pub fn new(index: usize, rep: usize, block: u8, take: QuietTake) -> Self {
        Self {
            index,
            rep,
            block,
            take,
            prev: None,
            iokit_at: Placement::QuietEnd,
            at_ns: None,
            quiet_from_ns: None,
            idle_us: None,
            queue_idle_us: None,
            submit_us: None,
            complete_us: None,
            gpu_us: None,
            window: None,
            cuts: 0,
            cut_fails: 0,
            last_cut_after_done_us: None,
            fp_quiet: None,
            fp_peak: None,
            fp_settled: None,
            gfx_fp: None,
            gfx_fp_peak: None,
            gfx_nofp: None,
            purg_vol: None,
            purg_vol_res: None,
            purg_vol_res_settled: None,
            device_pct: None,
            device_pct_settled: None,
            gpu_quiet_ns: None,
            gpu_ns: None,
            foreign: None,
            foreign_span_ns: None,
            foreign_take: None,
            keeps: 0,
            last_keep_to_submit_us: None,
            keeper_commits: None,
            keeper_first_commit_ms: None,
            keep_ok: None,
            releases: Vec::new(),
        }
    }

    /// [`WindowCpu::other_ns`], µs; `None` where the window was not read.
    #[must_use]
    pub fn other_us(&self) -> Option<f64> {
        Some(self.window.as_ref()?.other_ns? as f64 / 1e3)
    }

    /// [`WindowCpu::caller_ns`], µs.
    #[must_use]
    pub fn caller_us(&self) -> Option<f64> {
        Some(self.window.as_ref()?.caller_ns? as f64 / 1e3)
    }

    /// [`Foreign::ns`], µs: other processes' GPU time from the take's `QuietStart` mark to
    /// [`Self::iokit_at`], so over the quiet, and under [`Placement::Completed`] through the
    /// take's submit and completion too.
    #[must_use]
    pub fn foreign_us(&self) -> Option<f64> {
        Some(self.foreign.as_ref()?.ns as f64 / 1e3)
    }

    /// The take's foreign duty: [`Foreign::ns`] over [`Self::foreign_span_ns`], the share of
    /// that stretch other processes' GPU clients ran, summed over clients (so above 1 where
    /// several ran at once for most of it). The stretch leaves out the two walks, while a
    /// client's growth may hold GPU time inside them, so the walks' own length cannot dilute
    /// it. A back-to-back take's stretch holds no quiet (at [`Placement::QuietEnd`] the gap
    /// between two walks, 24 µs at the median in the grid's first full run; under
    /// [`Placement::Completed`] its untimed predecessor and the take itself), so its duty is
    /// recorded and [`admission`] never bounds it. `None` where either is unread or the
    /// stretch is zero.
    #[must_use]
    pub fn foreign_duty(&self) -> Option<f64> {
        let ns = self.foreign.as_ref()?.ns;
        let span = self.foreign_span_ns.filter(|&s| s > 0)?;
        Some(ns as f64 / span as f64)
    }

    /// The graphics footprint's rise over the take: its peak less its value at the quiet's end.
    #[must_use]
    pub fn gfx_rise(&self) -> Option<f64> {
        Some(self.gfx_fp_peak? - self.gfx_fp?)
    }

    fn fields(&self) -> Vec<(&'static str, Field)> {
        use Field::{Bool, Float, Int, Text};
        let f = |v: Option<f64>, places| v.map_or(Field::Null, |v| Float(v, places));
        let i = |v: Option<u64>| v.map_or(Field::Null, Int);
        let us = |v: Option<u64>| f(v.map(|ns| ns as f64 / 1e3), 1);
        let text = |v: Option<&str>| v.map_or(Field::Null, |v| Text(v.to_owned()));
        let t = &self.take;
        let w = self.window.as_ref();
        let wi = |g: fn(&WindowCpu) -> u64| i(w.map(g));
        let burst = w.and_then(|w| w.burst);
        let burst_row = w.zip(burst).and_then(|(w, b)| w.row(b.0));
        let fg = self.foreign.as_ref();
        let ft = self.foreign_take.as_ref();
        vec![
            ("i", Int(self.index as u64)),
            ("rep", Int(self.rep as u64)),
            ("block", Int(u64::from(self.block))),
            ("arm", Text(t.arm.name().to_owned())),
            ("quiet_ms", Int(t.quiet.as_millis() as u64)),
            ("keep", Text(t.keep_name().to_owned())),
            (
                "keep_period_ms",
                i(t.keep.map(|k| k.period().as_millis() as u64)),
            ),
            ("ballast_mib", Int(u64::from(t.ballast_mib))),
            ("prev_i", i(self.prev.map(|p| p.0 as u64))),
            ("prev_arm", text(self.prev.map(|p| p.1.name()))),
            ("iokit_at", Text(self.iokit_at.name().to_owned())),
            ("at_ns", i(self.at_ns)),
            ("quiet_from_ns", i(self.quiet_from_ns)),
            ("idle_us", f(self.idle_us, 1)),
            ("queue_idle_us", f(self.queue_idle_us, 1)),
            ("submit_us", f(self.submit_us, 1)),
            ("complete_us", f(self.complete_us, 1)),
            ("gpu_us", f(self.gpu_us, 1)),
            ("window", Bool(w.is_some())),
            ("cuts", Int(self.cuts as u64)),
            ("cut_fails", Int(u64::from(self.cut_fails))),
            ("last_cut_after_done_us", f(self.last_cut_after_done_us, 1)),
            ("caller_us", us(w.and_then(|w| w.caller_ns))),
            ("caller_user_us", us(w.and_then(|w| w.caller_user_ns))),
            ("caller_sys_us", us(w.and_then(|w| w.caller_system_ns))),
            ("other_us", us(w.and_then(|w| w.other_ns))),
            ("other_sys_us", us(w.map(|w| w.other_system_ns))),
            ("other_user_us", us(w.map(|w| w.other_user_ns))),
            ("other_instr", wi(|w| w.other_instructions)),
            ("other_cycles", wi(|w| w.other_cycles)),
            ("other_energy_nj", wi(|w| w.other_energy_nj)),
            ("burst_tid", i(burst.map(|b| b.0))),
            (
                "burst_name",
                text(burst_row.and_then(|r| r.name.as_deref())),
            ),
            (
                "burst_queue",
                text(burst_row.and_then(|r| r.queue.as_deref())),
            ),
            ("burst_us", us(burst.map(|b| b.1))),
            ("burst_seen_us", us(burst.map(|b| b.2))),
            ("threads_unread", wi(|w| w.unread as u64)),
            ("counts_unread", wi(|w| w.counts_unread as u64)),
            ("threads", wi(|w| w.threads.len() as u64)),
            ("fp_quiet", f(self.fp_quiet, 1)),
            ("fp_peak", f(self.fp_peak, 1)),
            ("fp_settled", f(self.fp_settled, 1)),
            ("gfx_fp", f(self.gfx_fp, 1)),
            ("gfx_fp_peak", f(self.gfx_fp_peak, 1)),
            ("gfx_nofp", f(self.gfx_nofp, 1)),
            ("purg_vol", f(self.purg_vol, 1)),
            ("purg_vol_res", f(self.purg_vol_res, 1)),
            ("purg_vol_res_settled", f(self.purg_vol_res_settled, 1)),
            ("device_pct", i(self.device_pct.map(u64::from))),
            (
                "device_pct_settled",
                i(self.device_pct_settled.map(u64::from)),
            ),
            ("gpu_quiet_ns", i(self.gpu_quiet_ns)),
            ("gpu_ns", i(self.gpu_ns)),
            ("foreign_gpu_us", us(fg.map(|g| g.ns))),
            ("foreign_span_us", us(self.foreign_span_ns)),
            ("foreign_duty", f(self.foreign_duty(), DUTY_PLACES)),
            ("foreign_pids", i(fg.map(|g| g.pids as u64))),
            ("foreign_unread", i(fg.map(|g| g.unread as u64))),
            ("foreign_gone", i(fg.map(|g| g.gone as u64))),
            (
                "foreign_top",
                text(fg.and_then(|g| g.top.as_ref()).map(|t| t.0.as_str())),
            ),
            (
                "foreign_top_us",
                us(fg.and_then(|g| g.top.as_ref()).map(|t| t.1)),
            ),
            ("foreign_take_gpu_us", us(ft.map(|g| g.ns))),
            ("foreign_take_pids", i(ft.map(|g| g.pids as u64))),
            ("keeps", Int(u64::from(self.keeps))),
            ("last_keep_to_submit_us", f(self.last_keep_to_submit_us, 1)),
            ("keeper_commits", i(self.keeper_commits)),
            ("keeper_first_commit_ms", f(self.keeper_first_commit_ms, 1)),
            ("keep_ok", self.keep_ok.map_or(Field::Null, Bool)),
            ("releases", Int(self.releases.len() as u64)),
        ]
    }

    /// One `TAKE` line: `key=value` words, `-` for unread; `threads` and `releases` are counts.
    #[must_use]
    pub fn line(&self) -> String {
        fields_line("TAKE", &self.fields())
    }

    /// One JSON object: `"kind":"take"`, then every `TAKE` field (`null` for unread), with
    /// `threads` the array of this take's thread rows and `releases` the array of its steps.
    #[must_use]
    pub fn json(&self) -> String {
        let mut s = String::from("{\"kind\":\"take\"");
        for (k, v) in self.fields() {
            s.push(',');
            s.push_str(&json_string(k));
            s.push(':');
            match k {
                "releases" => {
                    s.push_str(&array(self.releases.iter().map(ReleaseStep::json)));
                }
                "threads" => match &self.window {
                    Some(w) => s.push_str(&array(w.threads.iter().map(ThreadRow::json))),
                    None => s.push_str("null"),
                },
                _ => s.push_str(&v.json()),
            }
        }
        s.push('}');
        s
    }
}

impl ThreadRow {
    fn json(&self) -> String {
        let f = |v: Option<u64>| v.map_or(Field::Null, Field::Int);
        let us = |v: Option<u64>| v.map_or(Field::Null, |ns| Field::Float(ns as f64 / 1e3, 1));
        let text = |v: Option<&str>| v.map_or(Field::Null, |v| Field::Text(v.to_owned()));
        object(&[
            ("tid", Field::Int(self.id)),
            ("role", text(self.role.map(ThreadRole::name))),
            ("name", text(self.name.as_deref())),
            ("queue", text(self.queue.as_deref())),
            ("queue_asked", Field::Int(u64::from(self.queue_asked))),
            ("queue_labelled", Field::Int(u64::from(self.queue_labelled))),
            ("queue_unread", Field::Int(u64::from(self.queue_unread))),
            ("cpu_us", us(Some(self.cpu_ns))),
            ("user_us", us(self.user_ns)),
            ("sys_us", us(self.system_ns)),
            ("instr", f(self.instructions)),
            ("cycles", f(self.cycles)),
            ("energy_nj", f(self.energy_nj)),
            ("moved_us", us(Some(self.moved_ns))),
        ])
    }
}

impl ReleaseStep {
    fn fields(&self) -> [(&'static str, Field); 6] {
        let f = |v: Option<f64>| v.map_or(Field::Null, |v| Field::Float(v, 1));
        [
            ("phase", Field::Text(self.phase.name().to_owned())),
            ("at_ms", Field::Float(self.at_ms, 1)),
            ("from_mib", Field::Float(self.from_mib, 1)),
            ("to_mib", Field::Float(self.to_mib, 1)),
            ("gfx_from_mib", f(self.gfx_from_mib)),
            ("gfx_to_mib", f(self.gfx_to_mib)),
        ]
    }

    /// One `RELEASE` line for a step seen in take `index`.
    #[must_use]
    pub fn line(&self, index: usize) -> String {
        let mut s = format!("RELEASE i={index}");
        for (k, v) in self.fields() {
            let _ = write!(s, " {k}={}", v.word());
        }
        let _ = write!(s, " step_mib={:.1}", self.from_mib - self.to_mib);
        s
    }

    fn json(&self) -> String {
        object(&self.fields())
    }
}

/// One field's value, as a line word and as JSON.
#[derive(Clone, Debug, PartialEq)]
pub enum Field {
    /// Unread: `-` in a line, `null` in JSON.
    Null,
    /// A flag.
    Bool(bool),
    /// A count or an integer reading.
    Int(u64),
    /// A value and its decimal places; a non-finite one is unread.
    Float(f64, usize),
    /// Text: in a line, each space, tab, newline and `=` becomes `_` so the line stays
    /// `key=value` words, and empty text is `-`.
    Text(String),
}

impl Field {
    /// The value as one word of a `key=value` line.
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Self::Null => "-".to_owned(),
            Self::Float(v, _) if !v.is_finite() => "-".to_owned(),
            Self::Bool(b) => b.to_string(),
            Self::Int(n) => n.to_string(),
            Self::Float(v, places) => format!("{v:.places$}"),
            Self::Text(t) if t.is_empty() => "-".to_owned(),
            Self::Text(t) => t
                .chars()
                .map(|c| {
                    if c.is_whitespace() || c == '=' {
                        '_'
                    } else {
                        c
                    }
                })
                .collect(),
        }
    }

    fn json(&self) -> String {
        match self {
            Self::Null => "null".to_owned(),
            Self::Float(v, _) if !v.is_finite() => "null".to_owned(),
            Self::Text(t) => json_string(t),
            other => other.word(),
        }
    }
}

/// The decimal places every foreign duty is written with, the bound's among them.
pub const DUTY_PLACES: usize = 6;

/// `tag` and `fields` as one line of `key=value` words.
#[must_use]
pub fn fields_line(tag: &str, fields: &[(&str, Field)]) -> String {
    let mut s = String::from(tag);
    for (k, v) in fields {
        let _ = write!(s, " {k}={}", v.word());
    }
    s
}

/// One JSON object of `fields`, in their order.
#[must_use]
pub fn object(fields: &[(&str, Field)]) -> String {
    let mut s = String::from("{");
    for (n, (k, v)) in fields.iter().enumerate() {
        if n > 0 {
            s.push(',');
        }
        s.push_str(&json_string(k));
        s.push(':');
        s.push_str(&v.json());
    }
    s.push('}');
    s
}

/// One JSON array of already-written `items`.
fn array(items: impl Iterator<Item = String>) -> String {
    let mut s = String::from("[");
    for (n, item) in items.enumerate() {
        if n > 0 {
            s.push(',');
        }
        s.push_str(&item);
    }
    s.push(']');
    s
}

/// `s` as a JSON string: quotes, backslashes and control characters escaped.
#[must_use]
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The median of `values` (the mean of the middle two for an even count), `None` for none.
#[must_use]
pub fn median(mut values: Vec<f64>) -> Option<f64> {
    values.retain(|v| v.is_finite());
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    Some(if n % 2 == 1 {
        values[n / 2]
    } else {
        f64::midpoint(values[n / 2 - 1], values[n / 2])
    })
}

/// The `p` quantile of `values` by nearest rank (rounded), as the existing bench took it.
#[must_use]
pub fn quantile(mut values: Vec<f64>, p: f64) -> Option<f64> {
    values.retain(|v| v.is_finite());
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let at = ((values.len() as f64 - 1.0) * p).round() as usize;
    values.get(at.min(values.len() - 1)).copied()
}

/// The default bound on a take's foreign duty ([`TakeRecord::foreign_duty`]) for the decisions
/// to admit it: other processes' GPU clients may run for a tenth of the stretch between its
/// two reads of them. `QUIET_COMMIT_FOREIGN_MAX_DUTY` sets another.
pub const FOREIGN_MAX_DUTY: f64 = 0.10;

/// Whether the decisions take a take, and why not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Its window was read, and it is back to back or its foreign duty is at most the bound.
    Admitted,
    /// Its sampler window was not read ([`window_cpu`]).
    WindowVoid,
    /// It followed a quiet and its foreign duty ([`TakeRecord::foreign_duty`]) is unknown: a
    /// read failed, a client's growth could not be read or was lost to its exit, or the
    /// stretch between the reads is unread or zero.
    ForeignUnread,
    /// It followed a quiet and its foreign duty is above the bound.
    ForeignOver,
}

/// `r`'s admission under a bound of `foreign_max_duty` on its foreign duty
/// ([`TakeRecord::foreign_duty`]): other processes' GPU time from the take's `QuietStart` mark
/// to [`TakeRecord::iokit_at`] (which under [`Placement::Completed`] also spans the take's
/// submit to its completion) over the stretch between those two reads. A back-to-back take
/// (no quiet) is admitted on its window alone: it has no quiet for other processes' work to
/// fill, and its stretch holds none, so its duty divides GPU time a client may have run inside
/// the two walks by a gap that holds none of them.
#[must_use]
pub fn admission(r: &TakeRecord, foreign_max_duty: f64) -> Admission {
    if r.window.is_none() {
        return Admission::WindowVoid;
    }
    if r.take.quiet.is_zero() {
        return Admission::Admitted;
    }
    match (&r.foreign, r.foreign_duty()) {
        (Some(f), _) if f.unread > 0 || f.gone > 0 => Admission::ForeignUnread,
        (_, None) => Admission::ForeignUnread,
        (_, Some(duty)) if duty > foreign_max_duty => Admission::ForeignOver,
        (_, Some(_)) => Admission::Admitted,
    }
}

/// Which takes an `ARM` summary reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scope {
    /// Every take whose window was read, whatever other processes ran.
    All,
    /// The takes [`admission`] admits under this bound on the foreign duty.
    Quiet(f64),
}

/// A take shape, its placement among it: what the `ARM` lines group takes by, and with a
/// [`BlockState`] a foreign split's [`Stratum`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Shape {
    /// The arm.
    pub arm: QuietArm,
    /// The quiet, ms.
    pub quiet_ms: u64,
    /// The keep's name.
    pub keep: &'static str,
    /// The keep's period, ms (0 for none).
    pub keep_period_ms: u64,
    /// The ballast, MiB.
    pub ballast_mib: u32,
    /// Where the take's quiet-end-side heavy reads were made.
    pub iokit_at: Placement,
}

impl Shape {
    /// `r`'s shape.
    #[must_use]
    pub fn of(r: &TakeRecord) -> Self {
        let t = &r.take;
        Self {
            arm: t.arm,
            quiet_ms: t.quiet.as_millis() as u64,
            keep: t.keep_name(),
            keep_period_ms: t.keep.map_or(0, |k| k.period().as_millis() as u64),
            ballast_mib: t.ballast_mib,
            iokit_at: r.iokit_at,
        }
    }

    fn fields(&self) -> [(&'static str, Field); 6] {
        [
            ("arm", Field::Text(self.arm.name().to_owned())),
            ("quiet_ms", Field::Int(self.quiet_ms)),
            ("keep", Field::Text(self.keep.to_owned())),
            ("keep_period_ms", Field::Int(self.keep_period_ms)),
            ("ballast_mib", Field::Int(u64::from(self.ballast_mib))),
            ("iokit_at", Field::Text(self.iokit_at.name().to_owned())),
        ]
    }
}

/// One `ARM` line per take shape ([`Shape`]) of the takes `scope` reads, in [`QuietArm::ALL`]'s
/// order and then by quiet, keep, period, ballast and placement: its count, the count of that
/// shape's takes the scope left out, the median and 95th percentile of other-thread CPU, the
/// medians of the caller's CPU, the submit, the completion, the own queue's idle, the graphics
/// rise, the footprint at the quiet's end, other processes' GPU time
/// ([`TakeRecord::foreign_us`]) and the foreign duty.
#[must_use]
pub fn arm_lines(records: &[TakeRecord], scope: Scope) -> Vec<String> {
    let mut groups: BTreeMap<Shape, (Vec<&TakeRecord>, usize)> = BTreeMap::new();
    for r in records {
        let read = match scope {
            Scope::All => r.window.is_some(),
            Scope::Quiet(max) => admission(r, max) == Admission::Admitted,
        };
        let group = groups.entry(Shape::of(r)).or_default();
        if read {
            group.0.push(r);
        } else {
            group.1 += 1;
        }
    }
    let set = match scope {
        Scope::All => "ARM set=all".to_owned(),
        Scope::Quiet(max) => format!(
            "ARM set=quiet foreign_max_duty={}",
            Field::Float(max, DUTY_PLACES).word()
        ),
    };
    let word = |v: Option<f64>| v.map_or("-".to_owned(), |v| format!("{v:.1}"));
    let duty = |v: Option<f64>| {
        v.map_or(Field::Null, |v| Field::Float(v, DUTY_PLACES))
            .word()
    };
    groups
        .into_iter()
        .map(|(shape, (rs, left))| {
            let col = |f: fn(&TakeRecord) -> Option<f64>| -> Vec<f64> {
                rs.iter().filter_map(|r| f(r)).collect()
            };
            let other = col(TakeRecord::other_us);
            format!(
                "{} n={} left_out={left} other_p50_us={} other_p95_us={} caller_p50_us={} \
                 submit_p50_us={} complete_p50_us={} queue_idle_p50_us={} gfx_rise_p50_mib={} \
                 fp_quiet_p50_mib={} foreign_p50_us={} foreign_duty_p50={}",
                fields_line(&set, &shape.fields()),
                rs.len(),
                word(median(other.clone())),
                word(quantile(other, 0.95)),
                word(median(col(TakeRecord::caller_us))),
                word(median(col(|r| r.submit_us))),
                word(median(col(|r| r.complete_us))),
                word(median(col(|r| r.queue_idle_us))),
                word(median(col(TakeRecord::gfx_rise))),
                word(median(col(|r| r.fp_quiet))),
                word(median(col(TakeRecord::foreign_us))),
                duty(median(col(TakeRecord::foreign_duty))),
            )
        })
        .collect()
}

/// How many takes the decisions left out, by reason.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Excluded {
    /// The takes admitted.
    pub admitted: usize,
    /// [`Admission::WindowVoid`].
    pub window_void: usize,
    /// [`Admission::ForeignUnread`].
    pub foreign_unread: usize,
    /// [`Admission::ForeignOver`].
    pub foreign_over: usize,
}

impl Excluded {
    /// `records` counted under `foreign_max_duty`.
    #[must_use]
    pub fn of(records: &[TakeRecord], foreign_max_duty: f64) -> Self {
        let mut out = Self::default();
        for r in records {
            match admission(r, foreign_max_duty) {
                Admission::Admitted => out.admitted += 1,
                Admission::WindowVoid => out.window_void += 1,
                Admission::ForeignUnread => out.foreign_unread += 1,
                Admission::ForeignOver => out.foreign_over += 1,
            }
        }
        out
    }

    /// One `EXCLUDED` line.
    #[must_use]
    pub fn line(&self, foreign_max_duty: f64) -> String {
        format!(
            "EXCLUDED admitted={} window_void={} foreign_unread={} foreign_over={} \
             foreign_max_duty={}",
            self.admitted,
            self.window_void,
            self.foreign_unread,
            self.foreign_over,
            Field::Float(foreign_max_duty, DUTY_PLACES).word()
        )
    }
}

/// The smallest rise of a take's graphics footprint ([`TakeRecord::gfx_rise`]), MiB, at which
/// it re-took a block the driver had freed. A re-taken block is 128 MiB (blit), 168 MiB
/// (compute) or 170-178 MiB (render), and in the grid's first full run every take rose by at
/// most 40 MiB or by at least 128 MiB.
pub const RETAKE_RISE_MIB: f64 = 100.0;

/// Whether a take re-took a freed block or found its blocks held: the foreign split's strata
/// part the two, since a re-take costs milliseconds more than a held take of the same shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BlockState {
    /// Its graphics footprint rose by at least [`RETAKE_RISE_MIB`].
    Retake,
    /// It rose by less.
    Held,
}

impl BlockState {
    /// `r`'s state; `None` where its graphics rise is unread or not finite.
    #[must_use]
    pub fn of(r: &TakeRecord) -> Option<Self> {
        let rise = r.gfx_rise().filter(|v| v.is_finite())?;
        Some(if rise >= RETAKE_RISE_MIB {
            Self::Retake
        } else {
            Self::Held
        })
    }

    /// The state's name in every output line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Retake => "retake",
            Self::Held => "held",
        }
    }
}

/// One stratum of a foreign split: a take shape at one placement ([`Shape`]) in one
/// [`BlockState`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Stratum {
    /// The shape, its placement among it.
    pub shape: Shape,
    /// Whether its takes re-took a freed block.
    pub state: BlockState,
}

impl Stratum {
    fn fields(&self) -> Vec<(&'static str, Field)> {
        let mut out = vec![("state", Field::Text(self.state.name().to_owned()))];
        out.extend(self.shape.fields());
        out
    }
}

/// The fewest takes of one stratum a foreign split reads: a pair, the fewest a slope needs.
pub const STRATUM_MIN_TAKES: usize = 2;

/// The most the judged foreign split's effect ([`ForeignSplit::effect_us`]) may be, µs, either
/// way: half of the 500 µs `p_back_to_back` holds its median to, the smallest CPU bound
/// another control here holds, and a tenth of the 2.5 ms `p_idle_cost` requires of P's
/// re-take. The effect is the median pair slope times the duty range, so it bounds the cost
/// other processes' GPU work under the admission bound went with across the strata's pairs,
/// not what it did to any one take.
pub const SPLIT_MAX_EFFECT_US: f64 = 250.0;

/// The placement whose foreign split the `foreign_split` control judges. Its foreign read ends
/// at the quiet's end, before the take's submit, so it is read before the cost it would
/// explain. Under [`Placement::Completed`] the read runs on through the take's submit and
/// completion, and admits the take by what it read there, so a split there sets the take's
/// cost against a reading the take's own work and length fall inside: it is recorded, never
/// judged. A run with no take at this placement gives the control none (`no_takes`, void).
pub const SPLIT_JUDGED_AT: Placement = Placement::QuietEnd;

/// One stratum's takes as a foreign split read them. Its own slope is recorded and never
/// judged: from one stratum's few takes it is too noisy to hold to the bound alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StratumRow {
    /// The stratum.
    pub stratum: Stratum,
    /// Its takes.
    pub n: usize,
    /// Its pairs of takes of different foreign duty: the slopes it adds to the pooled median.
    pub pairs: usize,
    /// Its lowest foreign duty.
    pub duty_min: f64,
    /// Its highest foreign duty.
    pub duty_max: f64,
    /// Its median other-thread CPU, µs.
    pub other_p50_us: f64,
    /// The median of its own pairs' slopes, µs per unit of foreign duty; `None` where it has no
    /// pair of different duty.
    pub slope_us_per_duty: Option<f64>,
}

impl StratumRow {
    fn fields(&self) -> Vec<(&'static str, Field)> {
        let duty = |v| Field::Float(v, DUTY_PLACES);
        let mut out = self.stratum.fields();
        out.extend([
            ("n", Field::Int(self.n as u64)),
            ("pairs", Field::Int(self.pairs as u64)),
            ("duty_min", duty(self.duty_min)),
            ("duty_max", duty(self.duty_max)),
            ("other_p50_us", Field::Float(self.other_p50_us, 1)),
            (
                "slope_us_per_duty",
                self.slope_us_per_duty
                    .map_or(Field::Null, |v| Field::Float(v, 1)),
            ),
        ]);
        out
    }

    /// One `SPLIT` line: the stratum, its takes, its duty span and its own slope.
    #[must_use]
    pub fn line(&self) -> String {
        fields_line("SPLIT", &self.fields())
    }

    fn json(&self) -> String {
        object(&self.fields())
    }
}

/// What a foreign split says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitVerdict {
    /// No stratum holds [`STRATUM_MIN_TAKES`] takes: nothing was read, and the control is void.
    NoTakes,
    /// No stratum holds two takes of different foreign duty (on a machine where nothing else
    /// ran every take's duty is zero): the duty varied within no stratum, so no take's cost
    /// can be set against another's by it. The control holds.
    Flat,
    /// The effect is at most [`SPLIT_MAX_EFFECT_US`] either way.
    Holds,
    /// It is more: within the strata, other processes' GPU work under the admission bound went
    /// with a different cost.
    Over,
}

impl SplitVerdict {
    /// The verdict's name in every output line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NoTakes => "no_takes",
            Self::Flat => "flat",
            Self::Holds => "holds",
            Self::Over => "over",
        }
    }
}

/// Whether other processes' GPU work under the admission bound moved other-thread CPU, at one
/// placement: its strata's takes, and the slope pooled over them.
#[derive(Clone, Debug, PartialEq)]
pub struct ForeignSplit {
    /// The placement whose takes it read.
    pub placement: Placement,
    /// Every stratum with at least [`STRATUM_MIN_TAKES`] takes, in the `ARM` lines' order and
    /// then [`BlockState::Retake`] before [`BlockState::Held`].
    pub strata: Vec<StratumRow>,
    /// Takes alone in their stratum: in no pair, and outside [`Self::duty_range`].
    pub alone: usize,
    /// Admitted back-to-back takes, in no stratum: [`admission`] does not bound their duty,
    /// which holds no quiet.
    pub no_quiet: usize,
    /// Takes in no stratum: their graphics rise is unread.
    pub state_unread: usize,
    /// The pairs pooled: every stratum's pairs of takes of different foreign duty.
    pub pairs: usize,
    /// The median over those pairs of each pair's other-thread CPU difference over its foreign
    /// duty difference, µs per unit of duty (Sen's slope, its pairs taken within strata only);
    /// `None` where there is no pair.
    pub slope_us_per_duty: Option<f64>,
    /// The highest foreign duty of the takes in [`Self::strata`] less the lowest; `None` where
    /// there is no stratum.
    pub duty_range: Option<f64>,
}

impl ForeignSplit {
    /// The takes in [`Self::strata`].
    #[must_use]
    pub fn takes(&self) -> usize {
        self.strata.iter().map(|s| s.n).sum()
    }

    /// Whether the `foreign_split` control judges it: its placement is [`SPLIT_JUDGED_AT`].
    #[must_use]
    pub fn judged(&self) -> bool {
        self.placement == SPLIT_JUDGED_AT
    }

    /// [`Self::slope_us_per_duty`] times [`Self::duty_range`], µs: the cost the pooled slope
    /// puts between the least and the most foreign-busy take of the strata. `None` where
    /// either is.
    #[must_use]
    pub fn effect_us(&self) -> Option<f64> {
        Some(self.slope_us_per_duty? * self.duty_range?)
    }

    /// The split's verdict.
    #[must_use]
    pub fn verdict(&self) -> SplitVerdict {
        if self.strata.is_empty() {
            return SplitVerdict::NoTakes;
        }
        match self.effect_us() {
            None => SplitVerdict::Flat,
            Some(e) if e.abs() <= SPLIT_MAX_EFFECT_US => SplitVerdict::Holds,
            Some(_) => SplitVerdict::Over,
        }
    }

    /// Its placement, verdict and counts, pooled slope, duty range and effect.
    fn fields(&self) -> Vec<(&'static str, Field)> {
        let float = |v: Option<f64>, places| v.map_or(Field::Null, |v| Field::Float(v, places));
        vec![
            ("iokit_at", Field::Text(self.placement.name().to_owned())),
            ("verdict", Field::Text(self.verdict().name().to_owned())),
            ("strata", Field::Int(self.strata.len() as u64)),
            ("takes", Field::Int(self.takes() as u64)),
            ("alone", Field::Int(self.alone as u64)),
            ("no_quiet", Field::Int(self.no_quiet as u64)),
            ("state_unread", Field::Int(self.state_unread as u64)),
            ("pairs", Field::Int(self.pairs as u64)),
            ("slope_us_per_duty", float(self.slope_us_per_duty, 1)),
            ("duty_range", float(self.duty_range, DUTY_PLACES)),
            ("effect_us", float(self.effect_us(), 1)),
        ]
    }

    /// One `POOLED` line: whether the control judges this split, then its [`Self::fields`].
    #[must_use]
    pub fn line(&self) -> String {
        let mut fields = vec![("judged", Field::Bool(self.judged()))];
        fields.extend(self.fields());
        fields_line("POOLED", &fields)
    }

    fn json(&self) -> String {
        let mut fields = vec![("judged", Field::Bool(self.judged()))];
        fields.extend(self.fields());
        let mut s = object(&fields);
        s.pop();
        s.push_str(",\"strata\":");
        s.push_str(&array(self.strata.iter().map(StratumRow::json)));
        s.push('}');
        s
    }
}

/// Every pair of `takes` of different duty: each pair's `(duty, other-thread CPU µs)` slope.
fn pair_slopes(takes: &[(f64, f64)]) -> Vec<f64> {
    let mut out = Vec::new();
    for (i, a) in takes.iter().enumerate() {
        for b in &takes[i + 1..] {
            let dx = b.0 - a.0;
            if dx != 0.0 {
                out.push((b.1 - a.1) / dx);
            }
        }
    }
    out
}

/// The foreign split at `placement`: of the takes read there, those [`admission`] admits under
/// `foreign_max_duty` after a quiet whose foreign duty and other-thread CPU were read. Each
/// goes to its [`Stratum`], parted by [`BlockState`], so a shape whose takes straddle the
/// driver's release gives no slope from the step between re-take and held. Pairs are taken
/// within a stratum only, so no stratum's level enters a slope, and their slopes are pooled
/// into one median over every stratum: one stratum's own takes are too few to hold a slope to
/// the bound by chance. An admitted back-to-back take is counted and left out, since its duty
/// holds no quiet ([`admission`]).
#[must_use]
pub fn foreign_split(
    records: &[TakeRecord],
    foreign_max_duty: f64,
    placement: Placement,
) -> ForeignSplit {
    let mut out = ForeignSplit {
        placement,
        strata: Vec::new(),
        alone: 0,
        no_quiet: 0,
        state_unread: 0,
        pairs: 0,
        slope_us_per_duty: None,
        duty_range: None,
    };
    let mut groups: BTreeMap<Stratum, Vec<(f64, f64)>> = BTreeMap::new();
    for r in records
        .iter()
        .filter(|r| r.iokit_at == placement)
        .filter(|r| admission(r, foreign_max_duty) == Admission::Admitted)
    {
        if r.take.quiet.is_zero() {
            out.no_quiet += 1;
            continue;
        }
        let (Some(duty), Some(other)) = (r.foreign_duty(), r.other_us()) else {
            continue;
        };
        match BlockState::of(r) {
            Some(state) => groups
                .entry(Stratum {
                    shape: Shape::of(r),
                    state,
                })
                .or_default()
                .push((duty, other)),
            None => out.state_unread += 1,
        }
    }
    let mut pooled = Vec::new();
    let mut range: Option<(f64, f64)> = None;
    for (stratum, points) in groups {
        if points.len() < STRATUM_MIN_TAKES {
            out.alone += points.len();
            continue;
        }
        let slopes = pair_slopes(&points);
        let duties = || points.iter().map(|t| t.0);
        let (lo, hi) = (
            duties().fold(f64::INFINITY, f64::min),
            duties().fold(f64::NEG_INFINITY, f64::max),
        );
        range = Some(range.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
        out.strata.push(StratumRow {
            stratum,
            n: points.len(),
            pairs: slopes.len(),
            duty_min: lo,
            duty_max: hi,
            other_p50_us: median(points.iter().map(|t| t.1).collect()).unwrap_or(f64::NAN),
            slope_us_per_duty: median(slopes.clone()),
        });
        pooled.extend(slopes);
    }
    out.pairs = pooled.len();
    out.slope_us_per_duty = median(pooled);
    out.duty_range = range.map(|(lo, hi)| hi - lo);
    out
}

/// One validity control: whether it held, what it measured, and what it needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Control {
    /// Its name.
    pub name: &'static str,
    /// Whether it held. A run with any control that did not hold is void: recorded, never
    /// decided on.
    pub holds: bool,
    /// What it measured, as `key=value` words (`no_takes` where it had nothing to measure).
    pub value: String,
    /// What it needs.
    pub need: &'static str,
}

impl Control {
    /// `VALID …` where it held, `VOID …` where it did not.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "{} control={} {} need={}",
            if self.holds { "VALID" } else { "VOID" },
            self.name,
            self.value,
            self.need
        )
    }

    fn json(&self) -> String {
        object(&[
            ("name", Field::Text(self.name.to_owned())),
            ("holds", Field::Bool(self.holds)),
            ("value", Field::Text(self.value.clone())),
            ("need", Field::Text(self.need.to_owned())),
        ])
    }
}

/// The admitted takes of a bare `arm` after `quiet_ms` (no keep, no ballast), whichever block
/// they are.
fn bare<'a>(
    records: &'a [TakeRecord],
    arm: QuietArm,
    quiet_ms: u64,
    foreign_max_duty: f64,
) -> impl Iterator<Item = &'a TakeRecord> + 'a {
    let want = QuietTake::bare(arm, quiet_ms);
    records
        .iter()
        .filter(move |r| r.take == want && admission(r, foreign_max_duty) == Admission::Admitted)
}

/// The `foreign_split` control over `split`, the split at [`SPLIT_JUDGED_AT`]: it holds where
/// the verdict is [`SplitVerdict::Holds`] or [`SplitVerdict::Flat`], and is `no_takes` (void)
/// where no stratum had [`STRATUM_MIN_TAKES`] takes. Otherwise its value is the words of the
/// split's `POOLED` line ([`ForeignSplit::line`]) after `judged`: its placement, verdict and
/// counts, pooled slope, duty range and effect.
fn split_control(split: &ForeignSplit) -> Control {
    const NEED: &str = "abs(effect_us)<=250";
    let verdict = split.verdict();
    Control {
        name: "foreign_split",
        holds: matches!(verdict, SplitVerdict::Holds | SplitVerdict::Flat),
        value: if verdict == SplitVerdict::NoTakes {
            "no_takes".to_owned()
        } else {
            fields_line("", &split.fields()).trim_start().to_owned()
        },
        need: NEED,
    }
}

/// The run's four controls, each over the takes [`admission`] admits under `foreign_max_duty`:
/// - `clear_release` (positive): [`QuietArm::Clear`]'s median footprint at the quiet's end
///   drops by at least 64 MiB from the 1.0 s quiet to the 1.5 s one, so the driver's release
///   happened where the 2026-11-04 probe saw it;
/// - `p_idle_cost` (positive): [`QuietArm::P`] after 5 s spends at least 2.5 ms of other-thread
///   CPU at the median, with a median graphics rise of at least [`RETAKE_RISE_MIB`], so the
///   headless arm reproduces the window's burst;
/// - `p_back_to_back` (negative): [`QuietArm::P`] back to back spends at most 0.5 ms;
/// - `foreign_split` (negative): at [`SPLIT_JUDGED_AT`], the slope of other-thread CPU on
///   foreign duty, pooled over every stratum's pairs ([`foreign_split`]), times the duty range
///   of the strata's takes is at most [`SPLIT_MAX_EFFECT_US`] either way, so other processes'
///   GPU work under the bound went with no fitted cost difference larger than that
///   ([`split_control`]); a run with no stratum there, one made at [`Placement::Completed`]
///   alone among them, is void.
#[must_use]
pub fn controls(records: &[TakeRecord], foreign_max_duty: f64) -> Vec<Control> {
    let med = |arm, q, f: fn(&TakeRecord) -> Option<f64>| {
        median(
            bare(records, arm, q, foreign_max_duty)
                .filter_map(f)
                .collect(),
        )
    };
    let release = match (
        med(QuietArm::Clear, 1_000, |r| r.fp_quiet),
        med(QuietArm::Clear, 1_500, |r| r.fp_quiet),
    ) {
        (Some(held), Some(gone)) => Control {
            name: "clear_release",
            holds: held - gone >= 64.0,
            value: format!(
                "fp_quiet_1000_mib={held:.1} fp_quiet_1500_mib={gone:.1} drop_mib={:.1}",
                held - gone
            ),
            need: "drop_mib>=64",
        },
        _ => Control {
            name: "clear_release",
            holds: false,
            value: "no_takes".to_owned(),
            need: "drop_mib>=64",
        },
    };
    let idle = match (
        med(QuietArm::P, HOLD_QUIET_MS, TakeRecord::other_us),
        med(QuietArm::P, HOLD_QUIET_MS, TakeRecord::gfx_rise),
    ) {
        (Some(other), Some(rise)) => Control {
            name: "p_idle_cost",
            holds: other >= 2_500.0 && rise >= RETAKE_RISE_MIB,
            value: format!("other_p50_us={other:.1} gfx_rise_p50_mib={rise:.1}"),
            need: "other_p50_us>=2500,gfx_rise_p50_mib>=100",
        },
        _ => Control {
            name: "p_idle_cost",
            holds: false,
            value: "no_takes".to_owned(),
            need: "other_p50_us>=2500,gfx_rise_p50_mib>=100",
        },
    };
    let back = match med(QuietArm::P, 0, TakeRecord::other_us) {
        Some(other) => Control {
            name: "p_back_to_back",
            holds: other <= 500.0,
            value: format!("other_p50_us={other:.1}"),
            need: "other_p50_us<=500",
        },
        None => Control {
            name: "p_back_to_back",
            holds: false,
            value: "no_takes".to_owned(),
            need: "other_p50_us<=500",
        },
    };
    let split = split_control(&foreign_split(records, foreign_max_duty, SPLIT_JUDGED_AT));
    vec![release, idle, back, split]
}

/// The JSON file's last object: `"kind":"controls"`, the bound, the counts [`Excluded`] gives,
/// whether every control held, each control, and each of `splits` (`splits`, from
/// [`foreign_split`]: its placement, whether the control judges it, its figures and its
/// `strata`).
#[must_use]
pub fn controls_json(
    controls: &[Control],
    splits: &[ForeignSplit],
    excluded: &Excluded,
    foreign_max_duty: f64,
) -> String {
    let mut s = object(&[
        ("kind", Field::Text("controls".to_owned())),
        (
            "foreign_max_duty",
            Field::Float(foreign_max_duty, DUTY_PLACES),
        ),
        ("admitted", Field::Int(excluded.admitted as u64)),
        ("window_void", Field::Int(excluded.window_void as u64)),
        ("foreign_unread", Field::Int(excluded.foreign_unread as u64)),
        ("foreign_over", Field::Int(excluded.foreign_over as u64)),
        ("valid", Field::Bool(controls.iter().all(|c| c.holds))),
    ]);
    s.pop();
    s.push_str(",\"controls\":");
    s.push_str(&array(controls.iter().map(Control::json)));
    s.push_str(",\"splits\":");
    s.push_str(&array(splits.iter().map(ForeignSplit::json)));
    s.push('}');
    s
}
