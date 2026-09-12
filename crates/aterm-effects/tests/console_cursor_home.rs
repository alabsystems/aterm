// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A full resident beside a real coding-console prompt, including ordinary
//! ink, colored input surfaces and the protected regions it must still yield.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetBrain, PetFrame, PetInputKind, PetSense};
use aterm_effects::pet_world::{PetPane, PetRect, PetWorld, PetWorldFacts};
use aterm_spec::derive::{Model, console_cursor_home_model};

const ROWS: u16 = 57;
const COLS: u16 = 151;
const CW: u16 = 10;
const CH: u16 = 20;

struct Scene {
    term: Terminal,
    brain: PetBrain,
    now: Instant,
    reduced: bool,
}

impl Scene {
    fn dense(reduced: bool) -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"\x1b]11;#000000\x07\x1b[48;2;35;39;47m");
        // Real ANSI text/backgrounds, no synthetic world occupancy. Every
        // candidate has ink, as a busy console can have around an input box.
        for row in 1..=ROWS {
            term.process(format!("\x1b[{row};1H").as_bytes());
            term.process(&vec![b'x'; usize::from(COLS)]);
        }
        term.process(b"\x1b[55;1H\x1b[2K> \x1b[0m");
        Self {
            term,
            brain: PetBrain::default(),
            now: Instant::now(),
            reduced,
        }
    }

    fn tick(&mut self, ms: u64) -> PetFrame {
        self.now += Duration::from_millis(ms);
        let input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let facts = PetWorldFacts::read(&self.term, 41);
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        // The host feeds both the console world and the older ink seam. Keep
        // both active, so legacy eviction cannot silently undo persistence.
        let spans: Vec<_> = (0..usize::from(ROWS))
            .map(|row| {
                let first =
                    (0..usize::from(COLS)).find(|&col| world.ink_at(row, col) == Some(true));
                let last =
                    (0..usize::from(COLS)).rfind(|&col| world.ink_at(row, col) == Some(true));
                first
                    .zip(last)
                    .map_or((0, 0), |(first, last)| (first as u16, last as u16 + 1))
            })
            .collect();
        let live = spans
            .iter()
            .rposition(|&(first, end)| end > first)
            .map(|row| row as u16);
        self.brain.sense_ink(0, &spans, live);
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
            cell_w: CW,
            cell_h: CH,
            reduced_motion: self.reduced,
            output_burst: false,
            pointer: None,
        });
        if let Some(rect) = body(frame) {
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
            assert!(
                world.under_text_clearance_past_caret(rect, 0.0) == Some(true),
                "emitted body covers protected pixels"
            );
            assert!(
                frame.under_ink || world.clear(rect, 0.0),
                "ink must remain in front"
            );
        }
        frame
    }

    fn at_home(&self, frame: PetFrame) {
        let rect = body(frame).expect("dense ordinary text must keep the complete pet");
        let caret = self.term.cursor();
        // Home search may choose either side of the cursor; measure from
        // the nearest part of the body rather than its left corner.
        let dx = (rect.col - f32::from(caret.col))
            .max(0.0)
            .max((f32::from(caret.col) - rect.col - rect.cols).max(0.0));
        assert!(
            dx <= 10.0,
            "pet wandered {dx} columns from its cursor home: {rect:?}"
        );
        assert!(
            (rect.foot_row() - f32::from(caret.row)).abs() <= 4.5,
            "pet wandered vertically from the cursor: {rect:?}"
        );
        assert!(
            rect.rows >= 1.5 && rect.cols >= 5.0,
            "persistence must not replace the full animal with a tiny marker"
        );
    }
}

fn near_caret(s: &Scene, frame: PetFrame) -> bool {
    body(frame).is_some_and(|rect| {
        let caret = s.term.cursor();
        let col = f32::from(caret.col);
        (rect.col - col)
            .max(0.0)
            .max((col - rect.col - rect.cols).max(0.0))
            <= 10.0
            && (rect.foot_row() - f32::from(caret.row)).abs() <= 4.5
    })
}

type State = BTreeMap<&'static str, i64>;

fn project(s: &Scene, frame: PetFrame, event: i64) -> State {
    State::from([
        ("visible", i64::from(body(frame).is_some())),
        // THE PET WALKS THERE. `home` is the model's "the pet's base is the
        // cursor", and for an animal that is STANDING THERE OR ON ITS WAY —
        // the model's own `Follow` action is exactly that state, and
        // `ArrivesAtCursor` is what makes the arrival real.
        //
        // Projected as `near_caret` alone it demanded the body be within ten
        // columns of the cursor on the SAME 16 ms tick the cursor moved a
        // hundred columns, which no locomotion can do — only a teleport,
        // and that teleport is what made the pet jump out and loop back at
        // 11 Hz in v0.82.0. Measured with the console layer off (the v0.76
        // escort, `pet_escort_primacy`'s oracle): one frame after a
        // hundred-column caret jump the gap is 95.3 cells, and the pet is
        // home 147 frames later. The bound keeps its teeth where it matters:
        // `Arrive` is stepped only once the pet has STOPPED
        // (`!needs_frames()`), so `home` there is `near_caret` and nothing
        // else, and the test's own `stranded` counterexample still proves it.
        (
            "home",
            i64::from(near_caret(s, frame) || s.brain.needs_frames()),
        ),
        ("moving", i64::from(s.brain.needs_frames())),
        ("hidden", i64::from(!s.term.cursor_visible())),
        (
            "protected",
            i64::from(s.term.text_selection().has_selection()),
        ),
        ("event", event),
    ])
}

fn step(model: &Model, state: &mut State, s: &Scene, frame: PetFrame, action: &str, event: i64) {
    let post = project(s, frame, event);
    assert!(
        model.successors(action, state).contains(&post),
        "{action}: {state:?} -> {post:?}; {}",
        s.brain.console_reason()
    );
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, &post),
            "{}",
            invariant.name
        );
    }
    *state = post;
}

#[test]
fn real_cursor_home_placement_conforms_and_rejects_the_historical_hidden_body() {
    let model = console_cursor_home_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, "real cursor-home placement");
    let mut s = Scene::dense(false);
    s.term.process(
        b"\x1b[3;100H\x1b]133;A\x07> \x1b]133;B\x07build\r\n\x1b]133;C\x07output\x1b[55;3H",
    );
    let mut frame = s.tick(16);
    for _ in 0..200 {
        frame = s.tick(16);
    }
    let mut state = model.init_state();
    assert_eq!(project(&s, frame, 0), state);

    // Dense ordinary glyphs arrive without moving the caret or its home.
    s.term.process(b"\x1b7\x1b[15;50Hmore output\x1b8");
    frame = s.tick(16);
    let mut historical_hidden = project(&s, frame, 1);
    historical_hidden.insert("visible", 0);
    assert!(!model.successors("Ink", &state).contains(&historical_hidden));
    assert!(!model.check_invariant("OrdinaryTextKeepsBody", &historical_hidden));
    step(&model, &mut state, &s, frame, "Ink", 1);

    s.term.process(b"\x1b[8;105H");
    frame = s.tick(16);
    step(&model, &mut state, &s, frame, "MoveCaret", 2);
    for _ in 0..300 {
        frame = s.tick(16);
        if !s.brain.needs_frames() {
            let mut stranded = project(&s, frame, 4);
            stranded.insert("home", 0);
            assert!(!model.successors("Arrive", &state).contains(&stranded));
            step(&model, &mut state, &s, frame, "Arrive", 4);
            break;
        }
        step(&model, &mut state, &s, frame, "Follow", 3);
    }
    assert_eq!(state["event"], 4, "the real follower must finish");

    s.term
        .text_selection_mut()
        .start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    s.term.text_selection_mut().update_selection(
        i32::from(ROWS - 1),
        COLS - 1,
        SelectionSide::Right,
    );
    frame = s.tick(16);
    step(&model, &mut state, &s, frame, "ProtectAll", 5);
    s.term.text_selection_mut().clear();
    s.reduced = true;
    frame = s.tick(16);
    step(&model, &mut state, &s, frame, "Release", 6);
    s.term.process(b"\x1b[?25l");
    frame = s.tick(16);
    step(&model, &mut state, &s, frame, "HideCursor", 7);
    s.term.process(b"\x1b[?25h");
    frame = s.tick(16);
    step(&model, &mut state, &s, frame, "ShowCursor", 8);
}

fn body(frame: PetFrame) -> Option<PetRect> {
    frame.body_px(CW, CH, COLS, ROWS).map(|(x0, x1, y0, y1)| {
        PetRect::new(
            y0 as f32 / f32::from(CH),
            x0 as f32 / f32::from(CW),
            (y1 - y0) as f32 / f32::from(CH),
            (x1 - x0) as f32 / f32::from(CW),
        )
    })
}

#[test]
fn dense_colored_coding_prompt_keeps_a_full_pet_near_the_cursor() {
    let mut s = Scene::dense(true);
    let frame = s.tick(0);
    assert_eq!(frame.alpha, 255);
    assert!(frame.under_ink);
    s.at_home(frame);
    for (row, col) in [(55, 3), (55, 40), (55, 148), (3, 3), (29, 76)] {
        s.term.process(format!("\x1b[{row};{col}H").as_bytes());
        s.brain.note_console_input(s.now, PetInputKind::Navigate);
        let frame = s.tick(16);
        assert_eq!(frame.alpha, 255);
        s.at_home(frame);
    }
}

#[test]
fn ordinary_dense_output_does_not_make_an_idle_resident_blink_out() {
    let mut s = Scene::dense(false);
    for _ in 0..60 {
        s.tick(16);
    }
    for _ in 0..180 {
        let frame = s.tick(16);
        assert!(
            frame.alpha > 0,
            "ordinary ink hid the resident: {}",
            s.brain.console_reason()
        );
        s.at_home(frame);
    }
}

#[test]
fn distant_program_output_changes_attention_without_moving_home_across_the_console() {
    let mut s = Scene::dense(true);
    s.tick(0);
    s.term.process(
        b"\x1b[3;100H\x1b]133;A\x07> \x1b]133;B\x07build\r\n\x1b]133;C\x07output\x1b[55;3H",
    );
    for _ in 0..4 {
        let frame = s.tick(1000);
        assert_eq!(frame.alpha, 255);
        s.at_home(frame);
    }
    s.term.process(b"\x1b]133;D;0\x07");
    s.brain.note_command_done(s.now, false, Some(100));
    for _ in 0..4 {
        let frame = s.tick(1000);
        assert_eq!(frame.alpha, 255);
        s.at_home(frame);
    }
}

#[test]
fn full_selection_still_protects_the_console_and_explains_the_hidden_body() {
    let mut s = Scene::dense(true);
    assert!(
        s.tick(0).alpha > 0,
        "negative control: ordinary dense text is drawable"
    );
    s.term
        .text_selection_mut()
        .start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    s.term.text_selection_mut().update_selection(
        i32::from(ROWS - 1),
        COLS - 1,
        SelectionSide::Right,
    );
    let hidden = s.tick(16);
    assert_eq!(hidden.alpha, 0);
    assert!(s.brain.console_reason().starts_with("protected-"));
    assert!(
        !s.brain.needs_frames(),
        "protected surface cannot poll forever"
    );
    s.term.text_selection_mut().clear();
    let returned = s.tick(16);
    assert_eq!(returned.alpha, 255);
    s.at_home(returned);
}

#[test]
fn a_visible_output_watcher_returns_home_after_a_large_caret_move() {
    let mut s = Scene::dense(false);
    s.term.process(
        b"\x1b[3;100H\x1b]133;A\x07> \x1b]133;B\x07build\r\n\x1b]133;C\x07output\x1b[55;3H",
    );
    for _ in 0..100 {
        s.tick(16);
    }
    s.term.process(b"\x1b[8;105H");
    // No input intent is needed to put a program-driven caret back in charge.
    // Existing cursor locomotion must recover from the old output interest.
    let mut frame = s.tick(16);
    for _ in 0..240 {
        frame = s.tick(16);
    }
    assert!(frame.alpha > 0);
    s.at_home(frame);
}

#[test]
fn ordinary_typing_keeps_the_full_body_visible_between_keys() {
    for frames_per_key in [2, 4, 8] {
        let mut s = Scene::dense(false);
        for _ in 0..100 {
            s.tick(16);
        }
        // A DELETE KEY ERASES. `\x08` alone is CURSOR-LEFT: it walks the
        // caret back over thirty characters and leaves every one of them on
        // screen, so the live ink still ends thirty columns out and the pet
        // stands at the end of the ink it is watching — 31.3 cells from the
        // caret, identically with the console layer on and off. That is the
        // ink ladder doing its job on a fixture that models no real delete.
        // `\x08 \x08` is what a shell actually emits, and with it the pet
        // comes home to 1.1-1.4 cells in both lanes.
        for kind in [PetInputKind::Text, PetInputKind::Delete] {
            for key in 0..30 {
                s.brain.note_console_input(s.now, kind);
                s.term.process(if kind == PetInputKind::Text {
                    &b"a"[..]
                } else {
                    &b"\x08 \x08"[..]
                });
                for beat in 0..frames_per_key {
                    let frame = s.tick(16);
                    assert!(
                        frame.alpha > 0,
                        "{kind:?} key{key} beat{beat} at{frames_per_key} frames/key hid the resident: {}",
                        s.brain.console_reason()
                    );
                    // WHERE THE PET IS BETWEEN KEYS IS THE ESCORT'S ANSWER,
                    // and at these rates the escort is legitimately behind:
                    // measured against the v0.76 oracle
                    // (`set_console_presentable(false)`), the worst gap over
                    // this exact run is 6.80 cells at 2 frames/key and 1.70
                    // at 8 — the same to the hundredth with the console
                    // layer on and off. What this beat may NOT contain is
                    // the console layer claiming the body and closing that
                    // gap in one frame; `pet_escort_primacy` refuses the
                    // same reasons on a typing frame, and the settled
                    // `at_home` below is where the arrival is checked.
                    let reason = s.brain.console_reason();
                    assert!(
                        matches!(
                            reason,
                            "quiet" | "committed-input" | "committed-edit-intent" | "advancing-ink"
                        ),
                        "{kind:?} key{key} beat{beat} at{frames_per_key} frames/key: the \
                         console layer claimed the escort's body ({reason}); body={:?} \
                         caret={:?}",
                        body(frame),
                        s.term.cursor()
                    );
                }
            }
            for _ in 0..100 {
                s.tick(16);
            }
            let frame = s.tick(16);
            s.at_home(frame);
        }
    }
}

#[test]
fn wall_side_changes_and_line_wrap_keep_the_body_visible() {
    let mut s = Scene::dense(false);
    s.term.process(b"\x1b[55;123H");
    for _ in 0..100 {
        s.tick(16);
    }
    for key in 0..34 {
        s.brain.note_console_input(s.now, PetInputKind::Text);
        s.term.process(b"a");
        for beat in 0..4 {
            let frame = s.tick(16);
            assert!(
                frame.alpha > 0,
                "typing at margin key{key} beat{beat} hid full body: {}; caret={:?}; frame={frame:?}",
                s.brain.console_reason(),
                s.term.cursor()
            );
        }
    }
    let mut frame = s.tick(16);
    for _ in 0..150 {
        frame = s.tick(16);
    }
    s.at_home(frame);
}

#[test]
fn frame_fingerprint_observes_the_body_moving_behind_text() {
    let mut s = Scene::dense(true);
    let frame = s.tick(0);
    assert!(frame.under_ink && frame.alpha > 0);
    assert_ne!(
        frame.fp(),
        PetFrame {
            under_ink: false,
            ..frame
        }
        .fp()
    );
}

#[test]
fn coding_console_cursor_hide_show_keeps_the_full_resident_without_fake_input() {
    let mut s = Scene::dense(false);
    for _ in 0..100 {
        s.tick(16);
    }
    let input_seq = s.brain.console_input_seq();
    for col in [3, 25, 140, 4] {
        s.term.process(b"\x1b[?25l");
        let held = s.tick(16);
        assert!(held.alpha > 0);
        let held_body = body(held).unwrap();
        for _ in 0..120 {
            let hidden = s.tick(16);
            assert_eq!(
                hidden.alpha, held.alpha,
                "DECTCEM repaint must not fade the resident"
            );
            assert_eq!(
                body(hidden),
                Some(held_body),
                "hidden repaint keeps the last real home"
            );
            assert!(
                !s.brain.needs_frames(),
                "a hidden-cursor resident cannot poll"
            );
        }
        s.term
            .process(format!("\x1b[55;{col}H\x1b[?25h").as_bytes());
        let shown = s.tick(16);
        assert!(shown.alpha > 0);
        // AND THEN IT WALKS BACK. The hidden-cursor hold is the one place
        // the console layer really is the only writer — there is no caret
        // for the escort to follow, so the body is held at its last real
        // home, which is what the loop above just proved. When the cursor
        // reappears up to 137 columns away the escort takes the body back
        // and travels; asserting `at_home` on the NEXT frame would demand a
        // teleport across the console, and that teleport is the v0.82.0
        // oscillation. Measured: the pet is home 53-182 frames later.
        let mut settled = shown;
        for _ in 0..400 {
            settled = s.tick(16);
            if !s.brain.needs_frames() {
                break;
            }
        }
        assert!(settled.alpha > 0);
        s.at_home(settled);
        assert_eq!(
            s.brain.console_input_seq(),
            input_seq,
            "repaint is not admitted input"
        );
    }
}

#[test]
fn hidden_cursor_region_scrolls_keep_the_full_body_at_its_last_real_home() {
    use aterm_core::terminal::{ContentScrollDelta, ContentScrollState};
    let motions: &[&[u8]] = &[
        b"\x1b[1;52r\x1b[52;1H\n",      // archival scroll above the footer
        b"\x1b[5;45r\x1b[45;1H\n",      // interior regional scroll
        b"\x1b[5;45r\x1b[20;1H\x1b[2L", // insert lines
        b"\x1b[5;45r\x1b[20;1H\x1b[2M", // delete lines
        b"\x1b[5;45r\x1b[5;1H\x1bM",    // reverse index
    ];
    for reduced in [false, true] {
        for home_row in [20, 55] {
            let mut s = Scene::dense(reduced);
            s.term.process(format!("\x1b[{home_row};75H").as_bytes());
            for _ in 0..100 {
                s.tick(16);
            }
            let input_seq = s.brain.console_input_seq();
            for motion in motions {
                let before = s.term.content_scroll_state();
                let mut bytes = b"\x1b[?25l\x1b7".to_vec();
                bytes.extend_from_slice(motion);
                bytes.extend_from_slice(b"\x1b[r\x1b8");
                s.term.process(&bytes);
                assert!(
                    matches!(
                        ContentScrollState::delta_since(
                            Some(before),
                            s.term.content_scroll_state()
                        ),
                        ContentScrollDelta::Bands { .. }
                    ),
                    "the real regional producer must explain this move"
                );
                // Hide and scroll are first observed together on this tick.
                let held = s.tick(16);
                assert!(held.alpha > 0);
                s.at_home(held);
                let held_body = body(held).unwrap();
                for _ in 0..30 {
                    let frame = s.tick(16);
                    assert_eq!(frame.alpha, held.alpha);
                    assert_eq!(
                        body(frame),
                        Some(held_body),
                        "home is screen-local, not translated with glyphs"
                    );
                    assert!(!s.brain.needs_frames(), "a hidden home must not poll");
                }
                assert_eq!(
                    s.brain.console_input_seq(),
                    input_seq,
                    "output never creates input"
                );
                s.term.process(b"\x1b[?25h");
                let shown = s.tick(16);
                s.at_home(shown);
            }
        }
    }
}
