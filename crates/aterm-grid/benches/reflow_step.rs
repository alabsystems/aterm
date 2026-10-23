// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-line rewrap benchmark for offloaded scrollback reflow.

use std::time::{Duration, Instant};

use aterm_grid::{Grid, ReflowStep};
use aterm_scrollback::{Scrollback, ScrollbackStorage};

const ROWS: u16 = 24;
const COLS: u16 = 80;
const NEW_COLS: u16 = 40;
const LINES: usize = 50_000;
const STEP_BUDGET: usize = 4_000;

fn filled() -> Grid {
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 512_000_000).into();
    let mut g = Grid::with_tiered_scrollback(ROWS, COLS, 8, sb);
    for i in 0..LINES {
        let mut s = format!("L{i}-");
        while s.len() + 1 < usize::from(COLS) {
            s.push('x');
        }
        for c in s.chars() {
            g.write_char(c);
        }
        g.line_feed();
        g.carriage_return();
    }
    g
}

fn main() {
    let mut g = filled();
    let job = g
        .resize_offloading_scrollback(ROWS, NEW_COLS)
        .expect("a reflow job");
    let lines = job.line_count();
    let t0 = Instant::now();
    let done = job.reflow();
    let one_shot = t0.elapsed();
    println!(
        "one-shot: {lines} input lines (~{COLS} cols) -> {} output lines in {one_shot:?} \
         = {:.2} us/input-line",
        done.line_count(),
        one_shot.as_secs_f64() * 1e6 / lines as f64
    );

    let mut g = filled();
    let mut job = g
        .resize_offloading_scrollback(ROWS, NEW_COLS)
        .expect("a reflow job");
    let (mut steps, mut worst, mut total) = (0usize, Duration::ZERO, Duration::ZERO);
    loop {
        let t = Instant::now();
        let step = job.reflow_step(STEP_BUDGET);
        let dt = t.elapsed();
        steps += 1;
        worst = worst.max(dt);
        total += dt;
        match step {
            ReflowStep::InProgress(next) => job = next,
            ReflowStep::Done(_) => break,
        }
    }
    println!("stepped @ {STEP_BUDGET}/step: {steps} steps, worst step {worst:?}, total {total:?}");
}
