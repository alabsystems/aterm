// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The heat chain's per-reading costs as N2 built it (stats deck A4,
//! `docs/DESIGN-stats-deck-instruments-2026-11-06.md` §4, §13), in process on this Mac, each
//! p50 / p95 in µs over `n` passes with the 1-minute load before and after:
//!
//! - `read`: the coalition read aterm already takes every reading (`coalitions::coalitions`),
//!   which now decodes and sums the heat words in its own copy loop (context: it is not new);
//! - `fold`: that copy loop's added work alone, on records already copied out: each record's heat
//!   words decoded, kept by id and summed (`CoalHeat::note`), a read's worth per pass;
//! - `merge` and `merge_bare`: the start counter's merge of two reads a second apart, with the
//!   heat book (the gone coalitions handed back, their words off the earlier total) and without;
//!   `merge − bare` is the heat's share, timed in alternating order;
//! - `absorb`: the model's heat engine on that change (the judgement, the decile, the cut);
//! - `check`: the thermal pressure watch with nothing posted (`notify_check` alone), the cost
//!   every reading pays;
//! - `get_state`: the watch's read after a post (`notify_check` and `notify_get_state`), paid only
//!   when the level changes;
//! - `range`: the power manager's DVFS tables (`cpu_hardware::dvfs_range`), read once per process.
//!
//! `heat_cost [n]` (default 400). It reads only: no child, no privilege, no dialog.

fn main() {
    #[cfg(target_os = "macos")]
    mac::run(
        std::env::args()
            .nth(1)
            .and_then(|n| n.parse().ok())
            .unwrap_or(400),
    );
    #[cfg(not(target_os = "macos"))]
    eprintln!("heat_cost: macOS only (the coalition records and the notify keys are macOS's)");
}

#[cfg(target_os = "macos")]
mod mac {
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    use aterm_sysprobe::coalition_heat::CoalHeat;
    use aterm_sysprobe::coalitions;
    use aterm_vitals::explain::CoalLane;
    use aterm_vitals::heat::{self, ClockGate, Engine, HeatBook, Input, ThermalPressure};
    use aterm_vitals::starts::{self, CoalTaskRead, CoalTasks, StartCount};

    /// p50 and p95 of `n` timed passes of `f`, µs.
    fn time(n: usize, f: &mut dyn FnMut()) -> (f64, f64) {
        let mut v: Vec<f64> = (0..n)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed().as_secs_f64() * 1e6
            })
            .collect();
        v.sort_by(f64::total_cmp);
        (v[n / 2], v[(n * 95).div_ceil(100) - 1])
    }

    fn load1() -> String {
        aterm_sysprobe::load_average()
            .map_or_else(|| "unread".to_owned(), |l| format!("{:.1}", l[0]))
    }

    /// The start counter at a read: the task rows, with the read's heat book where `heat`.
    fn count(read: &coalitions::CoalitionRead, at_ns: u64, heat: bool) -> StartCount {
        let tasks = CoalTaskRead::of(
            read.newest,
            read.truncated,
            read.rows.iter().map(|u| CoalTasks {
                id: u.id,
                started: u.tasks_started,
                exited: u.tasks_exited,
            }),
        );
        let tasks = if heat {
            tasks.with_heat(HeatBook {
                at_ns,
                timebase: read.heat.timebase,
                by_id: read.heat.by_id.clone(),
                total: read.heat.total,
            })
        } else {
            tasks
        };
        StartCount::Coalitions {
            read: tasks,
            own: None,
        }
    }

    pub(super) fn run(n: usize) {
        let before = load1();
        let started = Instant::now();
        let at = || u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        // The cores' ticks around the same pair of reads, as the engine reads them each reading.
        let mut probe = aterm_sysprobe::Probe::new();
        let ticks_then = probe.core_ticks().expect("the core ticks read");
        let then_at = at();
        let then = coalitions::coalitions().expect("the coalition list reads");
        std::thread::sleep(Duration::from_secs(1));
        let now_at = at();
        let now = coalitions::coalitions().expect("the coalition list reads");
        let ticks_now = probe.core_ticks().expect("the core ticks read");
        let fracs: Vec<f64> = ticks_then
            .iter()
            .zip(&ticks_now)
            .map(|(a, b)| {
                let total = b.total().saturating_sub(a.total()).max(1);
                b.busy().saturating_sub(a.busy()) as f64 / total as f64
            })
            .collect();
        assert!(
            then.heat.total.is_ok() && now.heat.total.is_ok(),
            "the heat self-check refused this kernel's records: {:?} {:?}",
            then.heat.total,
            now.heat.total
        );
        let records = coalitions::records().expect("the records read");
        let timebase = now.heat.timebase;

        let read = time(n.min(100), &mut || {
            black_box(coalitions::coalitions());
        });
        let fold = time(n, &mut || {
            let mut heat = CoalHeat::with_capacity(records.records.len(), timebase);
            for (id, raw) in &records.records {
                heat.note(*id, black_box(raw));
            }
            black_box(heat);
        });
        let (a, b) = (count(&then, then_at, true), count(&now, now_at, true));
        let (a0, b0) = (count(&then, then_at, false), count(&now, now_at, false));
        let moved = starts::moved(&a, &b).expect("the merge counts");
        let change = moved
            .heat
            .expect("the reads carry heat")
            .expect("the change reads");
        let (mut with, mut bare) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for i in 0..n {
            let one = |x: &StartCount, y: &StartCount| {
                let t = Instant::now();
                black_box(starts::moved(black_box(x), black_box(y)));
                t.elapsed().as_secs_f64() * 1e6
            };
            if i % 2 == 0 {
                with.push(one(&a, &b));
                bare.push(one(&a0, &b0));
            } else {
                bare.push(one(&a0, &b0));
                with.push(one(&a, &b));
            }
        }
        let q = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            (v[n / 2], v[(n * 95).div_ceil(100) - 1])
        };
        let (merge, merge_bare) = (q(with), q(bare));
        let range = aterm_sysprobe::cpu_hardware::dvfs_range()
            .expect("macOS reads a range")
            .ok()
            .map(|r| heat::ClockRange {
                p_mhz: r.p_mhz,
                e_mhz: r.e_mhz,
            });
        let p_cores: Vec<usize> = aterm_sysprobe::cpu_layout()
            .clusters
            .first()
            .map(|c| c.logical.iter().map(|&i| usize::from(i)).collect())
            .unwrap_or_default();
        let brand = aterm_sysprobe::machine_words().cpu;
        let input = Input {
            lane: CoalLane::On,
            pressure: Some(Ok(ThermalPressure::Nominal)),
            moved: Some(Ok(change)),
            range: Some(range.ok_or("pmgr")),
            gate: ClockGate::of_chip(brand.as_deref()),
            cores: Some((&fracs, change.window_ns)),
            p_cores: Some(&p_cores),
            host: heat::HostWatts::default(),
        };
        let mut engine = Engine::default();
        let view = engine.absorb(&input);
        let absorb = time(n, &mut || {
            black_box(engine.absorb(black_box(&input)));
        });
        let mut watch = aterm_sysprobe::notify::thermal_pressure();
        let level = watch.state();
        let check = time(n, &mut || {
            let _ = black_box(watch.state());
        });
        // A fresh token's first check reports a post, so its first read takes the state; the
        // registration is outside the timing.
        let get_state = q((0..n)
            .map(|_| {
                let mut fresh = aterm_sysprobe::notify::thermal_pressure();
                let t = Instant::now();
                let _ = black_box(fresh.state());
                t.elapsed().as_secs_f64() * 1e6
            })
            .collect());
        let range_cost = time(20, &mut || {
            black_box(aterm_sysprobe::cpu_hardware::dvfs_range());
        });
        println!(
            "heat_cost n={n} records={} load1 before={before} after={}",
            records.records.len(),
            load1()
        );
        println!(
            "pressure={level:?} range={range:?} gate={:?}",
            ClockGate::of_chip(brand.as_deref())
        );
        println!(
            "change window_ns={} cpu_w={:?} chain={:?}",
            change.window_ns,
            view.watts.cpu,
            view.chain.map(|c| (
                c.judged.p_mhz,
                c.judged.e_mhz,
                c.judged.ipj,
                c.decile,
                c.cut
            ))
        );
        for (name, (p50, p95)) in [
            ("read", read),
            ("fold", fold),
            ("merge", merge),
            ("merge_bare", merge_bare),
            ("absorb", absorb),
            ("check", check),
            ("get_state", get_state),
            ("range", range_cost),
        ] {
            println!("{name:<20} p50={p50:>9.2} p95={p95:>9.2} us");
        }
    }
}
