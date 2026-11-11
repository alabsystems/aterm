// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ink::heat_step;
use crate::model::{GpuSample, MemSample, Sample, Ticks, Vitals};
use crate::ring::Cell;
use crate::runq::{QueueBasis, RunQueueRead};
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

/// A Mac of 18 cores, its GPU, run queue, swap and pressure now.
fn now(cpu: f64, gpu: f64, queue: f64, swap_mib: f64, pressure: Pressure) -> Now {
    Now {
        cpu: Some(cpu),
        gpu: Some(gpu),
        cores: Some(CORES),
        queue: Some(queue),
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
        queue: Some(f64::NAN),
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
    snap.facts.queue = Some(crate::runq::RunQueue {
        threads: 50.0,
        basis: crate::runq::QueueBasis::Runnable,
    });
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
                tau_ms: 10_000.0,
                over_ms: if over(k) { 10_000.0 } else { 0.0 },
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
        (now.cores, now.queue, now.swap_bps, now.pressure),
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

/// A ten-core Mac read once a second for 12 s, every core `busy`, the host's run-queue
/// reading `p` and the 1-minute load `load1` at each: the model's last snapshot.
fn steady(busy: f64, p: RunQueueRead, load1: f64) -> Snapshot {
    let mut v = Vitals::new();
    let mut snap = Snapshot::default();
    for t in 1..=12u64 {
        let core = Ticks {
            busy: (busy * TICKS as f64).round() as u64 * t,
            total: TICKS * t,
        };
        snap = v.absorb(&Sample {
            at_ns: t * 1_000_000_000,
            at_unix_ms: T0 + i64::try_from(t).expect("few") * 1000,
            mono_ms: t * 1000,
            cadence_ms: 1000,
            cores: Some(vec![core; 10]),
            load1: Some(load1),
            run_queue: p,
            ..Sample::default()
        });
    }
    snap
}

/// What the dial, the CPU row and its queue tile, and the CPU pointer say of one snapshot:
/// the dial's over, the row's over flag, the tile's, and the pointer's score.
fn three(snap: &Snapshot) -> (f32, bool, bool, f64) {
    let now = Now::of(snap);
    (
        over(&now),
        snap.resources[Resource::Cpu.index()].over,
        snap.units.queue.expect("the queue tile").over,
        cpu_score(&now).expect("busy is read"),
    )
}

fn milli(threads: f64) -> RunQueueRead {
    RunQueueRead::Milli((threads * 1000.0).round() as u64)
}

/// A4 §15 `the_pointer_the_dial_and_the_row_share_one_over`, the CPU's part (N2): the dial,
/// the CPU row's over flag and queue tile, and the CPU pointer's score are one rule over one
/// queue, P10, and load1 only where P10 is unread. Cores' ranking beside it lands with the
/// pointers' ranking (N5).
#[test]
fn the_pointer_the_dial_and_the_row_share_one_over() {
    // Every pair of busy share and runnable count on ten cores, away from the rule's edges:
    // the three agree, and over ink (a score of 1 or more) is exactly the dial over.
    for busy in [0.5, 0.94, 0.96, 1.0] {
        for p in [5.0, 9.9, 10.5, 16.59, 26.8, 100.0] {
            let snap = steady(busy, milli(p), 2.0);
            let (dial, row, tile, score) = three(&snap);
            let over = dial > 0.0;
            assert_eq!((row, tile), (over, over), "busy {busy}, P10 {p}");
            assert_eq!(score >= 1.0, over, "busy {busy}, P10 {p}: {score}");
            let q = snap.facts.queue.expect("P10 read");
            assert_eq!(q.basis, QueueBasis::Runnable, "P10, not the load of 2");
            assert!((q.threads - p).abs() < 1e-3, "{q:?}");
        }
    }

    // 09:59:25–34: the machine 0.654 busy with 16.59 waiting: within capacity, no pointer
    // (under the amber's 0.75).
    let (dial, row, tile, score) = three(&steady(0.654, milli(16.59), 16.59));
    assert_eq!((dial, row, tile), (0.0, false, false));
    assert!((score - 0.654).abs() < 1e-3, "{score}");
    // 10:42:59: every core busy, 26.8 runnable on 10: over 0.474, the pointer 1.474.
    let (dial, row, tile, score) = three(&steady(1.0, milli(26.8), 26.8));
    assert!((f64::from(dial) - 0.474).abs() < 1e-3, "{dial}");
    assert!(row && tile);
    assert!((score - 1.474).abs() < 1e-3, "{score}");

    // A burst just drained: P10 is back to 4 on 10 cores, the cores still 0.97 busy, and
    // load1 still holds 26.8 (it lags by a minute). P10 says within capacity, and all three
    // say so.
    let drained = steady(0.97, milli(4.0), 26.8);
    let (dial, row, tile, score) = three(&drained);
    assert_eq!((dial, row, tile), (0.0, false, false));
    assert!(score < 1.0, "{score}");
    // Negative control: a pointer that kept load1 as its queue paints CPU over (1.474) while
    // the needle reads nothing past the redline: two rules, not one.
    let now = Now::of(&drained);
    let mutant = cpu_over_depth(now.cpu, now.cores, drained.facts.load1)
        .map_or(now.cpu.expect("busy"), |o| 1.0 + o);
    assert!(mutant >= 1.0 && dial == 0.0, "the control reads {mutant}");

    // P10 unread (no reader on this OS, or its self-check failed): load1 is the queue, for
    // all three alike.
    for why in [RunQueueRead::Unsupported, RunQueueRead::UnreadFormat] {
        let snap = steady(1.0, why, 26.8);
        assert_eq!(snap.facts.queue.map(|q| q.basis), Some(QueueBasis::Load1));
        let (dial, row, tile, score) = three(&snap);
        assert!(dial > 0.0 && row && tile && score >= 1.0);
    }
}

#[test]
fn the_over_rule_reads_nothing_it_was_not_given() {
    assert_eq!(cpu_over_depth(Some(1.0), Some(10.0), None), None);
    assert_eq!(cpu_over_depth(None, Some(10.0), Some(40.0)), None);
    assert_eq!(cpu_over_depth(Some(1.0), Some(0.0), Some(40.0)), None);
    assert_eq!(cpu_over_depth(Some(1.0), Some(10.0), Some(f64::NAN)), None);
    assert_eq!(
        cpu_over_depth(Some(1.0), Some(10.0), Some(10.0)),
        None,
        "at, not past"
    );
    let floor = cpu_over_depth(Some(1.0), Some(10.0), Some(10.01)).expect("over");
    assert_eq!(
        floor,
        f64::from(OVER_FLOOR),
        "the first step past is visible"
    );
    assert_eq!(
        cpu_over_depth(Some(1.0), Some(10.0), Some(900.0)),
        Some(1.0)
    );
    assert_eq!(cpu_score(&Now::default()), None, "no busy share, no score");
}
