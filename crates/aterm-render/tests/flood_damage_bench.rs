// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// BOTTOM-PINNED OUTPUT FLOOD — CPU render cost, with two controls.
//
// The flood is the arm under test: `display_offset` stays 0 while `base_y`
// advances, so the row diff used to compare destination row `r` against SOURCE
// row `r` and repaint 49 of 50 rows per frame at every output rate. The
// shift-aware plan diffs against row `r + da` and blits the rest.
//
// Wall time on ONE build proves nothing here (a mixed lane swings ±11%), so this
// prints THREE arms per run and the comparison is across builds, interleaved:
//
//   * FLOOD   — the arm that must move.
//   * TYPING  — CONTROL: a one-cell keystroke echo. The shift planner refuses it
//               on the first clause it reaches (`da == 0`), so it must NOT move;
//               it is also where a regression would show up if merely ASKING the
//               planner cost anything on the frames that dominate a session.
//   * FULL    — CONTROL: forced full repaints (`reset_damage_cache`). Touches no
//               damage decision at all, so it is the machine's noise floor.
//
// Run (release, for realistic numbers):
//   targo --unverified test -p aterm-render --release --test flood_damage_bench \
//       -- --ignored --nocapture

use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_render::{RenderInput, Renderer, Theme, WindowCpu};

const ROWS: usize = 50;
const COLS: usize = 120;

fn renderer() -> Option<Renderer> {
    Renderer::from_system(16.0, Theme::default()).map(|mut r| {
        r.debug_block_on_lazy_fallbacks();
        r
    })
}

/// A flood at `rate` lines per frame: distinct lines, bottom-pinned.
fn flood_frames(rate: usize, frames: usize) -> Vec<RenderInput> {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    for i in 0..400 {
        term.process(format!("line {i} \u{2502} representative output text\r\n").as_bytes());
    }
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        for k in 0..rate {
            term.process(format!("flood {f}.{k} \u{2502} representative output\r\n").as_bytes());
        }
        out.push(term.cell_frame(ROWS, COLS));
    }
    out
}

/// CONTROL: keystroke echo — one cell changes per frame, no scroll.
fn typing_frames(frames: usize) -> Vec<RenderInput> {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    for i in 0..400 {
        term.process(format!("line {i} \u{2502} representative output text\r\n").as_bytes());
    }
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        term.process(&[b'a' + (f % 26) as u8]);
        out.push(term.cell_frame(ROWS, COLS));
    }
    out
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort_unstable();
    v[v.len() / 2]
}

/// One measured arm: its own warm renderer + window (so its damage cache state
/// is its own), its own frame sequence, and its own sample list.
struct Arm {
    label: String,
    warm: Renderer,
    wc: WindowCpu,
    frames: Vec<RenderInput>,
    full_repaint: bool,
    samples: Vec<Duration>,
}

impl Arm {
    fn new(label: &str, frames: Vec<RenderInput>, full_repaint: bool) -> Self {
        Self {
            label: label.to_string(),
            warm: renderer().expect("font (checked by caller)"),
            wc: WindowCpu::new(),
            frames,
            full_repaint,
            samples: Vec::new(),
        }
    }

    /// One timed pass over the arm's whole sequence; records us/frame.
    fn round(&mut self, record: bool) {
        let t0 = Instant::now();
        for f in &self.frames {
            if self.full_repaint {
                Renderer::reset_damage_cache(&mut self.wc);
            }
            let _ = self.warm.render_input_cached(&mut self.wc, f);
        }
        let per = t0.elapsed() / self.frames.len() as u32;
        if record {
            self.samples.push(per);
        }
    }

    /// The WORK the arm actually does, off the clock: mean rows repainted per
    /// frame and the damage-outcome mix. Wall time without this is unreadable —
    /// it cannot tell a cheaper decision from a cheaper machine.
    fn work(&mut self) -> (f64, String) {
        let mut painted = 0usize;
        let mut full = 0usize;
        let mut scroll = 0usize;
        let mut rowsd = 0usize;
        let mut gate = 0usize;
        for f in &self.frames {
            if self.full_repaint {
                Renderer::reset_damage_cache(&mut self.wc);
            }
            let _ = self.warm.render_input_cached(&mut self.wc, f);
            match self.wc.last_damage() {
                aterm_render::DamageOutcome::Full => {
                    full += 1;
                    painted += ROWS;
                }
                aterm_render::DamageOutcome::GateHit => gate += 1,
                aterm_render::DamageOutcome::Scroll { .. } => {
                    scroll += 1;
                    painted += self.wc.dirty_rows().iter().filter(|&&d| d).count();
                }
                aterm_render::DamageOutcome::Rows => {
                    rowsd += 1;
                    painted += self.wc.dirty_rows().iter().filter(|&&d| d).count();
                }
            }
        }
        (
            painted as f64 / self.frames.len() as f64,
            format!("full={full} scroll={scroll} rows={rowsd} gate={gate}"),
        )
    }
}

#[test]
#[ignore = "timing benchmark: run explicitly with --release --ignored --nocapture"]
fn flood_cpu_frame_cost() {
    if renderer().is_none() {
        eprintln!("SKIP: no system monospace font");
        return;
    }
    const RUNS: usize = 15;
    const WARMUP: usize = 3;
    const FRAMES: usize = 40;
    let mut arms: Vec<Arm> = Vec::new();
    for rate in [1usize, 2, 4, 8, 16] {
        arms.push(Arm::new(
            &format!("FLOOD rate={rate:2}"),
            flood_frames(rate, FRAMES),
            false,
        ));
    }
    arms.push(Arm::new("CONTROL typing ", typing_frames(FRAMES), false));
    arms.push(Arm::new("CONTROL full   ", flood_frames(1, FRAMES), true));

    // ROUND-ROBIN, not arm-by-arm: every arm is timed once per round, so a CPU
    // frequency ramp or a thermal drift over the run lands on all of them
    // instead of on whichever ran first. ROTATED each round as well, because a
    // fixed order is its own bias — the arm that always follows the full-repaint
    // control always starts on the cache that control just evicted (worth ~25%
    // here, measured: the rate-1 arm read SLOWER than the rate-2 arm until the
    // rotation went in).
    let n = arms.len();
    for round in 0..(WARMUP + RUNS) {
        for i in 0..n {
            arms[(i + round) % n].round(round >= WARMUP);
        }
    }
    // THE DECISION ALONE, no rasterization: `compute_dirty_rows` (which fails
    // fast on a flood — every row differs at its first cell) plus the shift plan
    // (which must walk EVERY cell of every retained row to prove it survived the
    // shift). This is why a rate-1 flood is not simply "5/50ths of a repaint":
    // the cheaper the frame, the larger the share the proof takes.
    for rate in [1usize, 2, 4, 8, 16] {
        let frames = flood_frames(rate, FRAMES);
        let mut dirty = Vec::new();
        let mut shift = Vec::new();
        let mut best = Duration::MAX;
        for _ in 0..RUNS {
            let t0 = Instant::now();
            for pair in frames.windows(2) {
                let d = aterm_render::compute_dirty_rows(
                    &pair[0], &pair[1], false, None, false, None, 16, &mut dirty,
                );
                std::hint::black_box(&d);
                let p = aterm_render::scroll_shift_plan(
                    &pair[0],
                    &pair[1],
                    false,
                    None,
                    false,
                    None,
                    16,
                    aterm_render::RetainedRows::Diff,
                    &mut shift,
                );
                std::hint::black_box(&p);
            }
            best = best.min(t0.elapsed() / (frames.len() - 1) as u32);
        }
        println!(
            "PLAN  rate={rate:2} best   {:>9.3} us/frame (diff + shift proof, no raster)",
            best.as_secs_f64() * 1e6
        );
    }
    for arm in &mut arms {
        let m = median(arm.samples.clone());
        let lo = arm.samples.iter().min().copied().unwrap_or_default();
        let hi = arm.samples.iter().max().copied().unwrap_or_default();
        let (painted, mix) = arm.work();
        println!(
            "{} median {:>9.3} us/frame  (min {:.3}, max {:.3})  rows/frame {painted:5.2} of \
             {ROWS}  [{mix}]",
            arm.label,
            m.as_secs_f64() * 1e6,
            lo.as_secs_f64() * 1e6,
            hi.as_secs_f64() * 1e6,
        );
    }
}
