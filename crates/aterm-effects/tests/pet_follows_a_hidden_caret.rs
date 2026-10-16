// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pet follows the caret it cannot see.
//!
//! Programs that hide the cursor with DECTCEM while repainting (progress bars,
//! spinners, pagers, editors, LLM consoles) still have a caret. The host sends the
//! caret cell and its visibility separately (`PetSense::caret` /
//! `PetSense::caret_drawn`), so the console's hidden-caret hold is bounded when the
//! caret is merely unpainted and unbounded only when it is absent (scrollback);
//! an unbounded hold would freeze the pet for the whole hide, then jump it.

use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetBrain, PetFrame, PetSense};
use aterm_effects::pet_world::{PetPane, PetWorldFacts};

const ROWS: u16 = 48;
const COLS: u16 = 100;
const CELL_W: u16 = 8;
const CELL_H: u16 = 16;

struct Scene {
    term: Terminal,
    brain: PetBrain,
    now: Instant,
}

impl Scene {
    fn new() -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"\x1b]11;#000000\x07");
        Self {
            term,
            brain: PetBrain::default(),
            now: Instant::now(),
        }
    }

    /// One frame. The caret's cell is fed whatever DECTCEM says, as the host
    /// does; `caret_drawn` carries the visibility.
    fn tick(&mut self, millis: u64) -> PetFrame {
        self.now += Duration::from_millis(millis);
        let input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let facts = PetWorldFacts::read(&self.term, 7);
        self.brain
            .observe_console(&input, &facts, PetPane::full(&input));
        self.brain.set_console_presentable(true);
        self.brain.tick(PetSense {
            now: self.now,
            caret: (input.display_offset == 0)
                .then_some((input.cursor_row as u16, input.cursor_col as u16)),
            caret_drawn: input.cursor_visible,
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

    fn settle(&mut self, frames: usize) -> PetFrame {
        let mut f = self.tick(16);
        for _ in 0..frames {
            f = self.tick(16);
        }
        f
    }
}

/// A caret that moves while the cursor is hidden is still a caret, and the pet
/// arrives at it.
#[test]
fn a_pet_follows_a_caret_that_moves_while_the_cursor_is_hidden() {
    let mut s = Scene::new();
    // A prompt near the top.
    s.term.process(b"\x1b[1;5H");
    let settled = s.settle(120);
    assert!(settled.alpha > 0, "the pet must be on glass to be judged");
    assert!(
        settled.row < 6.0,
        "the pet stations near a top-row caret (row {})",
        settled.row
    );

    // The program hides the cursor and moves the caret to the far side of the
    // screen, as a TUI does.
    s.term.process(b"\x1b[?25l");
    s.term.process(format!("\x1b[{};40H", ROWS - 3).as_bytes());

    // Two seconds of hidden repaints.
    let mut last = s.tick(16);
    for _ in 0..125 {
        last = s.tick(16);
    }

    let caret_row = f32::from(ROWS - 4);
    assert!(
        (last.row - caret_row).abs() <= 6.0,
        "the pet must follow a hidden caret: it is on row {} and the caret is on row {caret_row}",
        last.row,
    );
    assert!(
        last.row > 6.0,
        "…and in particular it must not still be sitting at the top (row {})",
        last.row
    );
}

/// The tennis watch: a console repainting a row reverses the caret's direction once
/// per repaint, three reversals is a frolic, and a second frolic inside
/// `TENNIS_AFTER` latches the watch. The watch must not stop the cat travelling to a
/// caret rows away.
#[test]
fn a_repainting_console_does_not_freeze_the_pet_away_from_the_caret() {
    let mut s = Scene::new();
    s.term.process(b"\x1b[1;5H");
    s.settle(150);
    let settled = s.settle(60);
    assert!(
        settled.alpha > 0 && settled.row < 6.0,
        "cat starts at the top"
    );

    // Repaint a row far below as a console does: to the margin, erase, write, back
    // to the caret column.
    let far = ROWS - 4;
    for i in 0..140 {
        let col = 12 + (i % 6);
        s.term
            .process(format!("\x1b[{far};1H\x1b[2Kbottom prompt {i}\x1b[{far};{col}H").as_bytes());
        s.tick(16);
    }
    let after = s.settle(120);

    let caret_row = f32::from(far - 1);
    assert!(
        (after.row - caret_row).abs() <= 6.0,
        "a repainting console left the pet on row {} with the caret on row {caret_row}",
        after.row
    );
}
