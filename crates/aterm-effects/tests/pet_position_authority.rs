// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One writer owns where the body is.
//!
//! The caret's escort is the pet's own locomotion (the chase and the flight,
//! continuous and speed-limited). The console observation layer's only
//! placement role is moving a body off a surface it may not occupy (a
//! selection, an image, a row with no certified cell-to-pixel projection), or
//! not drawing it. It must never relocate a body for being far from the caret:
//! a second writer closing that distance in one frame fights the escort, and
//! the sprite sweeps out and snaps back while output prints.
//!
//! The oracle is the pet itself, as in `pet_escort_primacy`:
//! `set_console_presentable(false)` returns every console entry point early,
//! so a twin brain with it off is the plain escort. Streaming output must not
//! tell the two apart.

use std::time::{Duration, Instant};

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetBrain, PetFrame, PetSense};
use aterm_effects::pet_world::{PetPane, PetRect, PetWorldFacts};

const ROWS: u16 = 24;
const COLS: u16 = 80;
const CELL_W: u16 = 8;
const CELL_H: u16 = 16;

/// A single-frame displacement this large is not locomotion. The chase is
/// speed-limited and the flight is a schedule; neither can cross two cells
/// between two 16 ms frames at the paces this crate ships.
const TELEPORT: f32 = 2.0;

struct Scene {
    term: Terminal,
    brain: PetBrain,
    presentable: bool,
}

impl Scene {
    /// `presentable == false` is the v0.76.0 escort with no console layer.
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

fn body(frame: PetFrame) -> Option<PetRect> {
    frame
        .body_px(CELL_W, CELL_H, COLS, ROWS)
        .map(|(x0, x1, y0, y1)| {
            PetRect::new(
                y0 as f32 / f32::from(CELL_H),
                x0 as f32 / f32::from(CELL_W),
                (y1 - y0) as f32 / f32::from(CELL_H),
                (x1 - x0) as f32 / f32::from(CELL_W),
            )
        })
}

/// One sample of the thing the owner is describing: where the sprite is,
/// where the caret is, and what the console layer said about it.
struct Sample {
    frame: u32,
    col: f32,
    row: f32,
    caret_col: f32,
    reason: &'static str,
}

/// A command printing into a screen not yet full: the caret walks down
/// through fresh rows. One 57-column line every 240 ms, the newline landing on
/// the next frame as a real `echo` does, so the caret is seen at the line's
/// end for one tick and then at column 0 below.
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

/// Every single-frame displacement at least [`TELEPORT`] cells wide.
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

/// The body is not yanked back to a console home while a command prints: it
/// may walk, fly or stand still, but not cross the pane in one frame and back.
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
}

/// The same seen through the reason string: `cursor-home` claims the escort's
/// body; `pet_escort_primacy` refuses it on a typing frame, and output is not
/// a licence either.
#[test]
fn a_printing_command_is_not_a_licence_to_claim_the_escorts_body() {
    let live = stream_output(true, 900);
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

/// A caret that jumps is not a reason to teleport the cat: the escort closes
/// distance at a walking pace, and that pace is the feature. Measured with no
/// keystroke, so only the placement pass could move the body.
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

    // Sixty columns and eight rows away, in one escape sequence.
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

/// The correction the layer does owe remains: a body standing on a surface it
/// may not occupy still yields.
#[test]
fn a_protected_surface_still_moves_or_hides_the_body() {
    let mut now = Instant::now();
    let mut scene = Scene::new(true, b"\x1b[12;40H");
    let mut frame = scene.tick(now);
    for _ in 0..400 {
        now += Duration::from_millis(16);
        frame = scene.tick(now);
        if !scene.brain.needs_frames() {
            break;
        }
    }
    let seated = body(frame).expect("precondition: a settled pet has a body");
    assert!(frame.alpha > 0, "precondition: the pet must be on glass");

    scene.term.text_selection_mut().start_selection(
        0,
        0,
        SelectionSide::Left,
        SelectionType::Simple,
    );
    scene.term.text_selection_mut().update_selection(
        i32::from(ROWS - 1),
        COLS - 1,
        SelectionSide::Right,
    );
    now += Duration::from_millis(16);
    let protected = scene.tick(now);
    assert!(
        protected.alpha == 0,
        "a pane-wide selection left the pet drawn at {:?} (it was seated at \
         {seated:?}); reason {}",
        body(protected),
        scene.brain.console_reason(),
    );

    scene.term.text_selection_mut().clear();
    let mut back = protected;
    for _ in 0..400 {
        now += Duration::from_millis(16);
        back = scene.tick(now);
        if back.alpha > 0 {
            break;
        }
    }
    assert!(
        back.alpha > 0,
        "the pet never came back after the selection was cleared; reason {}",
        scene.brain.console_reason(),
    );
}
