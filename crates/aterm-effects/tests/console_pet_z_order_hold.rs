// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance and typing-burst tests verifying that `PetFrame::under_ink` is held
//! across frames until the body has stood clear of glyph ink for `Z_DWELL`, rather
//! than strobing per frame at the keystroke rate.

use std::time::{Duration, Instant};

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetBrain, PetFrame, PetInputKind, PetSense};
use aterm_effects::pet_world::{PetPane, PetRect, PetWorld, PetWorldFacts};
use aterm_models_effects::console_pet_z_order_model;
use aterm_spec::derive::Model;

const ROWS: u16 = 24;
const COLS: u16 = 80;
const CELL_W: u16 = 8;
const CELL_H: u16 = 16;
const PANGRAM: &[u8] = b"the quick brown fox jumps over the lazy dog";
const DWELL: Duration = Duration::from_millis(250);

struct Fixture {
    brain: PetBrain,
    world: PetWorld,
    term: Terminal,
    now: Instant,
    rows: u16,
    cols: u16,
    cell: (u16, u16),
}

impl Fixture {
    fn new(rows: u16, cols: u16, cell: (u16, u16)) -> Self {
        let mut term = Terminal::new(rows, cols);
        term.process(b"\x1b]11;#000000\x07");
        Self {
            brain: PetBrain::default(),
            world: PetWorld::default(),
            term,
            now: Instant::now(),
            rows,
            cols,
            cell,
        }
    }

    fn console() -> Self {
        Self::new(ROWS, COLS, (CELL_W, CELL_H))
    }

    fn settle(&mut self) {
        for _ in 0..40 {
            self.tick(100);
        }
    }

    fn tick(&mut self, ms: u64) -> PetFrame {
        self.tick_at(self.now + Duration::from_millis(ms))
    }

    fn tick_at(&mut self, at: Instant) -> PetFrame {
        self.now = at;
        let (rows, cols) = (usize::from(self.rows), usize::from(self.cols));
        let input = self.term.cell_frame(rows, cols);
        let facts = PetWorldFacts::read(&self.term, 7);
        self.brain
            .observe_console(&input, &facts, PetPane::full(&input));
        self.world.observe(&input, &facts, PetPane::full(&input));
        self.brain.set_console_presentable(true);
        let cursor = self.term.cursor();
        self.brain.tick(PetSense {
            caret_drawn: self.term.cursor_visible(),
            now: self.now,
            caret: Some((cursor.row, cursor.col)),
            wrapped: false,
            rows: self.rows,
            cols: self.cols,
            cell_w: self.cell.0,
            cell_h: self.cell.1,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        })
    }

    fn drawn(&self, frame: &PetFrame) -> Option<PetRect> {
        let (cw, ch) = self.cell;
        frame
            .body_px(cw, ch, self.cols, self.rows)
            .map(|(x0, x1, y0, y1)| {
                PetRect::new(
                    y0 as f32 / f32::from(ch),
                    x0 as f32 / f32::from(cw),
                    (y1 - y0) as f32 / f32::from(ch),
                    (x1 - x0) as f32 / f32::from(cw),
                )
            })
    }

    fn ink_under(&self, rect: PetRect) -> bool {
        let r1 = (rect.row + rect.rows).ceil() as usize;
        let c1 = (rect.col + rect.cols).ceil() as usize;
        (rect.row.floor() as usize..r1).any(|r| {
            (rect.col.floor() as usize..c1).any(|c| self.world.ink_at(r, c) != Some(false))
        })
    }
}

/// One burst, in numbers.
#[derive(Debug, Default, Clone, Copy)]
struct Burst {
    drawn: usize,
    over_ink: usize,
    in_front: usize,
    flips: usize,
    ink_flips: usize,
    old_flips: usize,
    prev: Option<(bool, bool, bool)>,
}

impl Burst {
    fn observe(&mut self, f: &Fixture, frame: &PetFrame) {
        let Some(rect) = f.drawn(frame) else {
            self.prev = None;
            return;
        };
        self.drawn += 1;
        let ink = f.ink_under(rect);
        let old = !f.world.clear(rect, 0.0);
        let front = f.brain.front_pass();
        self.over_ink += usize::from(ink && !frame.under_ink && !front);
        self.in_front += usize::from(ink && !frame.under_ink && front);
        if let Some((under, was_ink, was_old)) = self.prev {
            self.flips += usize::from(under != frame.under_ink);
            self.ink_flips += usize::from(was_ink != ink);
            self.old_flips += usize::from(was_old != old);
        }
        self.prev = Some((frame.under_ink, ink, old));
    }
}

fn type_pangram(ms_per_key: u64) -> Burst {
    let mut f = Fixture::console();
    f.term.process(b"$ ");
    f.settle();
    let mut burst = Burst::default();
    for ch in PANGRAM {
        f.brain.note_console_input(f.now, PetInputKind::Text);
        f.term.process(&[*ch]);
        for _ in 0..ms_per_key.div_ceil(16) {
            let frame = f.tick(16);
            burst.observe(&f, &frame);
        }
    }
    for _ in 0..60 {
        let frame = f.tick(16);
        burst.observe(&f, &frame);
    }
    burst
}

/// Typing at human rates never paints the pet over ink and flips z-order at most twice: a
/// writing hand deals no leg in front.
#[test]
fn typing_never_paints_over_ink_and_holds_the_z_order() {
    let bursts = [80, 125, 200].map(|ms| (ms, type_pangram(ms)));
    for (ms, burst) in bursts {
        println!("{ms} ms/key: {burst:?}");
    }
    for (ms, burst) in bursts {
        assert!(burst.drawn > 200, "{ms} ms/key: the pet was barely drawn");
        assert_eq!(burst.over_ink, 0, "{ms} ms/key: painted over ink");
        assert_eq!(
            burst.in_front, 0,
            "{ms} ms/key: crossed the typed line in front"
        );
        assert!(burst.flips <= 2, "{ms} ms/key: z flips {burst:?}");
    }
    let worst = bursts.iter().map(|(_, b)| b.old_flips).max().unwrap_or(0);
    assert!(
        worst >= 10,
        "the control no longer reproduces the strobe: {bursts:?}"
    );
}

/// Ink toggling under a still pet faster than `DWELL` flips z-order only once each way.
#[test]
fn ink_toggling_under_the_body_faster_than_the_dwell_flips_once_each_way() {
    let mut f = Fixture::console();
    f.term.process(b"\x1b[12;10H");
    f.settle();
    let rest = f.tick(16);
    let rect = f.drawn(&rest).expect("a settled pet is on glass");
    assert!(
        !f.ink_under(rect) && !rest.under_ink,
        "starts clear and over"
    );
    let (row, col) = (
        (rect.row + rect.rows * 0.5) as u16 + 1,
        (rect.col + rect.cols * 0.5) as u16 + 1,
    );
    let mut burst = Burst::default();
    burst.observe(&f, &rest);
    for toggle in 0..24 {
        let glyph = if toggle % 2 == 0 { "#" } else { " " };
        f.term
            .process(format!("\x1b7\x1b[{row};{col}H{glyph}\x1b8").as_bytes());
        for _ in 0..5 {
            let frame = f.tick(16);
            burst.observe(&f, &frame);
        }
    }
    let clear_since = f.now - Duration::from_millis(4 * 16);
    for _ in 0..16 {
        if burst.prev.is_some_and(|(under, ..)| !under) {
            break;
        }
        let at = f
            .brain
            .next_change_deadline(f.now)
            .expect("a pet held under offers its surfacing");
        assert!(
            at <= clear_since + DWELL + Duration::from_millis(1),
            "surfacing offered {:?} after the ink left",
            at - clear_since
        );
        let frame = f.tick_at(at);
        burst.observe(&f, &frame);
    }
    println!("toggling ink: {burst:?}");
    assert_eq!(burst.over_ink, 0, "painted over ink: {burst:?}");
    assert!(
        burst.ink_flips >= 20,
        "the ink under the body did not toggle: {burst:?}"
    );
    assert_eq!(burst.flips, 2, "the hold did not hold: {burst:?}");
}

type State = std::collections::BTreeMap<&'static str, i64>;

fn project(f: &Fixture, frame: &PetFrame, prev: &State, under: bool, dwelt: bool) -> State {
    let rect = f.drawn(frame);
    State::from([
        ("drawn", i64::from(rect.is_some())),
        ("ink", i64::from(rect.is_some_and(|r| f.ink_under(r)))),
        ("under", i64::from(rect.is_some() && under)),
        ("prev_under", prev["under"]),
        ("dwelt", i64::from(dwelt)),
        (
            "front",
            i64::from(rect.is_some_and(|r| f.ink_under(r)) && f.brain.front_pass()),
        ),
    ])
}

fn action(post: &State) -> &'static str {
    match (post["drawn"], post["ink"], post["dwelt"]) {
        (0, _, _) => "Hide",
        (_, 1, _) if post["front"] == 1 => "InkOnFrontPass",
        (_, 1, _) => "Ink",
        (_, _, 1) => "ClearDwelt",
        _ => "Clear",
    }
}

struct Walk {
    model: Model,
    state: State,
    run_start: Option<Instant>,
    rejected: usize,
    steps: usize,
}

impl Walk {
    fn new() -> Self {
        let model = console_pet_z_order_model();
        let state = model.init_state();
        Self {
            model,
            state,
            run_start: None,
            rejected: 0,
            steps: 0,
        }
    }

    fn step(&mut self, f: &Fixture, frame: &PetFrame, old_rule: bool) {
        let rect = f.drawn(frame);
        let clear = rect.is_some_and(|r| !f.ink_under(r));
        self.run_start = if clear {
            Some(self.run_start.unwrap_or(f.now))
        } else {
            None
        };
        let dwelt = clear
            && self.state["under"] == 1
            && self
                .run_start
                .is_some_and(|start| f.now.saturating_duration_since(start) >= DWELL);
        let under = if old_rule {
            rect.is_some_and(|r| !f.world.clear(r, 0.0))
        } else {
            frame.under_ink
        };
        let post = project(f, frame, &self.state, under, dwelt);
        let name = action(&post);
        self.steps += 1;
        let ok = self.model.successors(name, &self.state).contains(&post)
            && self
                .model
                .invariants
                .iter()
                .all(|inv| self.model.check_invariant(inv.name, &post));
        if !ok {
            assert!(old_rule, "{name}: {:?} -> {post:?}", self.state);
            self.rejected += 1;
        }
        self.state = post;
    }
}

/// Real z-order transitions conform to `ConsolePetZOrder` while the old per-frame rule is rejected.
#[test]
fn the_real_z_order_conforms_to_the_derived_model_and_rejects_the_old_rule() {
    let mut f = Fixture::console();
    f.term.process(b"$ ");
    f.settle();
    let mut walk = Walk::new();
    let mut control = Walk::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut go = |f: &mut Fixture, walk: &mut Walk, control: &mut Walk| {
        let frame = f.tick(16);
        walk.step(f, &frame, false);
        control.step(f, &frame, true);
        seen.insert(action(&walk.state));
        frame
    };
    for ch in PANGRAM {
        f.brain.note_console_input(f.now, PetInputKind::Text);
        f.term.process(&[*ch]);
        for _ in 0..5 {
            go(&mut f, &mut walk, &mut control);
        }
    }
    for _ in 0..40 {
        go(&mut f, &mut walk, &mut control);
    }
    let rest = go(&mut f, &mut walk, &mut control);
    let rect = f.drawn(&rest).expect("on glass");
    let (row, col) = (
        (rect.row + rect.rows * 0.5) as u16 + 1,
        (rect.col + rect.cols * 0.5) as u16 + 1,
    );
    for toggle in 0..8 {
        let glyph = if toggle % 2 == 0 { "#" } else { " " };
        f.term
            .process(format!("\x1b7\x1b[{row};{col}H{glyph}\x1b8").as_bytes());
        for _ in 0..4 {
            go(&mut f, &mut walk, &mut control);
        }
    }
    for _ in 0..40 {
        go(&mut f, &mut walk, &mut control);
    }
    f.term
        .text_selection_mut()
        .start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    f.term.text_selection_mut().update_selection(
        i32::from(ROWS - 1),
        COLS - 1,
        SelectionSide::Right,
    );
    for _ in 0..10 {
        go(&mut f, &mut walk, &mut control);
    }
    f.term.text_selection_mut().clear();
    for _ in 0..20 {
        go(&mut f, &mut walk, &mut control);
    }
    println!(
        "walk: {} steps, actions {seen:?}; old rule rejected {} of {}",
        walk.steps, control.rejected, control.steps
    );
    for name in ["Ink", "Clear", "ClearDwelt", "Hide"] {
        assert!(seen.contains(name), "the walk never took {name}: {seen:?}");
    }
    assert!(
        control.rejected > 0,
        "the model accepts the pre-fix rule over the same frames"
    );
}

/// Legs the caret sends along a line of words while nobody types cross it in front one time in
/// three and under it otherwise, each frame a `ConsolePetZOrder` step.
#[test]
fn legs_along_a_line_of_words_cross_it_in_front_one_time_in_three() {
    let mut f = Fixture::console();
    for row in 10..=12 {
        f.term.process(format!("\x1b[{row};1H").as_bytes());
        f.term.process(PANGRAM);
    }
    f.term.process(b"\x1b[12;2H");
    f.settle();
    let (mut burst, mut walk) = (Burst::default(), Walk::new());
    let mut seen = std::collections::BTreeSet::new();
    for leg in 0..9 {
        let col = if leg % 2 == 0 { 40 } else { 2 };
        f.term.process(format!("\x1b[12;{col}H").as_bytes());
        for _ in 0..150 {
            let frame = f.tick(16);
            burst.observe(&f, &frame);
            walk.step(&f, &frame, false);
            seen.insert(action(&walk.state));
        }
    }
    println!("legs along words: {burst:?}; actions {seen:?}");
    assert_eq!(
        burst.over_ink, 0,
        "painted over ink off a front pass: {burst:?}"
    );
    assert!(
        burst.in_front > 0,
        "no leg crossed the words in front: {burst:?}"
    );
    for name in ["Ink", "InkOnFrontPass"] {
        assert!(seen.contains(name), "the legs never took {name}: {seen:?}");
    }
}

fn replay(f: &mut Fixture, src: &str, mut each: impl FnMut(&Fixture, &PetFrame)) {
    let t0 = f.now;
    let mut first = None;
    for line in src.lines().filter(|l| !l.is_empty()) {
        let mut it = line.splitn(3, ' ');
        let ns: u64 = it.next().and_then(|s| s.parse().ok()).expect("stamp");
        let kind = it.next().expect("kind");
        let hex = it.next().expect("bytes");
        let bytes: Vec<u8> = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("hex"))
            .collect();
        let at = t0 + Duration::from_nanos(ns - *first.get_or_insert(ns));
        while f.now + Duration::from_millis(16) <= at {
            let frame = f.tick(16);
            each(f, &frame);
        }
        f.now = f.now.max(at);
        match (kind, bytes.as_slice()) {
            ("O", _) => {
                f.term.process(&bytes);
                let frame = f.tick(0);
                each(f, &frame);
            }
            ("I", [key]) if (0x20..0x7f).contains(key) => {
                f.brain.note_console_input(f.now, PetInputKind::Text);
            }
            _ => {}
        }
    }
}

/// Replaying Claude Code's recorded composer conforms to `ConsolePetZOrder` without painting over ink.
#[test]
fn claude_code_s_recorded_composer_conforms_and_never_paints_over_ink() {
    let mut f = Fixture::new(40, 120, (7, 14));
    let (mut burst, mut walk, mut control) = (Burst::default(), Walk::new(), Walk::new());
    let src = include_str!("fixtures/claude-composer-2026-09-21.ptylog");
    replay(&mut f, src, |f, frame| {
        burst.observe(f, frame);
        walk.step(f, frame, false);
        control.step(f, frame, true);
    });
    println!(
        "claude composer: {burst:?}; old rule rejected {} of {}",
        control.rejected, control.steps
    );
    assert!(
        burst.drawn > 500,
        "the replay barely drew the pet: {burst:?}"
    );
    assert_eq!(burst.over_ink, 0, "painted over ink: {burst:?}");
    assert!(
        control.rejected > 0,
        "the model accepts the pre-fix rule over Claude Code's frames"
    );
}
