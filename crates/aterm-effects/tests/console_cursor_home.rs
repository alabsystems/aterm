// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance and placement tests for the cursor pet beside a coding-console prompt.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aterm_core::render::RenderInput;
use aterm_core::selection::{SelectionSide, SelectionType, TextSelection};
use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{
    OldPath, PetAction, PetBrain, PetFrame, PetInputKind, PetSense, head_law,
};
use aterm_effects::pet_world::{PetPane, PetRect, PetWorld, PetWorldFacts};
use aterm_models_effects::console_cursor_home_model;
use aterm_spec::derive::Model;

const ROWS: u16 = 57;
const COLS: u16 = 151;
const CW: u16 = 10;
const CH: u16 = 20;

struct Seen {
    mutations: u64,
    selection: TextSelection,
    facts: PetWorldFacts,
    input: RenderInput,
    world: PetWorld,
    spans: Vec<(u16, u16)>,
    live: Option<u16>,
}

impl Seen {
    fn read(term: &mut Terminal) -> Self {
        let mutations = term.screen_mutations();
        let input = term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let facts = PetWorldFacts::read(term, 41);
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
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
        Self {
            mutations,
            selection: term.text_selection().clone(),
            facts,
            input,
            world,
            spans,
            live,
        }
    }

    fn current(&self, term: &Terminal) -> bool {
        self.mutations == term.screen_mutations()
            && &self.selection == term.text_selection()
            && self.facts == PetWorldFacts::read(term, 41)
    }
}

struct Scene {
    term: Terminal,
    brain: PetBrain,
    now: Instant,
    reduced: bool,
    hidden_caret_travels: bool,
    /// Each tick a windowless still's ([`PetBrain::tick_static_capture`]).
    still: bool,
    painted: (u16, u16),
    seen: Option<Seen>,
}

impl Scene {
    fn dense(reduced: bool) -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"\x1b]11;#000000\x07\x1b[48;2;35;39;47m");
        for row in 1..=ROWS {
            term.process(format!("\x1b[{row};1H").as_bytes());
            term.process(&vec![b'x'; usize::from(COLS)]);
        }
        term.process(b"\x1b[55;1H\x1b[2K> \x1b[0m");
        let painted = (term.cursor().row, term.cursor().col);
        Self {
            term,
            brain: PetBrain::default(),
            now: Instant::now(),
            reduced,
            hidden_caret_travels: false,
            still: false,
            painted,
            seen: None,
        }
    }

    fn tick(&mut self, ms: u64) -> PetFrame {
        self.now += Duration::from_millis(ms);
        if !self.seen.as_ref().is_some_and(|s| s.current(&self.term)) {
            self.seen = Some(Seen::read(&mut self.term));
        }
        let Seen {
            input,
            facts,
            world,
            spans,
            live,
            ..
        } = self.seen.as_ref().expect("read above");
        self.brain.sense_ink(0, spans, *live);
        self.brain
            .observe_console(input, facts, PetPane::full(input));
        self.brain.set_console_presentable(true);
        let cell = (input.cursor_row as u16, input.cursor_col as u16);
        if input.cursor_visible {
            self.painted = cell;
        }
        let sense = PetSense {
            caret_drawn: input.cursor_visible || !self.hidden_caret_travels,
            now: self.now,
            caret: ((input.cursor_visible || self.hidden_caret_travels)
                && input.display_offset == 0)
                .then_some(cell),
            wrapped: false,
            rows: ROWS,
            cols: COLS,
            cell_w: CW,
            cell_h: CH,
            reduced_motion: self.reduced,
            output_burst: false,
            pointer: None,
        };
        let frame = if self.still {
            self.brain.tick_static_capture(sense)
        } else {
            self.brain.tick(sense)
        };
        if let Some(rect) = body(frame) {
            assert!(
                world.under_text_clearance_past_caret(rect, 0.0) == Some(true),
                "emitted body covers protected pixels"
            );
            assert!(
                frame.under_ink || world.ink_clearance(rect, 0.0) == Some(true),
                "ink must remain in front"
            );
        }
        frame
    }

    fn at_home(&self, frame: PetFrame) {
        let rect = body(frame).expect("dense ordinary text must keep the complete pet");
        let caret = self.term.cursor();
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
        // The prompt sits between lettered rows, so the head law fits the full animal to its
        // own row, whole and smaller ([`head_law`]): no tiny marker in its place.
        let (_, fit) = head_law(true, true);
        assert!(
            rect.rows >= 1.5 * fit && rect.cols >= 5.0 * fit,
            "persistence must not replace the full animal with a tiny marker"
        );
    }
}

fn near_caret(s: &Scene, frame: PetFrame) -> bool {
    body(frame).is_some_and(|rect| {
        let (row, col) = s.painted;
        let col = f32::from(col);
        (rect.col - col)
            .max(0.0)
            .max((col - rect.col - rect.cols).max(0.0))
            <= 10.0
            && (rect.foot_row() - f32::from(row)).abs() <= 4.5
    })
}

type State = BTreeMap<&'static str, i64>;

fn project(s: &Scene, frame: PetFrame, event: i64) -> State {
    State::from([
        ("visible", i64::from(body(frame).is_some())),
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
    s.hidden_caret_travels = true;
    s.term.process(b"\x1b[?25l");
    frame = s.tick(16);
    step(&model, &mut state, &s, frame, "HideCursor", 7);
    s.term.process(b"\x1b7\x1b[2;120Hrepaint");
    frame = s.tick(16);
    let mut chased = project(&s, frame, 9);
    chased.insert("home", 0);
    chased.insert("moving", 1);
    assert!(!model.successors("RepaintMove", &state).contains(&chased));
    assert!(!model.check_invariant("RepaintHoldsHome", &chased));
    step(&model, &mut state, &s, frame, "RepaintMove", 9);
    s.term.process(b"\x1b8\x1b[?25h");
    frame = s.tick(16);
    step(&model, &mut state, &s, frame, "ShowCursor", 8);
}

fn stance(frame: PetFrame) -> (f32, f32, Option<f32>) {
    (frame.col, frame.row, body(frame).map(PetRect::foot_row))
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
fn ordinary_typing_keeps_the_full_body_visible_between_keys() {
    crate::common::par_map(&[2, 4, 8], |&frames_per_key| {
        let mut s = Scene::dense(false);
        for _ in 0..100 {
            s.tick(16);
        }
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
    });
}

/// Typed along the dense page's prompt row (row 55 of 57, [`at_the_last_columns`]) to the grid's
/// end and over it, a key every 4 frames: the body is on glass at every key until the wall holds
/// it no seat (`OldPath::NoSeatHidesTheBody`, ATERM-KI-1107-4), from no sooner than the 25th key
/// (measured: the 27th, the caret at the grid's second-last column), and its box lying on the
/// row's letters there, the wall's cut (`OldPath::LettersCutTheFade`) draws it off the glass
/// within 3 frames of the hide (measured 3, at alphas 184, 118 and 57). From the wrap it is on
/// glass in every frame (it fades back in from off the glass, measured at full alpha from the 19th
/// frame on, against the 8th with the cut reverted, which brought it back from its fade's 146), no
/// frame drawn more than [`FADE_BACK_STEP`] over the one before (measured 19 at most, 14
/// reverted), and it ends at home.
/// Negative control (the cut reverted): its fade is on glass in 8 or more of the frames from the
/// hide to the wrap, two thirds of those measured (12 of 12, down to alpha 146). Negative control
/// by hand of the step (the cut snapped whole at the wrap, not lifted over the fade-in): the body
/// went from off the glass to 159 in the wrap's first frame (Linux arm64, 2026-11-09).
#[test]
fn wall_side_changes_and_line_wrap_keep_the_body_visible() {
    for cut in [true, false] {
        let mut s = Scene::dense(false);
        s.brain.set_old_path(OldPath::LettersCutTheFade, !cut);
        s.term.process(b"\x1b[55;123H");
        for _ in 0..100 {
            s.tick(16);
        }
        let mut hid = None;
        let mut drawn_in_the_hide = 0;
        let mut last = u8::MAX;
        for key in 0..34 {
            s.brain.note_console_input(s.now, PetInputKind::Text);
            s.term.process(b"a");
            for beat in 0..4 {
                let frame = s.tick(16);
                let before = core::mem::replace(&mut last, frame.alpha);
                if hid.is_none() && frame.alpha < u8::MAX {
                    hid = Some(key);
                }
                if hid.is_some() && s.term.cursor().row == 54 {
                    drawn_in_the_hide += usize::from(frame.alpha > 0);
                    continue;
                }
                assert!(
                    frame.alpha.saturating_sub(before) <= FADE_BACK_STEP,
                    "(cut {cut}) key{key} beat{beat}: the body came back at {} from {before}",
                    frame.alpha
                );
                assert!(
                    frame.alpha > 0,
                    "typing at margin key{key} beat{beat} (cut {cut}) hid full body: {}; \
                     caret={:?}; frame={frame:?}",
                    s.brain.console_reason(),
                    s.term.cursor()
                );
            }
        }
        let hid = hid.unwrap_or_else(|| panic!("fixture (cut {cut}): the wall never hid the body"));
        assert!(
            hid >= 24,
            "(cut {cut}) the wall hid the body from key{hid}, before the line reached it"
        );
        if cut {
            assert!(
                drawn_in_the_hide <= 3,
                "from the wall's hide to the wrap the body lay on the letters in \
                 {drawn_in_the_hide} frames"
            );
        } else {
            assert!(
                drawn_in_the_hide >= 8,
                "negative control: the cut reverted, from the wall's hide to the wrap its fade \
                 was on glass in only {drawn_in_the_hide} frames"
            );
        }
        let mut frame = s.tick(16);
        for _ in 0..150 {
            frame = s.tick(16);
        }
        s.at_home(frame);
    }
}

/// The most a frame of the body coming back from the wall's cut is drawn over the one before: twice
/// the fade-in's own step at 60 Hz (255 x 16 ms / 300 ms, 13.6), since the cut's lift and the
/// fade's run together and multiply (`OldPath::LettersCutTheFade`).
const FADE_BACK_STEP: u8 = 28;

/// The dense page's prompt row with its caret at row 55 of 57, column 123 (1-based), settled:
/// typed on a key every 4 frames ([`type_along`]), its line reaches the grid's end and the wall
/// holds the body no seat on its last letters
/// ([`wall_side_changes_and_line_wrap_keep_the_body_visible`]). It is not the grid's last row:
/// rows 56 and 57 under it are full of letters, and a wrap takes the caret down to row 56, which
/// ends the wall's hide at once; on a last row a wrap scrolls the page and leaves the caret on its
/// row (`PetBrain::unseated_sense`).
fn at_the_last_columns(cut: bool) -> Scene {
    let mut s = Scene::dense(false);
    s.brain.set_old_path(OldPath::LettersCutTheFade, !cut);
    s.term.process(b"\x1b[55;123H");
    for _ in 0..100 {
        s.tick(16);
    }
    s
}

/// Types `a` along the row a key every 4 frames, up to `keys` keys, handing `seen` each frame;
/// stops at the first frame it answers `true` for, and says whether one did.
fn type_along(s: &mut Scene, keys: usize, mut seen: impl FnMut(&Scene, PetFrame) -> bool) -> bool {
    for _ in 0..keys {
        s.brain.note_console_input(s.now, PetInputKind::Text);
        s.term.process(b"a");
        for _ in 0..4 {
            let frame = s.tick(16);
            if seen(s, frame) {
                return true;
            }
        }
    }
    false
}

/// The program hides its cursor (`ESC[?25l`) six frames into the wall's hide of the body on the
/// last letters of the dense page's prompt row (row 55 of 57, [`at_the_last_columns`]), its cut
/// having drawn the body off the glass (`OldPath::LettersCutTheFade`): the hidden cursor's hold
/// (`cursor-hidden-home`) does not take a body the cut takes off the letters, whose last seat is
/// the one on them. It stays off the glass in every frame of the cursor's hide, and the brain asks
/// for no frames once its fade has run out (`FADE_OUT`, 450 ms: measured none from the 23rd frame
/// of the cursor's hide; bound, none from the 28th). Shown again on that row, the cursor leaves it
/// out (the wall still holds it no seat), and the line typed on over the grid's end brings it back
/// from the wrap (on glass in every frame from the wrap's 2nd).
/// Negative control (the cut reverted): the hold takes the body at its fade's alpha and brings it
/// back on those letters, on glass in every frame of the cursor's hide (measured 214, 227, 241,
/// then 255). Negative control by hand (the hold taking the cut body as well): it drew the body
/// back on the letters at 11 to 245 in the cursor's hide's first 18 frames and at full from its
/// 19th (Linux arm64, 2026-11-09).
#[test]
fn a_cursor_hide_in_the_walls_cut_keeps_the_body_off_the_letters() {
    for cut in [true, false] {
        let mut s = at_the_last_columns(cut);
        let mut into = 0;
        assert!(
            type_along(&mut s, 34, |_, frame| {
                into += usize::from(into > 0 || frame.alpha < u8::MAX);
                into == 6
            }),
            "fixture (cut {cut}): the wall never hid the body"
        );
        assert_eq!(
            s.term.cursor().row,
            54,
            "fixture (cut {cut}): the line wrapped"
        );
        s.term.process(b"\x1b[?25l");
        let mut drawn = 0;
        let mut asked = 0;
        for k in 1..=60 {
            let frame = s.tick(16);
            drawn += usize::from(frame.alpha > 0);
            if s.brain.needs_frames() {
                asked = k;
            }
        }
        if !cut {
            assert_eq!(
                drawn, 60,
                "negative control: the cut reverted, the hidden cursor's hold drew the body in \
                 only {drawn} of the cursor's hide's 60 frames"
            );
            continue;
        }
        assert_eq!(
            drawn,
            0,
            "the hidden cursor's hold drew the body the wall's cut took off the letters in \
             {drawn} frames: {}",
            s.brain.console_reason()
        );
        assert!(
            asked < 28 && !s.brain.is_active(),
            "the body off the glass, the brain asked for frames to frame {asked} of the cursor's hide"
        );
        s.term.process(b"\x1b[?25h");
        for _ in 0..30 {
            let frame = s.tick(16);
            assert_eq!(
                frame.alpha, 0,
                "the cursor shown at the wall, the body came back"
            );
        }
        let mut past = 0;
        let wrapped = type_along(&mut s, 8, |s, frame| {
            if s.term.cursor().row == 54 {
                return false;
            }
            past += 1;
            assert!(
                past < 2 || frame.alpha > 0,
                "the line wrapped, the body was off the glass in the wrap's frame {past}"
            );
            past == 20
        });
        assert!(wrapped, "fixture: the line never wrapped");
    }
}

/// The program hides its cursor one or three frames past the wrap that ends the wall's hide on the
/// dense page's prompt row (row 55 of 57, [`at_the_last_columns`]: the wrap takes the caret down
/// to row 56), the body coming back from the wall's cut (which lifts over the fade-in,
/// `OldPath::LettersCutTheFade`), the typing stopped: the hidden cursor's hold lifts the cut on,
/// and the brain asks for frames until the body is whole and for none once it is (a host that
/// draws only the frames asked for leaves no body drawn short of full). Measured: full from the
/// 19th frame from the wrap at either (bound, the fade-in's 19 frames and two), no frame asked for
/// from there.
/// Negative controls by hand (Linux arm64, 2026-11-09): with the hold's frames not lifting the
/// cut, the body stayed at 14 or 41 of 255 and the brain asked for every frame; with the brain not
/// asking for the cut's lift, it asked for none from 109 or 150.
#[test]
fn a_cursor_hide_as_the_body_comes_back_from_the_cut_brings_it_back_whole() {
    for hide in [1, 3] {
        let mut s = at_the_last_columns(true);
        let mut hid = false;
        assert!(
            type_along(&mut s, 34, |s, frame| {
                hid |= frame.alpha < u8::MAX;
                hid && s.term.cursor().row != 54
            }),
            "fixture: the line never wrapped"
        );
        let mut full = None;
        for k in 2..=60 {
            if k == hide + 1 {
                s.term.process(b"\x1b[?25l");
            }
            let frame = s.tick(16);
            if full.is_none() && frame.alpha == u8::MAX {
                full = Some(k);
            }
            assert!(
                s.brain.needs_frames() || frame.alpha == u8::MAX,
                "(hidden {hide} past the wrap) the brain asked for no frame past frame {k} from \
                 the wrap, the body drawn at {}",
                frame.alpha
            );
        }
        assert!(
            full.is_some_and(|k| k <= 21),
            "(hidden {hide} past the wrap) the body came back whole from frame {full:?} from the \
             wrap"
        );
        assert!(
            !s.brain.needs_frames(),
            "(hidden {hide} past the wrap) the hidden cursor's hold went on asking for frames"
        );
    }
}

/// Windowless stills (`PetBrain::tick_static_capture`) through the wall's hide of the body on the
/// last letters of the dense page's prompt row (row 55 of 57, [`at_the_last_columns`]), the line
/// resting at the wall, as live frames: a still does not force a body the wall's cut has taken off
/// the glass back to full (`OldPath::LettersCutTheFade`), so its fade runs out under the cut as in
/// live frames. The body is off the glass from the hide's 4th frame, the brain asks for no frames
/// from its fade's end (`FADE_OUT`, 450 ms: measured none from the 29th frame; bound, none from
/// the 31st), and six Backspaces that leave the body its seat bring it back on glass within 3
/// frames (measured the 2nd; a still at full in the 1st).
/// Negative control by hand (stills forcing the body back over the cut; Linux arm64,
/// 2026-11-09): its fade started over in every still, the brain asked for frames in every one,
/// and the body stayed off the glass past the Backspaces.
#[test]
fn a_still_capture_lets_the_walls_cut_run_out() {
    for still in [false, true] {
        let mut s = at_the_last_columns(true);
        s.still = still;
        assert!(
            type_along(&mut s, 34, |_, frame| frame.alpha < u8::MAX),
            "fixture (still {still}): the wall never hid the body"
        );
        let mut asked = 1;
        for k in 2..=60 {
            let frame = s.tick(16);
            assert!(
                k < 4 || frame.alpha == 0,
                "(still {still}) the body was drawn at {} in the hide's frame {k}",
                frame.alpha
            );
            if s.brain.needs_frames() {
                asked = k;
            }
        }
        assert!(
            asked < 31,
            "(still {still}) the body off the glass, the brain asked for frames to frame \
             {asked} of the hide"
        );
        for _ in 0..6 {
            s.term.process(b"\x08\x1b[K");
        }
        let back = (1..=3).find_map(|k| {
            let alpha = s.tick(16).alpha;
            (alpha > 0).then_some((k, alpha))
        });
        assert!(
            back.is_some(),
            "(still {still}) six Backspaces left the body its seat, and it stayed off the glass"
        );
        if still {
            assert_eq!(
                back,
                Some((1, u8::MAX)),
                "a still shows the body back at full"
            );
        }
    }
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
    let mut before = s.tick(16);
    for _ in 1..100 {
        before = s.tick(16);
    }
    assert_eq!(before.action, PetAction::Sleep);
    let input_seq = s.brain.console_input_seq();
    for col in [3, 25, 140, 4] {
        s.term.process(b"\x1b[?25l");
        let held = s.tick(16);
        assert!(held.alpha > 0);
        assert_eq!(
            held.action, before.action,
            "the hold keeps the pet's own action"
        );
        for _ in 0..120 {
            let hidden = s.tick(16);
            assert_eq!(
                hidden.alpha, held.alpha,
                "DECTCEM repaint must not fade the resident"
            );
            assert_eq!(
                stance(hidden),
                stance(held),
                "hidden repaint keeps the last real home"
            );
            assert_eq!(hidden.action, held.action);
            assert!(
                !s.brain.needs_frames(),
                "a hidden-cursor resident cannot poll"
            );
        }
        s.term
            .process(format!("\x1b[55;{col}H\x1b[?25h").as_bytes());
        let shown = s.tick(16);
        assert!(shown.alpha > 0);
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
        before = settled;
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
    let scenes = [(false, 20), (false, 55), (true, 20), (true, 55)];
    crate::common::par_map(&scenes, |&(reduced, home_row)| {
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
                    ContentScrollState::delta_since(Some(before), s.term.content_scroll_state()),
                    ContentScrollDelta::Bands { .. }
                ),
                "the real regional producer must explain this move"
            );
            let held = s.tick(16);
            assert!(held.alpha > 0);
            s.at_home(held);
            for _ in 0..30 {
                let frame = s.tick(16);
                assert_eq!(frame.alpha, held.alpha);
                assert_eq!(
                    stance(frame),
                    stance(held),
                    "home is screen-local, not translated with glyphs"
                );
                assert_eq!(frame.action, held.action);
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
    });
}
