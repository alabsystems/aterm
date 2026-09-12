// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real terminal snapshots and the public brain/frame contract: a content
//! anchor moves with its rows, finite travel settles, and reduced motion needs
//! no frame train to become visible.

use std::time::{Duration, Instant};

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetAttention, PetBrain, PetFrame, PetSense};
use aterm_effects::pet_world::{PetPane, PetRect, PetWorld, PetWorldFacts};

const ROWS: u16 = 24;
const COLS: u16 = 80;
const CELL_W: u16 = 8;
const CELL_H: u16 = 16;

struct Scene {
    term: Terminal,
    brain: PetBrain,
    now: Instant,
    reduced: bool,
}

impl Scene {
    fn new(reduced: bool) -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"\x1b]11;#000000\x07\x1b[12;40H");
        Self {
            term,
            brain: PetBrain::default(),
            now: Instant::now(),
            reduced,
        }
    }

    fn tick(&mut self, millis: u64) -> PetFrame {
        self.now += Duration::from_millis(millis);
        let input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let facts = PetWorldFacts::read(&self.term, 7);
        self.brain
            .observe_console(&input, &facts, PetPane::full(&input));
        self.brain.set_console_presentable(true);
        let frame = self.brain.tick(PetSense {
            now: self.now,
            caret: (input.cursor_visible && input.display_offset == 0)
                .then_some((input.cursor_row as u16, input.cursor_col as u16)),
            wrapped: false,
            rows: ROWS,
            cols: COLS,
            cell_w: CELL_W,
            cell_h: CELL_H,
            reduced_motion: self.reduced,
            output_burst: false,
            pointer: None,
        });
        // Assert the emitted, pixel-rounded footprint against the same real
        // snapshot, so a plausible internal target cannot mask clipped text.
        if let Some((x0, x1, y0, y1)) = frame.body_px(CELL_W, CELL_H, COLS, ROWS) {
            let mut world = PetWorld::default();
            assert!(world.observe(&input, &facts, PetPane::full(&input)));
            let rect = PetRect::new(
                y0 as f32 / f32::from(CELL_H),
                x0 as f32 / f32::from(CELL_W),
                (y1 - y0) as f32 / f32::from(CELL_H),
                (x1 - x0) as f32 / f32::from(CELL_W),
            );
            // THE CARET'S OWN KEEP-OFF RING IS NOT "PROTECTED PIXELS" TO ITS
            // ESCORT. `STATION_LEAD` seats the pet one cell past the caret,
            // inside that 3x3 ring by construction — that is the shipped
            // escort law and what the pet looked like at v0.76.0. Read
            // strictly this assertion says the escort may never take its own
            // station. Everything else the strict reading guards — a
            // selection, an image, a row with no certified cell-to-pixel
            // projection — still fails here, and the caret CELL itself stays
            // uncovered at rest because the station law puts the body beside
            // it, not on it.
            assert!(world.under_text_clearance_past_caret(rect, 0.0) == Some(true));
            assert!(frame.under_ink || world.clear(rect, 0.0));
        }
        frame
    }

    fn work(&mut self) {
        self.term.process(
            b"\x1b[?25l\x1b[8;10H\x1b]133;A\x07$ \x1b]133;B\x07build\r\n\x1b]133;C\x07working",
        );
    }

    fn settle(&mut self) -> PetFrame {
        let mut frame = self.tick(16);
        for _ in 0..240 {
            if !self.brain.needs_frames() {
                break;
            }
            frame = self.tick(16);
        }
        assert!(!self.brain.needs_frames(), "finite travel did not settle");
        assert!(frame.alpha > 0, "clear fixture must yield a visible pet");
        frame
    }

    fn select(&mut self, start: i32, end: i32) {
        self.term.text_selection_mut().start_selection(
            start,
            2,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        self.term
            .text_selection_mut()
            .update_selection(end, 7, SelectionSide::Right);
    }
}

#[test]
fn reduced_motion_reading_is_opaque_on_its_first_caretless_tick() {
    let mut s = Scene::new(true);
    s.term.process(b"\x1b[?25l");
    s.select(9, 9);
    let frame = s.tick(0);
    assert_eq!(s.brain.console_attention(), PetAttention::Reading);
    assert_eq!(frame.alpha, 255, "reduced motion has no fade frame train");
    assert_eq!(frame.lift, 0.0);
    assert!(!s.brain.needs_frames());
    assert_eq!(s.brain.next_change_deadline(s.now), None);
    let later = s.tick(30_000);
    assert_eq!(later.fp(), frame.fp(), "held reading must remain static");
}

#[test]
fn retained_perch_translates_once_with_real_full_screen_output_scroll() {
    let mut s = Scene::new(false);
    s.work();
    let before = s.settle();
    let anchor = s.brain.console_anchor_id().expect("real executing block");
    let stamp = PetWorldFacts::read(&s.term, 7).stamp;
    s.term.process(b"\x1b[24;1H\n");
    let next = PetWorldFacts::read(&s.term, 7).stamp;
    assert_eq!(next.surface, stamp.surface);
    assert_eq!(next.top_absolute_row, stamp.top_absolute_row + 1);
    assert_eq!(next.uniform_up_rows, stamp.uniform_up_rows + 1);
    let scrolled = s.tick(0);
    assert_eq!(s.brain.console_anchor_id(), Some(anchor));
    assert_eq!(scrolled.col, before.col);
    assert_eq!(scrolled.row, before.row - 1.0);
    assert!(scrolled.alpha > 0);
    let duplicate = s.tick(0);
    assert_eq!(
        duplicate.fp(),
        scrolled.fp(),
        "same scroll must not apply twice"
    );
    assert!(!s.brain.needs_frames());
    assert_eq!(s.brain.next_change_deadline(s.now), None);
}

#[test]
fn hidden_cursor_home_stays_put_while_real_output_scrolls() {
    let mut s = Scene::new(false);
    let mut before = s.tick(16);
    for _ in 0..30 {
        before = s.tick(100);
    }
    s.work();
    let moving = s.tick(16);
    // A new output interest directs the gaze instead of commissioning a
    // distant perch trip. Actual flight/landing ownership is separately
    // exercised by console_handoff_conformance's real local-hop fixture.
    assert!(
        !s.brain.needs_frames(),
        "output interest must not move the base"
    );
    assert_eq!((moving.col, moving.row), (before.col, before.row));
    assert!(moving.alpha > 0);
    let anchor = s.brain.console_anchor_id().expect("real executing block");
    let old_top = PetWorldFacts::read(&s.term, 7).stamp.top_absolute_row;
    s.term.process(b"\x1b[24;1H\n");
    assert_eq!(
        PetWorldFacts::read(&s.term, 7).stamp.top_absolute_row,
        old_top + 1
    );
    let scrolled = s.tick(0);
    assert_eq!(s.brain.console_anchor_id(), Some(anchor));
    assert!((scrolled.col - moving.col).abs() < 0.001);
    // The last real cursor is still home while DECTCEM hides it. Output
    // scrolls its own anchor; it cannot carry the resident away from input.
    assert!((scrolled.row - moving.row).abs() < 0.001);
    let duplicate = s.tick(0);
    assert_eq!(duplicate.fp(), scrolled.fp());
    let settled = s.settle();
    assert_eq!(s.brain.console_anchor_id(), Some(anchor));
    assert_eq!(settled.lift, 0.0);
    assert_eq!(s.brain.next_change_deadline(s.now), None);
    let later = s.tick(30_000);
    assert_eq!(later.fp(), settled.fp(), "old output cannot restart a trip");
}

#[test]
fn output_over_the_old_perch_keeps_the_full_body_and_settles_without_polling() {
    let mut s = Scene::new(false);
    s.work();
    let before = s.settle();
    let anchor = s.brain.console_anchor_id().expect("real executing block");
    let old_top = PetWorldFacts::read(&s.term, 7).stamp.top_absolute_row;
    // The old body lived beside a short output line. This real output burst
    // both scrolls its content away and fills its old columns with new text,
    // while leaving a wide, certified gutter beside the current output.
    for _ in 0..30 {
        s.term.process(b"\r\nterrain observation line");
    }
    assert!(PetWorldFacts::read(&s.term, 7).stamp.top_absolute_row > old_top);
    let occluded = s.tick(16);
    assert_eq!(s.brain.console_anchor_id(), Some(anchor));
    assert!(
        occluded.alpha > 0,
        "ordinary output must not make the body blink out"
    );
    let recovered = s.settle();
    let input = s.term.cell_frame(usize::from(ROWS), usize::from(COLS));
    let facts = PetWorldFacts::read(&s.term, 7);
    let mut world = PetWorld::default();
    assert!(world.observe(&input, &facts, PetPane::full(&input)));
    let (x0, x1, y0, y1) = before.body_px(CELL_W, CELL_H, COLS, ROWS).unwrap();
    let old_body = PetRect::new(
        y0 as f32 / f32::from(CELL_H),
        x0 as f32 / f32::from(CELL_W),
        (y1 - y0) as f32 / f32::from(CELL_H),
        (x1 - x0) as f32 / f32::from(CELL_W),
    );
    assert!(
        !world.clear(old_body, 0.0),
        "fixture must actually draw output over the old full-body footprint"
    );
    assert_eq!(s.brain.console_anchor_id(), Some(anchor));
    assert_eq!(s.brain.next_change_deadline(s.now), None);
    let later = s.tick(30_000);
    assert_eq!(later.fp(), recovered.fp(), "re-entry is finite");
}

#[test]
fn fully_occupied_output_keeps_the_resident_under_text_without_polling() {
    let mut s = Scene::new(false);
    s.work();
    s.settle();
    s.term.process(b"\x1b[H");
    s.term
        .process(&vec![b'X'; usize::from(ROWS) * usize::from(COLS)]);
    let resident = s.settle();
    assert!(resident.alpha > 0);
    assert!(
        resident.under_ink,
        "ordinary output draws in front of the pet"
    );
    assert_ne!(s.brain.console_attention(), PetAttention::Yielding);
    assert!(!s.brain.needs_frames());
    assert_eq!(s.brain.next_change_deadline(s.now), None);
    let later = s.tick(30_000);
    assert_eq!(later.fp(), resident.fp());
    assert!(!s.brain.needs_frames());
    assert_eq!(s.brain.next_change_deadline(s.now), None);
}
