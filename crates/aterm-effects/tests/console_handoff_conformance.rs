// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for resident cadence ownership. Every projected bit comes from
//! public shipping-brain reads or a real terminal snapshot. Motion duration and
//! general perch-path geometry remain outside this small derived model.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetAttention, PetBrain, PetFrame, PetSense};
use aterm_effects::pet_world::{PetCell, PetPane, PetWorld, PetWorldFacts};
use aterm_spec::derive::{Model, console_resident_handoff_model};

type State = BTreeMap<&'static str, i64>;

struct Scene {
    term: Terminal,
    brain: PetBrain,
    now: Instant,
}

impl Scene {
    fn new() -> Self {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]11;#000000\x07\x1b[12;40H");
        Self {
            term,
            brain: PetBrain::default(),
            now: Instant::now(),
        }
    }

    fn tick(&mut self, ms: u64) -> PetFrame {
        self.now += Duration::from_millis(ms);
        let input = self.term.cell_frame(24, 80);
        let facts = PetWorldFacts::read(&self.term, 7);
        self.brain
            .observe_console(&input, &facts, PetPane::full(&input));
        self.brain.set_console_presentable(true);
        self.brain.tick(PetSense {
            now: self.now,
            caret: (input.cursor_visible && input.display_offset == 0)
                .then_some((input.cursor_row as u16, input.cursor_col as u16)),
            wrapped: false,
            rows: 24,
            cols: 80,
            cell_w: 8,
            cell_h: 16,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        })
    }

    fn flying() -> Self {
        let mut s = Self::new();
        for _ in 0..30 {
            s.tick(100);
        }
        // A local row hop still exercises legacy landing ownership. A
        // screen-wide cursor relocation now moves the base home immediately,
        // so it deliberately cannot strand a long flight across the pane.
        s.term.process(b"\x1b[9;43H");
        for _ in 0..80 {
            let f = s.tick(16);
            if f.action.airborne() && f.lift > 0.1 && f.alpha > 0 {
                return s;
            }
        }
        panic!("fixture must produce a genuine visible legacy flight");
    }

    fn watching() -> Self {
        let mut s = Self::new();
        s.term
            .process(b"\x1b[8;10H\x1b]133;A\x07$ \x1b]133;B\x07build\r\n\x1b]133;C\x07working");
        for _ in 0..30 {
            s.tick(100);
        }
        assert_eq!(s.brain.console_attention(), PetAttention::Output);
        assert!(s.tick(0).alpha > 0);
        assert!(!s.brain.needs_frames());
        s
    }

    fn select(&mut self, dense: bool) {
        let (row, col, end_row, end_col) = if dense {
            (0, 0, 23, 79)
        } else {
            (18, 2, 18, 7)
        };
        self.term.text_selection_mut().start_selection(
            row,
            col,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        self.term
            .text_selection_mut()
            .update_selection(end_row, end_col, SelectionSide::Right);
    }

    fn project(&mut self, event: i64) -> State {
        let input = self.term.cell_frame(24, 80);
        let facts = PetWorldFacts::read(&self.term, 7);
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        // The no-target fixture has no clear cell at all, which is stronger
        // than failure to fit one particular body. Never infer this from alpha.
        let blocked = (0..24).all(|r| (0..80).all(|c| world.cell(r, c) != PetCell::Clear));
        let resident = matches!(
            self.brain.console_attention(),
            PetAttention::Reading | PetAttention::Output | PetAttention::Yielding
        );
        State::from([
            (
                "legacy",
                i64::from(self.brain.console_legacy_motion_pending()),
            ),
            ("resident", i64::from(resident)),
            (
                "handoff",
                i64::from(self.brain.console_resident_handoff_pending()),
            ),
            ("touch", i64::from(self.brain.pending_pets() > 0)),
            ("blocked", i64::from(blocked)),
            ("wake", i64::from(self.brain.needs_frames())),
            ("event", event),
        ])
    }
}

fn step(model: &Model, state: &mut State, s: &mut Scene, action: &str, event: i64) {
    let post = s.project(event);
    assert!(
        model.successors(action, state).contains(&post),
        "{action}: real reason={}\n{state:?} -> {post:?}",
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
fn real_flight_landing_and_resident_consumption_conform_for_clear_and_dense_reading() {
    let model = console_resident_handoff_model();
    for dense in [false, true] {
        let mut s = Scene::flying();
        let mut state = model.init_state();
        assert_eq!(s.project(0), state);
        s.select(dense);
        s.tick(0);
        let action = if dense { "SelectDense" } else { "SelectClear" };
        let event = if dense { 2 } else { 1 };
        let mut parked_flight = s.project(event);
        parked_flight.insert("wake", 0);
        assert!(!model.successors(action, &state).contains(&parked_flight));
        assert!(!model.check_invariant("LegacyOwnsCadence", &parked_flight));
        step(&model, &mut state, &mut s, action, event);

        for _ in 0..300 {
            assert!(s.brain.needs_frames(), "legacy work cannot strand itself");
            s.tick(16);
            if s.brain.console_legacy_motion_pending() {
                step(&model, &mut state, &mut s, "FlightTick", 3);
            } else {
                let mut missing_handoff = s.project(4);
                missing_handoff.insert("handoff", 0);
                missing_handoff.insert("wake", 0);
                assert!(!model.successors("Land", &state).contains(&missing_handoff));
                assert!(!model.check_invariant("LandingOwesResidentTick", &missing_handoff));
                step(&model, &mut state, &mut s, "Land", 4);
                break;
            }
        }
        assert!(
            !s.brain.console_legacy_motion_pending(),
            "finite fixture landed"
        );
        assert!(s.brain.console_resident_handoff_pending());
        assert!(
            s.brain.needs_frames(),
            "an actual resident tick is still owed"
        );
        let frame = s.tick(0);
        if dense {
            assert_eq!(frame.alpha, 0);
            assert_eq!(s.brain.console_attention(), PetAttention::Yielding);
            step(&model, &mut state, &mut s, "NoTarget", 6);
        } else {
            assert!(frame.alpha > 0);
            assert_eq!(s.brain.console_attention(), PetAttention::Reading);
            step(&model, &mut state, &mut s, "ResidentTick", 5);
        }
        assert_eq!(s.brain.next_change_deadline(s.now), None);
        let later = s.tick(30_000);
        assert_eq!(later.fp(), frame.fp());
        step(&model, &mut state, &mut s, "Quiet", 8);
        assert_eq!(s.brain.next_change_deadline(s.now), None);
    }
}

#[test]
fn real_no_target_retirement_consumes_touch_and_cannot_poll_or_replay() {
    let model = console_resident_handoff_model();
    let mut s = Scene::watching();
    // The watcher enters the same modeled settled-resident state through a
    // command, rather than through a flight. Check exact state equality with
    // the derived model's reachable clear-handoff outcome before the stimulus.
    let mut state = model.init_state();
    for action in ["SelectClear", "Land", "ResidentTick"] {
        assert!(model.fire(action, &mut state));
    }
    assert_eq!(s.project(5), state);
    let content = s.brain.content();
    s.brain.note_petted(s.now);
    step(&model, &mut state, &mut s, "NotePet", 7);
    s.select(true);
    let frame = s.tick(16);
    assert_eq!(frame.alpha, 0);
    assert_eq!(s.brain.content(), content);
    let mut retained_touch = s.project(6);
    retained_touch.insert("touch", 1);
    retained_touch.insert("wake", 1);
    assert!(
        !model
            .successors("NoTarget", &state)
            .contains(&retained_touch)
    );
    assert!(!model.check_invariant("NoTargetConsumesTouch", &retained_touch));
    assert!(!model.check_invariant("NoTargetParks", &retained_touch));
    step(&model, &mut state, &mut s, "NoTarget", 6);
    assert_eq!(s.brain.next_change_deadline(s.now), None);
    s.tick(30_000);
    assert_eq!(s.brain.content(), content);
    step(&model, &mut state, &mut s, "Quiet", 8);
    assert_eq!(s.brain.next_change_deadline(s.now), None);
}
