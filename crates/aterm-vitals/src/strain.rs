// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How hard the whole Mac is working now, and for how long: the readings an at-a-glance strain
//! icon draws, whatever its art. The 2026-10-24 sketches for the Stats Deck's top-left slot (a
//! gauge, a fan, a flame and a chip) each answer the same two questions, so the answers live here
//! and the art only maps them.
//!
//! - [`Strain::level`] is now: the busier engine's share of its capacity, the larger of the CPU's
//!   and the GPU's now % that the deck prints, lifted by memory pressure ([`level()`]).
//! - [`Strain::over`] is how far past capacity now: 0 within capacity, otherwise a log scale
//!   from [`OVER_FLOOR`] that pins at 1 ([`over()`]).
//! - [`Strain::soak`] is how long: a recency-weighted share of the hour spent over capacity, an
//!   exponential average with time constant [`SOAK_TAU_S`]. It builds over about 15 minutes of
//!   hard load and gives the heat back as slowly ([`soak()`]).
//!
//! Every input is a reading the snapshot already keeps: the CPU and GPU rows' now and capacity,
//! [`RowFacts::queue`](crate::RowFacts::queue) (P10, the machine's runnable threads over 10 s,
//! where read, else the 1-minute load average: [`crate::runq`]) and
//! [`RowFacts::swap_rate`](crate::RowFacts::swap_rate), the memory pressure, and the hour rings'
//! per-bucket over flags and pressure. Two readings it would want are not kept, so it reads the
//! nearest kept one instead, and one kept reading it leaves out on purpose:
//!
//! - No bucket keeps its run queue or swap rate, so the soak cannot grade how far over each
//!   bucket was. It reads the buckets' over flags, so an hour at twice the cores soaks as much as
//!   an hour at ten times.
//! - The GPU keeps no over flag: its reading is a share of time busy, which cannot pass 100%. A
//!   GPU flat out lifts the level to 1 but never moves the over or the soak.
//! - The deck's CPU over needs a queue (the run queue past the cores) as well as busy cores. A
//!   CPU pegged with nothing waiting is at capacity, not over it, so it soaks nothing. The
//!   bucket means are kept, but the soak leaves them out on purpose: the owner's own light
//!   capture has pegged ten-second cells, and a light Mac must read calm.
//!
//! One rule of CPU over, [`cpu_over_depth`], holds the dial ([`over()`]), the CPU row's over
//! flags and the queue tile ([`crate::units::cpu_over`]) and the CPU cause pointer's score
//! ([`cpu_score`]), so the pointer never paints CPU over while the needle sits below the redline
//! (A4 §3).

use crate::model::{CPU_OVER_BUSY, Pressure, Resource, ResourceView, SWAP_OVER_BPS, Snapshot};
use crate::ring::{BUCKET_S, Cell, RING_BUCKETS};

/// The level memory pressure `warn` lifts [`Strain::level`] to: the middle of heat step 5
/// (`0.80..0.95` in [`crate::ink::HEAT_EDGES`]), as the chip and flame sketches lift it, so an
/// icon that steps the level by the deck's heat edges draws the kernel reclaiming in the
/// second-hottest ink.
pub const PRESSURE_WARN_LEVEL: f32 = 0.875;

/// The level memory pressure `critical` lifts [`Strain::level`] to: full.
pub const PRESSURE_CRITICAL_LEVEL: f32 = 1.0;

/// The least [`Strain::over`] that anything over capacity reads (the gauge sketch's floor), so
/// the first step past capacity is visible.
pub const OVER_FLOOR: f32 = 0.12;

/// Doublings of the run queue past the cores that pin [`Strain::over`] at 1: 3, so twice the
/// cores reads a third and eight times the cores pins it.
pub const OVER_LOAD_DOUBLINGS: f64 = 3.0;

/// Doublings of swap traffic past [`SWAP_OVER_BPS`] (4 MiB/s) that pin [`Strain::over`] at 1:
/// 4, so 16 MiB/s reads a half and 64 MiB/s pins it.
pub const OVER_SWAP_DOUBLINGS: f64 = 4.0;

/// The soak's time constant, seconds: 10 minutes, the fan sketch's.
///
/// From calm, continuous load past capacity soaks 0.39 by 5 minutes, 0.63 by 10, 0.78 by 15,
/// 0.95 by 30 and 1.00 by the hour. Once it stops, the soak halves every 6.9 minutes
/// (`τ ln 2`): an hour of it reads 0.55 after 6 calm minutes and 0.05 after 30.
pub const SOAK_TAU_S: f64 = 600.0;

/// What a bucket with memory pressure `warn` adds to the soak, against 1 for a bucket over
/// capacity: the kernel reclaiming is memory at capacity, not past it (the fan sketch's weight).
pub const WARN_SCORE: f64 = 0.5;

/// How hard the whole Mac is working now, and for how long.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Strain {
    /// The share of the Mac's capacity busy now, `0..=1`; `None` where neither the CPU, the GPU
    /// nor a raised memory pressure was read ([`level()`]).
    pub level: Option<f32>,
    /// How far past capacity now: 0 within capacity or unread, else
    /// [`OVER_FLOOR`]`..=1` ([`over()`]).
    pub over: f32,
    /// How much of the recent hour was spent past capacity, `0..=1` ([`soak()`]).
    pub soak: f32,
}

impl Strain {
    /// The strain of a snapshot: [`strain`] of its [`Now`] and its [`hour_of`].
    #[must_use]
    pub fn of(snap: &Snapshot) -> Self {
        strain(&Now::of(snap), hour_of(snap))
    }
}

/// The readings of the moment that [`level`] and [`over`] read. A field that is `None` (or not
/// finite) was not read.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Now {
    /// CPU: busy cores as a share of the cores, the deck's CPU now % (a 10 s trailing mean).
    pub cpu: Option<f64>,
    /// GPU: busy share of time, the deck's GPU now %.
    pub gpu: Option<f64>,
    /// CPU: logical cores.
    pub cores: Option<f64>,
    /// CPU: the run queue the over rule reads, threads: P10 where read, else the 1-minute load
    /// average ([`crate::runq::RunQueue`]).
    pub queue: Option<f64>,
    /// Memory: swap traffic, in plus out, bytes per second.
    pub swap_bps: Option<f64>,
    /// Memory: the kernel's pressure.
    pub pressure: Pressure,
}

impl Now {
    /// The readings a snapshot holds: the CPU and GPU rows' now against their capacity, the run
    /// queue and swap rate the rows' facts keep, and the memory reading's pressure.
    #[must_use]
    pub fn of(snap: &Snapshot) -> Self {
        let cpu = &snap.resources[Resource::Cpu.index()];
        Self {
            cpu: share(cpu),
            gpu: share(&snap.resources[Resource::Gpu.index()]),
            cores: cpu.capacity,
            queue: snap.facts.queue.map(|q| q.threads),
            swap_bps: snap.facts.swap_rate,
            pressure: snap
                .mem
                .as_ref()
                .map_or(Pressure::Unknown, |m| m.sample.pressure),
        }
    }
}

/// A row's now as a share of its capacity, as the deck's now % prints it.
fn share(row: &ResourceView) -> Option<f64> {
    let (now, cap) = (row.now?, row.capacity?);
    (cap > 0.0).then_some(now / cap)
}

/// One 10 s bucket of the hour as [`soak()`] reads it. The default is a bucket nothing was read in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    /// The CPU bucket was over ([`cpu_over_depth`]): the run queue past the cores with at least
    /// [`CPU_OVER_BUSY`] of them busy, for at least half its readings.
    pub cpu_over: bool,
    /// The Memory bucket was over: swapping at [`SWAP_OVER_BPS`] (4 MiB/s) or more, for at least
    /// half its readings.
    pub mem_over: bool,
    /// The worst memory pressure read in the bucket; `None` where none was read.
    pub pressure: Option<Pressure>,
}

impl Flags {
    /// What the bucket adds to the soak: 1 when over capacity (CPU over, Memory over, or
    /// pressure `critical`), [`WARN_SCORE`] for pressure `warn`, and 0 otherwise, an unread
    /// bucket included.
    #[must_use]
    pub fn score(self) -> f64 {
        match (self.cpu_over || self.mem_over, self.pressure) {
            (true, _) | (false, Some(Pressure::Critical)) => 1.0,
            (false, Some(Pressure::Warn)) => WARN_SCORE,
            (false, _) => 0.0,
        }
    }
}

/// The hour a snapshot keeps, oldest first and newest (the open bucket) last: the CPU and Memory
/// rows' over flags and Memory's pressure, bucket for bucket by wall-clock key, at most
/// [`RING_BUCKETS`]. A bucket a row did not hold reads as not over and unread.
pub fn hour_of(snap: &Snapshot) -> impl Iterator<Item = Flags> + '_ {
    let cpu = &snap.resources[Resource::Cpu.index()];
    let mem = &snap.resources[Resource::Memory.index()];
    let span = |v: &ResourceView| (!v.ring.is_empty()).then(|| (first_k(v), v.ring_k));
    let (first, last) = match (span(cpu), span(mem)) {
        (Some((a0, a1)), Some((b0, b1))) => (a0.min(b0), a1.max(b1)),
        (Some(one), None) | (None, Some(one)) => one,
        (None, None) => (1, 0),
    };
    let keep = i64::try_from(RING_BUCKETS).unwrap_or(i64::MAX);
    let first = first.max(last.saturating_sub(keep).saturating_add(1));
    (first..=last).map(move |k| {
        let mem_i = index(mem, k);
        Flags {
            cpu_over: index(cpu, k).is_some_and(|i| over_half(&cpu.ring[i])),
            mem_over: mem_i.is_some_and(|i| over_half(&mem.ring[i])),
            pressure: mem_i.and_then(|i| mem.pressure.get(i).copied().flatten()),
        }
    })
}

/// Whether at least half of a bucket's read time was over: the soak scores a bucket whole, so
/// it counts one over when it mostly was. The strip draws the share itself
/// (`Cell::over_share`, A1 §3).
fn over_half(c: &Cell) -> bool {
    c.tau_ms > 0.0 && c.over_share() * 2.0 >= 1.0
}

/// The bucket key of a row's oldest cell.
fn first_k(v: &ResourceView) -> i64 {
    let len = i64::try_from(v.ring.len()).unwrap_or(i64::MAX);
    v.ring_k.saturating_sub(len).saturating_add(1)
}

/// The index of bucket `k` in a row's ring, while the ring holds it.
fn index(v: &ResourceView, k: i64) -> Option<usize> {
    usize::try_from(k.checked_sub(first_k(v))?)
        .ok()
        .filter(|&i| i < v.ring.len())
}

/// The strain of the readings `now` and the hour `hour` (oldest first, one [`Flags`] per 10 s
/// bucket, newest last).
#[must_use]
pub fn strain(now: &Now, hour: impl IntoIterator<Item = Flags>) -> Strain {
    Strain {
        level: level(now),
        over: over(now),
        soak: soak(hour),
    }
}

/// The share of the Mac's capacity busy now, `0..=1`: the larger of the CPU's and the GPU's busy
/// shares (every sketch's rule), lifted to [`PRESSURE_WARN_LEVEL`] under memory pressure `warn`
/// and to [`PRESSURE_CRITICAL_LEVEL`] under `critical`. Memory in use does not count: a full file
/// cache is not work. `None` where none of the three was read or raised.
#[must_use]
pub fn level(now: &Now) -> Option<f32> {
    let floor = match now.pressure {
        Pressure::Warn => Some(f64::from(PRESSURE_WARN_LEVEL)),
        Pressure::Critical => Some(f64::from(PRESSURE_CRITICAL_LEVEL)),
        Pressure::Normal | Pressure::Unknown => None,
    };
    [read(now.cpu), read(now.gpu), floor]
        .into_iter()
        .flatten()
        .reduce(f64::max)
        .map(|v| v.clamp(0.0, 1.0) as f32)
}

/// How far past capacity the Mac is now: 0 within capacity or where nothing says otherwise, and
/// otherwise the larger depth below, held to [`OVER_FLOOR`]`..=1`:
///
/// - the CPU, by the deck's one over rule, [`cpu_over_depth`] (the run queue past the cores
///   while at least [`CPU_OVER_BUSY`] of them are busy): `log2(queue / cores) /
///   OVER_LOAD_DOUBLINGS`;
/// - swapping at [`SWAP_OVER_BPS`] (4 MiB/s) or more:
///   `log2(swap / SWAP_OVER_BPS) / OVER_SWAP_DOUBLINGS`;
/// - memory pressure `critical`, the kernel killing to make room: the floor, since it has no
///   scale of its own.
///
/// Within capacity is 0 even when the CPU or the GPU is pegged: full is not over until work
/// waits. Memory past capacity can read over while [`level()`] is low: an icon that draws one scale
/// puts the over past the level's end.
#[must_use]
pub fn over(now: &Now) -> f32 {
    let cpu = cpu_over_depth(now.cpu, now.cores, now.queue);
    let swap = read(now.swap_bps)
        .filter(|&r| r >= SWAP_OVER_BPS)
        .map(|r| (r / SWAP_OVER_BPS).log2() / OVER_SWAP_DOUBLINGS);
    let critical = (now.pressure == Pressure::Critical).then_some(0.0);
    [cpu, swap, critical]
        .into_iter()
        .flatten()
        .reduce(f64::max)
        .map_or(0.0, |d| d.clamp(f64::from(OVER_FLOOR), 1.0) as f32)
}

/// The CPU's depth past capacity by the deck's one over rule (A4 §3): where at least
/// [`CPU_OVER_BUSY`] of the `cores` are busy (`busy` their share, `0..=1`) and the run queue
/// `queue` (threads) passes them, `log2(queue / cores) / OVER_LOAD_DOUBLINGS` held to
/// [`OVER_FLOOR`]`..=1`; `None` within capacity or where any of the three is unread or not a
/// number.
///
/// The dial ([`over()`]), the CPU row's over flags and queue tile ([`crate::units::cpu_over`])
/// and the CPU cause pointer ([`cpu_score`]) all read it, each with its own `busy` (the dial and
/// the pointer the row's 10 s mean, a flag its own reading's) and the one queue
/// ([`crate::RowFacts::queue`]).
#[must_use]
pub fn cpu_over_depth(busy: Option<f64>, cores: Option<f64>, queue: Option<f64>) -> Option<f64> {
    let (busy, cores, queue) = (read(busy)?, read(cores)?, read(queue)?);
    (cores > 0.0 && queue > cores && busy >= CPU_OVER_BUSY)
        .then(|| ((queue / cores).log2() / OVER_LOAD_DOUBLINGS).clamp(f64::from(OVER_FLOOR), 1.0))
}

/// The CPU cause pointer's score (A4 §3): `1 + over` while the CPU is over
/// ([`cpu_over_depth`]), else its busy share held under 1; `None` where the busy share is unread.
/// A score of 1 or more draws in over ink, so it does exactly while the dial reads the CPU over:
/// every core pegged with nothing waiting is at capacity, not past it.
#[must_use]
pub fn cpu_score(now: &Now) -> Option<f64> {
    let busy = read(now.cpu)?;
    Some(
        cpu_over_depth(now.cpu, now.cores, now.queue)
            .map_or(busy.min(1.0 - f64::EPSILON), |over| 1.0 + over),
    )
}

/// How much of the recent hour was spent past capacity, `0..=1`: the exponential average of each
/// bucket's [`Flags::score`], oldest first, with time constant [`SOAK_TAU_S`], starting from
/// calm. Each bucket weighs `1 - e^(-10 s / τ)`, and everything before it fades by `e^(-10 s / τ)`,
/// so an unread bucket (a Mac asleep, a ring just started) cools like a calm one. A full hour
/// past capacity reads 0.998; anything older than the hour weighs at most `e^-6`.
#[must_use]
pub fn soak(hour: impl IntoIterator<Item = Flags>) -> f32 {
    let keep = (-(BUCKET_S as f64) / SOAK_TAU_S).exp();
    let s = hour
        .into_iter()
        .fold(0.0, |s, f| keep * s + (1.0 - keep) * f.score());
    s.clamp(0.0, 1.0) as f32
}

/// A reading, where it was read and is a number.
fn read(v: Option<f64>) -> Option<f64> {
    v.filter(|v| v.is_finite())
}

#[cfg(test)]
#[path = "strain_tests.rs"]
mod tests;
