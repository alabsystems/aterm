// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Alternate-screen repaint suppression tests for [`PetBrain::note_full_screen`].

use core::time::Duration;
use std::collections::BTreeMap;

use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_lexicon::Lexicon;
use aterm_spec::derive::{Model, Value};
use aterm_time::Instant;

use super::{PetBrain, PetInputKind, PetSense, VIGIL_AFTER, WATCH_GATE, WATCH_HUG_ROWS};
use crate::companion::{CompanionFrame, CompanionOwner, GlowOwnership, PetFacts};
use crate::cursor_glow::GlowStyle;
use crate::host::{CaptureMode, FrameGeom, HostFrameInput, SingFacts, TerminalFacts, Visibility};
use crate::pet_world::{PetPane, PetWorldFacts};
use crate::word_decorations::{DecoConfig, WordDecorations};

fn full_screen_repaint_model() -> Model {
    aterm_spec::ty_model! {
        FullScreenRepaint {
            const Buggy = 0;
            var alt = 0;
            var wrote = 0;
            var stream = 0;
            var hot = 0;
            var seen_alt = 0;
            var seen_write = 0;
            var burst = 0;
            var watch = 0;
            var hug = 0;
            var vigil = 0;
            action Enter when (alt == 0) { alt = 1; }
            action Leave when (alt == 1) { alt = 0; }
            action Write when (wrote == 0) { wrote = 1; }
            action StreamOn when (stream == 0) { stream = 1; }
            action StreamOff when (stream == 1) { stream = 0; }
            action Heat when (burst == 1 && hot == 0) { hot = 1; }
            action Cool when (hot == 1) { hot = 0; }
            action Capped when (watch == 1 && vigil == 0) { vigil = 1; }
            action Present {
                seen_alt = alt;
                seen_write = wrote;
                wrote = 0;
                burst = if Buggy == 2 {
                    0
                } else {
                    if wrote == 1 && (alt == 0 || Buggy == 1) { 1 } else { 0 }
                };
                hot = if alt == 1 && seen_alt == 0 && (Buggy == 0 || Buggy == 2) {
                    0
                } else {
                    hot
                };
                watch = if alt == 1 && seen_alt == 0 && (Buggy == 0 || Buggy == 2) {
                    0
                } else {
                    hot
                };
                vigil = if alt == 1 && seen_alt == 0 && (Buggy == 0 || Buggy == 2) {
                    0
                } else {
                    vigil
                };
                hug = if alt == 0 || Buggy == 1 || Buggy == 3 {
                    if stream == 1 { 1 } else { hot }
                } else {
                    0
                };
            }
            invariant FullScreenIsNotOutput: seen_alt == 0 || burst == 0;
            invariant NoStareOnFullScreen: seen_alt == 0 || watch == 0;
            invariant NoHugOnFullScreen: seen_alt == 0 || hug == 0;
            invariant NoVigilOnFullScreen: seen_alt == 0 || vigil == 0;
            invariant MainScreenStreamIsWatched: seen_alt == 1 || burst == seen_write;
        }
    }
}

const FULL_SCREEN_INVARIANTS: [&str; 4] = [
    "FullScreenIsNotOutput",
    "NoStareOnFullScreen",
    "NoHugOnFullScreen",
    "NoVigilOnFullScreen",
];

#[test]
fn full_screen_repaint_model_proves_and_catches() {
    let model = full_screen_repaint_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    let catches = |inv: &str, buggy: i64| {
        let mut alone = aterm_spec::interp::with_buggy(&model, buggy);
        alone.invariants.retain(|i| i.name == inv);
        aterm_spec::interp::bmc(&alone).is_err()
    };
    for inv in FULL_SCREEN_INVARIANTS {
        assert!(catches(inv, 1), "the shipped law must break {inv}");
    }
    for inv in &FULL_SCREEN_INVARIANTS[1..] {
        assert!(catches(inv, 3), "the burst law alone must break {inv}");
    }
    assert!(!catches("FullScreenIsNotOutput", 3));
    assert!(catches("MainScreenStreamIsWatched", 2));
    assert!(!catches("MainScreenStreamIsWatched", 1));
}

const ROWS: u16 = 40;
const COLS: u16 = 120;
const SESSION: u64 = 7;
const VSYNC: Duration = Duration::from_millis(16);
const REPAINT: Duration = Duration::from_millis(33);
const TORN_OPEN: Duration = Duration::from_millis(280);

const TAKE: &str = include_str!("../../tests/fixtures/claude-composer-2026-09-21.ptylog");

#[derive(Clone, Copy)]
struct Run {
    inline: bool,
    blind: bool,
    answer: Duration,
    monitor: Duration,
    torn: bool,
}

impl Run {
    const FULL_SCREEN: Self = Self {
        inline: false,
        blind: false,
        answer: Duration::from_secs(20),
        monitor: Duration::ZERO,
        torn: false,
    };
}

#[derive(Debug, Default)]
struct Take {
    frames: u32,
    alt_frames: u32,
    bursts: u32,
    stares: u32,
    hugs: u32,
    vigil_armed: u32,
    vigil_crouch: u32,
    alt_crouch: u32,
    alt_stream: u32,
    max_row: f32,
    composer: u16,
    executed: f32,
    violations: BTreeMap<&'static str, u32>,
}

struct Host {
    term: Terminal,
    owner: CompanionOwner,
    decos: WordDecorations,
    lexicon: Lexicon,
    cfg: DecoConfig,
    model: Model,
    blind: bool,
    now: Instant,
    vsync: Instant,
    dirty: bool,
    scroll: Option<ContentScrollState>,
    seq: Option<u64>,
    scanned: Option<u64>,
    take: Take,
}

impl Host {
    fn new(blind: bool) -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"\x1b]11;#000000\x07");
        let mut owner = CompanionOwner::default();
        owner.set_enabled_seed(true, 0x5EED);
        let now = Instant::now();
        Self {
            term,
            owner,
            decos: WordDecorations::default(),
            lexicon: Lexicon::with_languages(&["en"]),
            cfg: DecoConfig::default(),
            model: full_screen_repaint_model(),
            blind,
            now,
            vsync: now,
            dirty: false,
            scroll: None,
            seq: None,
            scanned: None,
            take: Take::default(),
        }
    }

    fn present(&mut self) {
        let state = self.term.content_scroll_state();
        let scrolled = matches!(
            ContentScrollState::delta_since(self.scroll, state),
            ContentScrollDelta::Translate(_)
        );
        self.scroll = Some(state);
        let mut facts = TerminalFacts::read(&self.term, SESSION, scrolled);
        let alt = facts.alt_screen;
        facts.alt_screen &= !self.blind;
        let wrote = self.seq.is_some_and(|s| facts.content_seq > s);
        self.seq = Some(facts.content_seq);
        let input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let world = PetWorldFacts::read(&self.term, SESSION);
        let _ = self
            .owner
            .observe_console(&input, &world, PetPane::full(&input), &[]);
        let host = HostFrameInput {
            now: self.now,
            visibility: Visibility::Focused,
            reduced_motion: false,
            serious: false,
            shed_envelope: 1.0,
            shed_active: false,
            pointer_px: None,
            capture: CaptureMode::Present,
            geometry: FrameGeom {
                rows: ROWS,
                cols: COLS,
                cell_w: 7,
                cell_h: 14,
                origin_px: (0, 0),
            },
        };
        let stare_before = self.owner.brain().watch_t;
        let tick = self.owner.prepare(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: GlowOwnership {
                    enabled: true,
                    style: GlowStyle::RainbowKitty,
                    style_raw_names_pet: true,
                },
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: None,
                room: None,
            },
            &mut self.decos,
        );
        let burst = tick.output_burst();
        let frame = self.owner.tick(tick);
        if self.scanned != Some(input.content_seq) {
            self.scanned = Some(input.content_seq);
            self.decos.rescan_from_cells(
                &input.cells,
                &input.line_sizes,
                usize::from(ROWS),
                usize::from(COLS),
                &self.lexicon,
                &self.cfg,
                input.content_seq,
                self.now,
            );
        }
        if facts.shell_executing && facts.live_viewport {
            let brain = self.owner.brain();
            let stare = brain.watch_t > stare_before;
            self.take
                .observe(&self.model, brain, &frame, (alt, wrote, burst, stare));
        }
    }

    fn run_to(&mut self, t: Instant) {
        while self.vsync <= t {
            let at = self.vsync;
            let due = self.dirty
                || self.owner.needs_frames()
                || self
                    .owner
                    .next_change_deadline(self.now)
                    .is_some_and(|d| d <= at);
            if due {
                self.now = at;
                self.present();
                self.dirty = false;
            }
            self.vsync += VSYNC;
        }
        self.now = self.now.max(t);
    }

    fn output(&mut self, at: Instant, bytes: &[u8]) {
        self.run_to(at);
        self.term.process(bytes);
        self.dirty = true;
    }

    fn key(&mut self, at: Instant, bytes: &[u8]) {
        self.run_to(at);
        let kind = match bytes {
            [c] if c.is_ascii_graphic() || *c == b' ' => PetInputKind::Text,
            [0x1b, b'[', .., b'C' | b'D'] => PetInputKind::Navigate,
            _ => return,
        };
        if self.take.composer == 0 {
            self.take.composer = self.term.cursor().row;
        }
        self.owner.note_console_input(self.now, kind);
    }
}

impl Take {
    fn observe(
        &mut self,
        model: &Model,
        brain: &PetBrain,
        frame: &CompanionFrame,
        (alt, wrote, burst, stare): (bool, bool, bool, bool),
    ) {
        let hug = brain.hug_armed;
        self.frames += 1;
        self.alt_frames += u32::from(alt);
        self.bursts += u32::from(burst);
        self.stares += u32::from(stare);
        self.hugs += u32::from(hug);
        self.vigil_armed += u32::from(brain.vigil_armed);
        self.vigil_crouch += u32::from(brain.vigil);
        self.alt_crouch += u32::from(brain.vigil && alt);
        self.alt_stream += u32::from(brain.stream && alt);
        if frame.on_glass && self.composer != 0 {
            self.max_row = self.max_row.max(frame.pet.row);
        }
        let projected: BTreeMap<&'static str, i64> = [
            ("alt", alt),
            ("wrote", false),
            ("stream", brain.stream),
            ("hot", brain.watch_heat >= WATCH_GATE),
            ("seen_alt", alt),
            ("seen_write", wrote),
            ("burst", burst),
            ("watch", stare),
            ("hug", hug),
            ("vigil", brain.vigil_armed),
        ]
        .into_iter()
        .map(|(k, v)| (k, i64::from(v)))
        .collect();
        let env = model.eval_env(&projected);
        for inv in &model.invariants {
            if !matches!(inv.expr.eval(&env), Value::Bool(true)) {
                *self.violations.entry(inv.name).or_default() += 1;
            }
        }
    }
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

fn replay(run: Run) -> Take {
    let mut h = Host::new(run.blind);
    let t0 = h.now;
    h.output(
        t0,
        b"\x1b]133;A\x07$ \x1b]133;B\x07claude\r\n\x1b]133;C\x07",
    );
    let t0 = t0 + Duration::from_millis(50);
    let mut first = None;
    for line in TAKE.lines().filter(|l| !l.trim().is_empty()) {
        let mut it = line.split_whitespace();
        let ns: u64 = it.next().and_then(|n| n.parse().ok()).expect("timestamp");
        let dir = it.next().expect("direction");
        let mut bytes = unhex(it.next().unwrap_or(""));
        if dir == "I" && bytes == b"\x1b[99;5u" {
            break;
        }
        let at = t0 + Duration::from_nanos(ns - *first.get_or_insert(ns));
        if dir == "I" {
            h.key(at, &bytes);
            continue;
        }
        if run.inline
            && let Some(i) = bytes.windows(8).position(|w| w == b"\x1b[?1049h")
        {
            bytes.drain(i..i + 8);
        }
        h.output(at, &bytes);
    }
    assert_eq!(
        h.term.is_alternate_screen(),
        !run.inline,
        "fixture: the take is on the screen it claims"
    );
    let caret = h.term.cursor();
    let glyphs = ["·", "✢", "✳", "✶", "✻", "✽"];
    let start = h.now;
    let mut at = start;
    let (mut row, mut col) = (2u16, 3u16);
    let mut k = 0usize;
    while at.saturating_duration_since(start) < run.answer {
        let secs = at.saturating_duration_since(start).as_secs();
        let frame = format!(
            "\x1b[?2026h\x1b[?25l\x1b[{row};{col}Htoken \x1b[35;1H\x1b[2K{} Thinking… \
             ({secs}s · ↓ {k} tokens · esc to interrupt)\x1b[{};{}H\x1b[?25h\x1b[?2026l",
            glyphs[k % glyphs.len()],
            caret.row + 1,
            caret.col + 1,
        );
        col += 6;
        if col > 110 {
            col = 3;
            row = if row >= 33 { 2 } else { row + 1 };
        }
        if run.torn {
            let frame = frame.replace("\x1b[?2026h", "").replace("\x1b[?2026l", "");
            let cut = frame.find("\x1b[35;1H").expect("the spinner line");
            h.output(at, &frame.as_bytes()[..cut]);
            h.output(at + TORN_OPEN, &frame.as_bytes()[cut..]);
            at += TORN_OPEN + REPAINT;
        } else {
            at += REPAINT;
            h.output(at, frame.as_bytes());
        }
        k += 1;
    }
    if !run.monitor.is_zero() {
        let mut page = String::from("\x1b[?1049h\x1b[2J\x1b[H");
        for r in 1..=38 {
            page.push_str(&format!("\x1b[{r};1Hproc-{r:02}  0.{r}%  idle"));
        }
        h.output(at, page.as_bytes());
        for s in 0..run.monitor.as_secs() {
            at += Duration::from_secs(1);
            let clock = format!("\x1b[40;1H\x1b[2Kup {s}s  q to quit\x1b[40;18H");
            h.output(at, clock.as_bytes());
        }
    }
    h.run_to(at + VSYNC);
    h.take.executed = h.now.saturating_duration_since(t0).as_secs_f32();
    h.take
}

#[test]
fn a_full_screen_repaint_is_not_output() {
    let run = Run::FULL_SCREEN;
    let t = replay(run);
    println!("full screen: {t:?}");
    assert!(
        t.alt_frames > 1000 && t.frames - t.alt_frames < 30,
        "fixture: the replay presented on the alt screen: {t:?}"
    );
    assert!(
        t.executed > VIGIL_AFTER + 2.0,
        "fixture: the replay outlasts the vigil's wait: {t:?}"
    );
    assert_eq!(t.bursts, 0, "a TUI repaint read as output: {t:?}");
    assert_eq!(t.stares, 0, "the stare held on the full screen: {t:?}");
    assert_eq!(t.hugs, 0, "the live-edge hug armed: {t:?}");
    assert!(
        t.max_row < f32::from(t.composer) + 1.0,
        "the cat left the composer row for the rows under it: {t:?}"
    );
    assert_eq!(t.vigil_armed, 0, "a vigil armed on the full screen: {t:?}");
    assert_eq!(t.vigil_crouch, 0, "the cat crouched in vigil: {t:?}");
    assert!(t.violations.is_empty(), "{:?}", t.violations);
}

#[test]
fn the_shipped_law_reads_the_same_take_as_a_stream() {
    let t = replay(Run {
        blind: true,
        ..Run::FULL_SCREEN
    });
    println!("shipped law: {t:?}");
    assert!(t.bursts > 100, "{t:?}");
    assert!(t.stares > 100, "{t:?}");
    assert!(t.hugs > 0, "{t:?}");
    assert!(
        t.max_row >= f32::from(t.composer) + WATCH_HUG_ROWS,
        "the control reaches the footer: {t:?}"
    );
    assert!(t.vigil_armed > 0 && t.alt_crouch > 0, "{t:?}");
    for inv in FULL_SCREEN_INVARIANTS {
        assert!(
            t.violations.get(inv).is_some_and(|&n| n > 0),
            "the control must break {inv}: {:?}",
            t.violations
        );
    }
}

#[test]
fn a_main_screen_stream_still_perks_hugs_and_waits() {
    let t = replay(Run {
        inline: true,
        ..Run::FULL_SCREEN
    });
    println!("main screen: {t:?}");
    assert_eq!(t.alt_frames, 0, "fixture: nothing on the alt screen");
    assert!(t.bursts > 100, "{t:?}");
    assert!(t.stares > 100, "{t:?}");
    assert!(t.hugs > 0, "{t:?}");
    assert!(
        t.max_row >= f32::from(t.composer) + WATCH_HUG_ROWS,
        "the hug still reaches the live edge: {t:?}"
    );
    assert!(t.vigil_armed > 0 && t.vigil_crouch > 0, "{t:?}");
    assert!(t.violations.is_empty(), "{:?}", t.violations);
}

#[test]
fn a_torn_redraw_stream_arms_no_hug() {
    let t = replay(Run {
        torn: true,
        ..Run::FULL_SCREEN
    });
    println!("torn: {t:?}");
    assert!(
        t.alt_stream > 100 && t.bursts == 0,
        "fixture: the caret's stream is up on the full screen, the heat is not: {t:?}"
    );
    assert_eq!(t.hugs, 0, "the stream armed the live-edge hug: {t:?}");
    assert!(t.violations.is_empty(), "{:?}", t.violations);
}

#[test]
fn a_full_screen_program_stands_down_a_vigil_armed_before_it() {
    let run = Run {
        inline: true,
        blind: false,
        answer: Duration::from_secs(12),
        monitor: Duration::from_secs(10),
        torn: false,
    };
    let t = replay(run);
    println!("inline, then monitor: {t:?}");
    assert!(
        t.alt_frames > 0 && t.vigil_armed > 0 && t.executed > VIGIL_AFTER + 2.0,
        "fixture: the stream armed the vigil before the monitor, which outlasts its wait: {t:?}"
    );
    assert_eq!(
        t.alt_crouch, 0,
        "the cat crouched in vigil on the full screen: {t:?}"
    );
    assert!(t.violations.is_empty(), "{:?}", t.violations);

    let control = replay(Run { blind: true, ..run });
    println!("control: {control:?}");
    assert!(
        control
            .violations
            .get("NoVigilOnFullScreen")
            .is_some_and(|&n| n > 0)
            && control.alt_crouch > 0,
        "the control keeps its vigil on the full screen: {control:?}"
    );
}

#[test]
fn a_live_stare_lets_go_the_tick_the_full_screen_comes_up() {
    fn sense(now: Instant, col: u16, burst: bool) -> PetSense {
        PetSense {
            caret_drawn: true,
            now,
            caret: Some((2, col)),
            wrapped: false,
            rows: 30,
            cols: 100,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: burst,
            pointer: None,
        }
    }
    let heated = || {
        let mut pet = PetBrain::default();
        let mut t = Instant::now();
        pet.note_executing(t, true);
        for k in 0..220u16 {
            t += VSYNC;
            let col = 10 + (k / 3).min(2);
            let _ = pet.tick(sense(t, col, k >= 160));
        }
        assert!(
            pet.watch_t > 0.0 && !pet.watch_spent,
            "fixture: the stare is live: {:?}",
            pet.action
        );
        (pet, t)
    };
    for told in [true, false] {
        let (mut pet, t) = heated();
        pet.note_full_screen(told);
        let stare = pet.watch_t;
        let _ = pet.tick(sense(t + VSYNC, 12, false));
        assert_eq!(pet.watch_t > stare, !told, "the stare, told={told}");
        let (mut pet, t) = heated();
        pet.sense_ink(0, &[(0, 14); 13], Some(12));
        pet.note_full_screen(told);
        let _ = pet.tick(sense(t + VSYNC, 12, false));
        assert_eq!(pet.hug_armed, !told, "the hug, told={told}");
        let mut pet = PetBrain::default();
        let mut t = Instant::now();
        for k in 0..160u16 {
            t += VSYNC;
            let _ = pet.tick(sense(t, 10 + (k / 3).min(2), false));
        }
        pet.sense_ink(0, &[(0, 14); 13], Some(12));
        pet.note_full_screen(told);
        let mut hugs = 0;
        for k in 0..40u16 {
            t += VSYNC;
            let row = 3 + k % 2;
            let _ = pet.tick(PetSense {
                caret: Some((row, 12)),
                ..sense(t, 12, false)
            });
            hugs += u32::from(pet.hug_armed);
        }
        assert!(
            pet.stream && pet.watch_heat == 0.0,
            "fixture: the stream is up, the heat is cold"
        );
        assert_eq!(hugs > 0, !told, "the stream's hug, told={told}: {hugs}");
    }
}
