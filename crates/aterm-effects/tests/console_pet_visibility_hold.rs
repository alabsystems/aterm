// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that typing, cursor navigation, and output bursts do not strobe the console pet.

use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{ART_ASPECT, ART_ROWS, PetBrain, PetFrame, PetInputKind, PetSense};
use aterm_effects::pet_world::{PetPane, PetRect, PetWorld, PetWorldFacts};

const ROWS: u16 = 24;
const COLS: u16 = 80;
const CELL_W: u16 = 8;
const CELL_H: u16 = 16;

fn body(col: f32, row: f32) -> PetRect {
    let cw = f32::from(CELL_W);
    let ch = f32::from(CELL_H);
    let h = (ART_ROWS * ch).round();
    let w = (h * ART_ASPECT).round();
    PetRect::new(
        (((row + 1.0) * ch).round() - h) / ch,
        (col * cw).round() / cw,
        h / ch,
        w / cw,
    )
}

struct Fixture {
    brain: PetBrain,
    /// A second, independent world, so the oracle reads occupancy without asking the brain.
    world: PetWorld,
    term: Terminal,
    now: Instant,
}

impl Fixture {
    fn new() -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"\x1b]11;#000000\x07");
        Self {
            brain: PetBrain::default(),
            world: PetWorld::default(),
            term,
            now: Instant::now(),
        }
    }

    fn settle(&mut self) {
        for _ in 0..40 {
            self.tick(100);
        }
    }

    fn tick(&mut self, ms: u64) -> PetFrame {
        self.now += Duration::from_millis(ms);
        let input = self.term.cell_frame(ROWS as usize, COLS as usize);
        let facts = PetWorldFacts::read(&self.term, 7);
        self.brain
            .observe_console(&input, &facts, PetPane::full(&input));
        self.world.observe(&input, &facts, PetPane::full(&input));
        self.brain.set_console_presentable(true);
        let cursor = self.term.cursor();
        self.brain.tick(PetSense {
            caret_drawn: true,
            now: self.now,
            caret: Some((cursor.row, cursor.col)),
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

    fn ink_under(&self, frame: &PetFrame) -> bool {
        let rect = body(frame.col, frame.row);
        let r1 = (rect.row + rect.rows).ceil() as usize;
        let c1 = (rect.col + rect.cols).ceil() as usize;
        (rect.row.floor() as usize..r1).any(|r| {
            (rect.col.floor() as usize..c1).any(|c| self.world.ink_at(r, c) != Some(false))
        })
    }
}

/// What a strobe looks like in numbers.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Strobe {
    frames: usize,
    /// Frames with nothing at all on glass.
    blackouts: usize,
    /// Transitions from visible to blank.
    cuts: usize,
    /// The largest single-frame drop in opacity. A fade steps by `255 * dt / VETO_FADE` ~ 21 at
    /// 16 ms; a cut by up to 255.
    worst_drop: i32,
    /// Blank frames with no glyph anywhere under the sprite: a veto that invented ink.
    blank_over_nothing: usize,
}

impl Strobe {
    fn observe(&mut self, alpha: u8, prev: u8, ink: bool) {
        self.frames += 1;
        self.worst_drop = self.worst_drop.max(i32::from(prev) - i32::from(alpha));
        if alpha == 0 {
            self.blackouts += 1;
            if prev > 0 {
                self.cuts += 1;
            }
            if !ink {
                self.blank_over_nothing += 1;
            }
        }
    }
}

#[test]
fn typing_never_blanks_the_pet() {
    let mut f = Fixture::new();
    f.term.process(b"$ ");
    f.settle();
    let mut strobe = Strobe::default();
    let mut prev = f.tick(16).alpha;
    for ch in b"the quick brown fox jumps over the lazy dog" {
        f.brain.note_console_input(f.now, PetInputKind::Text);
        f.term.process(&[*ch]);
        for _ in 0..8 {
            let frame = f.tick(16);
            let ink = f.ink_under(&frame);
            strobe.observe(frame.alpha, prev, ink);
            prev = frame.alpha;
        }
    }
    println!("typing: {strobe:?}");
    assert_eq!(strobe.blackouts, 0, "typing strobes the pet: {strobe:?}");
    assert_eq!(strobe.cuts, 0, "typing cuts the pet: {strobe:?}");
}

#[test]
fn the_caret_alone_never_blanks_its_escort() {
    let mut f = Fixture::new();
    f.settle();
    let mut strobe = Strobe::default();
    let mut prev = f.tick(16).alpha;
    for col in 1..=40u16 {
        f.term.process(format!("\x1b[12;{col}H").as_bytes());
        f.brain.note_console_input(f.now, PetInputKind::Navigate);
        for _ in 0..6 {
            let frame = f.tick(16);
            let ink = f.ink_under(&frame);
            strobe.observe(frame.alpha, prev, ink);
            prev = frame.alpha;
        }
    }
    println!("bare caret: {strobe:?}");
    assert_eq!(
        strobe.blackouts, 0,
        "a moving caret blanks its own escort over a blank screen: {strobe:?}"
    );
}

#[test]
fn an_output_burst_yields_by_fading_never_by_cutting() {
    let mut f = Fixture::new();
    f.settle();
    let mut strobe = Strobe::default();
    let mut prev = f.tick(16).alpha;
    let mut since_line = 0u64;
    for _ in 0..90 {
        since_line += 16;
        if since_line >= 100 {
            since_line = 0;
            f.term.process(b"ok\r\n");
        }
        let frame = f.tick(16);
        let ink = f.ink_under(&frame);
        strobe.observe(frame.alpha, prev, ink);
        prev = frame.alpha;
    }
    println!("output burst: {strobe:?}");
    assert!(
        strobe.worst_drop <= 40,
        "the pet is cut, not faded: {strobe:?}"
    );
    assert_eq!(
        strobe.blank_over_nothing, 0,
        "the pet is blanked with no glyph under it: {strobe:?}"
    );
}
