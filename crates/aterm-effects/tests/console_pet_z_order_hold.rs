// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pet's z-order is held, not recomputed per frame.
//!
//! `PetFrame::under_ink` decides whether the renderer draws the resident beneath the glyphs
//! (ribbon light, caret halo and sparkles over it) or above them. A per-frame `!clear(body)` rule
//! strobes: `clear` counts the caret's 3x3 keep-off ring as occupied, so the typing escort seated
//! at caret+1 flips layers at the keystroke rate with no glyph under it. Measured on a real
//! `Terminal` (typed text and Claude Code's recorded bytes) against an independent `PetWorld` and
//! the derived `ConsolePetZOrder` model, with that per-frame rule replayed over the same frames
//! as the negative control.

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
/// `Z_DWELL` in `console_life.rs`: how long a body clear of ink stays under
/// before it surfaces. The conformance walk holds the code to it.
const DWELL: Duration = Duration::from_millis(250);

struct Fixture {
    brain: PetBrain,
    /// A second, independent world over the same snapshot, so the oracle never asks the brain.
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
        // A presented host resolves its default background; an unresolved
        // COLOR_UNSET cannot certify a blank footprint.
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

    /// The drawn body in fractional cells, or `None` when nothing is on glass.
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

    /// Real glyph occupancy under the drawn body (`ink_at`, ignoring caret and selection
    /// protection). An unobserved cell counts as ink, so the oracle never excuses a body.
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
    /// Drawn frames with a real glyph under the body and the body over it.
    over_ink: usize,
    /// `under_ink` changes between consecutive drawn frames.
    flips: usize,
    /// Changes of the oracle's raw ink reading: what a per-frame ink rule with no hold would flip.
    ink_flips: usize,
    /// The per-frame `!clear(body)` rule replayed over the same frames.
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
        self.over_ink += usize::from(ink && !frame.under_ink);
        if let Some((under, was_ink, was_old)) = self.prev {
            self.flips += usize::from(under != frame.under_ink);
            self.ink_flips += usize::from(was_ink != ink);
            self.old_flips += usize::from(was_old != old);
        }
        self.prev = Some((frame.under_ink, ink, old));
    }
}

/// The pangram at `ms_per_key`, 16 ms frames, then a second of quiet so a
/// held z-order has to land before the burst is scored.
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

/// The pangram at three human rates: no frame paints the pet in front of a glyph under its drawn
/// body, and the burst changes layers at most twice (once under, once back over).
#[test]
fn typing_never_paints_over_ink_and_holds_the_z_order() {
    let bursts = [80, 125, 200].map(|ms| (ms, type_pangram(ms)));
    for (ms, burst) in bursts {
        println!("{ms} ms/key: {burst:?}");
    }
    for (ms, burst) in bursts {
        assert!(burst.drawn > 200, "{ms} ms/key: the pet was barely drawn");
        assert_eq!(burst.over_ink, 0, "{ms} ms/key: painted over ink");
        assert!(burst.flips <= 2, "{ms} ms/key: z flips {burst:?}");
    }
    // Negative control: the old rule over the same frames strobes at a human rate.
    let worst = bursts.iter().map(|(_, b)| b.old_flips).max().unwrap_or(0);
    assert!(
        worst >= 10,
        "the control no longer reproduces the strobe: {bursts:?}"
    );
}

/// Ink blinking under a still pet (a spinner, a toggled glyph) faster than the dwell. The raw ink
/// reading flips on every toggle; the held z-order goes under once and comes back once, a dwell
/// after the ink stops.
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
    // A glyph in the body's middle row, written and erased every 80 ms
    // without moving the caret.
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
    // Quiet from here: no frame train, only the wakes the brain offers. The
    // last erase was read on the first of the five frames above, and a
    // still pet must surface on an offered wake no later than a dwell after.
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

/// Project one real frame onto `ConsolePetZOrder`. `dwelt` is judged by the
/// test's own clock over the run of clear drawn frames, never by the code.
fn project(f: &Fixture, frame: &PetFrame, prev: &State, under: bool, dwelt: bool) -> State {
    let rect = f.drawn(frame);
    State::from([
        ("drawn", i64::from(rect.is_some())),
        ("ink", i64::from(rect.is_some_and(|r| f.ink_under(r)))),
        ("under", i64::from(rect.is_some() && under)),
        ("prev_under", prev["under"]),
        ("dwelt", i64::from(dwelt)),
    ])
}

/// Which model action a real frame is, from the stimulus alone.
fn action(post: &State) -> &'static str {
    match (post["drawn"], post["ink"], post["dwelt"]) {
        (0, _, _) => "Hide",
        (_, 1, _) => "Ink",
        (_, _, 1) => "ClearDwelt",
        _ => "Clear",
    }
}

/// A real frame sequence walked through the model's action relation: every transition is a
/// successor and every invariant holds after it. `old_rule` replaces the emitted z with
/// `!clear(body)`.
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

/// The real resident, under ink arriving, blinking and leaving and a selection that takes it off
/// glass, conforms to the derived model, and the model rejects the old rule over the same frames.
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
    // Typing: ink runs into the body, the escort catches up, the body clears.
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
    // Ink blinking under the still body, then gone for good.
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
    // A selection over everything takes the pet off glass; releasing it
    // brings it back on a fresh reading.
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

/// Replay a `ptyrec` log (`<ns> O|I <hex>`): each output read is presented
/// as it came, a printable key is noted as typed input, and a 16 ms frame
/// train runs between events.
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

/// Claude Code's recorded composer (alt screen, DEC 2026 synchronized output, a line typed at
/// 12 cps, then ⌥← hops into it and `INTO ` inserted): every frame of the real resident is a
/// model step and none paints over ink; the old rule over the same frames is rejected.
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
