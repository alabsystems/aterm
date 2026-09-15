// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fixed composer caret surrounded by sparse animated program glyphs.
//! The sampled Codex scene used braille particles throughout the three input
//! rows. They are ordinary terminal content, independent of aterm's effects.

use std::time::{Duration, Instant};

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetAction, PetBrain, PetFrame, PetInputKind, PetSense, art_cols};
use aterm_effects::pet_world::{PetCell, PetPane, PetRect, PetWorld, PetWorldFacts};
use aterm_effects::word_decorations::{DecoConfig, WordDecorations};
use aterm_lexicon::Lexicon;

const ROWS: u16 = 58;
const COLS: u16 = 116;

struct Scene {
    term: Terminal,
    brain: PetBrain,
    decos: WordDecorations,
    lexicon: Lexicon,
    cfg: DecoConfig,
    now: Instant,
    rows: u16,
    cols: u16,
}

impl Scene {
    fn new() -> Self {
        Self::sized(ROWS, COLS)
    }

    fn sized(rows: u16, cols: u16) -> Self {
        let mut term = Terminal::new(rows, cols);
        term.process(
            format!(
                "\x1b]11;#111318\x07\x1b[{};1HWorking\x1b[{};3H",
                rows - 5,
                rows - 2
            )
            .as_bytes(),
        );
        Self {
            term,
            brain: PetBrain::default(),
            decos: WordDecorations::default(),
            lexicon: Lexicon::with_languages(&["en"]),
            cfg: DecoConfig::default(),
            now: Instant::now(),
            rows,
            cols,
        }
    }

    fn particles(&mut self, near: u16) {
        // Complete synchronized batches always restore the same visible
        // caret. Only the sparse background glyphs move; no input is noted.
        let above = self.rows - 3;
        let caret_row = self.rows - 2;
        let below = self.rows - 1;
        let far_above = self.cols - 6;
        let far_caret = self.cols - 19;
        let far_below = self.cols - 4;
        let bytes = format!(
            "\x1b[?2026h\x1b[48;2;45;47;51m\x1b[{above};1H\x1b[2K\x1b[{above};{near}H⠁\x1b[{above};{far_above}H⡀\
             \x1b[{caret_row};1H\x1b[2K> Compose a message\x1b[{caret_row};{far_caret}H⠂\
             \x1b[{below};1H\x1b[2K\x1b[{below};{near}H⠄\x1b[{below};{far_below}H⠈\
             \x1b[{caret_row};3H\x1b[?2026l"
        );
        self.term.process(bytes.as_bytes());
        assert_eq!(
            (self.term.cursor().row, self.term.cursor().col),
            (self.rows - 3, 2)
        );
    }

    fn tick(&mut self) -> PetFrame {
        self.now += Duration::from_millis(16);
        let input = self
            .term
            .cell_frame(usize::from(self.rows), usize::from(self.cols));
        let facts = PetWorldFacts::read(&self.term, 7);
        self.brain
            .observe_console(&input, &facts, PetPane::full(&input));
        self.brain.set_console_presentable(true);
        // This is the shipping scanner, not a test-side approximation of its
        // ink spans. As on glass, the brain reads the last completed scan.
        let (spans, live) = self.decos.pet_ink();
        self.brain.sense_ink(0, spans, live);
        let sense = PetSense {
            now: self.now,
            caret: input
                .cursor_visible
                .then_some((input.cursor_row as u16, input.cursor_col as u16)),
            wrapped: false,
            rows: self.rows,
            cols: self.cols,
            cell_w: 8,
            cell_h: 16,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        };
        let frame = self.brain.tick(sense);
        self.decos.rescan_from_cells(
            &input.cells,
            &input.line_sizes,
            usize::from(self.rows),
            usize::from(self.cols),
            &self.lexicon,
            &self.cfg,
            input.content_seq,
            self.now,
        );
        frame
    }
}

#[test]
fn nearby_output_particles_allow_uninterrupted_sleep_after_the_idle_ladder() {
    for mode in ["far-output", "near-particles"] {
        let mut s = Scene::new();
        s.particles(33);
        for _ in 0..60 {
            s.tick();
        }
        s.brain.note_console_input(s.now, PetInputKind::Text);
        let mut sleeps = 0;
        let mut wakes = 0;
        let mut airborne = 0;
        let mut movement = 0;
        let mut frames = 0;
        let mut min_x = f32::MAX;
        let mut max_x = f32::MIN;
        let mut prior = None;
        let mut actions = std::collections::BTreeMap::new();
        for tick in 0..2813 {
            if tick % 32 == 0 {
                match mode {
                    "near-particles" => s.particles([2, 5, 33][tick / 32 % 3]),
                    _ => {
                        let glyph = if tick % 64 == 0 { '⠁' } else { '⠂' };
                        s.term.process(
                            format!("\x1b[?2026h\x1b[54;1HWorking {glyph}\x1b[56;3H\x1b[?2026l")
                                .as_bytes(),
                        );
                    }
                }
            }
            let frame = s.tick();
            if tick >= 2188 {
                assert!(frame.alpha > 0, "{mode}: the sleeping pet stays visible");
                frames += 1;
                sleeps += usize::from(frame.action == PetAction::Sleep);
                wakes += usize::from(frame.action == PetAction::Waking);
                airborne += usize::from(frame.action.airborne());
                if let Some((col, row)) = prior {
                    movement += usize::from(frame.col != col || frame.row != row);
                }
                min_x = min_x.min(frame.col);
                max_x = max_x.max(frame.col);
                *actions.entry(s.brain.pose_name()).or_insert(0) += 1;
            }
            prior = Some((frame.col, frame.row));
        }
        eprintln!(
            "{mode}: after35s frames={frames} sleep={sleeps} waking={wakes} airborne={airborne} moved={movement} xspan={} actions={actions:?} input={} reason={}",
            max_x - min_x,
            s.brain.console_input_seq(),
            s.brain.console_reason()
        );
        assert_eq!(
            movement, 0,
            "{mode}: a quiet pet with a clear nearby seat must not chase background particles"
        );
        assert_eq!(
            sleeps, frames,
            "{mode}: background redraws must allow uninterrupted sleep"
        );
    }
}

fn sleeping_scene() -> (Scene, PetFrame) {
    let mut s = Scene::new();
    // Establish a sleeping seat after the real text has been avoided.
    s.particles(5);
    // Real text, rather than a decorative dot, establishes the local detour.
    s.term.process(b"\x1b[55;5HX\x1b[57;5HX\x1b[56;3H");
    let mut frame = s.tick();
    for _ in 0..300 {
        frame = s.tick();
    }
    assert_eq!(frame.action, PetAction::Sleep);
    assert!(!s.brain.console_legacy_motion_pending());
    (s, frame)
}

fn observed_foot_is_clear(s: &mut Scene, frame: &PetFrame) -> bool {
    let input = s.term.cell_frame(usize::from(s.rows), usize::from(s.cols));
    let facts = PetWorldFacts::read(&s.term, 7);
    let mut world = PetWorld::default();
    assert!(world.observe(&input, &facts, PetPane::full(&input)));
    // Exclude the authored sprite's transparent 0.30-cell side margins.
    let first = (frame.col + 0.30).floor() as usize;
    let end = (frame.col + art_cols(8, 16) - 0.30).ceil() as usize;
    (first..end).all(|col| world.locomotion_ink_at(frame.row.round() as usize, col) == Some(false))
}

#[test]
fn an_actual_glyph_in_the_sleeping_footprint_still_causes_eviction() {
    let (mut s, before) = sleeping_scene();
    let row = before.row.round() as u16 + 1;
    let col = before.col.ceil() as u16 + 1;
    s.term
        .process(format!("\x1b[{row};{col}HX\x1b[56;3H").as_bytes());
    assert!(!observed_foot_is_clear(&mut s, &before));
    let mut after = s.tick();
    let mut moved = (after.col, after.row) != (before.col, before.row);
    for _ in 0..240 {
        after = s.tick();
        moved |= (after.col, after.row) != (before.col, before.row);
    }
    assert!(moved, "an occupied seat cannot be retained");
    assert!(
        observed_foot_is_clear(&mut s, &after),
        "the real glyph remains protected: {after:?}"
    );
}

#[test]
fn real_navigation_releases_the_sleeping_seat_and_follows_the_new_caret() {
    let (mut s, before) = sleeping_scene();
    s.brain.note_console_input(s.now, PetInputKind::Navigate);
    s.term.process(b"\x1b[56;61H");
    let mut after = s.tick();
    for _ in 0..360 {
        after = s.tick();
    }
    assert!(
        after.alpha > 0 && (after.col - 61.0).abs() < 2.0,
        "real navigation must still reach home: {before:?} -> {after:?}"
    );
    assert_eq!(
        s.brain.console_input_consumed_seq(),
        s.brain.console_input_seq()
    );
}

#[test]
fn single_dot_particles_remain_ink_without_displacing_the_pet() {
    // Each byte sequence puts the candidate at row2,col5. A missing or
    // decorated neighbour, a compound glyph and real text remain substantive.
    for (bytes, obstacle) in [
        ("⠁", false),
        ("⠂", false),
        ("⠄", false),
        ("⠈", false),
        ("⠐", false),
        ("⠠", false),
        ("⡀", false),
        ("⢀", false),
        ("⠙", true),
        ("⣿", true),
        ("X", true),
        ("你", true),
        ("⠁\u{301}", true),
        ("\x1b[4m⠁\x1b[0m", true),
        ("\x1b[9m⠁\x1b[0m", true),
        ("⠁x", true),
        ("⠁⠂", true),
        ("\x1b[3;5Hx⠁", true),
    ] {
        let mut term = Terminal::new(6, 20);
        term.process(format!("\x1b[3;6H{bytes}\x1b[1;1H").as_bytes());
        let input = term.cell_frame(6, 20);
        let facts = PetWorldFacts::read(&term, 7);
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        assert_eq!(world.ink_at(2, 5), Some(true), "visible ink: {bytes:?}");
        assert_eq!(world.cell(2, 5), PetCell::Ink, "placement remains strict");
        assert_eq!(world.locomotion_ink_at(2, 5), Some(obstacle), "{bytes:?}");
        assert_eq!(world.locomotion_ink_at(9, 5), None, "unknown is not clear");
        let mut clipped = PetPane::full(&input);
        clipped.col = 5;
        clipped.cols = 10;
        assert!(!world.observe(&input, &facts, clipped));
        assert_eq!(
            world.locomotion_ink_at(2, 0),
            None,
            "incoherent pane geometry is unknown"
        );
    }
}

#[test]
fn single_dot_particles_preserve_protected_surfaces_and_uncertain_boundaries() {
    for col in [0, 19] {
        let mut term = Terminal::new(6, 20);
        term.process(format!("\x1b[3;{}H⠁\x1b[1;1H", col + 1).as_bytes());
        let input = term.cell_frame(6, 20);
        let facts = PetWorldFacts::read(&term, 7);
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        assert_eq!(world.ink_at(2, col), Some(true));
        assert_eq!(
            world.locomotion_ink_at(2, col),
            Some(true),
            "neighbour outside pane is unknown"
        );
    }
    for selection in [false, true] {
        let mut term = Terminal::new(6, 20);
        term.process("\x1b[3;6H⠁\x1b[1;1H".as_bytes());
        if selection {
            term.text_selection_mut().start_selection(
                2,
                5,
                SelectionSide::Left,
                SelectionType::Simple,
            );
            term.text_selection_mut()
                .update_selection(2, 5, SelectionSide::Right);
        }
        let input = term.cell_frame(6, 20);
        let facts = PetWorldFacts::read(&term, 7);
        let mut world = PetWorld::default();
        let rect = PetRect::new(2.0, 5.0, 1.0, 1.0);
        let excluded = [rect];
        assert!(world.observe_with_exclusions(
            &input,
            &facts,
            PetPane::full(&input),
            if selection { &[] } else { &excluded }
        ));
        assert_eq!(world.ink_at(2, 5), Some(true));
        assert_eq!(world.locomotion_ink_at(2, 5), Some(false));
        assert_eq!(world.cell(2, 5), PetCell::Protected);
        assert_eq!(
            world.under_text_clearance_past_caret(rect, 0.0),
            Some(false)
        );
    }
}
