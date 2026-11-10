// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stats deck A4, step N2: what the machine-wide zombie count adds to a whole reading
//! (`docs/DESIGN-stats-deck-instruments-2026-11-06.md` §13). It reads nothing: the sweep every whole
//! reading takes is paid, and the count is the one [`ZombieBook`] grouping its rows by parent. This
//! times that grouping, `note` and `piles`, over a sweep the size N1 measured (2,101 processes, a
//! pile of 14 under one parent and 3 under another), each reading's rows filtered from the sweep
//! as the model filters them.
//!
//! `zombie_book_cost [--readings 2000]` prints p50, p95 and max wall time a reading, µs.

use std::time::Instant;

use aterm_vitals::blockage::ZombieBook;

fn main() {
    let mut readings = 2_000usize;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match (a.as_str(), args.next()) {
            ("--readings", Some(v)) => readings = v.parse().unwrap_or(readings),
            _ => {
                eprintln!("usage: zombie_book_cost [--readings 2000]");
                std::process::exit(2);
            }
        }
    }
    // (pid, ppid, zombie): 2,084 live processes, 14 zombies under 1476, 3 under 812.
    let sweep: Vec<(u32, u32, bool)> = (100..2_184)
        .map(|pid| (pid, 1, false))
        .chain((0..14).map(|i| (5_000 + i, 1_476, true)))
        .chain((0..3).map(|i| (6_000 + i, 812, true)))
        .collect();
    let mut book = ZombieBook::default();
    let mut us = Vec::with_capacity(readings);
    let mut count = 0u32;
    for r in 0..readings {
        let at = u64::try_from(r).unwrap_or(u64::MAX) * 1_000;
        let t0 = Instant::now();
        book.note(
            at,
            sweep
                .iter()
                .filter(|(_, _, z)| *z)
                .map(|&(pid, ppid, _)| (pid, ppid)),
        );
        count = book.piles(at).values().sum();
        us.push(t0.elapsed().as_secs_f64() * 1e6);
    }
    us.sort_by(f64::total_cmp);
    let rank = |q: f64| us[((q * us.len() as f64).ceil() as usize).clamp(1, us.len()) - 1];
    println!(
        "zombie book over {} processes: n={} p50={:.3}us p95={:.3}us max={:.3}us (count {count})",
        sweep.len(),
        us.len(),
        rank(0.5),
        rank(0.95),
        us[us.len() - 1]
    );
}
