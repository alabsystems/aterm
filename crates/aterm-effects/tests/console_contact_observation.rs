// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Contact is newly observed local ink during a recent input episode. A host
//! may observe more than once before ticking; scrolling old ink is not a new
//! glyph, and neither duplicate observations nor late input can replay it.

use std::time::{Duration, Instant};

use aterm_core::render::RenderInput;
use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetAttention, PetBrain, PetFrame, PetInputKind, PetSense};
use aterm_effects::pet_world::{PetPane, PetWorld, PetWorldFacts};

const ROWS: u16 = 20;
const COLS: u16 = 80;
const CELL_W: u16 = 10;
const CELL_H: u16 = 20;

struct Scene {
    term: Terminal,
    brain: PetBrain,
    now: Instant,
}

impl Scene {
    fn new() -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        // Configure real blank colors, retain one blank history row for the
        // viewport control, and keep the real caret at row 5, column 16.
        term.process(b"\x1b]11;#000000\x07\x1b[20;1H\n\x1b[6;17H");
        let mut scene = Self {
            term,
            brain: PetBrain::default(),
            now: Instant::now(),
        };
        for _ in 0..20 {
            scene.frame(100);
        }
        assert!(scene.frame(0).alpha > 0, "fixture has a real drawn body");
        scene
    }

    fn snapshot(&mut self) -> (RenderInput, PetWorldFacts) {
        let input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let facts = PetWorldFacts::read(&self.term, 7);
        (input, facts)
    }

    fn observe(&mut self) {
        let (input, facts) = self.snapshot();
        self.brain
            .observe_console(&input, &facts, PetPane::full(&input));
        self.brain.set_console_presentable(true);
    }

    fn tick(&mut self, millis: u64) -> PetFrame {
        self.now += Duration::from_millis(millis);
        let cursor = self.term.cursor();
        self.brain.tick(PetSense {
            now: self.now,
            caret: (self.term.cursor_visible() && self.term.grid().display_offset() == 0)
                .then_some((cursor.row, cursor.col)),
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

    fn frame(&mut self, millis: u64) -> PetFrame {
        self.observe();
        self.tick(millis)
    }

    fn text_intent(&mut self) {
        self.brain.note_console_input(self.now, PetInputKind::Text);
    }

    fn near_cell(&mut self) -> (u16, u16) {
        let frame = self.frame(0);
        let (x0, _, y0, y1) = frame
            .body_px(CELL_W, CELL_H, COLS, ROWS)
            .expect("derive contact geometry from the emitted body");
        // Adjacent to the left edge, at the body's middle row. This is
        // observed terminal ink, never a fabricated application edit witness.
        let col = u16::try_from(x0 / i32::from(CELL_W)).unwrap() - 1;
        let row = u16::try_from((y0 + y1) / (2 * i32::from(CELL_H))).unwrap();
        assert_eq!(self.ink_at(row, col), Some(false));
        (row, col)
    }

    fn put_ink(&mut self, row: u16, col: u16) {
        self.term
            .process(format!("\x1b[{};{}HX\x1b[6;17H", row + 1, col + 1).as_bytes());
    }

    fn unrelated_output(&mut self) {
        self.term.process(b"\x1b[17;50Hother output\x1b[6;17H");
    }

    fn ink_at(&mut self, row: u16, col: u16) -> Option<bool> {
        let (input, facts) = self.snapshot();
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        world.ink_at(usize::from(row), usize::from(col))
    }

    fn assert_contact(&self) {
        assert_eq!(self.brain.console_attention(), PetAttention::Contact);
        assert_eq!(self.brain.console_reason(), "advancing-ink");
        assert_eq!(self.brain.console_input_seq(), 1);
        assert_eq!(self.brain.console_input_consumed_seq(), 1);
    }
}

#[test]
fn newly_drawn_local_ink_is_contact_without_an_attributed_edit_witness() {
    let mut s = Scene::new();
    let (row, col) = s.near_cell();
    s.text_intent();
    s.put_ink(row, col);
    assert_eq!(s.ink_at(row, col), Some(true));
    let frame = s.frame(16);
    s.assert_contact();
    assert!(
        frame.alpha > 0,
        "the positive control is actually presented"
    );
    assert!(matches!(
        s.brain.pose_name(),
        "pet_contact_brace" | "pet_tail_tuck"
    ));
}

#[test]
fn repeated_observations_preserve_one_pending_local_contact() {
    for unrelated_second_observation in [false, true] {
        let mut s = Scene::new();
        let (row, col) = s.near_cell();
        s.text_intent();
        s.put_ink(row, col);
        s.observe();
        if unrelated_second_observation {
            s.unrelated_output();
        }
        s.observe();
        s.observe();
        s.tick(16);
        s.assert_contact();
    }
}

#[test]
fn duplicate_frames_do_not_extend_the_contact_hold() {
    let mut s = Scene::new();
    let (row, col) = s.near_cell();
    s.text_intent();
    s.put_ink(row, col);
    s.frame(16);
    s.assert_contact();
    s.frame(100);
    s.frame(100);
    s.frame(210);
    assert_ne!(s.brain.console_attention(), PetAttention::Contact);
    assert_eq!(s.brain.console_input_seq(), 1, "echo never adds an input");
}

#[test]
fn repeated_unchanged_observations_cannot_create_contact_from_input_alone() {
    let mut s = Scene::new();
    s.near_cell();
    s.text_intent();
    s.observe();
    s.observe();
    s.tick(16);
    assert_eq!(s.brain.console_attention(), PetAttention::Typing);
}

#[test]
fn uniform_output_scroll_moves_old_ink_without_creating_contact() {
    let mut s = Scene::new();
    let (row, col) = s.near_cell();
    // This glyph already exists before the input episode. A real output
    // scroll will move it up into the adjacent contact cell.
    s.put_ink(row + 1, col);
    s.frame(0);
    assert_eq!(s.ink_at(row, col), Some(false));
    assert_eq!(s.ink_at(row + 1, col), Some(true));
    let before = PetWorldFacts::read(&s.term, 7).stamp;
    s.text_intent();
    s.term.process(b"\x1b[20;1H\n\x1b[6;17H");
    let after = PetWorldFacts::read(&s.term, 7).stamp;
    assert_eq!(
        after.surface, before.surface,
        "owner identity did not change"
    );
    assert_eq!(after.top_absolute_row, before.top_absolute_row + 1);
    assert_eq!(after.uniform_up_rows, before.uniform_up_rows + 1);
    assert_eq!(after.display_offset, before.display_offset);
    assert_eq!(s.ink_at(row, col), Some(true), "old ink entered the mask");
    s.frame(16);
    assert_ne!(s.brain.console_attention(), PetAttention::Contact);
    assert_eq!(s.brain.console_input_consumed_seq(), 1);
}

#[test]
fn a_viewport_round_trip_discards_unconsumed_contact() {
    let mut s = Scene::new();
    let (row, col) = s.near_cell();
    s.text_intent();
    s.put_ink(row, col);
    s.observe();
    let before = PetWorldFacts::read(&s.term, 7).stamp;
    s.term.scroll_display(1);
    let history = PetWorldFacts::read(&s.term, 7).stamp;
    assert_eq!(history.surface, before.surface);
    assert_ne!(history.display_offset, before.display_offset);
    s.observe();
    s.term.scroll_display(-1);
    assert_eq!(PetWorldFacts::read(&s.term, 7).stamp, before);
    s.frame(16);
    assert_ne!(s.brain.console_attention(), PetAttention::Contact);
    assert_eq!(s.brain.console_input_consumed_seq(), 1);
}

#[test]
fn an_incoherent_observation_discards_unconsumed_contact() {
    let mut s = Scene::new();
    let (row, col) = s.near_cell();
    s.text_intent();
    s.put_ink(row, col);
    s.observe();
    let (stale, old_facts) = s.snapshot();
    s.unrelated_output();
    let current = PetWorldFacts::read(&s.term, 7);
    assert_ne!(current.stamp.content_seq, old_facts.stamp.content_seq);
    s.brain
        .observe_console(&stale, &current, PetPane::full(&stale));
    s.frame(16);
    assert_ne!(s.brain.console_attention(), PetAttention::Contact);
    assert_eq!(s.brain.console_input_consumed_seq(), 1);
}

#[test]
fn observed_ink_is_not_saved_for_a_later_input_episode() {
    let mut s = Scene::new();
    let (row, col) = s.near_cell();
    s.put_ink(row, col);
    s.frame(0);
    assert_ne!(s.brain.console_attention(), PetAttention::Contact);
    s.text_intent();
    // A fresh content sequence cannot make the already consumed local ink
    // belong to this later key. Only the remote corner changes now.
    s.unrelated_output();
    s.frame(16);
    assert_ne!(s.brain.console_attention(), PetAttention::Contact);
    assert_eq!(s.brain.console_input_consumed_seq(), 1);
}
