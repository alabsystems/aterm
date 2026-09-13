// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real erase sequences cross the terminal snapshot / pet boundary. Rewriting
//! cells refreshes occupancy; discarding history does not move live coordinates;
//! RIS explicitly retires their owner. The harness supplies input intent only
//! through the public host entry point, never by interpreting PTY bytes.

use std::time::{Duration, Instant};

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{Rgb, Terminal};
use aterm_effects::kitty_pet::{PetAttention, PetBrain, PetFrame, PetInputKind, PetSense};
use aterm_effects::pet_world::{PetCell, PetPane, PetRect, PetWorld, PetWorldFacts};

const ROWS: u16 = 24;
const COLS: u16 = 80;
const CELL_W: u16 = 8;
const CELL_H: u16 = 16;
const SESSION: u64 = 7;

struct Scene {
    term: Terminal,
    brain: PetBrain,
    now: Instant,
}

impl Scene {
    fn new(history: bool) -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        // This is host configuration, so RIS restores the same authoritative
        // background instead of making the reset snapshot COLOR_UNSET.
        term.set_default_background(Rgb { r: 0, g: 0, b: 0 });
        if history {
            for _ in 0..40 {
                term.process(b"history\r\n");
            }
            assert!(term.grid().scrollback_lines() > 0);
        }
        term.process(b"\x1b[12;40H");
        Self {
            term,
            brain: PetBrain::default(),
            now: Instant::now(),
        }
    }

    fn tick(&mut self, millis: u64) -> PetFrame {
        self.now += Duration::from_millis(millis);
        let input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let facts = PetWorldFacts::read(&self.term, SESSION);
        self.brain
            .observe_console(&input, &facts, PetPane::full(&input));
        self.brain.set_console_presentable(true);
        self.brain.tick(PetSense {
            now: self.now,
            caret: (input.cursor_visible && input.display_offset == 0)
                .then_some((input.cursor_row as u16, input.cursor_col as u16)),
            rows: ROWS,
            cols: COLS,
            cell_w: CELL_W,
            cell_h: CELL_H,
            wrapped: false,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        })
    }

    fn world(&mut self) -> PetWorld {
        let input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let facts = PetWorldFacts::read(&self.term, SESSION);
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        world
    }

    fn warm(&mut self) -> PetFrame {
        let mut frame = self.tick(0);
        for _ in 0..30 {
            frame = self.tick(100);
        }
        assert!(frame.alpha > 0, "fixture has a real presented resident");
        frame
    }

    fn work(&mut self) {
        self.term.process(
            b"\x1b[?25l\x1b[8;10H\x1b]133;A\x07$ \x1b]133;B\x07build\r\n\x1b]133;C\x07working",
        );
    }

    fn select(&mut self, row: usize, col: usize) {
        let selection = self.term.text_selection_mut();
        selection.start_selection(
            row as i32,
            col as u16,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        selection.update_selection(row as i32, col as u16, SelectionSide::Right);
        selection.complete_selection();
        assert!(self.term.text_selection().has_selection());
    }

    fn write_at(&mut self, row: usize, col: usize, ch: char) {
        // Preserve the visible caret: this is a program's stationary overwrite,
        // not a second input gesture or a cursor-motion contact witness.
        let cursor = self.term.cursor();
        let (caret_row, caret_col) = (cursor.row, cursor.col);
        self.term.process(
            format!(
                "\x1b[{};{}H{ch}\x1b[{};{}H",
                row + 1,
                col + 1,
                caret_row + 1,
                caret_col + 1
            )
            .as_bytes(),
        );
    }
}

fn body(frame: PetFrame) -> PetRect {
    let (x0, x1, y0, y1) = frame
        .body_px(CELL_W, CELL_H, COLS, ROWS)
        .expect("fixture must have a displayed body");
    PetRect::new(
        y0 as f32 / f32::from(CELL_H),
        x0 as f32 / f32::from(CELL_W),
        (y1 - y0) as f32 / f32::from(CELL_H),
        (x1 - x0) as f32 / f32::from(CELL_W),
    )
}

#[test]
fn ed2_clears_selected_occupancy_without_retiring_live_coordinates_or_input() {
    let mut s = Scene::new(false);
    s.work();
    s.warm();
    let old = s.world();
    let anchor = *old.anchors().iter().flatten().next().expect("OSC block");
    assert_eq!(s.brain.console_anchor_id(), Some(anchor.block_id));
    s.select(8, 2);
    s.brain.note_console_input(s.now, PetInputKind::Text);
    s.tick(0);
    assert_eq!(s.brain.console_attention(), PetAttention::Reading);
    let selected = s.world();
    assert_eq!(selected.cell(8, 2), PetCell::Protected);
    assert_eq!(selected.ink_at(8, 2), Some(true));
    assert!(selected.selection_target().is_some());
    let stamp = selected.stamp().unwrap();
    let input_seq = s.brain.console_input_seq();

    s.term.process(b"\x1b[2J");
    s.tick(0);
    let erased = s.world();
    let after = erased.stamp().unwrap();
    assert_eq!(after.surface, stamp.surface);
    assert_eq!(after.top_absolute_row, stamp.top_absolute_row);
    assert_eq!(after.uniform_up_rows, stamp.uniform_up_rows);
    assert_ne!(after.content_seq, stamp.content_seq);
    assert_eq!(erased.ink_at(8, 2), Some(false));
    assert_eq!(erased.cell(8, 2), PetCell::Clear);
    assert_eq!(erased.selection_target(), None);
    assert_eq!(erased.selection_rect(), None);
    assert!(!s.term.text_selection().has_selection());
    assert!(
        erased.resolve(anchor).is_some(),
        "the OSC block still exists"
    );
    assert_eq!(s.brain.console_input_kind(), Some(PetInputKind::Text));
    assert_eq!(s.brain.console_input_seq(), input_seq);
    assert_eq!(s.brain.console_attention(), PetAttention::Typing);
}

#[test]
fn ed3_discards_history_and_block_metadata_without_relocating_live_cells_or_pet() {
    let mut s = Scene::new(true);
    s.work();
    let before = s.warm();
    let old = s.world();
    let anchor = *old.anchors().iter().flatten().next().expect("OSC block");
    let stamp = old.stamp().unwrap();
    assert_eq!(s.brain.console_anchor_id(), Some(anchor.block_id));
    assert_eq!(old.ink_at(8, 2), Some(true));
    assert!(s.term.grid().scrollback_lines() > 0);

    s.term.process(b"\x1b[3J");
    let after = s.tick(0);
    let world = s.world();
    let next = world.stamp().unwrap();
    assert_eq!(s.term.grid().scrollback_lines(), 0);
    assert_eq!(next.surface, stamp.surface);
    assert_eq!(next.top_absolute_row, stamp.top_absolute_row);
    assert_eq!(next.uniform_up_rows, stamp.uniform_up_rows);
    assert_ne!(next.content_seq, stamp.content_seq);
    assert_eq!(world.ink_at(8, 2), Some(true));
    // The terminal's ED3 handler separately clears shell integration metadata
    // (handler_csi, #7667). Stable live coordinates cannot authorize a phantom
    // block after that explicit retirement; pixels and semantic ownership are
    // independent observations.
    assert!(s.term.all_blocks().next().is_none());
    assert!(world.anchors().iter().all(Option::is_none));
    assert!(world.resolve(anchor).is_none());
    assert_eq!(s.brain.console_anchor_id(), None);
    // Losing work interest may change the pose and its breathing scale. The
    // actual location must stay put; identical art bounds would prohibit that
    // legitimate posture change rather than detect a coordinate relocation.
    assert_eq!(
        (after.col, after.row, after.lift),
        (before.col, before.row, before.lift),
        "history removal does not relocate the pet"
    );
    assert!(world.under_text_clear(body(after), 0.0));
    assert!(after.under_ink || world.clear(body(after), 0.0));
    assert_ne!(s.brain.console_attention(), PetAttention::Contact);
}

#[test]
fn erases_and_selection_release_cannot_replay_old_ink_as_contact() {
    for (erase, removes_live_ink) in [(b"\x1b[2J".as_slice(), true), (b"\x1b[3J", false)] {
        let mut s = Scene::new(true);
        // The reading perch is the escort's own stand, inside the caret's
        // keep-off ring by construction (`pet_escort_primacy`), so beside a
        // VISIBLE caret the halo is always within the contact margin of the
        // body. Hide the caret after seating — as `work()` does for the other
        // scenes here — and move the selection: the body holds its spot at its
        // last real home with clear ground on every side, so what follows
        // measures ink contact rather than cursor proximity.
        s.select(9, 12);
        s.warm();
        s.term.process(b"\x1b[?25l");
        s.select(3, 70);
        let held = body(s.tick(0));
        // The natural sprite top has a fractional gap above it. This row is
        // outside the clear body margin but inside its contact margin, so the
        // later positive control exercises actual near-body contact geometry.
        let row = held.row.floor() as usize - 1;
        let col = (held.col + held.cols * 0.5).floor() as usize;
        assert!(s.world().clear(held, 0.45));
        s.write_at(row, col, 'X');
        s.tick(0);
        let ink = s.world();
        assert!(ink.clear(held, 0.10));
        assert!(!ink.clear(held, 0.45));
        s.select(row, col);
        assert_eq!(body(s.tick(0)), held);
        assert_eq!(s.brain.console_attention(), PetAttention::Reading);
        assert_eq!(s.world().cell(row, col), PetCell::Protected);

        // Explicit input is live, but the erase bytes themselves are not input.
        s.brain.note_console_input(s.now, PetInputKind::Text);
        let seq = s.brain.console_input_seq();
        s.term.process(erase);
        s.tick(0);
        let erased = s.world();
        assert!(!s.term.text_selection().has_selection());
        assert_eq!(erased.selection_target(), None);
        assert_eq!(erased.ink_at(row, col), Some(!removes_live_ink));
        assert_eq!(s.brain.console_input_kind(), Some(PetInputKind::Text));
        assert_eq!(s.brain.console_attention(), PetAttention::Typing);
        assert_eq!(s.brain.console_input_seq(), seq);

        // Positive control: genuinely NEW displayed ink at the same body edge
        // must react under that same input episode. ED3 preserved the old X,
        // so use the adjacent, previously blank cell instead of repainting it.
        let new_col = if removes_live_ink { col } else { col + 1 };
        assert_eq!(s.world().ink_at(row, new_col), Some(false));
        s.write_at(row, new_col, 'Y');
        s.tick(0);
        assert_eq!(s.brain.console_attention(), PetAttention::Contact);
        assert_eq!(s.brain.console_reason(), "advancing-ink");
        assert_eq!(s.brain.console_input_seq(), seq);
    }
}

#[test]
fn ris_retires_the_previous_surface_anchor_and_pending_input() {
    let mut s = Scene::new(true);
    s.work();
    s.warm();
    let old = s.world();
    let anchor = *old.anchors().iter().flatten().next().expect("OSC block");
    let stamp = old.stamp().unwrap();
    assert_eq!(s.brain.console_anchor_id(), Some(anchor.block_id));
    s.select(8, 2);
    s.brain.note_console_input(s.now, PetInputKind::Text);
    assert_eq!(s.brain.console_input_kind(), Some(PetInputKind::Text));

    s.term.process(b"\x1bc");
    s.tick(0);
    let reset = s.world();
    let after = reset.stamp().unwrap();
    assert_ne!(after.surface, stamp.surface);
    assert_eq!(
        after.surface.scroll_invalidation,
        stamp.surface.scroll_invalidation + 1,
        "RIS owns one explicit coordinate retirement"
    );
    assert_eq!(after.surface.terminal_id, stamp.surface.terminal_id);
    assert!(reset.resolve(anchor).is_none());
    assert!(reset.anchors().iter().all(Option::is_none));
    assert_eq!(reset.ink_at(8, 2), Some(false));
    assert_eq!(reset.selection_target(), None);
    assert!(!s.term.text_selection().has_selection());
    assert_eq!(s.brain.console_anchor_id(), None);
    assert_eq!(s.brain.console_input_kind(), None);
    assert_eq!(s.brain.console_event_seq(), 0);
    assert_eq!(
        s.brain.console_input_consumed_seq(),
        s.brain.console_input_seq()
    );
    assert_ne!(s.brain.console_attention(), PetAttention::Contact);
}
