// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fixed composer caret surrounded by sparse animated program glyphs.
//! The sampled Codex scene used braille particles throughout the three input
//! rows. They are ordinary terminal content, independent of aterm's effects.

use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetBrain, PetFrame, PetSense};
use aterm_effects::pet_world::{PetPane, PetWorldFacts};
use aterm_effects::word_decorations::{DecoConfig, WordDecorations};
use aterm_lexicon::Lexicon;

const ROWS: u16 = 58;
const COLS: u16 = 116;
const CARET: (u16, u16) = (55, 2);

struct Scene {
    term: Terminal,
    brain: PetBrain,
    decos: WordDecorations,
    lexicon: Lexicon,
    cfg: DecoConfig,
    now: Instant,
    incoherent: bool,
    rows: u16,
    cols: u16,
    presentable: bool,
    static_capture: bool,
    observe: bool,
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
            incoherent: false,
            rows,
            cols,
            presentable: true,
            static_capture: false,
            observe: true,
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
        let mut input = self
            .term
            .cell_frame(usize::from(self.rows), usize::from(self.cols));
        if self.incoherent {
            input.scroll_frac_px = 1;
        }
        let facts = PetWorldFacts::read(&self.term, 7);
        if self.observe {
            self.brain
                .observe_console(&input, &facts, PetPane::full(&input));
        }
        self.brain.set_console_presentable(self.presentable);
        // This is the shipping scanner, not a test-side approximation of its
        // ink spans. As on glass, the brain reads the last completed scan.
        let (spans, live) = self.decos.pet_ink();
        self.brain.sense_ink(0, spans, live);
        let sense = PetSense {
            caret_drawn: true,
            now: self.now,
            caret: (self.presentable && input.cursor_visible)
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
        let frame = if self.static_capture {
            self.brain.tick_static_capture(sense)
        } else {
            self.brain.tick(sense)
        };
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
fn sparse_program_particles_do_not_send_a_fixed_caret_pet_across_the_pane() {
    let mut s = Scene::new();
    s.particles(33);
    let mut frame = s.tick();
    for _ in 0..240 {
        frame = s.tick();
    }
    assert!(frame.alpha > 0, "fixture must put the resident on glass");
    let initial = frame.col;
    let mut min = initial;
    let mut max = initial;
    for round in 0..12 {
        // Some frames place a real particle under the old footprint. The
        // pet must avoid that glyph using nearby empty cells, not ignore it.
        s.particles([2, 5, 33][round % 3]);
        for _ in 0..32 {
            frame = s.tick();
            min = min.min(frame.col);
            max = max.max(frame.col);
        }
    }
    eprintln!(
        "fixed caret sparse particles: initial_col={initial:.3} min_col={min:.3} max_col={max:.3} travel_px={:.3} action={} reason={}",
        (max - min) * 8.0,
        s.brain.pose_name(),
        s.brain.console_reason()
    );
    assert_eq!(s.brain.console_input_seq(), 0);
    assert_eq!((s.term.cursor().row, s.term.cursor().col), CARET);
    // There is ample blank ground beside the caret in every particle frame:
    // a lone distant glyph cannot turn the intervening spaces into a wall.
    assert!(
        max - min < 6.0,
        "sparse ink caused a {}-cell trip",
        max - min
    );
}

#[test]
fn a_single_incoherent_observation_does_not_commission_a_far_trip() {
    let mut s = Scene::new();
    s.particles(2);
    let mut before = s.tick();
    for _ in 0..240 {
        before = s.tick();
    }
    assert_eq!(before.alpha, 255);
    s.incoherent = true;
    let unknown = s.tick();
    assert_eq!(
        unknown.alpha, 0,
        "the incoherent surface is correctly not drawn"
    );
    s.incoherent = false;
    let mut min = before.col;
    let mut max = before.col;
    for _ in 0..200 {
        let f = s.tick();
        if f.alpha > 0 {
            min = min.min(f.col);
            max = max.max(f.col);
        }
    }
    eprintln!(
        "one unknown frame: before=({},{}) visible min={min} max={max} travel_px={} reason={}",
        before.row,
        before.col,
        (max - min) * 8.0,
        s.brain.console_reason()
    );
    assert_eq!(s.brain.console_input_seq(), 0);
    assert_eq!((s.term.cursor().row, s.term.cursor().col), CARET);
    assert!(
        max - min < 6.0,
        "a temporary unknown commissioned a {}-cell trip",
        max - min
    );
}

#[test]
fn sparse_ink_stays_local_across_observation_budget_edges() {
    for (rows, cols) in [(64, 256), (65, 257), (100, 300), (200, 600)] {
        let mut s = Scene::sized(rows, cols);
        s.particles(33);
        for _ in 0..240 {
            s.tick();
        }
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for near in [2, 5, 33, 2, 5, 33] {
            s.particles(near);
            for _ in 0..32 {
                let f = s.tick();
                assert!(f.alpha > 0);
                min = min.min(f.col);
                max = max.max(f.col);
            }
        }
        assert!(
            max - min < 6.0,
            "{rows}x{cols}: {}-cell sparse trip",
            max - min
        );
    }
}

#[test]
fn invalid_surface_cannot_materialize_static_capture_or_replay_an_unfocused_trip() {
    for capture in [false, true] {
        let mut s = Scene::new();
        s.particles(2);
        for _ in 0..240 {
            s.tick();
        }
        s.incoherent = true;
        s.static_capture = capture;
        assert_eq!(s.tick().alpha, 0);
        assert!(!s.brain.console_legacy_motion_pending());
        s.presentable = false; // host also withholds caret when it gives up custody
        for _ in 0..8 {
            s.tick();
        }
        s.incoherent = false;
        s.presentable = true;
        s.static_capture = false;
        for _ in 0..160 {
            let f = s.tick();
            assert!(f.col < 9.0, "refocus replayed an invalid trip at {}", f.col);
        }
    }
}

#[test]
fn a_real_caret_move_waits_for_coherence_then_follows_normally() {
    let mut s = Scene::new();
    for _ in 0..100 {
        s.tick();
    }
    let before = s.tick();
    s.incoherent = true;
    s.brain
        .note_console_input(s.now, aterm_effects::kitty_pet::PetInputKind::Navigate);
    let input = s.brain.console_input_seq();
    s.term.process(b"\x1b[56;61H");
    let held = s.tick();
    assert_eq!((held.row, held.col), (before.row, before.col));
    assert!(s.brain.console_input_consumed_seq() < input);
    s.incoherent = false;
    let mut followed = s.tick();
    assert_eq!(s.brain.console_input_consumed_seq(), input);
    for _ in 0..300 {
        followed = s.tick();
    }
    assert!(
        followed.alpha > 0 && (followed.col - 61.0).abs() < 2.0,
        "real navigation must still reach home: {followed:?}"
    );
}

#[test]
fn real_failed_observations_conform_for_idle_and_airborne_locomotion() {
    use aterm_effects::pet_world::PetWorld;
    use aterm_spec::derive::console_observation_admission_model;
    let model = console_observation_admission_model();
    for airborne in [false, true] {
        let mut s = Scene::new();
        if !airborne {
            s.particles(2);
        }
        let mut before = s.tick();
        for _ in 0..240 {
            before = s.tick();
        }
        if airborne {
            s.term.process(b"\x1b[56;80H");
            for _ in 0..300 {
                before = s.tick();
                if before.action.airborne() && before.alpha > 0 {
                    break;
                }
            }
            assert!(
                before.action.airborne(),
                "positive control needs a real flight"
            );
        }
        let prior_legacy = i64::from(s.brain.console_legacy_motion_pending());
        assert_eq!(prior_legacy, i64::from(airborne));
        let mut state = model.successors("ObserveCoherent", &model.init_state())[0].clone();
        if airborne {
            state = model.successors("TickLaunch", &state)[0].clone();
        }
        state = model.successors("ObserveIncoherent", &state)[0].clone();
        // Independently establish the observation verdict on the exact
        // production input, rather than infer it from the resulting motion.
        let mut input = s.term.cell_frame(usize::from(s.rows), usize::from(s.cols));
        input.scroll_frac_px = 1;
        let facts = PetWorldFacts::read(&s.term, 7);
        assert!(!PetWorld::default().observe(&input, &facts, PetPane::full(&input)));
        s.incoherent = true;
        for _ in 0..3 {
            let held = s.tick();
            let mut post = state.clone();
            post.insert("prior_legacy", prior_legacy);
            post.insert("legacy", i64::from(s.brain.console_legacy_motion_pending()));
            post.insert(
                "moved",
                i64::from(held.col != before.col || held.row != before.row),
            );
            post.insert("ticked", 1);
            assert!(
                model.successors("TickStill", &state).contains(&post),
                "actual failed-observation transition diverged: {state:?} -> {post:?}"
            );
            assert!(model.check_invariant("IncoherentObservationFreezesMotion", &post));
            assert_eq!(held.alpha, 0);
            // Historical controls: the old idle path commissioned a flight;
            // the old airborne path advanced an already committed trajectory.
            let mut historical = post.clone();
            if airborne {
                historical.insert("moved", 1);
            } else {
                historical.insert("legacy", 1);
            }
            assert!(!model.successors("TickStill", &state).contains(&historical));
            assert!(!model.check_invariant("IncoherentObservationFreezesMotion", &historical));
            state = post;
        }
        s.incoherent = false;
        let resumed = s.tick();
        if airborne {
            assert!(
                resumed.col != before.col || resumed.row != before.row,
                "a coherent surface must resume the preserved flight"
            );
        }
    }
    // A host that never supplied a world retains the original locomotion
    // contract; missing observations are not failed observations.
    let mut s = Scene::new();
    s.observe = false;
    for _ in 0..100 {
        s.tick();
    }
    s.term.process(b"\x1b[56;80H");
    let mut flew = false;
    for _ in 0..300 {
        flew |= s.tick().action.airborne();
    }
    assert!(flew, "unfed legacy host must still follow the real caret");
}
