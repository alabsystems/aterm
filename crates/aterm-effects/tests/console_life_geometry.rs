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
            assert!(world.clear(
                PetRect::new(
                    y0 as f32 / f32::from(CELL_H),
                    x0 as f32 / f32::from(CELL_W),
                    (y1 - y0) as f32 / f32::from(CELL_H),
                    (x1 - x0) as f32 / f32::from(CELL_W),
                ),
                0.0,
            ));
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
fn in_flight_perch_translation_preserves_progress_and_finishes() {
    let mut s = Scene::new(false);
    for _ in 0..30 {
        s.tick(100);
    }
    s.work();
    let moving = s.tick(16);
    assert!(
        s.brain.needs_frames(),
        "fixture must exercise actual transit"
    );
    assert!(moving.alpha > 0);
    let anchor = s.brain.console_anchor_id().expect("real executing block");
    s.term.process(b"\x1b[24;1H\n");
    let scrolled = s.tick(0);
    assert_eq!(s.brain.console_anchor_id(), Some(anchor));
    assert!((scrolled.col - moving.col).abs() < 0.001);
    assert!((scrolled.row - (moving.row - 1.0)).abs() < 0.001);
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
fn output_that_occludes_the_old_perch_recovers_without_unrelated_redraws() {
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
    assert_eq!(occluded.alpha, 0, "an occluded body cannot jump opaquely");
    let deadline = s.brain.next_change_deadline(s.now);
    assert!(
        s.brain.needs_frames() || deadline.is_some(),
        "a known clear destination must have a finite re-entry wake"
    );
    if !s.brain.needs_frames() {
        let until = deadline.unwrap().saturating_duration_since(s.now);
        s.tick(until.as_millis() as u64 + 1);
    }
    let recovered = s.settle();
    assert!(
        (recovered.col - before.col).abs() > 1.0,
        "fixture must exercise a new clear output-side placement"
    );
    assert_eq!(s.brain.console_anchor_id(), Some(anchor));
    assert_eq!(s.brain.next_change_deadline(s.now), None);
    let later = s.tick(30_000);
    assert_eq!(later.fp(), recovered.fp(), "re-entry is finite");
}

#[test]
fn fully_occupied_output_never_reenters_or_polls_for_a_perch() {
    let mut s = Scene::new(false);
    s.work();
    s.settle();
    s.term.process(b"\x1b[H");
    s.term
        .process(&vec![b'X'; usize::from(ROWS) * usize::from(COLS)]);
    let hidden = s.tick(16);
    assert_eq!(hidden.alpha, 0);
    assert_eq!(s.brain.console_attention(), PetAttention::Yielding);
    assert!(!s.brain.needs_frames());
    assert_eq!(s.brain.next_change_deadline(s.now), None);
    let later = s.tick(30_000);
    assert_eq!(later.alpha, 0);
    assert!(!s.brain.needs_frames());
    assert_eq!(s.brain.next_change_deadline(s.now), None);
}
