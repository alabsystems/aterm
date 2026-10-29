// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cost of one Prism Glass `PaletteSolve` unit (`docs/design/PRISM-GLASS.md` §15.2 1.4), the
//! measurement `SOLVE_UNITS_PER_FRAME` is set from.
//!
//! Every shipped light page (six glass, two lip-forward) is solved unit by unit, [`REPS`] times;
//! each unit's cost is its least over the repetitions, so a preemption does not count. The
//! report gives, per unit kind, the count, p50, p99 and most of one unit, and the units a frame
//! can run inside [`FRAME_BUDGET_US`] at the most costly unit.
//!
//! `targo --unverified run --release -p aterm-bench --example prism_glass_units`

use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::Instant;

use aterm_effects::rainbow_kitty::prism_glass::{PaletteSolve, SHIPPED_PAGES};

/// Repetitions of each page's solve.
const REPS: usize = 5;

/// The frame time a light frame's solve may take, µs.
const FRAME_BUDGET_US: f64 = 1000.0;

/// Each unit's kind and cost (µs) over one solve of `(fg, bg)`.
fn solve_once(fg: u32, bg: u32) -> Vec<(&'static str, f64)> {
    let mut solve = PaletteSolve::new(fg, bg);
    let mut out = Vec::new();
    while solve.done().is_none() {
        let kind = solve.next_unit();
        let t0 = Instant::now();
        black_box(solve.advance(1));
        out.push((kind, t0.elapsed().as_secs_f64() * 1e6));
    }
    out
}

fn main() {
    let mut by_kind: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
    let mut worst = (0.0_f64, "", "");
    for &(name, fg, bg) in &SHIPPED_PAGES {
        let mut least = solve_once(fg, bg);
        for _ in 1..REPS {
            for (slot, (_, us)) in least.iter_mut().zip(solve_once(fg, bg)) {
                slot.1 = slot.1.min(us);
            }
        }
        let total: f64 = least.iter().map(|(_, us)| us).sum();
        eprintln!(
            "prism_glass_units: {name}: {} units, {total:.0} us in all",
            least.len()
        );
        for (kind, us) in least {
            if us > worst.0 {
                worst = (us, kind, name);
            }
            by_kind.entry(kind).or_default().push(us);
        }
    }
    for (kind, costs) in &mut by_kind {
        costs.sort_by(f64::total_cmp);
        let at = |q: f64| costs[((costs.len() - 1) as f64 * q) as usize];
        eprintln!(
            "prism_glass_units: {kind:>11}: {:>5} units, p50 {:>8.2} us, p99 {:>8.2} us, most \
             {:>8.2} us",
            costs.len(),
            at(0.5),
            at(0.99),
            at(1.0)
        );
    }
    let per_frame = (FRAME_BUDGET_US / worst.0).floor();
    eprintln!(
        "prism_glass_units: the most costly unit is {}'s {} at {:.2} us: {per_frame} units in a \
         {FRAME_BUDGET_US} us frame budget",
        worst.2, worst.1, worst.0
    );
    println!(
        "{{\"prism_glass_unit_max_us\":{:.3},\"units_per_frame\":{per_frame},\"reps\":{REPS}}}",
        worst.0
    );
}
