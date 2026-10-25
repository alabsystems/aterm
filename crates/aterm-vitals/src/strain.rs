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
//! [`RowFacts::load1`](crate::RowFacts::load1) and [`RowFacts::swap_rate`](crate::RowFacts::swap_rate),
//! the memory pressure, and the hour rings' per-bucket over flags and pressure. Two readings it
//! would want are not kept, so it reads the nearest kept one instead, and one kept reading it
//! leaves out on purpose:
//!
//! - No bucket keeps its load average or swap rate, so the soak cannot grade how far over each
//!   bucket was. It reads the buckets' over flags, so an hour at twice the cores soaks as much as
//!   an hour at ten times.
//! - The GPU keeps no over flag: its reading is a share of time busy, which cannot pass 100%. A
//!   GPU flat out lifts the level to 1 but never moves the over or the soak.
//! - The deck's CPU over needs a queue (the load average past the cores) as well as busy cores.
//!   A CPU pegged with nothing waiting is at capacity, not over it, so it soaks nothing. The
//!   bucket means are kept, but the soak leaves them out on purpose: the owner's own light
//!   capture has pegged ten-second cells, and a light Mac must read calm.

use crate::model::{CPU_OVER_BUSY, Pressure, Resource, ResourceView, SWAP_OVER_BPS, Snapshot};
use crate::ring::{BUCKET_S, RING_BUCKETS};

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

/// Doublings of the load average past the cores that pin [`Strain::over`] at 1: 3, so twice the
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
    /// CPU: the 1-minute load average.
    pub load1: Option<f64>,
    /// Memory: swap traffic, in plus out, bytes per second.
    pub swap_bps: Option<f64>,
    /// Memory: the kernel's pressure.
    pub pressure: Pressure,
}

impl Now {
    /// The readings a snapshot holds: the CPU and GPU rows' now against their capacity, the load
    /// average and swap rate the rows' facts keep, and the memory reading's pressure.
    #[must_use]
    pub fn of(snap: &Snapshot) -> Self {
        let cpu = &snap.resources[Resource::Cpu.index()];
        Self {
            cpu: share(cpu),
            gpu: share(&snap.resources[Resource::Gpu.index()]),
            cores: cpu.capacity,
            load1: snap.facts.load1,
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
    (cap > 0.0).then(|| now / cap)
}

/// One 10 s bucket of the hour as [`soak()`] reads it. The default is a bucket nothing was read in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    /// The CPU bucket was over: the load average past the cores with at least
    /// [`CPU_OVER_BUSY`] of them busy, for at least half its readings (the strip's over cap).
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
            cpu_over: index(cpu, k).is_some_and(|i| cpu.ring[i].over),
            mem_over: mem_i.is_some_and(|i| mem.ring[i].over),
            pressure: mem_i.and_then(|i| mem.pressure.get(i).copied().flatten()),
        }
    })
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
/// - the CPU, by the deck's own over rule (the load average past the cores while at least
///   [`CPU_OVER_BUSY`] of them are busy): `log2(load1 / cores) / OVER_LOAD_DOUBLINGS`;
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
    let cpu = match (read(now.cpu), read(now.cores), read(now.load1)) {
        (Some(busy), Some(cores), Some(load)) if cores > 0.0 && load > cores => {
            (busy >= CPU_OVER_BUSY).then(|| (load / cores).log2() / OVER_LOAD_DOUBLINGS)
        }
        _ => None,
    };
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
mod tests {
    use super::*;
    use crate::ink::heat_step;
    use crate::model::{GpuSample, MemSample, Sample, Ticks, Vitals};
    use crate::ring::Cell;
    use Pressure::{Critical, Normal, Warn};

    const CORES: f64 = 18.0;
    const MIB: f64 = 1024.0 * 1024.0;
    /// One bucket's share of the soak, and what every earlier bucket keeps of it.
    const STEP: f64 = BUCKET_S as f64 / SOAK_TAU_S;

    const CALM: Flags = Flags {
        cpu_over: false,
        mem_over: false,
        pressure: Some(Normal),
    };
    const OVER: Flags = Flags {
        cpu_over: true,
        ..CALM
    };

    fn buckets(min: f64) -> usize {
        (min * 6.0).round() as usize
    }

    fn run(min: f64, f: Flags) -> impl Iterator<Item = Flags> {
        std::iter::repeat_n(f, buckets(min))
    }

    /// The soak after `hard` minutes past capacity from calm, then `calm` minutes calm.
    fn soaked(hard: f64, calm: f64) -> f64 {
        (1.0 - (-STEP * buckets(hard) as f64).exp()) * (-STEP * buckets(calm) as f64).exp()
    }

    fn near(a: f32, b: f64, tol: f64) -> bool {
        (f64::from(a) - b).abs() <= tol
    }

    /// A Mac of 18 cores, its GPU, load average, swap and pressure now.
    fn now(cpu: f64, gpu: f64, load1: f64, swap_mib: f64, pressure: Pressure) -> Now {
        Now {
            cpu: Some(cpu),
            gpu: Some(gpu),
            cores: Some(CORES),
            load1: Some(load1),
            swap_bps: Some(swap_mib * MIB),
            pressure,
        }
    }

    struct Scene {
        name: &'static str,
        now: Now,
        hour: Vec<Flags>,
    }

    impl Scene {
        fn strain(&self) -> Strain {
            let s = strain(&self.now, self.hour.iter().copied());
            assert_eq!(self.hour.len(), RING_BUCKETS, "{}: an hour", self.name);
            s
        }
    }

    /// The six scenarios the icon sketches drew, as the hour ring would hold them. A strip cell
    /// that is pegged with nothing waiting (light's) carries no over flag, so it is calm here.
    fn scenes() -> [Scene; 6] {
        // Swapping every third bucket and pressure warn: hot-hard's memory.
        let hot = |i: usize, warn: bool| Flags {
            cpu_over: true,
            mem_over: i % 3 == 1,
            pressure: Some(if warn { Warn } else { Normal }),
        };
        [
            Scene {
                name: "idle: load 0.3 on 18 cores, CPU 1%, calm all hour",
                now: now(0.01, 0.0, 0.3, 0.0, Normal),
                hour: run(60.0, CALM).collect(),
            },
            Scene {
                name: "light: CPU 5%, load 4.1 (the 0.134 capture)",
                now: now(0.046, 0.0, 4.1, 0.0, Normal),
                hour: run(60.0, CALM).collect(),
            },
            Scene {
                name: "busy: load 14, CPU 70%, steady 20 min",
                now: now(0.70, 0.30, 14.0, 0.0, Normal),
                hour: run(60.0, CALM).collect(),
            },
            Scene {
                name: "spike: over capacity from 5 to 2 min ago, now 22%",
                now: now(0.22, 0.0, 11.6, 0.0, Normal),
                hour: run(55.0, CALM)
                    .chain(run(3.0, OVER))
                    .chain(run(2.0, CALM))
                    .collect(),
            },
            Scene {
                name: "hot-hard: over capacity all hour, load 162 on 18, swapping, warn 40 min",
                now: now(1.0, 0.12, 162.4, 5.2, Warn),
                hour: (0..RING_BUCKETS)
                    .map(|i| hot(i, i >= buckets(20.0)))
                    .collect(),
            },
            Scene {
                name: "recovering: hot-hard for 45 min until 6 min ago, calm since",
                now: now(0.06, 0.01, 2.7, 0.0, Normal),
                hour: run(9.0, CALM)
                    .chain((0..buckets(45.0)).map(|i| hot(i, i >= buckets(15.0))))
                    .chain(run(6.0, CALM))
                    .collect(),
            },
        ]
    }

    #[test]
    fn the_six_sketch_scenarios_read_in_the_order_the_icons_draw() {
        let [idle, light, busy, spike, hot, rec] = scenes().map(|s| (s.name, s.strain()));
        let step = |s: Strain| heat_step(s.level.expect("read"));
        for (name, calm) in [idle, light] {
            assert!(step(calm) <= 1, "{name}: low, the two coldest heat steps");
            assert_eq!((calm.over, calm.soak), (0.0, 0.0), "{name}: calm all hour");
        }
        let (name, busy) = busy;
        assert!((3..=4).contains(&step(busy)), "{name}: in between");
        assert_eq!(
            (busy.over, busy.soak),
            (0.0, 0.0),
            "{name}: full is not over"
        );

        let (name, spike) = spike;
        assert_eq!(spike.over, 0.0, "{name}");
        assert!(near(spike.soak, soaked(3.0, 2.0), 1e-6), "{name}");
        assert!((0.15..0.3).contains(&spike.soak), "{name}: a small soak");

        let (name, hot) = hot;
        assert_eq!(step(hot), 6, "{name}");
        assert_eq!(hot.over, 1.0, "{name}: 9x the cores pins it");
        assert!(hot.soak > 0.99, "{name}: soaked");

        let (name, rec) = rec;
        assert!(step(rec) <= 1, "{name}: low now");
        assert_eq!(rec.over, 0.0, "{name}");
        assert!(near(rec.soak, soaked(45.0, 6.0), 1e-6), "{name}");
        assert!(
            rec.soak > spike.soak + 0.25,
            "{name}: a long run leaves more heat than a spike"
        );
        assert!(rec.soak < hot.soak - 0.3, "{name}: cooling, not still hot");
    }

    #[test]
    fn the_soak_builds_over_a_quarter_hour_and_halves_every_seven_calm_minutes() {
        for (min, want) in [(5.0, 0.39), (10.0, 0.63), (15.0, 0.78), (30.0, 0.95)] {
            let s = soak(run(60.0 - min, CALM).chain(run(min, OVER)));
            assert!(near(s, want, 0.005), "{min} min over reads {s}, not {want}");
        }
        let hour = soak(run(60.0, OVER));
        assert!(near(hour, soaked(60.0, 0.0), 1e-6), "an hour over");
        assert!((0.9975..0.998).contains(&hour), "{hour} reads 0.998");
        for (calm, want) in [(6.0, 0.55), (30.0, 0.05)] {
            let s = soak(run(60.0, OVER).chain(run(calm, CALM)));
            assert!(near(s, want, 0.005), "{calm} calm min read {s}, not {want}");
        }
        let half = SOAK_TAU_S * std::f64::consts::LN_2 / 60.0;
        assert!((6.9..7.0).contains(&half), "the half-life is {half} min");
    }

    #[test]
    fn over_capacity_soaks_full_and_pressure_warn_half() {
        let hour = |f: Flags| f64::from(soak(run(60.0, f)));
        let full = hour(OVER);
        let mem = Flags {
            mem_over: true,
            ..CALM
        };
        let critical = Flags {
            pressure: Some(Critical),
            ..CALM
        };
        let warn = Flags {
            pressure: Some(Warn),
            ..CALM
        };
        assert_eq!(hour(mem), full, "swapping hard");
        assert_eq!(hour(critical), full, "the kernel killing");
        assert!((hour(warn) - WARN_SCORE * full).abs() < 1e-6, "reclaiming");
        assert_eq!(
            hour(Flags {
                cpu_over: true,
                ..warn
            }),
            full,
            "over wins"
        );
        assert_eq!(hour(CALM), 0.0);
        assert_eq!(Flags::default().score(), 0.0, "unread is calm");
    }

    #[test]
    fn over_is_the_decks_over_on_a_log_scale_that_pins() {
        let cpu = |busy: f64, load: f64| over(&now(busy, 0.0, load, 0.0, Normal));
        let third = 1.0 / 3.0;
        assert!(near(cpu(1.0, 2.0 * CORES), third, 1e-6), "twice the cores");
        assert!(near(cpu(1.0, 4.0 * CORES), 2.0 * third, 1e-6));
        assert_eq!(cpu(1.0, 8.0 * CORES), 1.0, "eight times pins it");
        assert_eq!(cpu(1.0, 162.4), 1.0);
        assert_eq!(cpu(0.95, 1.01 * CORES), OVER_FLOOR, "just over still shows");
        assert_eq!(
            cpu(0.9, 3.0 * CORES),
            0.0,
            "a queue with idle cores is a lag"
        );
        assert_eq!(cpu(1.0, CORES), 0.0, "pegged with nothing waiting is full");

        let swap = |mib: f64| over(&now(0.1, 0.0, 1.0, mib, Normal));
        assert_eq!(swap(3.9), 0.0);
        assert_eq!(swap(4.0), OVER_FLOOR, "the deck's over");
        assert!(near(swap(16.0), 0.5, 1e-6));
        assert_eq!(swap(64.0), 1.0);
        assert_eq!(swap(1024.0), 1.0);

        let pressed = |p| over(&now(0.1, 0.0, 1.0, 0.0, p));
        assert_eq!(pressed(Critical), OVER_FLOOR);
        assert_eq!(pressed(Warn), 0.0, "reclaiming is at capacity, not past it");
        assert!(
            near(over(&now(1.0, 0.0, 2.0 * CORES, 64.0, Critical)), 1.0, 1e-6),
            "the deepest wins"
        );
    }

    #[test]
    fn the_level_is_the_busier_engine_lifted_by_pressure() {
        let level_of = |cpu, gpu, p| level(&now(cpu, gpu, 1.0, 0.0, p)).expect("read");
        assert!(
            near(level_of(0.3, 0.6, Normal), 0.6, 1e-6),
            "the GPU is busier"
        );
        assert!(near(level_of(0.7, 0.3, Normal), 0.7, 1e-6));
        assert_eq!(level_of(1.2, 0.0, Normal), 1.0, "held to full");
        assert_eq!(level_of(-0.1, 0.0, Normal), 0.0);
        assert_eq!(level_of(0.1, 0.0, Warn), PRESSURE_WARN_LEVEL);
        assert_eq!(heat_step(PRESSURE_WARN_LEVEL), 5);
        assert!(
            near(level_of(0.9, 0.0, Warn), 0.9, 1e-6),
            "a lift never lowers"
        );
        assert_eq!(level_of(0.1, 0.0, Critical), PRESSURE_CRITICAL_LEVEL);
    }

    #[test]
    fn nothing_read_says_nothing() {
        let none = strain(&Now::default(), std::iter::empty());
        assert_eq!(none, Strain::default());
        assert_eq!(
            (none.level, none.over, none.soak),
            (None, 0.0, 0.0),
            "unknown is never a level"
        );
        assert_eq!(Strain::of(&Snapshot::default()), Strain::default());
        assert_eq!(hour_of(&Snapshot::default()).count(), 0);
    }

    #[test]
    fn unread_readings_and_spans_are_not_zero_and_cool_like_calm() {
        let only_gpu = Now {
            gpu: Some(0.3),
            ..Now::default()
        };
        assert!(near(level(&only_gpu).expect("the GPU"), 0.3, 1e-6));
        let warn_alone = Now {
            pressure: Warn,
            ..Now::default()
        };
        assert_eq!(level(&warn_alone), Some(PRESSURE_WARN_LEVEL));
        let not_numbers = Now {
            cpu: Some(f64::NAN),
            gpu: Some(f64::INFINITY),
            cores: Some(CORES),
            load1: Some(f64::NAN),
            swap_bps: Some(f64::INFINITY),
            pressure: Normal,
        };
        assert_eq!(level(&not_numbers), None, "not a number is not read");
        assert_eq!(over(&not_numbers), 0.0);
        let no_cores = Now {
            cores: None,
            ..now(1.0, 0.0, 200.0, 0.0, Normal)
        };
        assert_eq!(over(&no_cores), 0.0, "no queue without a core count");

        let asleep = soak(
            run(20.0, OVER)
                .chain(run(30.0, Flags::default()))
                .chain(run(10.0, CALM)),
        );
        let awake = soak(run(20.0, OVER).chain(run(40.0, CALM)));
        assert_eq!(asleep, awake, "an unread span cools as calm does");
        assert!(near(asleep, soaked(20.0, 40.0), 1e-6));
        assert_eq!(soak(run(60.0, Flags::default())), 0.0);
    }

    #[test]
    fn a_zero_capacity_or_core_count_reads_nothing_rather_than_dividing_by_it() {
        let mut snap = Snapshot::default();
        snap.resources[Resource::Cpu.index()] = ResourceView {
            now: Some(4.0),
            capacity: Some(0.0),
            ..ResourceView::default()
        };
        snap.resources[Resource::Gpu.index()] = ResourceView {
            now: Some(30.0),
            capacity: Some(0.0),
            ..ResourceView::default()
        };
        snap.facts.load1 = Some(50.0);
        let held = Now::of(&snap);
        assert_eq!((held.cpu, held.gpu, held.cores), (None, None, Some(0.0)));
        assert_eq!(Strain::of(&snap), Strain::default());

        let zero_cores = Now {
            cores: Some(0.0),
            ..now(1.0, 0.0, 50.0, 0.0, Normal)
        };
        assert_eq!(over(&zero_cores), 0.0, "no queue past zero cores");
    }

    /// A row of `n` cells ending at bucket `last`, over where `over` says.
    fn row(last: i64, n: usize, over: impl Fn(i64) -> bool) -> ResourceView {
        let first = last - i64::try_from(n).expect("a ring's length") + 1;
        ResourceView {
            ring: (first..=last)
                .map(|k| Cell {
                    mean: Some(1.0),
                    max: Some(1.0),
                    over: over(k),
                })
                .collect(),
            ring_k: last,
            ..ResourceView::default()
        }
    }

    #[test]
    fn rows_that_disagree_line_up_by_bucket_and_keep_the_newest_hour() {
        let cpu_over = |k: i64| (1300..1360).contains(&k);
        let mem_over = |k: i64| (1400..1460).contains(&k);
        let warn = |k: i64| if k < 1200 { Warn } else { Normal };
        let mut snap = Snapshot::default();
        snap.resources[Resource::Cpu.index()] = row(1359, RING_BUCKETS, cpu_over);
        let mut mem = row(1459, RING_BUCKETS, mem_over);
        mem.pressure = (1100..=1459).map(|k| Some(warn(k))).collect();
        snap.resources[Resource::Memory.index()] = mem.clone();

        // The rows span 1000..=1459 together; the newest hour is 1100..=1459, and the CPU
        // row held nothing past 1359.
        let want: Vec<Flags> = (1100..=1459)
            .map(|k| Flags {
                cpu_over: cpu_over(k),
                mem_over: mem_over(k),
                pressure: Some(warn(k)),
            })
            .collect();
        assert_eq!(hour_of(&snap).collect::<Vec<_>>(), want);
        let s = Strain::of(&snap).soak;
        let hand = soak(want.iter().copied());
        assert!(s > 0.0 && s.to_bits() == hand.to_bits(), "{s} vs {hand}");

        // Under an hour together, the older row's start leads: the CPU's first ten minutes over,
        // before Memory's ring began.
        let mut short = Snapshot::default();
        short.resources[Resource::Cpu.index()] = row(1059, 60, |k| k < 1010);
        short.resources[Resource::Memory.index()] = row(1099, 70, |_| false);
        let hour: Vec<Flags> = hour_of(&short).collect();
        assert_eq!(hour.len(), 100, "1000..=1099");
        assert!(
            hour[..10]
                .iter()
                .all(|f| f.cpu_over && f.pressure.is_none())
        );
        assert!(!hour[10..].iter().any(|f| f.cpu_over || f.mem_over));

        // Memory keeping no pressure reads none; a row alone still speaks for its span.
        mem.pressure = std::sync::Arc::from([]);
        snap.resources[Resource::Memory.index()] = mem;
        assert!(hour_of(&snap).all(|f| f.pressure.is_none()));
        snap.resources[Resource::Memory.index()] = ResourceView::default();
        let cpu_alone: Vec<Flags> = hour_of(&snap).collect();
        assert_eq!(cpu_alone.len(), RING_BUCKETS);
        assert_eq!(cpu_alone.iter().filter(|f| f.cpu_over).count(), 60);
        assert!(cpu_alone.last().is_some_and(|f| f.cpu_over), "ends at 1359");
    }

    #[test]
    fn a_single_bucket_barely_registers() {
        let one = soak([OVER]);
        assert!(near(one, 1.0 - (-STEP).exp(), 1e-7), "{one}");
        assert!(one < 0.02, "ten seconds over is not a soak");
        assert_eq!(soak([CALM]), 0.0);
    }

    /// One 10 s reading of a two-core Mac whose GPU reads 30%: each core's busy share over the
    /// interval it ends, the load average, swap traffic and pressure.
    #[derive(Clone, Copy)]
    struct Load {
        busy: f64,
        load1: f64,
        swap_mib: f64,
        pressure: Pressure,
    }

    const T0: i64 = 1_792_000_800_000;
    const TICKS: u64 = 1000;

    /// Feeds `phases` to a fresh model, one reading per 10 s bucket (`None` passes a bucket
    /// unread), and answers the last snapshot with the flags each bucket should hold. A reading
    /// makes a rate (busy cores, swap traffic) only from a reading in the bucket before; the
    /// phases' unread spans are long enough (over three cadences) that the one after makes none.
    fn drive(phases: &[(f64, Option<Load>)]) -> (Snapshot, Vec<Flags>) {
        let mut v = Vitals::new();
        let mut snap = Snapshot::default();
        let mut want = Vec::new();
        let (mut busy, mut swapped_kib, mut t, mut prev) = (0u64, 0u64, 0u64, false);
        for &(min, load) in phases {
            for _ in 0..buckets(min) {
                t += 1;
                let Some(l) = load else {
                    want.push(Flags::default());
                    prev = false;
                    continue;
                };
                busy += (l.busy * TICKS as f64).round() as u64;
                swapped_kib += (l.swap_mib * 1024.0 * BUCKET_S as f64).round() as u64;
                let ms = t * 10_000;
                let core = Ticks {
                    busy,
                    total: t * TICKS,
                };
                let s = Sample {
                    at_ns: ms * 1_000_000,
                    at_unix_ms: T0 + ms as i64,
                    mono_ms: ms,
                    cadence_ms: 10_000,
                    cores: Some(vec![core; 2]),
                    gpu: Some(GpuSample {
                        device_pct: Some(30),
                        ..GpuSample::default()
                    }),
                    mem: Some(MemSample {
                        total: 32 << 30,
                        app: 8 << 30,
                        pressure: l.pressure,
                        ..MemSample::default()
                    }),
                    load1: Some(l.load1),
                    swapped_kib: Some(swapped_kib),
                    ..Sample::default()
                };
                snap = v.absorb(&s);
                want.push(Flags {
                    cpu_over: prev && l.busy >= CPU_OVER_BUSY && l.load1 > 2.0,
                    mem_over: prev && l.swap_mib * MIB >= SWAP_OVER_BPS,
                    pressure: Some(l.pressure),
                });
                prev = true;
            }
        }
        (snap, want)
    }

    #[test]
    fn a_snapshot_reads_its_rows_now_and_its_rings_bucket_for_bucket() {
        let calm = Load {
            busy: 0.1,
            load1: 0.5,
            swap_mib: 0.0,
            pressure: Normal,
        };
        let queued = Load {
            busy: 1.0,
            load1: 8.0,
            ..calm
        };
        let swapping = Load {
            busy: 0.2,
            swap_mib: 8.0,
            pressure: Warn,
            ..calm
        };
        let phases = [
            (2.0, Some(calm)),
            (20.0, Some(queued)),
            (5.0, Some(swapping)),
            (30.0, None),
            (6.0, Some(calm)),
        ];

        let (hard, want) = drive(&phases[..2]);
        assert_eq!(
            hour_of(&hard).collect::<Vec<_>>(),
            want,
            "a short ring, whole"
        );
        let s = Strain::of(&hard);
        assert_eq!(s.level, Some(1.0), "both cores busy");
        assert!(near(s.over, 2.0 / 3.0, 1e-6), "load 8 on 2 cores: 4x");
        assert!(near(s.soak, soaked(20.0, 0.0), 1e-6), "20 min queued");

        let (swap, _) = drive(&phases[..3]);
        let s = Strain::of(&swap);
        assert_eq!(s.level, Some(PRESSURE_WARN_LEVEL), "warn lifts 20% busy");
        assert!(near(s.over, 0.25, 1e-6), "8 MiB/s is one doubling of four");

        let (snap, want) = drive(&phases);
        let now = Now::of(&snap);
        assert!(near(now.cpu.expect("busy") as f32, 0.1, 1e-6));
        assert!(near(now.gpu.expect("the GPU") as f32, 0.3, 1e-6));
        assert_eq!(
            (now.cores, now.load1, now.swap_bps, now.pressure),
            (Some(2.0), Some(0.5), Some(0.0), Normal)
        );
        let hour: Vec<Flags> = hour_of(&snap).collect();
        assert_eq!(hour.len(), RING_BUCKETS, "the ring keeps the newest hour");
        assert_eq!(hour, want[want.len() - RING_BUCKETS..]);
        assert!(
            hour.iter().any(|f| f.cpu_over) && hour.iter().any(|f| f.mem_over),
            "both rings' flags are read"
        );
        let s = Strain::of(&snap);
        assert_eq!(s, strain(&now, hour.iter().copied()));
        assert!(near(s.level.expect("read"), 0.3, 1e-6), "the GPU is busier");
        assert_eq!(s.over, 0.0);
        // The ring dropped the oldest 3 minutes; 24 minutes past capacity remain, then 30 asleep
        // and 6 calm.
        assert!(near(s.soak, soaked(24.0, 36.0), 1e-6), "{}", s.soak);
    }

    #[test]
    fn one_reading_makes_no_rate_so_only_the_gpu_and_pressure_speak() {
        let load = Load {
            busy: 1.0,
            load1: 8.0,
            swap_mib: 64.0,
            pressure: Normal,
        };
        let (snap, want) = drive(&[(1.0 / 6.0, Some(load))]);
        assert_eq!(want.len(), 1);
        let s = Strain::of(&snap);
        assert!(
            near(s.level.expect("the GPU"), 0.3, 1e-6),
            "busy cores need two"
        );
        assert_eq!((s.over, s.soak), (0.0, 0.0), "no busy, no swap rate");
        assert_eq!(hour_of(&snap).collect::<Vec<_>>(), want);
    }
}
