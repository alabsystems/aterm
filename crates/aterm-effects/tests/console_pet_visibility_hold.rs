// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pet must not strobe while the user types.
//!
//! The pet's visibility is held rather than a per-frame boolean of screen
//! content, so keystrokes (even the caret's own protection halo) cannot flip
//! it at the key rate. These tests measure a real `Terminal` snapshot stream:
//! blackout frames, on->off cuts, and the largest single-frame opacity drop
//! (which separates a fade from a cut). The guard remains: a protected surface
//! must still take the pet to zero, and ordinary ink moves the resident behind
//! the glyphs rather than switching it off.

use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{ART_ASPECT, ART_ROWS, PetBrain, PetFrame, PetInputKind, PetSense};
use aterm_effects::pet_world::{PetPane, PetRect, PetWorld, PetWorldFacts};

const ROWS: u16 = 24;
const COLS: u16 = 80;
const CELL_W: u16 = 8;
const CELL_H: u16 = 16;

/// The sprite rectangle the emitter would project for these feet — the same
/// arithmetic as `PetBrain::console_body`, restated here so the oracle does
/// not borrow the code under test. Independent of `alpha`, unlike
/// `PetFrame::body_px`, so a blanked frame still has a measurable footprint.
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
    /// A SECOND, independent world, so the oracle reads occupancy without
    /// asking the brain what it decided.
    world: PetWorld,
    term: Terminal,
    now: Instant,
    presentable: bool,
}

impl Fixture {
    fn new(presentable: bool) -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        // Match a presented host: an unresolved COLOR_UNSET default cannot
        // certify a blank sprite footprint even on an empty terminal.
        term.process(b"\x1b]11;#000000\x07");
        Self {
            brain: PetBrain::default(),
            world: PetWorld::default(),
            term,
            now: Instant::now(),
            presentable,
        }
    }

    /// Let the pet arrive and finish its fade before anything is measured.
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
        self.brain.set_console_presentable(self.presentable);
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

    /// Real glyph occupancy under the sprite (`ink_at`, independent of caret
    /// and selection protection). `None` (unobserved) reads as inked, so this
    /// oracle never accuses the veto unfairly.
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
    /// The largest single-frame drop in opacity. A FADE steps by
    /// `255 * dt / VETO_FADE` ~ 21 at 16 ms; a CUT steps by up to 255.
    worst_drop: i32,
    /// Blank frames with NO glyph anywhere under the sprite — a veto that
    /// invented ink.
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

/// The pangram typed one key at a time at 125 ms/key, with the prompt already
/// on screen so the pet starts from a settled station.
#[test]
fn typing_never_blanks_the_pet() {
    let mut f = Fixture::new(true);
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

/// The caret is not an obstruction to its own escort: the caret walks across
/// a blank line with no ink near the sprite, so no frame may be blank.
#[test]
fn the_caret_alone_never_blanks_its_escort() {
    let mut f = Fixture::new(true);
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

/// 10 lines/s of output for 1.44 s at 16 ms (90 frames). The pet's station is
/// one cell right of the caret, where output lands, so a real obstruction is
/// legitimate; reaching it by a cut, or blanking over blank cells, is not.
#[test]
fn an_output_burst_yields_by_fading_never_by_cutting() {
    let mut f = Fixture::new(true);
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
    // One fade step at 16 ms is 255 * 0.016 / VETO_FADE(0.20) ~ 21; the
    // pet's own ramps can compound with it. Anything past this is a CUT.
    assert!(
        strobe.worst_drop <= 40,
        "the pet is cut, not faded: {strobe:?}"
    );
    assert_eq!(
        strobe.blank_over_nothing, 0,
        "the pet is blanked with no glyph under it: {strobe:?}"
    );
}

/// The veto's reason to exist: ordinary ink in front of the body is a z-order
/// fact (the resident stays whole and renders under the glyphs,
/// `PetFrame::under_ink`; see `console_cursor_home.rs`'s
/// `ordinary_dense_output_does_not_make_an_idle_resident_blink_out`). A held
/// verdict never licenses painting in front of the user's text, and a
/// protected surface (a full-screen selection) still takes the pet to zero.
#[test]
fn a_full_screen_of_text_goes_under_the_ink_and_a_protected_one_still_yields() {
    use aterm_core::selection::{SelectionSide, SelectionType};

    let mut f = Fixture::new(true);
    f.settle();
    let mut fill = Vec::new();
    for row in 1..=ROWS {
        fill.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
        fill.extend(std::iter::repeat_n(b'#', COLS as usize));
    }
    fill.extend_from_slice(b"\x1b[12;40H");
    f.term.process(&fill);
    let mut last = f.tick(16);
    for _ in 0..89 {
        last = f.tick(16);
    }
    assert!(
        last.alpha > 0,
        "ordinary ink is a z-order fact: the resident stays whole"
    );
    assert!(
        last.under_ink,
        "…and it may never be painted IN FRONT of the user's text"
    );

    // A PROTECTED surface is the other half, and it still yields to zero.
    f.term
        .text_selection_mut()
        .start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    f.term.text_selection_mut().update_selection(
        i32::from(ROWS - 1),
        COLS - 1,
        SelectionSide::Right,
    );
    let mut last = 255;
    for _ in 0..90 {
        last = f.tick(16).alpha;
    }
    assert_eq!(
        last, 0,
        "the pet must not stand on a completely selected screen"
    );
}

/// The console-life gate is the only difference between these two runs, and
/// with it OFF there is no veto at all — the control for every number above.
#[test]
fn the_gate_is_the_only_difference() {
    let mut off = Fixture::new(false);
    off.term.process(b"$ ");
    off.settle();
    let mut frames = 0;
    let mut blackouts = 0;
    for ch in b"the quick brown fox jumps over the lazy dog" {
        off.brain.note_console_input(off.now, PetInputKind::Text);
        off.term.process(&[*ch]);
        for _ in 0..8 {
            frames += 1;
            blackouts += usize::from(off.tick(16).alpha == 0);
        }
    }
    println!("gate OFF: frames={frames} blackouts={blackouts}");
    assert_eq!(blackouts, 0, "control run must never blank");
}
