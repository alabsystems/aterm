// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cost of each situation reader of stats deck A4 N2 (`docs/DESIGN-stats-deck-instruments-2026-11-06.md`
//! §8 and §9), measured in process as the vitals thread calls it.
//!
//! `situ_cost cost [--n N]` times each reader N times (default 2,000; the type and the watch's
//! start 200) and prints one line per reader with its p50 and p95 in µs and the 1-minute load
//! before and after:
//!
//! - `power_rest`: a reading of a started `PowerWatch` with no post (its two `notify_check`s);
//! - `power_read`: one read of the power sources (`IOPSCopyPowerSourcesInfo`): the read a charge
//!   post costs;
//! - `adapter`: the adapter's details (`IOPSCopyExternalPowerAdapterDetails`), which a source
//!   post on AC adds to it;
//! - `lid`: `AppleClamshellState` off the root domain the process holds (the read at a wake);
//! - `awake`: the kernel's boot, sleep and wake times and the root domain's `Last Sleep Reason`
//!   (the read at a wake);
//! - `heard`: what a reading takes of the watch (a lock and a clone);
//! - `displays`: one listing of the display pipes, the census a display's arrival costs at most;
//! - `type`: the computer's type, read in process (once per host);
//! - `watch_start`: a watch's start (its thread, the registrations, their first drains, the
//!   lid and last-sleep reads) to its first answer, and its drop.
//!
//! `situ_cost idle --secs S` starts the readers a vitals host holds (the watch, the power tokens,
//! the sleep and wake listener) and idles S seconds on one sleep: its wakes are counted from
//! outside with `notify_idle watch PID`.

#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]

use std::hint::black_box;
use std::time::{Duration, Instant};

use aterm_sysprobe::situation;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse::<u64>().ok())
    };
    let code = match args.first().map(String::as_str) {
        Some("cost") => {
            cost(usize::try_from(flag("--n").unwrap_or(2_000)).unwrap_or(2_000));
            0
        }
        Some("idle") => {
            idle(flag("--secs").unwrap_or(600));
            0
        }
        _ => {
            eprintln!("usage: situ_cost cost [--n N] | idle [--secs S]");
            2
        }
    };
    std::process::exit(code);
}

fn load1() -> f64 {
    aterm_sysprobe::load_average().map_or(f64::NAN, |l| l[0])
}

/// `n` timings of `f` in µs, as p50 and p95.
fn time(n: usize, mut f: impl FnMut()) -> (f64, f64) {
    let mut us: Vec<f64> = (0..n)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    us.sort_by(f64::total_cmp);
    let at = |q: usize| us[(us.len() * q / 100).min(us.len() - 1)];
    (at(50), at(95))
}

fn row(name: &str, n: usize, f: impl FnMut()) {
    let before = load1();
    let (p50, p95) = time(n, f);
    println!(
        "{name:<12} n={n:<5} p50_us={p50:>9.2} p95_us={p95:>9.2} load1={before:.2}->{:.2}",
        load1()
    );
}

fn cost(n: usize) {
    let few = (n / 10).max(20);
    let mut power = situation::PowerWatch::new();
    let first = power.read(0);
    println!("power at start: {first:?}");
    println!("adapter: {:?}", situation::adapter_watts());
    println!("external displays: {:?}", situation::external_displays());
    println!("type: {:?}", aterm_sysprobe::machine_type::read());
    let watch = situation::Watch::start();
    println!("heard: {:?}", watch.heard());
    row("power_rest", n, || {
        black_box(power.read(1));
    });
    row("power_read", n, || {
        black_box(situation::power_sources());
    });
    row("adapter", n, || {
        black_box(situation::adapter_watts());
    });
    row("lid", n, || {
        black_box(aterm_sysprobe::power::lid_closed());
    });
    row("awake", n, || {
        black_box(situation::awake());
    });
    row("heard", n, || {
        black_box(watch.heard());
    });
    row("displays", n, || {
        black_box(situation::external_displays());
    });
    row("type", few, || {
        black_box(aterm_sysprobe::machine_type::read());
    });
    drop(watch);
    row("watch_start", few, || {
        let w = situation::Watch::start();
        black_box(w.heard());
        drop(w);
    });
}

fn idle(secs: u64) {
    let watch = situation::Watch::start();
    let handle = watch.handle();
    let woke = aterm_uds::power::listen(Box::new(move |power| {
        if power == aterm_uds::power::Power::Woke {
            handle.woke();
        }
    }));
    let mut power = situation::PowerWatch::new();
    let _ = power.read(0);
    println!(
        "pid {} idling {secs} s: watch, power tokens, wake listener {}",
        std::process::id(),
        if woke.is_some() { "on" } else { "off" }
    );
    std::thread::sleep(Duration::from_secs(secs));
    println!("heard at the end: {:?}", watch.heard());
    drop((watch, woke));
}
