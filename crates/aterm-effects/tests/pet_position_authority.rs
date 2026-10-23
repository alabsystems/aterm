// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests verifying that console placement never teleports the kitty pet.

use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetBrain, PetFrame, PetSense};
use aterm_effects::pet_world::{PetPane, PetWorldFacts};

const ROWS: u16 = 24;
const COLS: u16 = 80;
const CELL_W: u16 = 8;
const CELL_H: u16 = 16;

const TELEPORT: f32 = 2.0;

struct Scene {
    term: Terminal,
    brain: PetBrain,
    presentable: bool,
}

impl Scene {
    fn new(presentable: bool, home: &[u8]) -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"\x1b]11;#000000\x07");
        term.process(home);
        Self {
            term,
            brain: PetBrain::default(),
            presentable,
        }
    }

    fn tick(&mut self, now: Instant) -> PetFrame {
        let input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let facts = PetWorldFacts::read(&self.term, 7);
        self.brain
            .observe_console(&input, &facts, PetPane::full(&input));
        self.brain.set_console_presentable(self.presentable);
        self.brain.tick(PetSense {
            caret_drawn: true,
            now,
            caret: (input.cursor_visible && input.display_offset == 0)
                .then_some((input.cursor_row as u16, input.cursor_col as u16)),
            wrapped: false,
            rows: ROWS,
            cols: COLS,
            cell_w: CELL_W,
            cell_h: CELL_H,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        })
    }
}

struct Sample {
    frame: u32,
    col: f32,
    row: f32,
    caret_col: f32,
    reason: &'static str,
}

fn stream_output(presentable: bool, frames: u32) -> Vec<Sample> {
    let mut now = Instant::now();
    let mut scene = Scene::new(presentable, b"\x1b[1;1H");
    let mut out = Vec::new();
    let mut line = 0u32;
    for i in 0..frames {
        now += Duration::from_millis(16);
        if i % 15 == 0 {
            let text = format!("output line {line} ------------------------------------------");
            scene.term.process(text.as_bytes());
            line += 1;
        }
        if i % 15 == 1 {
            scene.term.process(b"\r\n");
        }
        let frame = scene.tick(now);
        out.push(Sample {
            frame: i,
            col: frame.col,
            row: frame.row,
            caret_col: f32::from(scene.term.cursor().col),
            reason: scene.brain.console_reason(),
        });
    }
    out
}

fn teleports(series: &[Sample]) -> Vec<(u32, f32)> {
    series
        .windows(2)
        .filter_map(|w| {
            let d = w[1].col - w[0].col;
            (d.abs() >= TELEPORT).then_some((w[1].frame, d))
        })
        .collect()
}

fn trace(tag: &str, series: &[Sample], from: usize, to: usize) -> String {
    series[from.min(series.len())..to.min(series.len())]
        .iter()
        .map(|s| {
            format!(
                "\n  {tag} {:3} col={:7.2} row={:6.2} caret_col={:5.1} {}",
                s.frame, s.col, s.row, s.caret_col, s.reason
            )
        })
        .collect()
}

/// Streaming output never teleports the body or claims it via the console placement layer.
#[test]
fn streaming_output_never_teleports_the_body() {
    let live = stream_output(true, 900);
    let bare = stream_output(false, 900);
    let live_jumps = teleports(&live);
    let bare_jumps = teleports(&bare);
    assert!(
        bare_jumps.is_empty(),
        "precondition: the v0.76 escort teleports too ({} times, worst {:.2}){}",
        bare_jumps.len(),
        bare_jumps.iter().map(|j| j.1.abs()).fold(0.0, f32::max),
        trace("bare", &bare, 110, 160),
    );
    assert!(
        live_jumps.is_empty(),
        "the console layer teleported the body {} times in {} frames, worst \
         {:.2} cells (v0.76 escort: {} in the same run). First ten: {:?}{}",
        live_jumps.len(),
        live.len(),
        live_jumps.iter().map(|j| j.1.abs()).fold(0.0, f32::max),
        bare_jumps.len(),
        &live_jumps[..live_jumps.len().min(10)],
        trace("live", &live, 110, 160),
    );
    let claimed: Vec<_> = live
        .iter()
        .filter(|s| !matches!(s.reason, "quiet" | "committed-input" | "advancing-ink"))
        .map(|s| (s.frame, s.reason))
        .collect();
    assert!(
        claimed.is_empty(),
        "the console layer claimed the escort's body on {} of {} output \
         frames. First ten: {:?}{}",
        claimed.len(),
        live.len(),
        &claimed[..claimed.len().min(10)],
        trace("live", &live, 110, 160),
    );
}

/// A jumping caret closes distance at a walking pace without teleporting the body.
#[test]
fn a_caret_that_jumps_does_not_teleport_the_body() {
    let mut now = Instant::now();
    let mut scene = Scene::new(true, b"\x1b[12;10H");
    let mut frame = scene.tick(now);
    for _ in 0..400 {
        now += Duration::from_millis(16);
        frame = scene.tick(now);
        if !scene.brain.needs_frames() {
            break;
        }
    }
    let seated = frame.col;
    assert!(frame.alpha > 0, "precondition: the pet must be on glass");

    scene.term.process(b"\x1b[20;70H");
    let mut worst: (u32, f32, &'static str) = (0, 0.0, "quiet");
    let mut series = Vec::new();
    let mut prev = frame.col;
    for i in 0..120u32 {
        now += Duration::from_millis(16);
        let f = scene.tick(now);
        let step = f.col - prev;
        series.push(Sample {
            frame: i,
            col: f.col,
            row: f.row,
            caret_col: f32::from(scene.term.cursor().col),
            reason: scene.brain.console_reason(),
        });
        if step.abs() > worst.1.abs() {
            worst = (i, step, scene.brain.console_reason());
        }
        prev = f.col;
    }
    assert!(
        worst.1.abs() < TELEPORT,
        "the body moved {:.2} cells in one frame (frame {}, reason {}) after \
         the caret jumped; it was seated at {seated:.2}{}",
        worst.1,
        worst.0,
        worst.2,
        trace("jump", &series, 0, 24),
    );
}
