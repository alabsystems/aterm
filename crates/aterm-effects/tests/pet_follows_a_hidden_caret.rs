// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PET FOLLOWS THE CARET IT CANNOT SEE.
//!
//! Owner report, 2026-09-19, against the shipped v0.88.0: *"the cursorkitty is
//! not correctly following the cursor reliably. it is getting shoved away as
//! like a glitch sometimes, and sometimes it's getting trapped at the top of
//! the screen."*
//!
//! MEASURED on that build with `aterm ctl trail status`, driving a fixture that
//! hides the cursor with DECTCEM while it repaints — the choreography every
//! progress bar, spinner, pager, editor and LLM console uses: with the caret
//! parked on row 44 the pet sat at **row 0.0 for 20.04 s**, and in the same
//! fixture started mid-screen it sat at **row 23.4 for 20.15 s**. It holds
//! wherever it stands, for as long as the hide lasts, and "the top of the
//! screen" is simply where it was most often standing — a fresh prompt, a
//! `clear`, a console whose input row is near the top. When the cursor came
//! back it crossed **28.4 cells in one frame**.
//!
//! The cause was one conflation in the host: `cursor_visible && display_offset
//! == 0` decided whether there was a caret AT ALL, so "I am not painting it"
//! arrived at the brain as "it is not there" — a bound returned as a fact. The
//! two facts travel separately now (`PetSense::caret` /
//! `PetSense::caret_drawn`), and the console's hidden-caret hold is bounded
//! when the caret is merely unpainted and unbounded only when it is genuinely
//! absent (scrollback), where there is nothing better to do.
//!
//! The other two laws here are the same family — a bound answering a question
//! about where the animal is — found by the same audit and measured in code:
//! the console's scroll compensation must not carry the body off the top of the
//! glass, and a flight born this tick must not be advanced by this tick's
//! clock.

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

    /// One frame. The caret's CELL is fed whatever DECTCEM says, exactly as the
    /// host does since 2026-09-19; `caret_drawn` carries the visibility.
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

/// THE REPORT, as a law. A caret that moves while the cursor is hidden is
/// still a caret, and the pet is expected to arrive at it — not to sit where it
/// was standing when the hide began, for as long as the program keeps working.
#[test]
fn a_pet_follows_a_caret_that_moves_while_the_cursor_is_hidden() {
    let mut s = Scene::new();
    // A prompt near the TOP, the seat the owner kept finding the cat stuck in.
    s.term.process(b"\x1b[1;5H");
    let settled = s.settle(120);
    assert!(settled.alpha > 0, "the pet must be on glass to be judged");
    assert!(
        settled.row < 6.0,
        "the pet stations near a top-row caret (row {})",
        settled.row
    );

    // The program hides the cursor and works — repainting, moving the caret to
    // the far side of the screen, exactly as a TUI does.
    s.term.process(b"\x1b[?25l");
    s.term.process(format!("\x1b[{};40H", ROWS - 3).as_bytes());

    // Two seconds of hidden repaints. The shipped build sat still for twenty.
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

/// THE ARM THE OWNER'S REPORT WAS ACTUALLY ABOUT — the tennis watch.
///
/// A console that repaints a row (`\r` or `ESC[row;1H`, erase, write, back to
/// the caret column) reverses the caret's direction once per repaint. Three
/// reversals is a frolic, a second frolic inside `TENNIS_AFTER` latches the
/// watch, and the watch's `PetAction::Sit if self.tennis` arm returns BEFORE
/// every door — the row-hop included — for as long as repaints keep refreshing
/// it. The cat could not travel at all, and nothing asked where the rally was.
///
/// Measured on v0.88.0: 20.03 s frozen with the caret 44 rows away. After the
/// fix the same fixture crosses in 1.38 s and holds a median gap of 0.5 rows.
#[test]
fn a_repainting_console_does_not_freeze_the_pet_away_from_the_caret() {
    let mut s = Scene::new();
    // Settle the cat at a prompt near the TOP.
    s.term.process(b"\x1b[1;5H");
    s.settle(150);
    let settled = s.settle(60);
    assert!(
        settled.alpha > 0 && settled.row < 6.0,
        "cat starts at the top"
    );

    // Now REPAINT a row far below, the way a console does: to the margin,
    // erase, write, then back to the caret column. That is the rally.
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
