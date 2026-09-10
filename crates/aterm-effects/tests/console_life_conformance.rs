// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 console attention: real Terminal snapshots, injected clocks and the
//! shipping PetBrain projected onto the derived episode ownership relation.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetAttention, PetBrain, PetInputKind, PetSense};
use aterm_effects::pet_world::{PetPane, PetWorldFacts};
use aterm_spec::derive::{Model, console_life_episode_model};
use aterm_spec::verify;

type State = BTreeMap<&'static str, i64>;

struct Fixture {
    brain: PetBrain,
    term: Terminal,
    now: Instant,
    // The harness counts calls and ownership replacements, not pet state.
    inputs: i64,
    surface: i64,
}

impl Fixture {
    fn new() -> Self {
        let mut term = Terminal::new(24, 80);
        // Match a presented host: unresolved COLOR_UNSET cannot certify a
        // blank sprite footprint even on an otherwise empty terminal.
        term.process(b"\x1b]11;#000000\x07\x1b[12;40H");
        let mut f = Self {
            brain: PetBrain::default(),
            term,
            now: Instant::now(),
            inputs: 0,
            surface: 0,
        };
        for _ in 0..30 {
            f.tick(100);
        }
        f
    }

    fn tick(&mut self, ms: u64) {
        self.now += Duration::from_millis(ms);
        let input = self.term.cell_frame(24, 80);
        let facts = PetWorldFacts::read(&self.term, 7);
        self.brain
            .observe_console(&input, &facts, PetPane::full(&input));
        self.brain.set_console_presentable(true);
        let cursor = self.term.cursor();
        let _ = self.brain.tick(PetSense {
            now: self.now,
            caret: Some((cursor.row, cursor.col)),
            wrapped: false,
            rows: 24,
            cols: 80,
            cell_w: 8,
            cell_h: 16,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        });
    }

    fn input(&mut self, kind: PetInputKind) {
        self.brain.note_console_input(self.now, kind);
        self.inputs += 1;
        self.tick(10);
    }

    fn select(&mut self) {
        self.term.text_selection_mut().start_selection(
            10,
            14,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        self.term
            .text_selection_mut()
            .update_selection(10, 17, SelectionSide::Right);
        self.tick(10);
    }

    fn work(&mut self) {
        self.term
            .process(b"\x1b[8;10H\x1b]133;A\x07$ \x1b]133;B\x07build\r\n\x1b]133;C\x07working");
        self.tick(1500);
    }

    fn complete(&mut self, stale: bool) {
        self.term.process(b"\r\n\x1b]133;D;0\x07");
        self.brain.note_command_done(self.now, false, Some(3000));
        self.tick(if stale { 2200 } else { 10 });
    }

    fn change_surface(&mut self) {
        self.term = Terminal::new(24, 80);
        self.term.process(b"\x1b]11;#000000\x07\x1b[12;40H");
        self.surface += 1;
        self.tick(10);
    }

    fn project(&self, event: i64) -> State {
        let attention = match self.brain.console_attention() {
            PetAttention::Rest => 0,
            PetAttention::Typing | PetAttention::Editing | PetAttention::Contact => 1,
            PetAttention::Reading => 2,
            PetAttention::Output | PetAttention::Progress | PetAttention::Exploring => 3,
            PetAttention::Result => 4,
            PetAttention::Yielding => panic!("fixture has ample clear space; unexpected yielding"),
        };
        State::from([
            ("seq", self.brain.console_input_seq() as i64),
            ("consumed", self.brain.console_input_consumed_seq() as i64),
            ("inputs", self.inputs),
            ("attention", attention),
            (
                "anchor",
                i64::from(self.brain.console_anchor_id().is_some()),
            ),
            (
                "selected",
                i64::from(self.term.text_selection().has_selection()),
            ),
            (
                "result",
                i64::from(self.brain.console_attention() == PetAttention::Result),
            ),
            ("surface", self.surface),
            ("event", event),
        ])
    }
}

fn step(model: &Model, state: &mut State, f: &Fixture, action: &str, event: i64) {
    let post = f.project(event);
    let (ok, why) = verify::validate_transition_tiered(
        model,
        &[],
        state,
        &post,
        Some(action),
        "real console attention ownership",
    );
    assert!(
        ok,
        "{action}: {why}; real reason={}\n{state:?} -> {post:?}",
        f.brain.console_reason()
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
fn input_echo_selection_and_surface_ownership_conform() {
    let model = console_life_episode_model();
    let mut f = Fixture::new();
    let mut state = model.init_state();
    assert_eq!(f.project(0), state);
    f.input(PetInputKind::Text);
    step(&model, &mut state, &f, "Input", 1);
    let consumed = f.brain.console_input_consumed_seq();
    // A delayed stationary overwrite arrives as output, not another input.
    f.term.process(b"\x1b[12;40Hx\x1b[12;40H");
    f.tick(20);
    step(&model, &mut state, &f, "Echo", 2);
    assert_eq!(f.brain.console_input_consumed_seq(), consumed);
    f.tick(20);
    step(&model, &mut state, &f, "Echo", 2);
    f.select();
    step(&model, &mut state, &f, "Select", 3);
    f.input(PetInputKind::Navigate);
    step(&model, &mut state, &f, "Input", 1);
    assert_eq!(f.brain.console_reason(), "selection");
    f.term.text_selection_mut().clear();
    f.tick(1500);
    step(&model, &mut state, &f, "ReleaseSelection", 4);
    f.input(PetInputKind::Delete);
    step(&model, &mut state, &f, "Input", 1);
    f.change_surface();
    step(&model, &mut state, &f, "Surface", 9);
    assert_eq!(
        f.brain.console_input_kind(),
        None,
        "old surface input retired"
    );
    assert_eq!(f.brain.console_event_seq(), 0, "old event identity retired");
}

#[test]
fn multiple_commits_coalesce_to_the_latest_subject_without_echo_replay() {
    let model = console_life_episode_model();
    let mut f = Fixture::new();
    let mut state = model.init_state();
    f.brain.note_console_input(f.now, PetInputKind::Text);
    f.brain.note_console_input(f.now, PetInputKind::Navigate);
    f.inputs += 2;
    f.tick(10);
    step(&model, &mut state, &f, "BatchInput", 1);
    assert_eq!(f.brain.console_input_kind(), Some(PetInputKind::Navigate));
    assert_eq!(f.brain.console_event_seq(), 2);

    f.term.process(b"\x1b[12;40Hx\x1b[12;40H");
    f.tick(20);
    let mut duplicate = f.project(2);
    duplicate.insert("seq", 3);
    assert!(!model.check_invariant("OnlyInputAdvancesSequence", &duplicate));
    let (accepted, _) = verify::validate_transition_tiered(
        &model,
        &[],
        &state,
        &duplicate,
        Some("Echo"),
        "echo as duplicate input negative control",
    );
    assert!(
        !accepted,
        "an echo cannot manufacture another input sequence"
    );
    step(&model, &mut state, &f, "Echo", 2);
    assert_eq!(f.brain.console_input_consumed_seq(), 2);
}

#[test]
fn direct_input_revokes_a_real_command_perch_and_its_result() {
    let model = console_life_episode_model();
    let mut f = Fixture::new();
    let mut state = model.init_state();
    f.work();
    step(&model, &mut state, &f, "Work", 5);
    let id = f
        .brain
        .console_anchor_id()
        .expect("real OSC command anchor");
    f.complete(false);
    step(&model, &mut state, &f, "Complete", 6);
    assert_eq!(f.brain.console_anchor_id(), Some(id));
    f.input(PetInputKind::Paste);
    step(&model, &mut state, &f, "Input", 1);
    assert_eq!(f.brain.console_anchor_id(), None);
    f.select();
    step(&model, &mut state, &f, "Select", 3);
    f.change_surface();
    step(&model, &mut state, &f, "Surface", 9);
}

#[test]
fn completed_and_already_stale_results_expire_without_a_backlog() {
    let model = console_life_episode_model();
    for stale in [false, true] {
        let mut f = Fixture::new();
        let mut state = model.init_state();
        f.work();
        step(&model, &mut state, &f, "Work", 5);
        f.complete(stale);
        if stale {
            step(&model, &mut state, &f, "StaleComplete", 8);
        } else {
            step(&model, &mut state, &f, "Complete", 6);
            let before = state.clone();
            f.tick(1500);
            let mut replay = f.project(7);
            replay.insert("result", 1);
            replay.insert("attention", 4);
            assert!(!model.check_invariant("ExpiryCannotReplay", &replay));
            let (accepted, _) = verify::validate_transition_tiered(
                &model,
                &[],
                &before,
                &replay,
                Some("Expire"),
                "stale result replay negative control",
            );
            assert!(!accepted, "a result replay must not conform");
            step(&model, &mut state, &f, "Expire", 7);
        }
        // Further identical observations cannot rediscover the expired verdict.
        f.tick(1500);
        step(&model, &mut state, &f, "Echo", 2);
        f.change_surface();
        step(&model, &mut state, &f, "Surface", 9);
    }
}
